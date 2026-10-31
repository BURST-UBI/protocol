//! Certified panel completion and pending expiry, signed by the previous roster.

use std::collections::HashSet;

use burst_consensus::checkpoint::{
    Checkpoint, CheckpointAnchor, CheckpointCertificate, HumanRoster,
};
use burst_store::StoreError;
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, NetworkId, ProtocolParams};
use serde::{Deserialize, Serialize};

use crate::{
    checkpoint_store::{chain, MAX_CHECKPOINTS},
    confirmed_history::{ConfirmedHistory, HistoryHead},
    confirmed_replay::ConfirmedReplay,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadlineManifest {
    pub checkpoint: Checkpoint,
    pub heads: Vec<HistoryHead>,
}

pub struct PreparedDeadline {
    manifest: DeadlineManifest,
    parent: CheckpointAnchor,
    roster: HumanRoster,
    history_hashes: HashSet<BlockHash>,
}

pub struct CertifiedDeadline {
    prepared: PreparedDeadline,
    certificate: CheckpointCertificate,
}

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}

impl PreparedDeadline {
    #[cfg(test)]
    pub(crate) fn prepare_next(
        store: &LmdbStore,
        network: NetworkId,
        params: &ProtocolParams,
        cutoff: u64,
    ) -> Result<Option<Self>, StoreError> {
        let events = chain::load(store, network, params)?;
        Self::prepare_next_in_chain(store, network, params, cutoff, &events)
    }

    pub(crate) fn prepare_next_in_chain(
        store: &LmdbStore,
        network: NetworkId,
        params: &ProtocolParams,
        cutoff: u64,
        events: &[chain::CertifiedEvent],
    ) -> Result<Option<Self>, StoreError> {
        let anchor = chain::next_anchor(events, network, params)?;
        if cutoff <= anchor.cutoff() {
            return Ok(None);
        }
        if !events.iter().any(|event| {
            matches!(event, chain::CertifiedEvent::Activation(value)
            if value.manifest().checkpoint.cutoff.checked_add(params.verification_timeout_secs.min(params.challenge_duration_secs))
                .is_some_and(|deadline| cutoff >= deadline))
        }) && cutoff < params.trst_expiry_secs
            && !events.iter().any(|event| matches!(event, chain::CertifiedEvent::Governance(_))) {
            return Ok(None);
        }
        let heads = ConfirmedHistory::heads_at(store, cutoff)?;
        let history = ConfirmedHistory::read(
            store,
            network,
            anchor.genesis_hash(),
            cutoff,
            &heads,
            events.last().map_or(&[][..], chain::CertifiedEvent::heads),
        )?;
        let replay = ConfirmedReplay::replay_deadline_candidate(&history, network, params, events)
            .map_err(invalid)?;
        let mut replay = replay;
        let active_params = replay.params().clone();
        let Some(root) = replay
            .apply_agreed_deadlines(&history, &active_params)
            .map_err(invalid)?
        else {
            return Ok(None);
        };
        Self::from_completed_replay(&history, &anchor, params, replay, root).map(Some)
    }

    pub fn replay_manifest(
        store: &LmdbStore,
        params: &ProtocolParams,
        manifest: &DeadlineManifest,
    ) -> Result<Self, StoreError> {
        let mut events = chain::load(store, manifest.checkpoint.network, params)?;
        let height = manifest.checkpoint.height;
        if !(1..=MAX_CHECKPOINTS).contains(&height) || height > events.len() as u64 + 1 {
            return Err(invalid(
                "deadline checkpoint has no certified parent prefix",
            ));
        }
        events.truncate(height as usize - 1);
        let anchor = chain::next_anchor(&events, manifest.checkpoint.network, params)?;
        let history = ConfirmedHistory::read(
            store,
            anchor.network(),
            anchor.genesis_hash(),
            manifest.checkpoint.cutoff,
            &manifest.heads,
            events.last().map_or(&[][..], chain::CertifiedEvent::heads),
        )?;
        let replay =
            ConfirmedReplay::replay_deadline_candidate(&history, anchor.network(), params, &events)
                .map_err(invalid)?;
        let prepared = Self::from_replay(&history, &anchor, params, replay)?;
        if prepared.manifest != *manifest {
            return Err(invalid(
                "deadline manifest differs from signed-history replay",
            ));
        }
        Ok(prepared)
    }

    pub(crate) fn from_replay(
        history: &ConfirmedHistory,
        anchor: &CheckpointAnchor,
        params: &ProtocolParams,
        mut replay: ConfirmedReplay,
    ) -> Result<Self, StoreError> {
        let active_params = replay.params().clone();
        let root = replay
            .apply_agreed_deadlines(history, &active_params)
            .map_err(invalid)?
            .ok_or_else(|| invalid("deadline checkpoint has no pending completions"))?;
        Self::from_completed_replay(history, anchor, params, replay, root)
    }

    fn from_completed_replay(
        history: &ConfirmedHistory,
        anchor: &CheckpointAnchor,
        _initial_params: &ProtocolParams,
        replay: ConfirmedReplay,
        root: BlockHash,
    ) -> Result<Self, StoreError> {
        if history.network() != anchor.network() || history.genesis() != anchor.genesis_hash() {
            return Err(invalid(
                "deadline replay differs from its certified authority",
            ));
        }
        let (manifest, roster) = Self::manifest_for_cut(
            anchor,
            history.cutoff(),
            history.commitment(),
            history.heads(),
            &replay,
            root,
        )?;
        Ok(Self {
            manifest,
            parent: anchor.clone(),
            roster,
            history_hashes: history
                .replay_blocks()
                .iter()
                .map(|block| block.hash)
                .collect(),
        })
    }

    pub(crate) fn manifest_for_cut(
        anchor: &CheckpointAnchor,
        cutoff: u64,
        ledger_root: BlockHash,
        heads: &[HistoryHead],
        replay: &ConfirmedReplay,
        root: BlockHash,
    ) -> Result<(DeadlineManifest, HumanRoster), StoreError> {
        if replay.payment_epoch() != anchor.hash() {
            return Err(invalid(
                "deadline replay differs from its certified authority",
            ));
        }
        let roster = replay.human_roster().map_err(invalid)?;
        let checkpoint = Checkpoint {
            network: anchor.network(),
            genesis: anchor.genesis_hash(),
            parent: anchor.hash(),
            height: anchor
                .height()
                .checked_add(1)
                .ok_or_else(|| invalid("deadline height exhausted"))?,
            cutoff,
            ledger_root,
            params_hash: replay.params().params_hash(),
            next_roster: roster.commitment(),
            selection_root: root,
            next_quorum_bps: anchor.quorum_bps(),
        };
        anchor.validate_successor(&checkpoint).map_err(invalid)?;
        Ok((
            DeadlineManifest {
                checkpoint,
                heads: heads.to_vec(),
            },
            roster,
        ))
    }

    pub fn manifest(&self) -> &DeadlineManifest {
        &self.manifest
    }
    pub fn parent(&self) -> &CheckpointAnchor {
        &self.parent
    }
    pub(crate) fn history_hashes(&self) -> &HashSet<BlockHash> {
        &self.history_hashes
    }

    pub fn certify(
        self,
        certificate: CheckpointCertificate,
    ) -> Result<CertifiedDeadline, StoreError> {
        if certificate.checkpoint != self.manifest.checkpoint {
            return Err(invalid("deadline certificate differs from replay"));
        }
        self.parent
            .verify_certificate(&certificate)
            .map_err(invalid)?;
        Ok(CertifiedDeadline {
            prepared: self,
            certificate,
        })
    }
}

impl CertifiedDeadline {
    pub fn manifest(&self) -> &DeadlineManifest {
        self.prepared.manifest()
    }
    pub fn certificate(&self) -> &CheckpointCertificate {
        &self.certificate
    }
    pub(crate) fn parent(&self) -> &CheckpointAnchor {
        self.prepared.parent()
    }
    pub(crate) fn next_roster(&self) -> &HumanRoster {
        &self.prepared.roster
    }
}
