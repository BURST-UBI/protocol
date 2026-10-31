//! Authority-only cuts replay the ledger without creating a selection or
//! settling a challenge. The previous roster, never the proposed one, signs.

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
pub struct RosterManifest {
    pub checkpoint: Checkpoint,
    pub heads: Vec<HistoryHead>,
}

pub struct PreparedRoster {
    manifest: RosterManifest,
    parent: CheckpointAnchor,
    roster: HumanRoster,
    history_hashes: HashSet<BlockHash>,
}

pub struct CertifiedRoster {
    prepared: PreparedRoster,
    certificate: CheckpointCertificate,
}

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}

/// A separate event domain prevents treating a roster update as a committee
/// request or an outcome, even with identical heads and human commitments.
pub(crate) fn commitment() -> BlockHash {
    BlockHash::new(burst_crypto::blake2b_256(b"BURST/roster-checkpoint/v1\0"))
}

impl PreparedRoster {
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
        let heads = ConfirmedHistory::heads_at(store, cutoff)?;
        let history = ConfirmedHistory::read(
            store,
            network,
            anchor.genesis_hash(),
            cutoff,
            &heads,
            events.last().map_or(&[][..], chain::CertifiedEvent::heads),
        )?;
        let replay = ConfirmedReplay::replay_with_events(&history, network, params, events)
            .map_err(invalid)?;
        if replay.human_roster().map_err(invalid)? == *anchor.roster()
            && !(crate::checkpoint_recovery::valid_cut(anchor.cutoff(), cutoff)
                && crate::checkpoint_recovery::requested(store, anchor.hash(), cutoff)?)
        {
            return Ok(None);
        }
        Self::from_replay(&history, &anchor, params, replay).map(Some)
    }

    pub fn replay_manifest(
        store: &LmdbStore,
        params: &ProtocolParams,
        manifest: &RosterManifest,
    ) -> Result<Self, StoreError> {
        let mut events = chain::load(store, manifest.checkpoint.network, params)?;
        let height = manifest.checkpoint.height;
        if !(1..=MAX_CHECKPOINTS).contains(&height) || height > events.len() as u64 + 1 {
            return Err(invalid("roster checkpoint has no certified parent prefix"));
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
            ConfirmedReplay::replay_with_events(&history, anchor.network(), params, &events)
                .map_err(invalid)?;
        let prepared = Self::from_replay(&history, &anchor, params, replay)?;
        if prepared.manifest != *manifest {
            return Err(invalid(
                "roster manifest differs from signed-history replay",
            ));
        }
        Ok(prepared)
    }

    pub(crate) fn from_replay(
        history: &ConfirmedHistory,
        anchor: &CheckpointAnchor,
        _initial_params: &ProtocolParams,
        replay: ConfirmedReplay,
    ) -> Result<Self, StoreError> {
        if anchor.height() >= MAX_CHECKPOINTS
            || history.network() != anchor.network()
            || history.genesis() != anchor.genesis_hash()
            || replay.payment_epoch() != anchor.hash()
        {
            return Err(invalid(
                "roster replay differs from its certified authority",
            ));
        }
        let (manifest, roster) = Self::manifest_for_cut(
            anchor,
            history.cutoff(),
            history.commitment(),
            history.heads(),
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
        replay: &ConfirmedReplay,
    ) -> Result<(RosterManifest, HumanRoster), StoreError> {
        if replay.payment_epoch() != anchor.hash() {
            return Err(invalid("roster state differs from its certified authority"));
        }
        let roster = replay.human_roster().map_err(invalid)?;
        let selection_root = if &roster == anchor.roster() {
            if !crate::checkpoint_recovery::valid_cut(anchor.cutoff(), cutoff) {
                return Err(invalid("recovery checkpoint cut is too early"));
            }
            crate::checkpoint_recovery::commitment()
        } else {
            commitment()
        };
        let checkpoint = Checkpoint {
            network: anchor.network(),
            genesis: anchor.genesis_hash(),
            parent: anchor.hash(),
            height: anchor
                .height()
                .checked_add(1)
                .ok_or_else(|| invalid("roster height exhausted"))?,
            cutoff,
            ledger_root,
            params_hash: replay.params().params_hash(),
            next_roster: roster.commitment(),
            selection_root,
            next_quorum_bps: anchor.quorum_bps(),
        };
        anchor.validate_successor(&checkpoint).map_err(invalid)?;
        Ok((
            RosterManifest {
                checkpoint,
                heads: heads.to_vec(),
            },
            roster,
        ))
    }

    pub fn manifest(&self) -> &RosterManifest {
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
    ) -> Result<CertifiedRoster, StoreError> {
        if certificate.checkpoint != self.manifest.checkpoint {
            return Err(invalid("roster certificate differs from replay"));
        }
        self.parent
            .verify_certificate(&certificate)
            .map_err(invalid)?;
        Ok(CertifiedRoster {
            prepared: self,
            certificate,
        })
    }
}

impl CertifiedRoster {
    pub fn manifest(&self) -> &RosterManifest {
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
