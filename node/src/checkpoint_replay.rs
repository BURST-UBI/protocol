//! Checkpoint commitments derived from authenticated, dependency-closed history.
//! Unsupported lifecycle transitions fail replay rather than inheriting a peer's
//! roster, pool, parameters, or the local node's provisional account snapshots.

use burst_consensus::checkpoint::{Checkpoint, CheckpointAnchor, HumanRoster};
use burst_consensus::checkpoint_round::Phase;
use burst_store::StoreError;
use burst_types::{ProtocolParams, WalletAddress};
use burst_verification::selection_request::SelectionRequest;
use serde::{Deserialize, Serialize};

mod receipt;
pub(crate) use receipt::SelectionReceipt;

use crate::checkpoint_agreement::Validation;
use crate::confirmed_history::{ConfirmedHistory, HistoryHead};
use crate::confirmed_replay::ConfirmedReplay;

/// Untrusted replay input. Only replaying the named signed history grants it
/// authority; deserialization and a matching checkpoint hash do not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointManifest {
    pub checkpoint: Checkpoint,
    pub heads: Vec<HistoryHead>,
    pub target: WalletAddress,
}

#[derive(Clone)]
pub struct ReplayedCheckpoint {
    checkpoint: Checkpoint,
    roster: HumanRoster,
    request: SelectionRequest,
    heads: Vec<HistoryHead>,
    endorsements: burst_types::BlockHash,
    parent_roster: burst_types::BlockHash,
    parent_quorum_bps: u16,
    bootstrap_complete: bool,
    selection_history: burst_types::BlockHash,
    lifecycle: SelectionLifecycle,
    payment_epoch: burst_types::BlockHash,
    history_hashes: Option<std::collections::HashSet<burst_types::BlockHash>>,
}

struct SelectionCut<'a> {
    cutoff: u64,
    commitment: burst_types::BlockHash,
    heads: &'a [HistoryHead],
    history_hashes: Option<std::collections::HashSet<burst_types::BlockHash>>,
}

/// Derived from the authenticated cut, never accepted as a peer-supplied label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum SelectionLifecycle {
    Verification(burst_types::BlockHash),
    Challenge(burst_types::BlockHash),
}

/// An absent clock bound prohibits fresh signatures. A measured offset alone
/// is not a bound on error; the clock service must supply its uncertainty too.
#[derive(Clone, Copy)]
pub struct SigningClock {
    pub now: u64,
    pub error_bound: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct ObservedSigningClock {
    clock: SigningClock,
    observed: std::time::Instant,
}

impl ObservedSigningClock {
    pub(crate) fn new(clock: SigningClock) -> Self {
        Self {
            clock,
            observed: std::time::Instant::now(),
        }
    }

    pub(crate) fn current(self) -> Option<SigningClock> {
        self.clock.after(self.observed.elapsed())
    }
}

impl SigningClock {
    /// Keep the upper bound conservative while replay or a writer lock takes
    /// time. An offset measured before that work is not a fresh observation.
    fn after(self, elapsed: std::time::Duration) -> Option<Self> {
        let seconds = elapsed
            .as_secs()
            .checked_add(u64::from(elapsed.subsec_nanos() != 0))?;
        Some(Self {
            now: self.now,
            error_bound: self.error_bound.checked_add(seconds)?,
        })
    }
}

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}

impl ReplayedCheckpoint {
    pub fn genesis_anchor(
        network: burst_types::NetworkId,
        initial_params: &ProtocolParams,
    ) -> Result<CheckpointAnchor, StoreError> {
        use burst_consensus::checkpoint::HumanDelegation;
        let genesis = crate::genesis_state::block(network, initial_params);
        let roster = HumanRoster::new(vec![HumanDelegation {
            human: genesis.account.clone(),
            representative: genesis.account,
        }])
        .map_err(invalid)?;
        CheckpointAnchor::genesis(network, genesis.hash, 0, roster, 6700).map_err(invalid)
    }

    /// The history reader must also check extension of the trusted parent's
    /// heads. `initial_params` comes from genesis configuration, not the peer.
    pub fn replay(
        history: &ConfirmedHistory,
        anchor: &CheckpointAnchor,
        initial_params: &ProtocolParams,
        target: &WalletAddress,
    ) -> Result<Self, StoreError> {
        if anchor.height() != 0 {
            return Err(invalid(
                "successor selection requires replay of the prior certified lifecycle",
            ));
        }
        let genesis = Self::genesis_anchor(anchor.network(), initial_params)?;
        if anchor.roster() != genesis.roster() || anchor.quorum_bps() != genesis.quorum_bps() {
            return Err(invalid(
                "initial checkpoint authority differs from configured genesis",
            ));
        }
        let replay =
            ConfirmedReplay::replay(history, anchor.network(), initial_params).map_err(invalid)?;
        Self::from_replay(history, anchor, initial_params, target, replay, &[])
    }

    pub(crate) fn from_replay(
        history: &ConfirmedHistory,
        anchor: &CheckpointAnchor,
        _initial_params: &ProtocolParams,
        target: &WalletAddress,
        mut replay: ConfirmedReplay,
        ancestors: &[&ReplayedCheckpoint],
    ) -> Result<Self, StoreError> {
        if history.network() != anchor.network() || history.genesis() != anchor.genesis_hash() {
            return Err(invalid(
                "checkpoint history does not match its trusted anchor",
            ));
        }
        Self::from_cut(
            SelectionCut {
                cutoff: history.cutoff(),
                commitment: history.commitment(),
                heads: history.heads(),
                history_hashes: Some(
                    history
                        .replay_blocks()
                        .iter()
                        .map(|block| block.hash)
                        .collect(),
                ),
            },
            anchor,
            target,
            &mut replay,
            ancestors.iter().map(|old| Ok(std::borrow::Cow::Borrowed(*old))),
        )
    }

    pub(crate) fn from_compact(
        tail: &crate::confirmed_replay::snapshot::tail::SnapshotTail,
        anchor: &CheckpointAnchor,
        target: &WalletAddress,
        replay: &mut ConfirmedReplay,
        ancestors: &(impl crate::confirmed_replay::snapshot::SelectionSource + ?Sized),
    ) -> Result<Self, StoreError> {
        let context = tail.context();
        if context.network != anchor.network()
            || context.genesis != anchor.genesis_hash()
            || context.epoch != anchor.hash()
        {
            return Err(invalid(
                "compact selection differs from its authenticated base",
            ));
        }
        Self::from_cut(
            SelectionCut {
                cutoff: tail.cutoff(),
                commitment: tail.commitment(),
                heads: tail.heads(),
                history_hashes: None,
            },
            anchor,
            target,
            replay,
            ancestors.read_all().map(|certificate| {
                let certificate = certificate.map_err(invalid)?;
                Ok(match certificate {
                    std::borrow::Cow::Borrowed(certificate) =>
                        std::borrow::Cow::Borrowed(certificate.replayed()),
                    std::borrow::Cow::Owned(certificate) =>
                        std::borrow::Cow::Owned(certificate.replayed().clone()),
                })
            }),
        )
    }

    fn from_cut<'a>(
        cut: SelectionCut<'_>,
        anchor: &CheckpointAnchor,
        target: &WalletAddress,
        replay: &mut ConfirmedReplay,
        ancestors: impl IntoIterator<Item = Result<std::borrow::Cow<'a, ReplayedCheckpoint>, StoreError>>,
    ) -> Result<Self, StoreError> {
        if replay.payment_epoch() != anchor.hash() {
            return Err(invalid(
                "selection payment authority differs from its certified parent",
            ));
        }
        let roster = replay
            .human_roster()
            .map_err(invalid)?
            .reuse_if_equal(anchor.roster());
        let lifecycle = replay.selection_lifecycle(target).map_err(invalid)?;
        let request = replay.prepare_selection(target).map_err(invalid)?;
        for old in ancestors {
            let old = old?;
            if !replay
                .has_endorsement_commitment(old.request.target(), old.endorsements)
                .map_err(invalid)?
            {
                return Err(invalid("history changed an already frozen endorsement set"));
            }
            if old.request().target() == target
                && old.lifecycle == lifecycle
                && old.request().attempt() >= request.attempt()
            {
                return Err(invalid("selection attempt is already certified"));
            }
        }
        let endorsements = replay.endorsement_commitment(target).map_err(invalid)?;
        let selection_history = replay
            .selection_history_commitment(target)
            .map_err(invalid)?;
        let checkpoint = Checkpoint {
            network: anchor.network(),
            genesis: anchor.genesis_hash(),
            parent: anchor.hash(),
            height: anchor
                .height()
                .checked_add(1)
                .ok_or_else(|| invalid("checkpoint height exhausted"))?,
            cutoff: cut.cutoff,
            ledger_root: cut.commitment,
            params_hash: replay.params().params_hash(),
            next_roster: roster.commitment(),
            selection_root: request.commitment().map_err(invalid)?,
            next_quorum_bps: anchor.quorum_bps(),
        };
        anchor.validate_successor(&checkpoint).map_err(invalid)?;
        Ok(Self {
            checkpoint,
            roster,
            request,
            heads: cut.heads.to_vec(),
            endorsements,
            parent_roster: anchor.roster().commitment(),
            parent_quorum_bps: anchor.quorum_bps(),
            bootstrap_complete: replay.bootstrap_complete(),
            selection_history,
            lifecycle,
            payment_epoch: replay.payment_epoch(),
            history_hashes: cut.history_hashes,
        })
    }

    pub fn checkpoint(&self) -> &Checkpoint {
        &self.checkpoint
    }

    pub(crate) fn payment_epoch(&self) -> burst_types::BlockHash {
        self.payment_epoch
    }

    pub(crate) fn history_hashes(
        &self,
    ) -> Result<&std::collections::HashSet<burst_types::BlockHash>, StoreError> {
        self.history_hashes.as_ref().ok_or_else(|| {
            invalid("compact recovery has no complete history inventory for local signing")
        })
    }

    pub(crate) fn is_compact(&self) -> bool {
        self.history_hashes.is_none()
    }

    pub fn roster(&self) -> &HumanRoster {
        &self.roster
    }

    pub(crate) fn bootstrap_complete(&self) -> bool {
        self.bootstrap_complete
    }

    pub fn request(&self) -> &SelectionRequest {
        &self.request
    }

    pub(crate) fn lifecycle(&self) -> SelectionLifecycle {
        self.lifecycle
    }

    pub(crate) fn selection_history_commitment(&self) -> burst_types::BlockHash {
        self.selection_history
    }

    pub fn manifest(&self) -> CheckpointManifest {
        CheckpointManifest {
            checkpoint: self.checkpoint.clone(),
            heads: self.heads.clone(),
            target: self.request.target().clone(),
        }
    }

    pub(crate) fn validate_parent(&self, anchor: &CheckpointAnchor) -> Result<(), StoreError> {
        self.history_hashes()?;
        self.validate_parent_commitments(anchor)
    }

    pub(crate) fn validate_parent_commitments(
        &self,
        anchor: &CheckpointAnchor,
    ) -> Result<(), StoreError> {
        if anchor.roster().commitment() != self.parent_roster
            || anchor.quorum_bps() != self.parent_quorum_bps
        {
            return Err(invalid(
                "signing authority differs from the replayed checkpoint parent",
            ));
        }
        anchor.validate_successor(&self.checkpoint).map_err(invalid)
    }

    pub(crate) fn validate(
        &self,
        anchor: &CheckpointAnchor,
        input: Validation<'_>,
        clock: Option<SigningClock>,
    ) -> Result<(), StoreError> {
        self.history_hashes()?;
        self.validate_commitments(anchor, input, clock)
    }

    pub(crate) fn validate_commitments(
        &self,
        anchor: &CheckpointAnchor,
        input: Validation<'_>,
        clock: Option<SigningClock>,
    ) -> Result<(), StoreError> {
        if input.checkpoint != &self.checkpoint {
            return Err(invalid(
                "checkpoint commitments differ from confirmed replay",
            ));
        }
        if input.decided {
            // The signing transition verifies the precommit certificate before
            // invoking this predicate. Import must still replay its exact cut.
            return Ok(());
        }
        if let Some(proof) = input.proof {
            proof.verify(anchor).map_err(invalid)?;
            if proof.ballot.phase != Phase::Prevote
                || proof.ballot.value != Some(self.checkpoint.hash())
            {
                return Err(invalid(
                    "prepare proof does not bind the replayed checkpoint",
                ));
            }
            return Ok(());
        }
        let clock =
            clock.ok_or_else(|| invalid("fresh selection requires a trusted clock bound"))?;
        self.request
            .validate_before_signing(&self.checkpoint, clock.now, clock.error_bound)
            .map_err(invalid)
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn observed_clock_bounds_include_replay_and_lock_wait_time() {
        let clock = SigningClock {
            now: 100,
            error_bound: 2,
        };
        for (elapsed, expected) in [
            (Duration::ZERO, 2),
            (Duration::from_nanos(1), 3),
            (Duration::from_secs(2), 4),
            (Duration::from_millis(2001), 5),
        ] {
            let aged = clock.after(elapsed).unwrap();
            assert_eq!(aged.now, 100);
            assert_eq!(aged.error_bound, expected);
        }
        assert!(SigningClock {
            now: 100,
            error_bound: u64::MAX
        }
        .after(Duration::from_nanos(1))
        .is_none());
        assert!(clock.after(Duration::MAX).is_none());
    }
}
