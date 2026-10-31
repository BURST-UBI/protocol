use super::*;
use crate::checkpoint_activation::{ActivationManifest, PreparedActivation};
use burst_store::StoreError;

pub struct CompactActivationCoverage {
    pub(super) manifest: ActivationManifest,
    pub(super) cut: CompactCutCoverage,
    pub(super) signing_deadline: u64,
}

impl CompactActivationCoverage {
    pub fn manifest(&self) -> &ActivationManifest {
        &self.manifest
    }

    pub(crate) fn validate_fresh_time(
        &self,
        clock: crate::checkpoint_replay::SigningClock,
    ) -> Result<(), StoreError> {
        PreparedActivation::validate_signing_deadline(self.signing_deadline, clock)
    }

    pub(crate) fn archive_rows(&self) -> super::super::tail::ArchiveRows<'_> {
        self.cut.archive.rows()
    }

    pub(crate) fn validate_parent(&self, anchor: &CheckpointAnchor) -> Result<(), StoreError> {
        if self.cut.parent != anchor.hash()
            || self.cut.authority
                != crate::confirmed_replay::installation::authority_bytes(anchor, anchor.cutoff())?
        {
            return Err(StoreError::Corruption(
                "activation coverage differs from its parent".into(),
            ));
        }
        anchor
            .validate_successor(&self.manifest.checkpoint)
            .map_err(|error| StoreError::Corruption(error.to_string()))
    }

    pub(crate) fn check_signing_parent(
        &self,
        batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
        anchor: &CheckpointAnchor,
    ) -> Result<(), StoreError> {
        self.validate_parent(anchor)?;
        self.cut.check_signing_parent(batch, anchor)
    }

    pub(crate) fn covers_local_promises(
        &self, batch: &burst_store_lmdb::LmdbWriteTransaction<'_>, representative: &WalletAddress,
    ) -> Result<bool, StoreError> {
        batch.covers_consensus_votes(representative, &self.cut.parent, &self.cut.parent, &self.cut.included)
    }

    pub(crate) fn seal_local_votes(
        &self,
        batch: &mut burst_store_lmdb::LmdbWriteTransaction<'_>,
        anchor: &CheckpointAnchor,
        representative: &WalletAddress,
    ) -> Result<(), StoreError> {
        self.validate_parent(anchor)?;
        self.cut.seal_local_votes(batch, anchor, representative)
    }
}

impl SnapshotContinuation {
    pub fn activation_vote_coverage_for_manifest(
        &self,
        tail: &SnapshotTail,
        manifest: &ActivationManifest,
        selections: &(impl SelectionSource + ?Sized),
    ) -> Result<CompactActivationCoverage, ReplayError> {
        self.anchor
            .validate_successor(&manifest.checkpoint)
            .map_err(|error| super::super::invalid(error.to_string()))?;
        if bincode::serialized_size(manifest)
            .map_err(|error| super::super::invalid(error.to_string()))?
            > crate::checkpoint_store::MAX_RECORD_BYTES
        {
            return Err(super::super::invalid(
                "activation manifest exceeds byte limit",
            ));
        }
        self.check_selection_receipts(selections)?;
        let mut candidate = Self {
            anchor: self.anchor.clone(),
            context: self.context,
            state: self.state.clone(),
        };
        candidate.execute(tail, selections, false)?;
        candidate
            .state
            .require_closed_governance(burst_types::Timestamp::new(tail.cutoff()))?;
        let selection = selections
            .find_selection(|selection| {
                selection.certificate().checkpoint.hash() == manifest.activation.selection
            })?
            .ok_or_else(|| super::super::invalid("activation has no recovered selection"))?;
        let params = candidate.state.params().clone();
        candidate
            .state
            .apply_activation(&selection, &manifest.activation, &params)?;
        let (expected, _, signing_deadline) = PreparedActivation::manifest_for_cut(
            &self.anchor,
            tail.cutoff(),
            tail.commitment(),
            tail.heads(),
            &manifest.activation,
            &selection,
            &candidate.state,
        )
        .map_err(|error| super::super::invalid(error.to_string()))?;
        if expected != *manifest {
            return Err(super::super::invalid(
                "activation proposal differs from compact replay",
            ));
        }
        Ok(CompactActivationCoverage {
            manifest: expected,
            cut: self.cut_coverage(tail)?,
            signing_deadline,
        })
    }
}
