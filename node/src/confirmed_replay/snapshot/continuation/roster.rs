use super::*;
use crate::checkpoint_roster::{PreparedRoster, RosterManifest};
use burst_store::StoreError;

pub struct CompactRosterCoverage {
    pub(super) manifest: RosterManifest,
    pub(super) cut: CompactCutCoverage,
}

impl CompactRosterCoverage {
    pub fn manifest(&self) -> &RosterManifest {
        &self.manifest
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
                "roster coverage differs from its parent".into(),
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
    pub fn roster_vote_coverage_for_manifest(
        &self,
        tail: &SnapshotTail,
        manifest: &RosterManifest,
        selections: &(impl SelectionSource + ?Sized),
    ) -> Result<CompactRosterCoverage, ReplayError> {
        self.anchor
            .validate_successor(&manifest.checkpoint)
            .map_err(|error| super::super::invalid(error.to_string()))?;
        if bincode::serialized_size(manifest)
            .map_err(|error| super::super::invalid(error.to_string()))?
            > crate::checkpoint_store::MAX_RECORD_BYTES
        {
            return Err(super::super::invalid("roster manifest exceeds byte limit"));
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
        let (expected, _) = PreparedRoster::manifest_for_cut(
            &self.anchor,
            tail.cutoff(),
            tail.commitment(),
            tail.heads(),
            &candidate.state,
        )
        .map_err(|error| super::super::invalid(error.to_string()))?;
        if expected != *manifest {
            return Err(super::super::invalid(
                "roster proposal differs from compact replay",
            ));
        }
        Ok(CompactRosterCoverage {
            manifest: expected,
            cut: self.cut_coverage(tail)?,
        })
    }
}
