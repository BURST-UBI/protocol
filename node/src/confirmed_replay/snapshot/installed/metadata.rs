//! Exact installed-epoch recovery without publishing authority or economics.

use super::super::tail::cemented::{AppliedTail, CementedTail};
use super::*;
use crate::confirmed_replay::scratch_records::ScratchSequence;
use crate::{checkpoint_sync::witness, connection_registry::ConnectionRegistry};
use burst_types::{BlockHash, Timestamp};
use std::sync::Arc;
use tokio::sync::RwLock;

mod installation;
mod source;
pub(crate) use installation::CompactPreparation;
pub use installation::PreparedCompactInstallation;
use source::{RecoveryBase, RecoverySource};

/// Authenticated installed metadata and cemented governance, not permission to
/// start admission. Applied economics and persisted engine snapshots are checked;
/// remaining indexes and live publication still require startup integration.
/// Local journals are validated and pinned without changing their promises.
pub struct RecoveredInstalledMetadata {
    initial: BlockHash,
    recovered: RecoveredSnapshot,
    cemented: Arc<CementedTail>,
    base_commitment: RecoveryBase,
    inventory: Arc<super::retained::RetainedInventory>,
    journals: super::journals::JournalGuard,
    applied: Arc<AppliedTail>,
    economics: Arc<super::economics::AppliedEconomics>,
}

/// Uncommitted startup index reconstruction. Undo staging must pair these rows
/// with a matching runtime bundle before the startup coordinator can commit.
pub struct RecoveredIndexProjection<'db> {
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
        self,
    ) -> Result<(super::runtime::RecoveredRuntime, ReplayCache), StoreError> {
        if !cfg!(test)
            && !burst_verification::checkpoint_selection_enabled(self.runtime.anchor.network())
        {
            return Err(invalid(
                "compact runtime recovery is behind the release gate",
            ));
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
        self.verify_unchanged()?;
        self.rows.stage_caches(&mut self.transaction)?;
        for proof in self.metadata.applied.tail().retained().iter() {
            let proof = proof?;
            let key = format!("checkpoint_history/v1/{}", proof.block().hash);
            let bytes = bincode::serialize(proof.block()).map_err(invalid)?;
            if let Some(old) = self.transaction.get_meta(&key)? {
                if old != bytes {
                    return Err(invalid("retained dependency body changed"));
                }
            } else {
                self.transaction.put_meta(&key, &bytes)?;
            }
        }
        let mut records = ScratchSequence::new(self.transaction.storage_directory())?;
        let row_wallets = crate::trst_storage::row_format(&self.transaction)?;
        let row_graph = crate::graph_storage::row_format(&self.transaction)?;
        for record in self.economic_changes.iter() {
            let record = record?;
            let mut record = burst_store_lmdb::block_undo::BlockUndo {
                block: record.block,
                account: record.account,
                height: record.height,
                changes: record.changes,
            };
            if row_wallets {
                record = crate::trst_storage::convert_undo(record)?;
            }
            if row_graph {
                record = crate::graph_storage::convert_undo(&self.transaction, record)?;
            }
            records.push(&record)?;
        }
        self.transaction
            .replace_checkpoint_undo_stream(records.iter())?;
        let runtime = Arc::unwrap_or_clone(self.metadata.economics).into_runtime(
            &self.transaction,
            self.metadata.recovered.continuation.anchor().clone(),
            self.cache_at,
        )?;
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
        let derived = super::changes::derive(
            store.env().path(),
            &self.recovered,
            &self.applied,
            cache_at,
            &self.economics.ordering,
            self.economics.state.verification.snapshot(),
        )?;
        let mut transaction = store.tx_begin_write()?;
        transaction.require_clean_checkpoint_boundary()?;
        self.verify_unchanged(&transaction)?;
        let economic_changes =
            super::changes::finality::reconcile(&transaction, derived.records.iter())?;
        transaction.replace_checkpoint_token_indexes(&self.economics.mint_indexes())?;
        Ok(RecoveredIndexProjection {
            transaction,
            metadata: self,
            economic_changes,
            rows: derived.rows,
            cache_at,
        })
    }

    pub fn recovered(&self) -> &RecoveredSnapshot {
        &self.recovered
    }

    /// Extract a checkpoint worker context from checked installed economics.
    /// This does not publish runtime caches or install an accepted certificate.
    pub(crate) fn into_checkpoint_recovery(
        self,
        store: &LmdbStore,
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
        let batch = store.tx_begin_write()?;
        self.verify_unchanged(&batch)?;
        drop(batch);
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
        self.base_commitment.verify(batch)?;
        self.recovered.verify_records_unchanged(batch)?;
        self.inventory.verify_unchanged(batch)?;
        self.journals.verify_unchanged(batch)?;
        self.applied.verify_unchanged(batch)?;
        self.economics.verify_unchanged(batch)?;
        self.recovered
            .continuation
            .verify_installed_metadata_in(batch, &self.cemented)
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
) -> impl std::future::Future<Output = Result<Option<RecoveredInstalledMetadata>, StoreError>> + Send
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

pub(crate) async fn recover_metadata_with_cache(
    store: Arc<LmdbStore>,
    network: NetworkId,
    initial_params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
    cached: Option<ReplayCache>,
) -> Result<Option<RecoveredInstalledMetadata>, StoreError> {
    // Recovery is nested inside live checkpoint-worker futures. Keep its state
    // off their stacks, including in unoptimized builds.
    Box::pin(recover_metadata(store, network, initial_params, registry, cached, None)).await
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
    Box::pin(recover_metadata(
        store,
        network,
        initial_params,
        registry,
        cached,
        Some(progress),
    ))
    .await
}

async fn recover_metadata(
    store: Arc<LmdbStore>,
    network: NetworkId,
    initial_params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
    cached: Option<ReplayCache>,
    progress: Option<&mut Option<ReplayCache>>,
) -> Result<Option<RecoveredInstalledMetadata>, StoreError> {
    let started = std::time::Instant::now();
    let initial_hash = initial_params.params_hash();
    let projections = registry.read().await.recovery_projection.clone();
    let journal_params = initial_params.clone();
    let recovery_params = initial_params.clone();
    let source = Arc::clone(&store);
    let loaded = tokio::task::spawn_blocking(move || {
        let txn = source.tx_begin_read()?;
        let meta = source.meta_store();
        let (snapshot, commitment) = RecoverySource::read(
            |key| meta.get_meta_txn(&txn, key),
            network,
            &initial_params,
            source.env().path(),
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
    })
    .await
    .map_err(invalid)??;
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
    let tails_started = std::time::Instant::now();
    let source = store.clone();
    let shared = projections.clone();
    let (recovered, cached_projection) = tokio::task::spawn_blocking(move || {
        let batch = source.tx_begin_write()?;
        let projection = shared.get(
            &source,
            network,
            initial_hash,
            epoch,
            &recovered.continuation,
            &batch,
        )?;
        Ok::<_, StoreError>((recovered, projection))
    })
    .await
    .map_err(invalid)??;
    let reused_projection = cached_projection.is_some();
    let (recovered, cemented, applied, inventory, cached_economics) = if let Some(projection) =
        cached_projection
    {
        (
            recovered,
            projection.cemented,
            projection.applied,
            projection.inventory,
            Some(projection.economics),
        )
    } else {
        let source = Arc::clone(&store);
        let (recovered, plan, applied_plan) = tokio::task::spawn_blocking(move || {
            let plan = recovered
                .continuation
                .plan_cemented_tail(&source)
                .map_err(invalid)?;
            let applied = recovered
                .continuation
                .plan_applied_tail(&source)
                .map_err(invalid)?;
            Ok::<_, StoreError>((recovered, plan, applied))
        })
        .await
        .map_err(invalid)??;
        let retained =
            witness::fetch_plan(&registry, &recovered.continuation, plan.witness_plan()).await?;
        let available: std::collections::HashMap<_, _> = retained
            .iter()
            .map(|proof| (proof.block().hash, proof))
            .collect();
        let reused: Option<Vec<_>> = applied_plan
            .witness_plan()
            .witness_hashes()
            .iter()
            .map(|hash| available.get(hash).copied().cloned())
            .collect();
        let applied_retained = match reused {
            Some(proofs) => proofs,
            None => {
                witness::fetch_plan(
                    &registry,
                    &recovered.continuation,
                    applied_plan.witness_plan(),
                )
                .await?
            }
        };
        let (recovered, inventory) =
            super::retained::verify(Arc::clone(&store), recovered, &registry).await?;
        let (cemented, applied) = tokio::task::spawn_blocking(move || {
            Ok::<_, StoreError>((
                Arc::new(plan.finish(retained).map_err(invalid)?),
                Arc::new(applied_plan.finish(applied_retained).map_err(invalid)?),
            ))
        })
        .await
        .map_err(invalid)??;
        (recovered, cemented, applied, Arc::new(inventory), None)
    };
    tracing::debug!(%epoch, reused_projection, elapsed_ms = tails_started.elapsed().as_millis(),
        "installed recovery verified tails and inventory");
    let accepted_started = std::time::Instant::now();
    let installed_height = recovered.continuation.anchor().height();
    let source = Arc::clone(&store);
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
    // A local decision may be ahead of the installed epoch. Authenticate the
    // accepted path without substituting its economics for installed state.
    let accepted = if let Some(base) = accepted_base {
        Some(
            base.advance_target_from_peers(Arc::clone(&store), Arc::clone(&registry), None)
                .await?,
        )
    } else {
        None
    };
    tracing::debug!(%epoch, replayed_accepted_path = accepted.is_some(),
        elapsed_ms = accepted_started.elapsed().as_millis(),
        "installed recovery authenticated journal authority");
    tokio::task::spawn_blocking(move || {
        let projection_started = std::time::Instant::now();
        let batch = store.tx_begin_write()?;
        let ordering = super::order::read(&batch, &applied, &recovered.continuation)?;
        let economics = match cached_economics {
            Some(economics) => {
                if economics.ordering != ordering {
                    return Err(invalid("applied ordering changed during recovery"));
                }
                economics
            },
            None => Arc::new(super::economics::AppliedEconomics::replay(
                &batch, &recovered, &applied, ordering,
            )?),
        };
        tracing::debug!(%epoch, reused_projection, elapsed_ms = projection_started.elapsed().as_millis(),
            "installed recovery replayed applied economics");
        let journals_started = std::time::Instant::now();
        let journals = super::journals::JournalGuard::capture(&batch, &recovered, accepted)?;
        let result = RecoveredInstalledMetadata {
            initial: initial_hash,
            recovered,
            cemented,
            base_commitment,
            inventory,
            journals,
            applied,
            economics,
        };
        result.verify_unchanged(&batch)?;
        projections.publish(&store, network, initial_hash, epoch,
            super::projection_cache::Projection {
                cemented: result.cemented.clone(), applied: result.applied.clone(),
                inventory: result.inventory.clone(), economics: result.economics.clone(),
            })?;
        tracing::debug!(%epoch, elapsed_ms = journals_started.elapsed().as_millis(),
            total_ms = started.elapsed().as_millis(),
            "installed recovery verified publication guards");
        Ok(Some(result))
    })
    .await
    .map_err(invalid)?
}
