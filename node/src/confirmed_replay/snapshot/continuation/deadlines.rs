use super::*;
use crate::checkpoint_deadline::{DeadlineManifest, PreparedDeadline};

pub struct CompactDeadlineCoverage {
    pub(super) manifest: DeadlineManifest,
    pub(super) cut: CompactCutCoverage,
}

impl CompactDeadlineCoverage {
    pub(crate) fn archive_rows(&self) -> super::super::tail::ArchiveRows<'_> {
        self.cut.archive.rows()
    }
    pub fn manifest(&self) -> &DeadlineManifest {
        &self.manifest
    }

    pub(crate) fn validate_parent(
        &self,
        anchor: &CheckpointAnchor,
    ) -> Result<(), burst_store::StoreError> {
        if self.cut.parent != anchor.hash()
            || self.cut.authority
                != crate::confirmed_replay::installation::authority_bytes(anchor, anchor.cutoff())?
        {
            return Err(burst_store::StoreError::Corruption(
                "deadline coverage differs from its parent".into(),
            ));
        }
        anchor
            .validate_successor(&self.manifest.checkpoint)
            .map_err(|error| burst_store::StoreError::Corruption(error.to_string()))
    }

    pub(crate) fn check_signing_parent(
        &self,
        batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
        anchor: &CheckpointAnchor,
    ) -> Result<(), burst_store::StoreError> {
        self.validate_parent(anchor)?;
        self.cut.check_signing_parent(batch, anchor)
    }

    pub(crate) fn covers_local_promises(
        &self, batch: &burst_store_lmdb::LmdbWriteTransaction<'_>, representative: &WalletAddress,
    ) -> Result<bool, burst_store::StoreError> {
        batch.covers_consensus_votes(representative, &self.cut.parent, &self.cut.parent, &self.cut.included)
    }

    pub(crate) fn seal_local_votes(
        &self,
        batch: &mut burst_store_lmdb::LmdbWriteTransaction<'_>,
        anchor: &CheckpointAnchor,
        representative: &WalletAddress,
    ) -> Result<(), burst_store::StoreError> {
        self.validate_parent(anchor)?;
        self.cut.seal_local_votes(batch, anchor, representative)
    }
}

impl SnapshotContinuation {
    pub fn deadline_vote_coverage_for_manifest(
        &self,
        tail: &SnapshotTail,
        manifest: &DeadlineManifest,
        selections: &(impl SelectionSource + ?Sized),
    ) -> Result<CompactDeadlineCoverage, ReplayError> {
        self.anchor
            .validate_successor(&manifest.checkpoint)
            .map_err(|error| super::super::invalid(error.to_string()))?;
        if bincode::serialized_size(manifest)
            .map_err(|error| super::super::invalid(error.to_string()))?
            > crate::checkpoint_store::MAX_RECORD_BYTES
        {
            return Err(super::super::invalid(
                "deadline manifest exceeds byte limit",
            ));
        }
        let coverage = self
            .deadline_vote_coverage(tail, selections)?
            .ok_or_else(|| super::super::invalid("deadline proposal has no due effects"))?;
        if coverage.manifest() != manifest {
            return Err(super::super::invalid(
                "deadline proposal differs from compact replay",
            ));
        }
        Ok(coverage)
    }

    pub fn deadline_vote_coverage(
        &self,
        tail: &SnapshotTail,
        selections: &(impl SelectionSource + ?Sized),
    ) -> Result<Option<CompactDeadlineCoverage>, ReplayError> {
        let Some(manifest) = self.prepare_deadline(tail, selections)? else {
            return Ok(None);
        };
        if bincode::serialized_size(&manifest)
            .map_err(|error| super::super::invalid(error.to_string()))?
            > crate::checkpoint_store::MAX_RECORD_BYTES
        {
            return Err(super::super::invalid(
                "deadline manifest exceeds byte limit",
            ));
        }
        Ok(Some(CompactDeadlineCoverage {
            manifest,
            cut: self.cut_coverage(tail)?,
        }))
    }
    /// Derive due deadline effects without applying them to the authenticated
    /// base. A manifest alone grants neither signing nor installation authority.
    pub fn prepare_deadline(
        &self,
        tail: &SnapshotTail,
        selections: &(impl SelectionSource + ?Sized),
    ) -> Result<Option<DeadlineManifest>, ReplayError> {
        self.check_selection_receipts(selections)?;
        let mut candidate = Self {
            anchor: self.anchor.clone(),
            context: self.context,
            state: self.state.clone(),
        };
        let blocks = candidate.execute(tail, selections, true)?;
        candidate
            .state
            .require_closed_governance(burst_types::Timestamp::new(tail.cutoff()))?;
        let cut = CompactCut {
            cutoff: tail.cutoff(),
            commitment: tail.commitment(),
            blocks: &blocks,
        };
        let params = candidate.state.params().clone();
        let Some(root) = candidate.state.apply_agreed_deadlines(&cut, &params)? else {
            return Ok(None);
        };
        let (manifest, _) = PreparedDeadline::manifest_for_cut(
            &self.anchor,
            tail.cutoff(),
            tail.commitment(),
            tail.heads(),
            &candidate.state,
            root,
        )
        .map_err(|error| super::super::invalid(error.to_string()))?;
        Ok(Some(manifest))
    }
}
