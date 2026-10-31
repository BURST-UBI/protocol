//! The previous roster certifies due ballots and ordered activations at one cut.

use crate::{
    checkpoint_store::{chain, MAX_CHECKPOINTS},
    confirmed_history::{ConfirmedHistory, HistoryHead},
    confirmed_replay::ConfirmedReplay,
};
use burst_consensus::checkpoint::{
    Checkpoint, CheckpointAnchor, CheckpointCertificate, HumanRoster,
};
use burst_store::StoreError;
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, NetworkId, ProtocolParams, Timestamp, TxHash};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceManifest {
    pub checkpoint: Checkpoint,
    pub heads: Vec<HistoryHead>,
    pub proposals: Vec<TxHash>,
}

pub struct PreparedGovernance {
    manifest: GovernanceManifest,
    parent: CheckpointAnchor,
    roster: HumanRoster,
    history_hashes: HashSet<BlockHash>,
}

pub struct CertifiedGovernance {
    prepared: PreparedGovernance,
    certificate: CheckpointCertificate,
}

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}

impl PreparedGovernance {
    /// Discovery does not authorize signing or install a ballot. Other lifecycle
    /// events may have to finish before this governance boundary.
    pub(crate) fn due_at_in_chain(
        store: &LmdbStore,
        network: NetworkId,
        params: &ProtocolParams,
        through: u64,
        events: &[chain::CertifiedEvent],
    ) -> Result<Option<u64>, StoreError> {
        let anchor = chain::next_anchor(events, network, params)?;
        if through <= anchor.cutoff() {
            return Ok(None);
        }
        let parent = events.last().map_or(&[][..], chain::CertifiedEvent::heads);
        let read = |cutoff| {
            let heads = ConfirmedHistory::heads_at(store, cutoff)?;
            ConfirmedHistory::read(
                store,
                network,
                anchor.genesis_hash(),
                cutoff,
                &heads,
                parent,
            )
        };
        let history = read(through)?;
        if !history
            .replay_blocks()
            .iter()
            .any(|block| block.block_type == burst_ledger::BlockType::GovernanceProposal)
        {
            return Ok(None);
        }
        let replay =
            ConfirmedReplay::replay_governance_discovery(&history, network, params, events)
                .map_err(invalid)?;
        let Some(cutoff) = replay
            .next_governance_cut(Timestamp::new(through))
            .map_err(invalid)?
        else {
            return Ok(None);
        };
        if cutoff <= anchor.cutoff() {
            return Err(invalid(
                "governance boundary does not extend its certified parent",
            ));
        }
        Ok(Some(cutoff))
    }

    #[cfg(test)]
    pub(crate) fn prepare_next(
        store: &LmdbStore,
        network: NetworkId,
        params: &ProtocolParams,
        through: u64,
    ) -> Result<Option<Self>, StoreError> {
        let events = chain::load(store, network, params)?;
        Self::prepare_next_in_chain(store, network, params, through, &events)
    }

    /// Replay the earliest due governance cut so later delegation changes cannot
    /// affect its count. Signing separately checks durable final-vote coverage.
    pub(crate) fn prepare_next_in_chain(
        store: &LmdbStore,
        network: NetworkId,
        params: &ProtocolParams,
        through: u64,
        events: &[chain::CertifiedEvent],
    ) -> Result<Option<Self>, StoreError> {
        let Some(cutoff) = Self::due_at_in_chain(store, network, params, through, events)? else {
            return Ok(None);
        };
        let anchor = chain::next_anchor(events, network, params)?;
        let heads = ConfirmedHistory::heads_at(store, cutoff)?;
        let history = ConfirmedHistory::read(
            store,
            network,
            anchor.genesis_hash(),
            cutoff,
            &heads,
            events.last().map_or(&[][..], chain::CertifiedEvent::heads),
        )?;
        let replay =
            ConfirmedReplay::replay_governance_candidate(&history, network, params, events)
                .map_err(invalid)?;
        Self::from_replay(&history, &anchor, params, replay).map(Some)
    }

    pub fn replay_manifest(
        store: &LmdbStore,
        params: &ProtocolParams,
        manifest: &GovernanceManifest,
    ) -> Result<Self, StoreError> {
        let mut events = chain::load(store, manifest.checkpoint.network, params)?;
        let height = manifest.checkpoint.height;
        if !(1..=MAX_CHECKPOINTS).contains(&height) || height > events.len() as u64 + 1 {
            return Err(invalid(
                "governance checkpoint has no certified parent prefix",
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
        let replay = ConfirmedReplay::replay_governance_candidate(
            &history,
            anchor.network(),
            params,
            &events,
        )
        .map_err(invalid)?;
        let prepared = Self::from_replay(&history, &anchor, params, replay)?;
        if prepared.manifest != *manifest {
            return Err(invalid(
                "governance manifest differs from signed-history replay",
            ));
        }
        Ok(prepared)
    }

    pub(crate) fn from_replay(
        history: &ConfirmedHistory,
        anchor: &CheckpointAnchor,
        _initial_params: &ProtocolParams,
        mut replay: ConfirmedReplay,
    ) -> Result<Self, StoreError> {
        if anchor.height() >= MAX_CHECKPOINTS
            || history.network() != anchor.network()
            || history.genesis() != anchor.genesis_hash()
            || replay.payment_epoch() != anchor.hash()
        {
            return Err(invalid(
                "governance cut differs from its certified authority",
            ));
        }
        let (proposals, root) = replay.apply_governance_cut(history).map_err(invalid)?;
        let (manifest, roster) = Self::manifest_for_cut(
            anchor,
            history.cutoff(),
            history.commitment(),
            history.heads(),
            proposals,
            root,
            &replay,
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
        proposals: Vec<TxHash>,
        root: BlockHash,
        replay: &ConfirmedReplay,
    ) -> Result<(GovernanceManifest, HumanRoster), StoreError> {
        if replay.payment_epoch() != anchor.hash() {
            return Err(invalid(
                "governance state differs from its certified authority",
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
                .ok_or_else(|| invalid("governance height exhausted"))?,
            cutoff,
            ledger_root,
            params_hash: replay.params().params_hash(),
            next_roster: roster.commitment(),
            selection_root: root,
            next_quorum_bps: anchor.quorum_bps(),
        };
        anchor.validate_successor(&checkpoint).map_err(invalid)?;
        Ok((
            GovernanceManifest {
                checkpoint,
                heads: heads.to_vec(),
                proposals,
            },
            roster,
        ))
    }

    pub fn manifest(&self) -> &GovernanceManifest {
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
    ) -> Result<CertifiedGovernance, StoreError> {
        if certificate.checkpoint != self.manifest.checkpoint {
            return Err(invalid("governance certificate differs from replay"));
        }
        self.parent
            .verify_certificate(&certificate)
            .map_err(invalid)?;
        Ok(CertifiedGovernance {
            prepared: self,
            certificate,
        })
    }
}

impl CertifiedGovernance {
    pub fn manifest(&self) -> &GovernanceManifest {
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
