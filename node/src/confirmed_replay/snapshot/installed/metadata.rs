//! Exact installed-epoch recovery without publishing authority or economics.

use super::super::tail::cemented::{AppliedTail, CementedTail};
use super::*;
use crate::confirmed_replay::scratch_records::ScratchSequence;
use crate::{checkpoint_sync::witness, connection_registry::ConnectionRegistry};
use burst_types::{BlockHash, Timestamp};
use std::sync::Arc;
use tokio::sync::RwLock;

mod installation;
mod capture;
pub(crate) use capture::{recover_retirement_catalogue, recover_checkpoint_authority};
mod source;
mod retention;
mod resolution;
pub(crate) use resolution::BodyOrigin;
#[cfg(test)]
pub(crate) use resolution::ResolvedBody;
#[cfg(test)]
pub(crate) use retention::{SignerRecord, PaymentPromise};
pub use retention::RecoveredTrstReferences;
pub(crate) use retention::OwnedRetirement;
pub(crate) use installation::CompactPreparation;
pub use installation::PreparedCompactInstallation;
use source::{RecoveryBase, RecoverySource};

/// Authenticated installed metadata and cemented governance, not permission to
/// start admission. Applied economics and persisted engine snapshots are checked;
/// remaining indexes and live publication still require startup integration.
/// Local journals are validated and pinned without changing their promises.
pub struct RecoveredInstalledMetadata {
    verified_cut: std::sync::Mutex<Option<burst_store_lmdb::write_batch::CleanStorageCut>>,
    verified_view: std::sync::Mutex<Option<burst_store_lmdb::write_batch::CleanStorageCut>>,
    initial: BlockHash,
    recovered: RecoveredSnapshot,
    cemented: Arc<CementedTail>,
    base_commitment: RecoveryBase,
    inventory: Arc<super::retained::RetainedInventory>,
    journals: super::journals::JournalGuard,
    applied: Arc<AppliedTail>,
    economics: Arc<super::economics::AppliedEconomics>,
}

/// Replay-derived preparation without a retained database transaction. Records
/// describe the observed provisional cut; staging must recheck live finality.
/// Inspection alone grants neither rollback nor deletion authority.
pub(crate) struct PreparedRecoveryProjection {
    metadata: RecoveredInstalledMetadata,
    derived: super::changes::DerivedChanges,
    cache_at: Timestamp,
}

impl PreparedRecoveryProjection {
    /// Stream authenticated transition deltas without collecting all journals
    /// or holding the publication writer. Callback failure stops this traversal.
    pub(crate) fn visit_replayed_changes(
        &self,
        mut visit: impl FnMut(&super::ReplayedEconomicChange) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        for record in self.derived.records.iter() {
            visit(&record?)?;
        }
        Ok(())
    }

    pub(crate) fn stage(self, store: &LmdbStore) -> Result<RecoveredIndexProjection<'_>, StoreError> {
        let mut transaction = store.tx_begin_write()?;
        transaction.require_clean_checkpoint_boundary()?;
        self.metadata.verify_unchanged(&transaction)?;
        self.derived.rows.verify_format(&transaction)?;
        let economic_changes = super::changes::finality::reconcile(
            &transaction, self.derived.records.iter(),
        )?;
        transaction.replace_checkpoint_token_indexes(&self.metadata.economics.mint_indexes())?;
        Ok(RecoveredIndexProjection {
            #[cfg(test)]
            projected_transitions: self.derived.projected_transitions,
            transaction,
            metadata: self.metadata,
            economic_changes,
            rows: self.derived.rows,
            cache_at: self.cache_at,
        })
    }
}

/// Uncommitted startup index reconstruction. Undo staging must pair these rows
/// with a matching runtime bundle before the startup coordinator can commit.
pub struct RecoveredIndexProjection<'db> {
    #[cfg(test)]
    projected_transitions: usize,
    transaction: LmdbWriteTransaction<'db>,
    metadata: RecoveredInstalledMetadata,
    economic_changes: ScratchSequence<super::ReplayedEconomicChange>,
    rows: super::changes::Rows,
    cache_at: Timestamp,
}

/// Journals, matching cache bytes and replay-derived engines staged together.
/// Only the gated startup coordinator can commit and publish the runtime.
pub struct RecoveredUndoProjection<'db> {
    transaction: LmdbWriteTransaction<'db>,
    runtime: super::runtime::RecoveredRuntime,
    recovery: ReplayCache,
}

impl RecoveredUndoProjection<'_> {
    /// Startup must hold every runtime publication lock and keep admission
    /// closed. The caller must force-sync before publishing these engines.
    pub(crate) fn commit_runtime(
        mut self,
    ) -> Result<(super::runtime::RecoveredRuntime, ReplayCache), StoreError> {
        if !cfg!(test)
            && !burst_verification::checkpoint_selection_enabled(self.runtime.anchor.network())
        {
            return Err(invalid(
                "compact runtime recovery is behind the release gate",
            ));
        }
        if !self.transaction.uses_normalized_verification_records()? {
            // Journals and runtime are already authenticated and staged together.
            // Activate their storage format in this commit, as fresh installation
            // does, instead of rewriting the same state in a second transaction.
            self.transaction.publish_normalized_recovered_verification(
                &self.runtime.verification,
            )?;
        }
        self.transaction.commit()?;
        Ok((self.runtime, self.recovery))
    }

    /// Matches the staged rows, but grants no permission to publish or sign.
    pub fn runtime(&self) -> &super::runtime::RecoveredRuntime {
        &self.runtime
    }

    pub fn journal(
        &self,
        block: &BlockHash,
    ) -> Result<Option<burst_store_lmdb::block_undo::BlockUndo>, StoreError> {
        self.transaction.block_undo(block)
    }

    #[cfg(test)]
    pub(in crate::confirmed_replay) fn commit_for_test(self) -> Result<(), StoreError> {
        self.transaction.commit()
    }
}

impl<'db> RecoveredIndexProjection<'db> {
    pub fn stage_undo(mut self) -> Result<RecoveredUndoProjection<'db>, StoreError> {
        // stage() verified dependencies under this exclusive writer. It has
        // remained privately owned, with only derived mint indexes replaced;
        // no external writer can invalidate that verification before staging.
        let started = std::time::Instant::now();
        self.rows.stage_caches(&mut self.transaction)?;
        // The writer now owns the staged bytes. Release the projection before
        // runtime verification restores and compares the staged engines.
        drop(self.rows);
        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis(),
            "recovery publication staged caches"
        );
        let started = std::time::Instant::now();
        // These writes are independent hash-keyed bodies, not ordered undo.
        // Authenticate the whole inventory in one scan instead of root walks
        // for each witness. The same writer aborts all rows on any mismatch.
        self.metadata.applied.tail().retained().try_visit(|proof| {
            let key = format!("checkpoint_history/v1/{}", proof.block().hash);
            let bytes = bincode::serialize(proof.block()).map_err(invalid)?;
            if let Some(old) = self.transaction.get_meta(&key)? {
                if old != bytes {
                    return Err(invalid("retained dependency body changed"));
                }
            } else {
                self.transaction.put_meta(&key, &bytes)?;
            }
            Ok(())
        })?;
        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis(),
            "recovery publication staged retained history"
        );
        let started = std::time::Instant::now();
        let mut records = ScratchSequence::new(self.transaction.storage_directory())?;
        let stable_wallets = self.transaction.uses_stable_trst_records()?;
        let row_wallets = crate::trst_storage::uses_row_storage(&self.transaction)?;
        let row_graph = self.transaction.uses_graph_records()?;
        for record in self.economic_changes.iter() {
            let record = record?;
            let governance = record.governance;
            let mut record = burst_store_lmdb::block_undo::BlockUndo {
                block: record.block,
                account: record.account,
                height: record.height,
                changes: record.changes,
            };
            if row_wallets && !stable_wallets {
                record = crate::trst_storage::convert_undo(record)?;
            }
            if row_graph {
                record = crate::graph_storage::convert_undo(&self.transaction, record)?;
            }
            if let Some(projection) = governance {
                record = self
                    .transaction
                    .stage_recovered_governance_undo(record, projection)?;
            } else {
                record = self.transaction.project_recovered_governance_undo(record)?;
            }
            records.push(&record)?;
        }
        self.transaction
            .replace_checkpoint_undo_stream(records.iter())?;
        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis(),
            "recovery publication staged ordered undo"
        );
        let started = std::time::Instant::now();
        let runtime = Arc::unwrap_or_clone(self.metadata.economics).into_runtime(
            &self.transaction,
            self.metadata.recovered.continuation.anchor().clone(),
            self.cache_at,
        )?;
        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis(),
            "recovery publication prepared runtime"
        );
        Ok(RecoveredUndoProjection {
            transaction: self.transaction,
            runtime,
            recovery: ReplayCache {
                base: self.metadata.base_commitment.commitment(),
                recovered: self.metadata.recovered,
            },
        })
    }
    /// Provisional changes only, with existing confirmation heights preserved.
    /// Expiry caches use the selected recovery clock. Matching cache publication
    /// and lifecycle reconciliation remain required before installation.
    #[cfg(test)]
    pub fn economic_changes(&self) -> Vec<super::ReplayedEconomicChange> {
        assert_eq!(
            self.projected_transitions as u64,
            self.economic_changes.iter().count() as u64,
            "only provisional transitions may incur delta projection work"
        );
        self.economic_changes
            .iter()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    pub fn metadata(&self) -> &RecoveredInstalledMetadata {
        &self.metadata
    }

    pub fn verify_unchanged(&self) -> Result<(), StoreError> {
        self.metadata.verify_unchanged(&self.transaction)
    }

    #[cfg(test)]
    pub(in crate::confirmed_replay) fn transaction(&self) -> &LmdbWriteTransaction<'_> {
        &self.transaction
    }
}

impl RecoveredInstalledMetadata {

    /// Expiry lookups can have been cleaned locally or reflect earlier governed
    /// periods. Rebuild all mint lookups from replay, never from live holdings.
    /// Dropping the result aborts every write; no admission or signing is enabled.
    pub fn project_token_indexes(
        self,
        store: &LmdbStore,
    ) -> Result<RecoveredIndexProjection<'_>, StoreError> {
        let cache_at = Timestamp::new(self.recovered.continuation.anchor().cutoff());
        self.project_token_indexes_at(store, cache_at)
    }

    /// Use one local clock for both reconstructed journal caches and final
    /// account/portfolio caches. Signed replay, pending returns, lifecycle
    /// deadlines and the authenticated cutoff do not use this clock.
    /// The caller must supply its validated startup clock, at or after the cut.
    /// This remains an uncommitted projection, not permission to start admission.
    pub fn project_token_indexes_at(
        self,
        store: &LmdbStore,
        cache_at: Timestamp,
    ) -> Result<RecoveredIndexProjection<'_>, StoreError> {
        self.prepare_token_indexes_at(store, cache_at)?.stage(store)
    }

    /// Explicit certified migration: derive stable holdings, keyed governance
    /// and matching journals from authenticated replay, then stage atomically. This grants no
    /// publication authority; gated compact startup selects this before publication.
    pub fn project_token_indexes_with_stable_holdings(
        self,
        store: &LmdbStore,
        cache_at: Timestamp,
    ) -> Result<RecoveredIndexProjection<'_>, StoreError> {
        self.prepare_token_indexes_for_storage(store, cache_at, true)?.stage(store)
    }

    /// Prepare replay-derived scratch records for retention inspection and later
    /// staging. The database is unchanged; the result retains no writer.
    pub(crate) fn prepare_token_indexes_at(
        self,
        store: &LmdbStore,
        cache_at: Timestamp,
    ) -> Result<PreparedRecoveryProjection, StoreError> {
        self.prepare_token_indexes_for_storage(store, cache_at, false)
    }

    fn prepare_token_indexes_for_storage(
        self,
        store: &LmdbStore,
        cache_at: Timestamp,
        migrate_holdings: bool,
    ) -> Result<PreparedRecoveryProjection, StoreError> {
        self.prepare_token_indexes_read(store, cache_at, migrate_holdings, None, &|_| false)?
            .ok_or_else(|| invalid("uncancelled projection was interrupted"))
    }

    #[cfg(test)]
    pub(crate) fn prepare_token_indexes_view(
        self, store: &LmdbStore, view: &burst_store_lmdb::LmdbReadTransaction<'_>,
        cache_at: Timestamp,
    ) -> Result<PreparedRecoveryProjection, StoreError> {
        self.prepare_token_indexes_view_until(store, view, cache_at, &|_| false)?
            .ok_or_else(|| invalid("uncancelled projection was interrupted"))
    }

    pub(crate) fn prepare_token_indexes_view_until(
        self, store: &LmdbStore, view: &burst_store_lmdb::LmdbReadTransaction<'_>,
        cache_at: Timestamp, cancelled: &dyn Fn(&'static str) -> bool,
    ) -> Result<Option<PreparedRecoveryProjection>, StoreError> {
        self.prepare_token_indexes_read(store, cache_at, false, Some(&store.read_view(view)?), cancelled)
    }

    fn prepare_token_indexes_read(
        self, store: &LmdbStore, cache_at: Timestamp, migrate_holdings: bool,
        captured: Option<&burst_store_lmdb::read_view::LedgerReadView<'_>>,
        cancelled: &dyn Fn(&'static str) -> bool,
    ) -> Result<Option<PreparedRecoveryProjection>, StoreError> {
        if cancelled("retirement-capture:projection-start") { return Ok(None); }
        let (
            stable_holdings,
            rate_records,
            wallet_records,
            normalized_verification,
            keyed_governance,
            provisional,
        ) = {
            let writer = if captured.is_none() { Some(store.tx_begin_write()?) } else { None };
            let batch = match captured {
                Some(view) => { self.verify_view(view)?; view.clone() }
                None => {
                    let writer = writer.as_ref().expect("live projection has a writer");
                    self.verify_unchanged(writer)?;
                    writer.read_view()
                }
            };
            let mut provisional = std::collections::HashSet::new();
            for account in batch.account_entries()? {
                let account = account?;
                if cancelled("retirement-capture:projection-account") { return Ok(None); }
                if account.confirmation_height > account.block_count {
                    return Err(invalid("confirmation exceeds account height"));
                }
                for previous_height in account.confirmation_height..account.block_count {
                    if cancelled("retirement-capture:projection-height") { return Ok(None); }
                    let height = previous_height + 1;
                    let hash = batch
                        .block_at_height(&account.address, height)?
                        .ok_or_else(|| invalid("provisional history index is missing"))?;
                    if provisional.len() >= super::changes::MAX_ROWS || !provisional.insert(hash) {
                        return Err(invalid("duplicate or excessive provisional history"));
                    }
                }
            }
            (
                batch.uses_stable_trst_records()?,
                batch.uses_brn_rate_records()?,
                batch.uses_brn_wallet_records()?,
                batch.uses_normalized_verification_records()?,
                batch.uses_governance_records()?,
                provisional,
            )
        };
        let Some(derived) = super::changes::derive(
            store.env().path(),
            &self.recovered,
            &self.applied,
            cache_at,
            &provisional,
            &self.economics,
            stable_holdings || migrate_holdings,
            stable_holdings,
            true, // Authenticated replay replaces rate rows and all matching journals together.
            rate_records,
            true, // Wallet projection and all provisional journals migrate together.
            wallet_records,
            normalized_verification,
            keyed_governance || migrate_holdings,
            keyed_governance,
            &|| cancelled("retirement-capture:rollback-transition"),
        )? else { return Ok(None); };
        Ok(Some(PreparedRecoveryProjection { metadata: self, derived, cache_at }))
    }

    pub fn recovered(&self) -> &RecoveredSnapshot {
        &self.recovered
    }

    /// Extract a checkpoint worker context from checked installed economics.
    /// This does not publish runtime caches or install an accepted certificate.
    #[cfg(test)]
    pub(crate) fn into_checkpoint_recovery(
        self,
        store: &LmdbStore,
        previous: Option<&burst_consensus::checkpoint::CheckpointAnchor>,
    ) -> Result<ReplayCache, StoreError> {
        let reader = store.tx_begin_read()?;
        self.into_checkpoint_recovery_view(&store.read_view(&reader)?, previous)
    }

    fn into_checkpoint_recovery_view(
        self,
        view: &burst_store_lmdb::read_view::LedgerReadView<'_>,
        previous: Option<&burst_consensus::checkpoint::CheckpointAnchor>,
    ) -> Result<ReplayCache, StoreError> {
        if !cfg!(test)
            && !burst_verification::checkpoint_selection_enabled(
                self.recovered.continuation.anchor().network(),
            )
        {
            return Err(invalid(
                "compact authority handoff is behind the release gate",
            ));
        }
        let next = self.recovered.continuation.anchor();
        if let Some(previous) = previous {
            let mut ancestor = false;
            for anchor in self.recovered.selections.authority.iter() {
                let anchor = anchor?;
                ancestor |=
                    anchor.height() == previous.height() && anchor.hash() == previous.hash();
            }
            if next.height() <= previous.height()
                || next.network() != previous.network()
                || next.genesis_hash() != previous.genesis_hash()
                || !ancestor
            {
                return Err(invalid(
                    "compact authority does not advance the current branch",
                ));
            }
        }
        self.verify_view(view)?;
        Ok(ReplayCache {
            base: self.base_commitment.commitment(),
            recovered: self.recovered,
        })
    }

    pub fn cemented(&self) -> &CementedTail {
        &self.cemented
    }

    pub fn applied(&self) -> &AppliedTail {
        &self.applied
    }

    #[cfg(test)]
    pub(in crate::confirmed_replay) fn shares_projection(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.economics, &other.economics)
            && Arc::ptr_eq(&self.cemented, &other.cemented)
            && Arc::ptr_eq(&self.applied, &other.applied)
            && Arc::ptr_eq(&self.inventory, &other.inventory)
    }

    #[cfg(test)]
    pub(in crate::confirmed_replay) fn uses_committed_projection(
        &self,
        other: &super::CommittedProjection,
    ) -> bool {
        Arc::ptr_eq(&self.economics, &other.projection.economics)
            && Arc::ptr_eq(&self.cemented, &other.projection.cemented)
            && Arc::ptr_eq(&self.applied, &other.projection.applied)
            && Arc::ptr_eq(&self.inventory, &other.projection.inventory)
    }

    pub fn applied_account(
        &self,
        address: &burst_types::WalletAddress,
    ) -> Option<&burst_store::account::AccountInfo> {
        self.economics.account(address)
    }

    /// Recheck under the caller's writer before any eventual publication. This
    /// neither commits the writer nor validates the remaining startup state.
    pub fn verify_unchanged(&self, batch: &LmdbWriteTransaction<'_>) -> Result<(), StoreError> {
        if self.verified_cut.lock().map_err(|_| invalid("recovery verification lock poisoned"))?
            .as_ref().is_some_and(|cut| batch.matches_clean_storage_cut(cut)) {
            return Ok(());
        }
        if self.verified_view.lock().map_err(|_| invalid("recovery verification lock poisoned"))?
            .as_ref().is_some_and(|cut| batch.matches_clean_storage_cut(cut)) {
            return Ok(());
        }
        let started = std::time::Instant::now();
        let mut previous = started;
        let mut report = |stage: &'static str| {
            let now = std::time::Instant::now();
            tracing::debug!(stage, elapsed_us = now.duration_since(previous).as_micros(),
                total_us = now.duration_since(started).as_micros(), "recovery publication checked dependency");
            previous = now;
        };
        self.base_commitment.verify(batch)?;
        report("base");
        self.recovered.verify_records_unchanged(batch)?;
        report("records");
        self.inventory.verify_unchanged(batch)?;
        report("inventory");
        self.journals.verify_unchanged(batch)?;
        report("journals");
        self.applied.verify_unchanged(batch)?;
        report("applied");
        self.economics.verify_unchanged(batch)?;
        report("economics");
        self.recovered
            .continuation
            .verify_installed_metadata_in(batch, &self.cemented)?;
        report("installed");
        *self.verified_cut.lock().map_err(|_| invalid("recovery verification lock poisoned"))? =
            batch.clean_storage_cut();
        Ok(())
    }
    /// Verify and cache this exact committed view. A later writer may reuse the
    /// result only if it still matches that view; otherwise it rechecks dependencies.
    pub(crate) fn verify_view(&self, batch: &burst_store_lmdb::read_view::LedgerReadView<'_>) -> Result<(), StoreError> {
        self.verify_view_with_poll(batch, &|_| Ok(()))
    }

    pub(crate) fn verify_view_until(
        &self, batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
        cancelled: &dyn Fn(&'static str) -> bool,
    ) -> Result<bool, StoreError> {
        let stopped = std::cell::Cell::new(false);
        let result = self.verify_view_with_poll(batch, &|stage| {
            if cancelled(stage) {
                stopped.set(true);
                return Err(invalid("captured metadata verification cancelled"));
            }
            Ok(())
        });
        if stopped.get() { return Ok(false); }
        result.map(|()| true)
    }

    fn verify_view_with_poll(&self, batch: &burst_store_lmdb::read_view::LedgerReadView<'_>, poll: &dyn Fn(&'static str) -> Result<(), StoreError>) -> Result<(), StoreError> {
        poll("metadata:verify-start")?;
        if self.has_verified_view(batch)? { return Ok(()); }
        self.base_commitment.verify_view(batch)?;
        poll("metadata:record-observations")?;
        self.recovered.verify_records_view_with_poll(batch, poll)?;
        self.inventory.verify_view_with_poll(batch, poll)?;
        self.journals.verify_view_with_poll(batch, poll)?;
        poll("metadata:applied-tail")?;
        self.applied.verify_view_with_poll(batch, poll)?;
        self.economics.verify_view_with_poll(batch, poll)?;
        poll("metadata:installed-state")?;
        self.recovered
            .continuation
            .verify_installed_metadata_view_with_poll(batch, &self.cemented, poll)?;
        poll("metadata:verification-complete")?;
        *self.verified_view.lock().map_err(|_| invalid("captured verification lock poisoned"))? =
            batch.clean_storage_cut();
        Ok(())
    }

    pub(in crate::confirmed_replay) fn has_verified_view(
        &self, batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    ) -> Result<bool, StoreError> {
        let Some(current) = batch.clean_storage_cut() else { return Ok(false); };
        Ok(self.verified_view.lock().map_err(|_| invalid("captured verification lock poisoned"))?
            .as_ref().is_some_and(|cut| cut.same_committed_view(&current)))
    }

}

/// Read the base and installed epoch from one database view, authenticate that
/// exact epoch through retained certificates and fetch missing ancestry proofs.
/// A newer accepted tip is never substituted for the installed authority.
/// Without an archive, a non-genesis installed epoch is replayed from configured
/// genesis through the same incremental verifier. No storage is changed.
pub fn recover_metadata_from_peers(
    store: Arc<LmdbStore>,
    network: NetworkId,
    initial_params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<RecoveredInstalledMetadata>, StoreError>> + Send>>
{
    // Keep the recovery state machine off callers' async frames. It carries
    // ledger candidates across several independent peer waits.
    Box::pin(recover_metadata_with_cache(
        store,
        network,
        initial_params,
        registry,
        None,
    ))
}

pub(crate) enum StartupRecovery {
    Ready(Option<RecoveredInstalledMetadata>),
    Cancelled,
}

#[derive(Clone, Default)]
pub(super) struct RecoveryControl {
    pub(super) shutdown: Option<Arc<crate::shutdown::ShutdownController>>,
    parent_shutdown: Option<Arc<crate::shutdown::ShutdownController>>,
    capture_limits: Option<crate::body_retirement::capture_budget::CaptureLimits>,
    pub(super) durable_inputs: Option<(crate::confirmed_replay::scratch_file::ScratchLimit, usize)>,
    #[cfg(test)]
    probe: Option<fn(&str)>,
}

impl RecoveryControl {
    pub(super) async fn for_shutdown(
        _registry: &Arc<RwLock<ConnectionRegistry>>,
        shutdown: Arc<crate::shutdown::ShutdownController>,
    ) -> Self {
        Self {
            shutdown: Some(shutdown),
            #[cfg(test)]
            probe: _registry.read().await.recovery_probe,
            ..Self::default()
        }
    }

    /// Only for cancellation-safe peer requests, never blocking disk workers.
    /// Dropping the request closes its response channel; no partial proof is used.
    pub(super) async fn peer_wait<T>(
        &self,
        request: impl std::future::Future<Output = Result<T, StoreError>>,
    ) -> Result<Option<T>, StoreError> {
        let Some(shutdown) = &self.shutdown else { return request.await.map(Some); };
        let mut stop = shutdown.subscribe();
        tokio::select! {
            biased;
            _ = stop.recv() => Ok(None),
            result = request => result.map(Some),
        }
    }

    pub(super) fn checkpoint(&self, stage: &str) -> bool {
        #[cfg(test)]
        if let Some(probe) = self.probe {
            probe(stage);
        }
        let cancelled = self
            .shutdown
            .as_ref()
            .is_some_and(|stop| stop.is_requested())
            || self.parent_shutdown.as_ref().is_some_and(|stop| stop.is_requested());
        if cancelled {
            tracing::debug!(stage, "private installed recovery stopped at checkpoint");
        }
        cancelled
    }
}

/// Test convenience for recovery without a durable-input fixture.
#[cfg(test)]
pub(crate) async fn recover_metadata_until_shutdown(
    store: Arc<LmdbStore>,
    network: NetworkId,
    initial_params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
    shutdown: Arc<crate::shutdown::ShutdownController>,
) -> Result<StartupRecovery, StoreError> {
    recover_metadata_with_durable_inputs(store, network, initial_params, registry, shutdown, None).await
}

/// On requested shutdown, await the current phase before returning Cancelled.
/// Keep polling this future through shutdown so blocking workers are joined;
/// dropping it early does not wait for those workers.
pub(crate) async fn recover_metadata_with_durable_inputs(
    store: Arc<LmdbStore>, network: NetworkId, initial_params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>, shutdown: Arc<crate::shutdown::ShutdownController>,
    durable_inputs: Option<(crate::confirmed_replay::scratch_file::ScratchLimit, usize)>,
) -> Result<StartupRecovery, StoreError> {
    let control = RecoveryControl {
        shutdown: Some(shutdown.clone()),
        durable_inputs,
        parent_shutdown: None,
        capture_limits: None,
        #[cfg(test)]
        probe: registry.read().await.recovery_probe,
    };
    let recovered = recover_metadata(
        store,
        network,
        initial_params,
        registry,
        None,
        None,
        control,
    )
    .await?;
    // Preserve any actual recovery error above, including one racing shutdown.
    Ok(if shutdown.is_requested() {
        StartupRecovery::Cancelled
    } else {
        StartupRecovery::Ready(recovered)
    })
}

pub(crate) async fn recover_metadata_with_cache(
    store: Arc<LmdbStore>,
    network: NetworkId,
    initial_params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
    cached: Option<ReplayCache>,
) -> Result<Option<RecoveredInstalledMetadata>, StoreError> {
    // Recovery is nested inside live checkpoint-worker futures. Keep its state
    // off their stacks, including in unoptimized builds.
    recover_metadata(
        store,
        network,
        initial_params,
        registry,
        cached,
        None,
        RecoveryControl::default(),
    )
    .await
}

/// Retain only authenticated replay progress if later installed-state checks
/// fail. Reuse must still revalidate records, tails, economics and journals.
pub(crate) async fn recover_metadata_retaining_progress(
    store: Arc<LmdbStore>,
    network: NetworkId,
    initial_params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
    progress: &mut Option<ReplayCache>,
) -> Result<Option<RecoveredInstalledMetadata>, StoreError> {
    let cached = progress.take();
    recover_metadata(
        store,
        network,
        initial_params,
        registry,
        cached,
        Some(progress),
        RecoveryControl::default(),
    )
    .await
}

struct RecoverySeed {
    recovered: RecoveredSnapshot,
    base_commitment: RecoveryBase,
    epoch: BlockHash,
    initial_hash: BlockHash,
    journal_params: ProtocolParams,
    projections: Arc<super::projection_cache::ProjectionCache>,
    started: std::time::Instant,
}

async fn authenticate_metadata(
    store: Arc<LmdbStore>,
    network: NetworkId,
    initial_params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
    cached: Option<ReplayCache>,
    progress: Option<&mut Option<ReplayCache>>,
    control: RecoveryControl,
) -> Result<Option<RecoverySeed>, StoreError> {
    if control.checkpoint("private-recovery:start") {
        return Ok(None);
    }
    let started = std::time::Instant::now();
    let initial_hash = initial_params.params_hash();
    let projections = registry.read().await.recovery_projection.clone();
    let journal_params = initial_params.clone();
    let recovery_params = initial_params.clone();
    let source = Arc::clone(&store);
    let source_control = control.clone();
    #[cfg(test)]
    let source_probe = control.probe;
    let loaded = tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        if let Some(probe) = source_probe {
            probe("private-recovery:source-worker");
        }
        let poll = source_control
            .capture_limits
            .map(|limits| crate::body_retirement::capture_budget::CapturePoll::start(&source, limits))
            .transpose()?;
        if poll.as_ref().is_some_and(|poll| poll.cancelled()) {
            return Ok(None);
        }
        // The source reader must drop before its optional growth lease.
        let txn = source.tx_begin_read()?;
        #[cfg(test)]
        if let Some(probe) = source_probe {
            probe("private-recovery:source-reader");
        }
        let meta = source.meta_store();
        let interrupted = std::cell::Cell::new(false);
        let loaded = (|| {
            let (snapshot, commitment) = RecoverySource::read(
                |key| {
                    if let Some(poll) = &poll {
                        if source_control.checkpoint("private-recovery:source-page")
                            || poll.cancelled()
                        {
                            interrupted.set(true);
                            return Err(invalid("retirement source authentication cancelled"));
                        }
                    }
                    meta.get_meta_txn(&txn, key)
                },
                network,
                &initial_params,
                source.env().path(),
                &|stage| {
                    if let Some(poll) = &poll {
                        if source_control.checkpoint(stage) || poll.cancelled() {
                            interrupted.set(true);
                            return Err(invalid("retirement source authentication cancelled"));
                        }
                    }
                    Ok(())
                },
            )?;
            let epoch = meta.get_meta_txn(&txn, burst_store::meta::PAYMENT_EPOCH_KEY)?;
            if matches!(snapshot, RecoverySource::Genesis) {
                let genesis = crate::genesis_state::block(network, &initial_params).hash;
                if epoch
                    .as_deref()
                    .is_none_or(|bytes| bytes == genesis.as_bytes())
                {
                    return Ok(None);
                }
            }
            let epoch = epoch.ok_or_else(|| invalid("recovery source has no installed epoch"))?;
            let epoch = BlockHash::new(
                epoch
                    .as_slice()
                    .try_into()
                    .map_err(|_| invalid("stored snapshot installed epoch is malformed"))?,
            );
            if epoch.is_zero() {
                return Err(invalid("stored snapshot installed epoch is zero"));
            }
            Ok::<_, StoreError>(Some((snapshot, commitment, epoch)))
        })();
        drop(txn);
        let exhausted = poll.map(|poll| poll.finish()).transpose()?.unwrap_or(false);
        if interrupted.get() {
            return Ok(None);
        }
        if exhausted {
            loaded?; // Preserve a real decode/storage error racing pressure.
            return Ok(None);
        }
        loaded
    })
    .await
    .map_err(invalid)??;
    if control.checkpoint("private-recovery:source") {
        return Ok(None);
    }
    let Some((snapshot, base_commitment, epoch)) = loaded else {
        return Ok(None);
    };
    let cached = if cached.as_ref().is_some_and(|cached| {
        cached.base == base_commitment.commitment()
            && cached.recovered.continuation.anchor().hash() == epoch
    }) {
        cached
    } else {
        let shared = projections.clone();
        let source = store.clone();
        let committed = tokio::task::spawn_blocking(move || {
            shared.replay(
                &source,
                network,
                initial_hash,
                base_commitment.commitment(),
                epoch,
            )
        })
        .await
        .map_err(invalid)??;
        if committed.is_some() {
            tracing::debug!(%epoch, "metadata recovery reused installed replay");
        }
        // Installation may already have replayed the epoch this worker needs.
        // This is only a starting point: recover below rechecks durable history.
        committed.or(cached)
    };
    if control.checkpoint("private-recovery:cache") {
        return Ok(None);
    }
    let had_cache = cached.is_some();
    let reused = match cached {
        Some(cached) => {
            cached
                .recover(
                    base_commitment.commitment(),
                    store.clone(),
                    registry.clone(),
                    epoch,
                )
                .await?
        }
        None => None,
    };
    if control.checkpoint("private-recovery:cached-epoch") {
        return Ok(None);
    }
    let recovered = match reused {
        Some(recovered) => recovered,
        None => {
            snapshot
                .recover(
                    Arc::clone(&store),
                    network,
                    recovery_params,
                    Arc::clone(&registry),
                    epoch,
                )
                .await?
        }
    };
    tracing::debug!(%epoch, had_cache, elapsed_ms = started.elapsed().as_millis(),
        "installed recovery authenticated epoch");
    if control.checkpoint("private-recovery:epoch") {
        return Ok(None);
    }
    let recovered = if let Some(progress) = progress {
        let directory = store.env().path().to_path_buf();
        let (recovered, retained) = tokio::task::spawn_blocking(move || {
            let retained = ReplayCache {
                base: base_commitment.commitment(),
                recovered: recovered.fork(&directory)?,
            };
            Ok::<_, StoreError>((recovered, retained))
        })
        .await
        .map_err(invalid)??;
        *progress = Some(retained);
        recovered
    } else {
        recovered
    };
    if control.checkpoint("private-recovery:progress") {
        return Ok(None);
    }
    Ok(Some(RecoverySeed { recovered, base_commitment, epoch, initial_hash,
        journal_params, projections, started }))
}

fn recover_metadata<'a>(
    store: Arc<LmdbStore>,
    network: NetworkId,
    initial_params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
    cached: Option<ReplayCache>,
    progress: Option<&'a mut Option<ReplayCache>>,
    control: RecoveryControl,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<RecoveredInstalledMetadata>, StoreError>> + Send + 'a>> {
    Box::pin(async move {
        let Some(seed) = Box::pin(authenticate_metadata(
            store.clone(), network, initial_params, registry.clone(), cached, progress, control.clone(),
        )).await? else { return Ok(None); };
        // Authentication and projection replay have large polling frames in debug
        // builds. Run them as separate stages rather than nesting those frames.
        Box::pin(recover_authenticated_metadata(store, network, registry, control, seed)).await
    })
}

async fn recover_authenticated_metadata(
    store: Arc<LmdbStore>,
    network: NetworkId,
    registry: Arc<RwLock<ConnectionRegistry>>,
    control: RecoveryControl,
    seed: RecoverySeed,
) -> Result<Option<RecoveredInstalledMetadata>, StoreError> {
    let RecoverySeed { recovered, base_commitment, epoch, initial_hash,
        journal_params, projections, started } = seed;
    let tails_started = std::time::Instant::now();
    let source = store.clone();
    let shared = projections.clone();
    let (recovered, cached_projection) = tokio::task::spawn_blocking(move || {
        let reader = source.tx_begin_read()?;
        let view = source.read_view(&reader)?;
        let projection = shared.get_view(
            &source,
            network,
            initial_hash,
            epoch,
            &recovered.continuation,
            &view,
        )?;
        Ok::<_, StoreError>((recovered, projection))
    })
    .await
    .map_err(invalid)??;
    if control.checkpoint("private-recovery:projection-cache") {
        return Ok(None);
    }
    let reused_projection = cached_projection.is_some();
    tracing::debug!(%epoch, reused_projection, elapsed_ms = tails_started.elapsed().as_millis(),
        "installed recovery checked projection cache");
    let (recovered, cemented, applied, inventory, cached_economics) =
        if let Some(projection) = cached_projection {
            (
                recovered,
                projection.cemented,
                projection.applied,
                projection.inventory,
                Some(projection.economics),
            )
        } else {
            let source = Arc::clone(&store);
            let (recovered, plan, applied_plan, authenticated) =
                tokio::task::spawn_blocking(move || {
                    let started = std::time::Instant::now();
                    let (plan, applied, authenticated) = recovered
                        .continuation
                        .plan_installed_tails(&source)
                        .map_err(invalid)?;
                    tracing::debug!(
                        elapsed_ms = started.elapsed().as_millis(),
                        confirmed_heads = plan.witness_plan().heads().len(),
                        applied_heads = applied.witness_plan().heads().len(),
                        "installed recovery planned both tails"
                    );
                    Ok::<_, StoreError>((recovered, plan, applied, authenticated))
                })
                .await
                .map_err(invalid)??;
            if control.checkpoint("private-recovery:tails-planned") {
                return Ok(None);
            }
            let started = std::time::Instant::now();
            let Some(retained) = control.peer_wait(witness::fetch_plan_reusing(
                &registry,
                &recovered.continuation,
                plan.witness_plan(),
                &authenticated,
            )).await? else { return Ok(None); };
            tracing::debug!(
                elapsed_ms = started.elapsed().as_millis(),
                witnesses = retained.len(),
                "installed recovery fetched cemented witnesses"
            );
            if control.checkpoint("private-recovery:confirmed-witnesses") {
                return Ok(None);
            }
            let same_witnesses = plan.witness_plan().witness_hashes()
                == applied_plan.witness_plan().witness_hashes();
            let applied_retained = if same_witnesses {
                None
            } else {
                let Some(proofs) = control.peer_wait(witness::fetch_plan_reusing(
                    &registry,
                    &recovered.continuation,
                    applied_plan.witness_plan(),
                    retained.clone(),
                )).await? else { return Ok(None); };
                Some(proofs)
            };
            if control.checkpoint("private-recovery:applied-witnesses") {
                return Ok(None);
            }
            let inventory_started = std::time::Instant::now();
            // Keep paged verification state out of the enclosing recovery frame.
            let Some((recovered, inventory)) = Box::pin(super::retained::verify(
                Arc::clone(&store), recovered, &registry, control.clone(), authenticated,
            )).await? else { return Ok(None); };
            tracing::debug!(
                elapsed_ms = inventory_started.elapsed().as_millis(),
                "installed recovery verified retained inventory"
            );
            if control.checkpoint("private-recovery:inventory") {
                return Ok(None);
            }
            let directory = store.env().path().to_path_buf();
            let (cemented, applied) = tokio::task::spawn_blocking(move || {
                let started = std::time::Instant::now();
                let staged = Arc::new(super::super::history::HistoricalBlocks::stage(
                    &directory, retained,
                )?);
                let cemented = Arc::new(plan.finish_staged(staged.clone()).map_err(invalid)?);
                let applied = Arc::new(match applied_retained {
                    None => applied_plan.finish_staged(staged).map_err(invalid)?,
                    Some(proofs) => applied_plan.finish(proofs).map_err(invalid)?,
                });
                tracing::debug!(
                    elapsed_ms = started.elapsed().as_millis(),
                    same_witnesses,
                    "installed recovery staged and finished tails"
                );
                Ok::<_, StoreError>((cemented, applied))
            })
            .await
            .map_err(invalid)??;
            (recovered, cemented, applied, Arc::new(inventory), None)
        };
    tracing::debug!(%epoch, reused_projection, elapsed_ms = tails_started.elapsed().as_millis(),
        "installed recovery verified tails and inventory");
    if control.checkpoint("private-recovery:tails-finished") {
        return Ok(None);
    }
    let Some((recovered, accepted)) = Box::pin(recover_journal_authority(
        recovered, &store, network, journal_params, &registry, base_commitment, &control,
    )).await? else { return Ok(None); };
    tokio::task::spawn_blocking(move || {
        if control.checkpoint("private-recovery:before-economics") {
            return Ok(None);
        }
        let projection_started = std::time::Instant::now();
        let ordering = match cached_economics.as_ref() {
            Some(economics) => economics.ordering.clone(),
            None => {
                let reader = store.tx_begin_read()?;
                super::order::read_view(&store.read_view(&reader)?, &applied, &recovered.continuation)?
            }
        };
        if control.checkpoint("private-recovery:replay-start") {
            return Ok(None);
        }
        // Tail authentication and execution use owned inputs, not ledger state.
        // Pin a fresh read view for comparison; actual publication rechecks its writer.
        let replayed = if cached_economics.is_none() {
            Some(super::economics::AppliedEconomics::replay_tail(
                &recovered, &applied, &ordering,
            )?)
        } else { None };
        if control.checkpoint("private-recovery:replayed-economics") {
            return Ok(None);
        }
        let reader = store.tx_begin_read()?;
        let view = store.read_view(&reader)?;
        if super::order::read_view(&view, &applied, &recovered.continuation)? != ordering {
            return Err(invalid("applied ordering changed during recovery"));
        }
        let economics = match cached_economics {
            Some(economics) => economics,
            None => Arc::new(super::economics::AppliedEconomics::materialize_view(
                &view, &recovered, replayed.expect("uncached tail was replayed"), ordering,
            )?),
        };
        tracing::debug!(%epoch, reused_projection, elapsed_ms = projection_started.elapsed().as_millis(),
            "installed recovery replayed applied economics");
        if control.checkpoint("private-recovery:economics") {
            return Ok(None);
        }
        let journals_started = std::time::Instant::now();
        let journals = super::journals::JournalGuard::capture_view(&view, &recovered, accepted)?;
        let result = RecoveredInstalledMetadata {
            verified_cut: std::sync::Mutex::new(None),
            verified_view: std::sync::Mutex::new(None),
            initial: initial_hash,
            recovered,
            cemented,
            base_commitment,
            inventory,
            journals,
            applied,
            economics,
        };
        if !result.verify_view_until(&view, &|stage| control.checkpoint(stage))? {
            return Ok(None);
        }
        if control.checkpoint("private-recovery:publication-guards") {
            return Ok(None);
        }
        let verified = view.clean_storage_cut();
        drop(view);
        drop(reader);
        // Do not replace a newer shared projection with an older pinned view.
        // The writer protects only this identity comparison and cache handoff.
        let batch = store.tx_begin_write()?;
        if verified.as_ref().is_some_and(|cut| batch.matches_clean_storage_cut(cut)) {
            projections.publish(&store, network, initial_hash, epoch,
                super::projection_cache::Projection {
                    cemented: result.cemented.clone(), applied: result.applied.clone(),
                    inventory: result.inventory.clone(), economics: result.economics.clone(),
                })?;
        }
        drop(batch);
        tracing::debug!(%epoch, elapsed_ms = journals_started.elapsed().as_millis(),
            total_ms = started.elapsed().as_millis(),
            "installed recovery verified publication guards");
        Ok(Some(result))
    })
    .await
    .map_err(invalid)?
}

async fn recover_journal_authority(
    recovered: RecoveredSnapshot, store: &Arc<LmdbStore>, network: NetworkId,
    journal_params: ProtocolParams, registry: &Arc<RwLock<ConnectionRegistry>>,
    base_commitment: RecoveryBase, control: &RecoveryControl,
) -> Result<Option<(RecoveredSnapshot, Option<RecoveredSnapshot>)>, StoreError> {
    let epoch = recovered.continuation.anchor().hash();
    let accepted_started = std::time::Instant::now();
    let installed_height = recovered.continuation.anchor().height();
    let source = Arc::clone(store);
    let (recovered, accepted_base) = tokio::task::spawn_blocking(
        move || -> Result<(RecoveredSnapshot, Option<RecoveredSnapshot>), StoreError> {
            if super::journals::has_rounds(&source)? {
                if crate::checkpoint_store::CertifiedCheckpoint::stored_tip(
                    &source,
                    network,
                    &journal_params,
                )? == installed_height
                {
                    // The installed recovery already authenticates this whole path.
                    // Its record observations pin the head through publication;
                    // JournalGuard still validates every local signer promise.
                    return Ok((recovered, None));
                }
                let batch = source.tx_begin_write()?;
                base_commitment.verify(&batch)?;
                recovered.verify_records_unchanged(&batch)?;
                let accepted = recovered.fork(source.env().path())?;
                Ok((recovered, Some(accepted)))
            } else {
                Ok((recovered, None))
            }
        },
    )
    .await
    .map_err(invalid)??;
    if control.checkpoint("private-recovery:accepted-base") {
        return Ok(None);
    }
    // A local decision may be ahead of the installed epoch. Authenticate the
    // accepted path without substituting its economics for installed state.
    let accepted = if let Some(base) = accepted_base {
        Some(
            base.advance_target_from_peers(Arc::clone(store), Arc::clone(registry), None)
                .await?,
        )
    } else {
        None
    };
    if control.checkpoint("private-recovery:accepted-path") {
        return Ok(None);
    }
    tracing::debug!(%epoch, replayed_accepted_path = accepted.is_some(),
        elapsed_ms = accepted_started.elapsed().as_millis(),
        "installed recovery authenticated journal authority");
    Ok(Some((recovered, accepted)))
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[tokio::test]
    async fn peer_wait_closes_pending_response_on_shutdown() {
        let shutdown = Arc::new(crate::shutdown::ShutdownController::new());
        let control = RecoveryControl { shutdown: Some(shutdown.clone()), probe: None, ..Default::default() };
        let (response, received) = tokio::sync::oneshot::channel::<()>();
        let (started, entered) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            control.peer_wait(async {
                started.send(()).unwrap();
                received.await.map_err(invalid)
            }).await
        });
        entered.await.unwrap();
        shutdown.shutdown();
        assert!(tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await.unwrap().unwrap().unwrap().is_none());
        assert!(response.is_closed());
    }

    #[tokio::test]
    async fn peer_wait_preserves_results_errors_and_preexisting_shutdown() {
        let control = RecoveryControl::default();
        assert_eq!(control.peer_wait(async { Ok(7) }).await.unwrap(), Some(7));
        assert!(control.peer_wait::<()>(async { Err(invalid("witness failed")) })
            .await.unwrap_err().to_string().contains("witness failed"));
        let shutdown = Arc::new(crate::shutdown::ShutdownController::new());
        shutdown.shutdown();
        let control = RecoveryControl { shutdown: Some(shutdown), probe: None, ..Default::default() };
        let polled = std::sync::atomic::AtomicBool::new(false);
        let result = control.peer_wait(async {
            polled.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }).await.unwrap();
        assert!(result.is_none());
        assert!(!polled.load(std::sync::atomic::Ordering::Relaxed));
        let parent = Arc::new(crate::shutdown::ShutdownController::new());
        let local = Arc::new(crate::shutdown::ShutdownController::new());
        let control = RecoveryControl { shutdown: Some(local.clone()),
            parent_shutdown: Some(parent.clone()), ..Default::default() };
        assert!(!control.checkpoint("before-parent-stop"));
        parent.shutdown();
        assert!(control.checkpoint("after-parent-stop"));
        assert!(!local.is_requested(), "checkpoint detection must not require the async relay to run");
    }
}
