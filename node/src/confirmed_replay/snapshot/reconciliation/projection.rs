use super::*;
use burst_store_lmdb::write_batch::checkpoint_projection::CompactProjection;

/// Sparse ledger rows in the reconciliation transaction. Project the economic
/// tables before committing; dropping this guard aborts the transaction.
pub struct ProjectedSnapshot<'db> {
    boundary: ReconciledSnapshot<'db>,
    projection: CompactProjection,
}

/// Ledger, economic tables and the authenticated base share one uncommitted
/// writer with the next epoch. Startup keeps admission closed until recovery;
/// live installation publishes matching caches under the runtime's engine locks.
pub struct PreparedSnapshotState<'db> {
    projected: ProjectedSnapshot<'db>,
    governance: crate::governance_commit::CandidateGovernance,
    #[cfg(test)]
    fail_sync: bool,
}

impl PreparedSnapshotState<'_> {
    #[cfg(test)]
    pub(crate) fn with_sync_failure(mut self, fail: bool) -> Self {
        self.fail_sync = fail;
        self
    }

    /// Commit and force-sync before returning state for runtime publication.
    /// The caller must hold the runtime installation locks. A sync error occurs
    /// after commit and does not roll back the database.
    pub(crate) fn commit_runtime(
        self,
        store: &burst_store_lmdb::LmdbStore,
    ) -> Result<crate::confirmed_replay::installation::InstalledCheckpoint, StoreError> {
        if !cfg!(test)
            && !burst_verification::checkpoint_selection_enabled(
                self.projected.snapshot().anchor().network(),
            )
        {
            return Err(invalid(
                "live snapshot installation is behind the release gate",
            ));
        }
        let ledger_counts = self.projected.boundary.transaction.ledger_counts()?;
        let anchor = self.projected.snapshot().anchor().clone();
        let ProjectedSnapshot {
            boundary,
            projection,
        } = self.projected;
        let ReconciledSnapshot {
            transaction,
            snapshot,
            retained_promises,
            ..
        } = boundary;
        transaction.commit()?;
        #[cfg(test)]
        if self.fail_sync {
            return Err(StoreError::Backend("injected snapshot sync failure".into()));
        }
        store.force_sync().map_err(invalid)?;
        let state = snapshot.into_state();
        Ok(crate::confirmed_replay::installation::InstalledCheckpoint {
            brn: state.brn,
            trst: state.trst,
            verification: state.verification,
            governance: self.governance,
            anchor,
            retired_blocks: projection.retired_blocks,
            minority_promises: Vec::new(),
            retained_promises,
            ledger_counts,
            frontiers: state
                .accounts
                .into_values()
                .filter(|account| account.block_count > 0)
                .map(|account| (account.address, account.head))
                .collect(),
        })
    }

    /// Only gated startup may call this while admission remains closed. It must
    /// force-sync and recover all installed-state caches before opening admission.
    pub(crate) fn commit_for_startup(self) -> Result<(), StoreError> {
        if !cfg!(test)
            && !burst_verification::checkpoint_selection_enabled(
                self.projected.snapshot().anchor().network(),
            )
        {
            return Err(invalid("snapshot bootstrap is behind the release gate"));
        }
        self.projected.boundary.transaction.commit()
    }

    pub fn snapshot(&self) -> &AuthenticatedSnapshot {
        self.projected.snapshot()
    }
    pub fn retained_blocks(&self) -> u64 {
        self.projected.retained_blocks()
    }
    pub fn ledger_blocks(&self) -> u64 {
        self.projected.ledger_blocks()
    }
    pub fn retained_promises(&self) -> &[BlockHash] {
        &self.projected.boundary.retained_promises
    }

    #[cfg(test)]
    pub(in crate::confirmed_replay) fn transaction(&self) -> &LmdbWriteTransaction<'_> {
        &self.projected.boundary.transaction
    }
}

impl<'db> ProjectedSnapshot<'db> {
    pub fn project_economics(mut self) -> Result<PreparedSnapshotState<'db>, StoreError> {
        #[cfg(test)]
        let started = std::time::Instant::now();
        #[cfg(test)]
        let report = |stage: &str, batch: &LmdbWriteTransaction<'_>| {
            if std::env::var_os("BURST_SNAPSHOT_INSTALL_STAGE_LOG").is_some() {
                eprintln!("snapshot-install stage={stage} elapsed_ms={}", started.elapsed().as_millis());
                crate::paged_metadata::report_metadata_pages(batch, stage);
            }
        };
        #[cfg(test)]
        report("validate-retired", &self.boundary.transaction);
        let previous = self.boundary.transaction.payment_epoch()?;
        crate::confirmed_replay::installation::validate_retired(
            &self.boundary.transaction,
            previous,
            &self.projection.retired_blocks,
        )?;
        let snapshot = &self.boundary.snapshot;
        #[cfg(test)]
        report("stage-records", &self.boundary.transaction);
        let records = crate::checkpoint_store::stage_snapshot_head(
            &mut self.boundary.transaction,
            snapshot
                .authority_path()
                .map(|result| result.map_err(invalid)),
        )?;
        // Whole records need contiguous LMDB spans before per-account economic
        // writes fragment them. Every projection still shares this atomic writer.
        #[cfg(test)]
        report("archive-records", &self.boundary.transaction);
        records.archive(&mut self.boundary.transaction, snapshot)?;
        #[cfg(test)]
        report("persist-economics", &self.boundary.transaction);
        let governance = snapshot
            .state
            .persist_projection(&mut self.boundary.transaction, snapshot.anchor())?;
        #[cfg(test)]
        report("persist-snapshot", &self.boundary.transaction);
        super::super::installed::persist(snapshot, &mut self.boundary.transaction)?;
        #[cfg(test)]
        report("advance-epoch", &self.boundary.transaction);
        let retained = self
            .boundary
            .transaction
            .advance_snapshot_payment_epoch(&previous, &snapshot.anchor().hash())?;
        if retained != self.boundary.retained_promises {
            return Err(invalid(
                "economic projection changed local signing promises",
            ));
        }
        #[cfg(test)]
        report("prepared", &self.boundary.transaction);
        Ok(PreparedSnapshotState {
            projected: self,
            governance,
            #[cfg(test)]
            fail_sync: false,
        })
    }
}

impl ProjectedSnapshot<'_> {
    pub fn snapshot(&self) -> &AuthenticatedSnapshot {
        &self.boundary.snapshot
    }
    pub fn retained_blocks(&self) -> u64 {
        self.projection.retained_blocks
    }
    pub fn ledger_blocks(&self) -> u64 {
        self.projection.ledger_blocks
    }
    /// Removed from the local applied projection, not proven absent from the
    /// certified history. Their original bodies remain archived in the writer.
    pub fn retired_blocks(&self) -> &[BlockHash] {
        &self.projection.retired_blocks
    }
}

impl<'db> ReconciledSnapshot<'db> {
    /// Additional bodies must already be authenticated before taking the writer.
    /// Retains the reconciliation proofs and local cemented bodies; each opened
    /// snapshot account also requires its authenticated head body.
    pub fn project_ledger(
        mut self,
        witnesses: &[HistoricalBlock],
    ) -> Result<ProjectedSnapshot<'db>, StoreError> {
        let context = self.snapshot.history_context();
        let mut seen = HashSet::new();
        for witness in witnesses {
            if witness.context() != context || !seen.insert(witness.block().hash) {
                return Err(invalid("duplicate or foreign compact projection witness"));
            }
            self.witnesses.insert(witness)?;
        }
        let epoch = self.transaction.payment_epoch()?;
        let projection = self
            .transaction
            .replace_compact_checkpoint_ledger_account_map(
                epoch,
                self.witnesses.iter(),
                &self.snapshot.state.accounts,
                Some(&self.snapshot.state.brn.wallets),
            )?;
        Ok(ProjectedSnapshot {
            boundary: self,
            projection,
        })
    }
}
