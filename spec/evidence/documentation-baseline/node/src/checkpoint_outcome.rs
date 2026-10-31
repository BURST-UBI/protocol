//! Replay and authenticate an upheld challenge or completed appeal at an exact
//! ledger boundary. Restoration also needs the previous human roster's quorum.
//!
//! This is a read-only candidate/certificate boundary. Live installation and
//! post-outcome payment epochs are deliberately not exposed by this module.

use burst_consensus::checkpoint::{
    Checkpoint, CheckpointAnchor, CheckpointCertificate, HumanRoster,
};
use burst_store::StoreError;
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, NetworkId, ProtocolParams, WalletAddress};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[cfg(test)]
use crate::checkpoint_store::CertifiedCheckpoint;
use crate::checkpoint_store::{chain, MAX_CHECKPOINTS};
use crate::confirmed_history::{ConfirmedHistory, HistoryHead};
use crate::confirmed_replay::ConfirmedReplay;

/// The outcome domain is distinct from a verifier-selection request, even when
/// both use the same account heads and target. Deserialization grants no trust.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChallengeOutcomeManifest {
    pub checkpoint: Checkpoint,
    pub heads: Vec<HistoryHead>,
    pub target: WalletAddress,
}

pub struct PreparedChallengeOutcome {
    manifest: ChallengeOutcomeManifest,
    parent: CheckpointAnchor,
    roster: HumanRoster,
    state: ConfirmedReplay,
    history_hashes: HashSet<BlockHash>,
}

/// Constructible only after replay and the previous roster's quorum signature
/// check. It authenticates this cut, not a live account tail or a new payment.
pub struct CertifiedChallengeOutcome {
    prepared: PreparedChallengeOutcome,
    certificate: CheckpointCertificate,
}

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}

impl PreparedChallengeOutcome {
    /// Choose only a complete upheld panel reconstructed from confirmed blocks.
    /// Discovery does not settle live balances or grant a signing exception.
    pub(crate) fn prepare_next(
        store: &LmdbStore,
        network: NetworkId,
        params: &ProtocolParams,
        cutoff: u64,
    ) -> Result<Option<Self>, StoreError> {
        let ancestors = chain::load(store, network, params)?;
        Self::prepare_next_in_chain(store, network, params, cutoff, &ancestors)
    }

    pub(crate) fn prepare_next_in_chain(
        store: &LmdbStore,
        network: NetworkId,
        params: &ProtocolParams,
        cutoff: u64,
        ancestors: &[chain::CertifiedEvent],
    ) -> Result<Option<Self>, StoreError> {
        let Some(parent) = ancestors.last() else {
            return Ok(None);
        };
        let anchor = chain::next_anchor(ancestors, network, params)?;
        if cutoff <= parent.certificate().checkpoint.cutoff {
            return Ok(None);
        }
        let heads = ConfirmedHistory::heads_at(store, cutoff)?;
        let history = ConfirmedHistory::read(
            store,
            network,
            anchor.genesis_hash(),
            cutoff,
            &heads,
            parent.heads(),
        )?;
        let replay = ConfirmedReplay::replay_with_events(&history, network, params, ancestors)
            .map_err(invalid)?;
        let Some(target) = replay.upheld_targets().into_iter().next() else {
            return Ok(None);
        };
        Self::from_replay(&history, &anchor, params, &target, replay).map(Some)
    }

    pub fn prepare(
        store: &LmdbStore,
        network: NetworkId,
        initial_params: &ProtocolParams,
        cutoff: u64,
        heads: &[HistoryHead],
        target: &WalletAddress,
    ) -> Result<Self, StoreError> {
        let ancestors = chain::load(store, network, initial_params)?;
        let parent = ancestors
            .last()
            .ok_or_else(|| invalid("challenge outcome needs certified selection ancestry"))?;
        let history = ConfirmedHistory::read(
            store,
            network,
            parent.certificate().checkpoint.genesis,
            cutoff,
            heads,
            parent.heads(),
        )?;
        Self::from_events(&history, initial_params, &ancestors, target)
    }

    /// Validate received input against configured genesis and the exact parent
    /// prefix, including after the local selection chain has advanced.
    pub fn replay_manifest(
        store: &LmdbStore,
        initial_params: &ProtocolParams,
        manifest: &ChallengeOutcomeManifest,
    ) -> Result<Self, StoreError> {
        let mut ancestors = chain::load(store, manifest.checkpoint.network, initial_params)?;
        let height = manifest.checkpoint.height;
        if !(2..=MAX_CHECKPOINTS).contains(&height) || height > ancestors.len() as u64 + 1 {
            return Err(invalid("outcome has no certified parent prefix"));
        }
        ancestors.truncate(height as usize - 1);
        let parent = ancestors
            .last()
            .ok_or_else(|| invalid("outcome parent is missing"))?;
        let history = ConfirmedHistory::read(
            store,
            manifest.checkpoint.network,
            parent.certificate().checkpoint.genesis,
            manifest.checkpoint.cutoff,
            &manifest.heads,
            parent.heads(),
        )?;
        let prepared = Self::from_events(&history, initial_params, &ancestors, &manifest.target)?;
        if prepared.manifest != *manifest {
            return Err(invalid("challenge outcome manifest differs from replay"));
        }
        Ok(prepared)
    }

    #[cfg(test)]
    pub(crate) fn build(
        history: &ConfirmedHistory,
        initial_params: &ProtocolParams,
        ancestors: &[CertifiedCheckpoint],
        target: &WalletAddress,
    ) -> Result<Self, StoreError> {
        let parent = ancestors
            .last()
            .ok_or_else(|| invalid("challenge outcome needs certified selection ancestry"))?;
        if ancestors.len() as u64 != parent.certificate().checkpoint.height {
            return Err(invalid("incomplete outcome ancestry"));
        }
        let anchor = parent
            .parent()
            .advance(parent.certificate(), parent.replayed().roster().clone())
            .map_err(invalid)?;
        if history.network() != anchor.network() || history.genesis() != anchor.genesis_hash() {
            return Err(invalid("outcome history differs from its certified anchor"));
        }
        history.validate_extension(parent.heads())?;
        let state = ConfirmedReplay::replay_with_certificates(
            history,
            anchor.network(),
            initial_params,
            ancestors,
        )
        .map_err(invalid)?;
        Self::from_replay(history, &anchor, initial_params, target, state)
    }

    fn from_events(
        history: &ConfirmedHistory,
        params: &ProtocolParams,
        ancestors: &[chain::CertifiedEvent],
        target: &WalletAddress,
    ) -> Result<Self, StoreError> {
        let parent = ancestors
            .last()
            .ok_or_else(|| invalid("challenge outcome needs certified ancestry"))?;
        history.validate_extension(parent.heads())?;
        let anchor = chain::next_anchor(ancestors, history.network(), params)?;
        let state =
            ConfirmedReplay::replay_with_events(history, anchor.network(), params, ancestors)
                .map_err(invalid)?;
        Self::from_replay(history, &anchor, params, target, state)
    }

    pub(crate) fn from_replay(
        history: &ConfirmedHistory,
        anchor: &CheckpointAnchor,
        _initial_params: &ProtocolParams,
        target: &WalletAddress,
        mut state: ConfirmedReplay,
    ) -> Result<Self, StoreError> {
        if anchor.height() == 0
            || anchor.height() >= MAX_CHECKPOINTS
            || history.network() != anchor.network()
            || history.genesis() != anchor.genesis_hash()
            || state.payment_epoch() != anchor.hash()
        {
            return Err(invalid("outcome history differs from its certified anchor"));
        }
        let active_params = state.params().clone();
        let outcome_root = state
            .apply_upheld(history, &active_params, target)
            .map_err(invalid)?;
        let (manifest, roster) = Self::manifest_for_cut(
            anchor,
            history.cutoff(),
            history.commitment(),
            history.heads(),
            target,
            &state,
            outcome_root,
        )?;
        Ok(Self {
            manifest,
            parent: anchor.clone(),
            roster,
            state,
            history_hashes: history
                .replay_blocks()
                .iter()
                .map(|block| block.hash)
                .collect(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn manifest_for_cut(
        anchor: &CheckpointAnchor,
        cutoff: u64,
        ledger_root: BlockHash,
        heads: &[HistoryHead],
        target: &WalletAddress,
        state: &ConfirmedReplay,
        outcome_root: BlockHash,
    ) -> Result<(ChallengeOutcomeManifest, HumanRoster), StoreError> {
        if anchor.height() == 0 || state.payment_epoch() != anchor.hash() {
            return Err(invalid("outcome state differs from its certified anchor"));
        }
        let roster = state.human_roster().map_err(invalid)?;
        let checkpoint = Checkpoint {
            network: anchor.network(),
            genesis: anchor.genesis_hash(),
            parent: anchor.hash(),
            height: anchor
                .height()
                .checked_add(1)
                .ok_or_else(|| invalid("outcome height exhausted"))?,
            cutoff,
            ledger_root,
            params_hash: state.params().params_hash(),
            next_roster: roster.commitment(),
            selection_root: outcome_root,
            next_quorum_bps: anchor.quorum_bps(),
        };
        anchor.validate_successor(&checkpoint).map_err(invalid)?;
        Ok((
            ChallengeOutcomeManifest {
                checkpoint,
                heads: heads.to_vec(),
                target: target.clone(),
            },
            roster,
        ))
    }

    pub fn manifest(&self) -> &ChallengeOutcomeManifest {
        &self.manifest
    }

    pub fn parent(&self) -> &CheckpointAnchor {
        &self.parent
    }

    pub(crate) fn history_hashes(&self) -> &HashSet<BlockHash> {
        &self.history_hashes
    }

    /// Preview only. No public method converts this state into live mutations.
    pub fn state(&self) -> &ConfirmedReplay {
        &self.state
    }

    pub fn certify(
        self,
        certificate: CheckpointCertificate,
    ) -> Result<CertifiedChallengeOutcome, StoreError> {
        if certificate.checkpoint != self.manifest.checkpoint {
            return Err(invalid("outcome certificate does not bind this replay"));
        }
        self.parent
            .verify_certificate(&certificate)
            .map_err(invalid)?;
        Ok(CertifiedChallengeOutcome {
            prepared: self,
            certificate,
        })
    }
}

impl CertifiedChallengeOutcome {
    /// Authenticate a bounded outcome record against locally reconstructed
    /// ancestry and signed history. This does not install its economic state.
    pub fn import_record(
        store: &LmdbStore,
        network: NetworkId,
        initial_params: &ProtocolParams,
        height: u64,
        bytes: &[u8],
    ) -> Result<Self, StoreError> {
        if bytes.first() != Some(&2) {
            return Err(invalid("not an outcome checkpoint record"));
        }
        match chain::import_record(store, network, initial_params, height, bytes)? {
            chain::CertifiedEvent::Outcome(value) => Ok(*value),
            chain::CertifiedEvent::Selection(_)
            | chain::CertifiedEvent::Roster(_)
            | chain::CertifiedEvent::Activation(_)
            | chain::CertifiedEvent::Deadline(_)
            | chain::CertifiedEvent::Governance(_) => {
                Err(invalid("outcome import returned a selection"))
            }
        }
    }

    /// Persist only the certified event. Live economic installation and payment
    /// continuation remain gated; the canonical reader must not discard it.
    pub fn persist(
        self,
        store: &LmdbStore,
        network: NetworkId,
        initial_params: &ProtocolParams,
    ) -> Result<Self, StoreError> {
        use crate::checkpoint_store::chain::{self, CertifiedEvent};
        let manifest = self.manifest().clone();
        match chain::accept_outcome(
            store,
            network,
            initial_params,
            self.certificate,
            manifest.heads,
            manifest.target,
        )? {
            CertifiedEvent::Outcome(value) => Ok(*value),
            CertifiedEvent::Selection(_)
            | CertifiedEvent::Roster(_)
            | CertifiedEvent::Activation(_)
            | CertifiedEvent::Deadline(_)
            | CertifiedEvent::Governance(_) => {
                Err(invalid("outcome persistence returned a selection"))
            }
        }
    }

    pub(crate) fn parent(&self) -> &CheckpointAnchor {
        self.prepared.parent()
    }
    pub fn manifest(&self) -> &ChallengeOutcomeManifest {
        self.prepared.manifest()
    }

    pub fn certificate(&self) -> &CheckpointCertificate {
        &self.certificate
    }

    pub fn state(&self) -> &ConfirmedReplay {
        &self.prepared.state
    }

    pub fn next_roster(&self) -> &HumanRoster {
        &self.prepared.roster
    }
}
