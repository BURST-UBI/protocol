//! Agree when a certified committee's voting window starts.

use std::collections::HashSet;

use burst_consensus::checkpoint::{
    Checkpoint, CheckpointAnchor, CheckpointCertificate, HumanRoster,
};
use burst_store::StoreError;
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, NetworkId, ProtocolParams};
use burst_vrf::RandomOutput;
use serde::{Deserialize, Serialize};

use crate::{
    checkpoint_store::{chain, CertifiedCheckpoint, MAX_CHECKPOINTS},
    confirmed_history::{ConfirmedHistory, HistoryHead},
    confirmed_replay::ConfirmedReplay,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationProof {
    pub selection: BlockHash,
    pub value: [u8; 32],
    pub proof: Vec<u8>,
    pub round: u64,
}

impl ActivationProof {
    pub(crate) fn beacon(&self) -> RandomOutput {
        RandomOutput {
            value: self.value,
            proof: self.proof.clone(),
            round: self.round,
        }
    }

    pub(crate) fn from_selection(selection: &CertifiedCheckpoint) -> Option<Self> {
        let beacon = selection.beacon()?;
        Some(Self {
            selection: selection.certificate().checkpoint.hash(),
            value: beacon.value,
            proof: beacon.proof.clone(),
            round: beacon.round,
        })
    }

    pub(crate) fn commitment(&self) -> Result<BlockHash, StoreError> {
        if self.selection.is_zero() || self.proof.len() > 4096 {
            return Err(invalid("invalid activation proof bounds"));
        }
        Ok(BlockHash::new(burst_crypto::blake2b_256_multi(&[
            b"BURST/committee-activation/v1\0",
            &bincode::serialize(self).map_err(invalid)?,
        ])))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationManifest {
    pub checkpoint: Checkpoint,
    pub heads: Vec<HistoryHead>,
    pub activation: ActivationProof,
}

pub struct PreparedActivation {
    manifest: ActivationManifest,
    parent: CheckpointAnchor,
    roster: HumanRoster,
    history_hashes: HashSet<BlockHash>,
    signing_deadline: u64,
}

pub struct CertifiedActivation {
    prepared: PreparedActivation,
    certificate: CheckpointCertificate,
}

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}

impl PreparedActivation {
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
        let heads = ConfirmedHistory::heads_at(store, cutoff)?;
        let history = ConfirmedHistory::read(
            store,
            network,
            anchor.genesis_hash(),
            cutoff,
            &heads,
            events.last().map_or(&[][..], chain::CertifiedEvent::heads),
        )?;
        let state = ConfirmedReplay::replay_with_events(&history, network, params, events)
            .map_err(invalid)?;
        for selection in events.iter().filter_map(chain::CertifiedEvent::selection) {
            let Some(proof) = ActivationProof::from_selection(selection) else {
                continue;
            };
            if cutoff
                < selection
                    .replayed()
                    .request()
                    .beacon_time()
                    .map_err(invalid)?
            {
                continue;
            }
            if state.can_activate(selection).map_err(invalid)? {
                return Self::from_replay(&history, &anchor, params, &proof, selection, state)
                    .map(Some);
            }
        }
        Ok(None)
    }

    pub fn replay_manifest(
        store: &LmdbStore,
        params: &ProtocolParams,
        manifest: &ActivationManifest,
    ) -> Result<Self, StoreError> {
        let mut events = chain::load(store, manifest.checkpoint.network, params)?;
        let height = manifest.checkpoint.height;
        if !(2..=MAX_CHECKPOINTS).contains(&height) || height > events.len() as u64 + 1 {
            return Err(invalid("activation has no certified parent prefix"));
        }
        events.truncate(height as usize - 1);
        chain::hydrate_activation(&mut events, &manifest.activation)?;
        let anchor = chain::next_anchor(&events, manifest.checkpoint.network, params)?;
        let history = ConfirmedHistory::read(
            store,
            anchor.network(),
            anchor.genesis_hash(),
            manifest.checkpoint.cutoff,
            &manifest.heads,
            events.last().map_or(&[][..], chain::CertifiedEvent::heads),
        )?;
        let selection = events
            .iter()
            .filter_map(chain::CertifiedEvent::selection)
            .find(|entry| entry.certificate().checkpoint.hash() == manifest.activation.selection)
            .ok_or_else(|| invalid("activation selection is absent from its ancestry"))?;
        let state =
            ConfirmedReplay::replay_with_events(&history, anchor.network(), params, &events)
                .map_err(invalid)?;
        let prepared = Self::from_replay(
            &history,
            &anchor,
            params,
            &manifest.activation,
            selection,
            state,
        )?;
        if prepared.manifest != *manifest {
            return Err(invalid("activation manifest differs from replay"));
        }
        Ok(prepared)
    }

    pub(crate) fn from_replay(
        history: &ConfirmedHistory,
        anchor: &CheckpointAnchor,
        params: &ProtocolParams,
        proof: &ActivationProof,
        selection: &CertifiedCheckpoint,
        mut state: ConfirmedReplay,
    ) -> Result<Self, StoreError> {
        if anchor.height() >= MAX_CHECKPOINTS
            || history.network() != anchor.network()
            || history.genesis() != anchor.genesis_hash()
            || state.payment_epoch() != anchor.hash()
            || proof.selection != selection.certificate().checkpoint.hash()
        {
            return Err(invalid(
                "activation differs from replayed authority or selection",
            ));
        }
        state
            .apply_activation(selection, proof, params)
            .map_err(invalid)?;
        let (manifest, roster, signing_deadline) = Self::manifest_for_cut(
            anchor,
            history.cutoff(),
            history.commitment(),
            history.heads(),
            proof,
            selection,
            &state,
        )?;
        Ok(Self {
            manifest,
            parent: anchor.clone(),
            roster,
            history_hashes: history.replay_blocks().iter().map(|b| b.hash).collect(),
            signing_deadline,
        })
    }

    pub(crate) fn manifest_for_cut(
        anchor: &CheckpointAnchor,
        cutoff: u64,
        ledger_root: BlockHash,
        heads: &[HistoryHead],
        proof: &ActivationProof,
        selection: &CertifiedCheckpoint,
        state: &ConfirmedReplay,
    ) -> Result<(ActivationManifest, HumanRoster, u64), StoreError> {
        if state.payment_epoch() != anchor.hash()
            || proof.selection != selection.certificate().checkpoint.hash()
        {
            return Err(invalid(
                "activation differs from replayed authority or selection",
            ));
        }
        let roster = state.human_roster().map_err(invalid)?;
        let checkpoint = Checkpoint {
            network: anchor.network(),
            genesis: anchor.genesis_hash(),
            parent: anchor.hash(),
            height: anchor
                .height()
                .checked_add(1)
                .ok_or_else(|| invalid("activation height exhausted"))?,
            cutoff,
            ledger_root,
            params_hash: state.params().params_hash(),
            next_roster: roster.commitment(),
            selection_root: proof.commitment()?,
            next_quorum_bps: anchor.quorum_bps(),
        };
        anchor.validate_successor(&checkpoint).map_err(invalid)?;
        // Fresh proposals cannot backdate the window against an honest signer.
        // Prepared/decided recovery retains the previously agreed cutoff.
        let duration = match selection.replayed().lifecycle() {
            crate::checkpoint_replay::SelectionLifecycle::Challenge(_) => {
                state.params().challenge_duration_secs
            }
            crate::checkpoint_replay::SelectionLifecycle::Verification(_) => {
                state.params().verification_timeout_secs
            }
        };
        let signing_deadline = cutoff
            .checked_add(duration.min(60))
            .ok_or_else(|| invalid("activation signing deadline overflow"))?;
        Ok((
            ActivationManifest {
                checkpoint,
                heads: heads.to_vec(),
                activation: proof.clone(),
            },
            roster,
            signing_deadline,
        ))
    }

    pub fn manifest(&self) -> &ActivationManifest {
        &self.manifest
    }

    /// Retry the start time, never the already certified committee or beacon.
    pub(crate) fn prepare_later(
        &self,
        store: &LmdbStore,
        params: &ProtocolParams,
        cutoff: u64,
    ) -> Result<Option<Self>, StoreError> {
        if cutoff <= self.manifest.checkpoint.cutoff {
            return Err(invalid("activation retry must advance its cutoff"));
        }
        let mut events = chain::load(store, self.parent.network(), params)?;
        let anchor = chain::next_anchor(&events, self.parent.network(), params)?;
        if anchor.hash() != self.parent.hash() {
            return Err(invalid("activation retry parent has advanced"));
        }
        chain::hydrate_activation(&mut events, &self.manifest.activation)?;
        let heads = ConfirmedHistory::heads_at(store, cutoff)?;
        let history = ConfirmedHistory::read(
            store,
            anchor.network(),
            anchor.genesis_hash(),
            cutoff,
            &heads,
            events.last().map_or(&[][..], chain::CertifiedEvent::heads),
        )?;
        let selection = events
            .iter()
            .filter_map(chain::CertifiedEvent::selection)
            .find(|entry| {
                entry.certificate().checkpoint.hash() == self.manifest.activation.selection
            })
            .ok_or_else(|| invalid("activation retry lost its certified selection"))?;
        let state =
            ConfirmedReplay::replay_with_events(&history, anchor.network(), params, &events)
                .map_err(invalid)?;
        if !state.can_activate(selection).map_err(invalid)? {
            return Ok(None);
        }
        Self::from_replay(
            &history,
            &anchor,
            params,
            &self.manifest.activation,
            selection,
            state,
        )
        .map(Some)
    }

    pub fn parent(&self) -> &CheckpointAnchor {
        &self.parent
    }
    pub(crate) fn history_hashes(&self) -> &HashSet<BlockHash> {
        &self.history_hashes
    }
    pub(crate) fn validate_fresh_time(
        &self,
        clock: crate::checkpoint_replay::SigningClock,
    ) -> Result<(), StoreError> {
        Self::validate_signing_deadline(self.signing_deadline, clock)
    }

    pub(crate) fn validate_signing_deadline(
        signing_deadline: u64,
        clock: crate::checkpoint_replay::SigningClock,
    ) -> Result<(), StoreError> {
        if clock
            .now
            .checked_add(clock.error_bound)
            .is_none_or(|latest| latest >= signing_deadline)
        {
            return Err(invalid("activation signing window has elapsed"));
        }
        Ok(())
    }
    pub fn certify(
        self,
        certificate: CheckpointCertificate,
    ) -> Result<CertifiedActivation, StoreError> {
        if certificate.checkpoint != self.manifest.checkpoint {
            return Err(invalid("activation certificate differs from replay"));
        }
        self.parent
            .verify_certificate(&certificate)
            .map_err(invalid)?;
        Ok(CertifiedActivation {
            prepared: self,
            certificate,
        })
    }
}

impl CertifiedActivation {
    pub fn manifest(&self) -> &ActivationManifest {
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
