use super::*;
use crate::checkpoint_governance::{GovernanceManifest, PreparedGovernance};
use burst_store::StoreError;

pub struct CompactGovernanceCoverage {
    pub(super) manifest: GovernanceManifest,
    pub(super) cut: CompactCutCoverage,
}

impl CompactGovernanceCoverage {
    pub fn manifest(&self) -> &GovernanceManifest {
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
                "governance coverage differs from its parent".into(),
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
        &self,
        batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
        representative: &WalletAddress,
    ) -> Result<bool, StoreError> {
        batch.covers_consensus_votes(
            representative,
            &self.cut.parent,
            &self.cut.parent,
            &self.cut.included,
        )
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
    pub fn governance_vote_coverage_for_manifest(
        &self,
        tail: &SnapshotTail,
        manifest: &GovernanceManifest,
        selections: &(impl SelectionSource + ?Sized),
    ) -> Result<CompactGovernanceCoverage, ReplayError> {
        self.anchor
            .validate_successor(&manifest.checkpoint)
            .map_err(|error| super::super::invalid(error.to_string()))?;
        if bincode::serialized_size(manifest)
            .map_err(|error| super::super::invalid(error.to_string()))?
            > crate::checkpoint_store::MAX_RECORD_BYTES
        {
            return Err(super::super::invalid(
                "governance manifest exceeds byte limit",
            ));
        }
        self.check_selection_receipts(selections)?;
        let mut candidate = Self {
            anchor: self.anchor.clone(),
            context: self.context,
            state: self.state.clone(),
        };
        let blocks = candidate.execute(tail, selections, true)?;
        let cut = CompactCut {
            cutoff: tail.cutoff(),
            commitment: tail.commitment(),
            blocks: &blocks,
        };
        let (proposals, root) = candidate.state.apply_governance_cut(&cut)?;
        let (expected, _) = PreparedGovernance::manifest_for_cut(
            &self.anchor,
            tail.cutoff(),
            tail.commitment(),
            tail.heads(),
            proposals,
            root,
            &candidate.state,
        )
        .map_err(|error| super::super::invalid(error.to_string()))?;
        if expected != *manifest {
            return Err(super::super::invalid(
                "governance proposal differs from compact replay",
            ));
        }
        Ok(CompactGovernanceCoverage {
            manifest: expected,
            cut: self.cut_coverage(tail)?,
        })
    }
}
