use super::*;
use crate::checkpoint_store::compact;
use crate::confirmed_replay::snapshot::continuation::CompactCutCoverage;

/// A replayed immediate successor, still bound to the exact installed state.
/// The gated live publisher must hold its engine locks while committing this
/// input and replacing runtime state. Preparation alone authorizes no signing.
pub struct PreparedCompactInstallation {
    installed: RecoveredInstalledMetadata,
    next: RecoveredSnapshot,
    cut: CompactCutCoverage,
    observations: compact::RecordObservations,
    witnesses: Arc<super::super::super::history::HistoricalBlocks>,
    #[cfg(test)]
    reused_accepted: bool,
}

pub(crate) enum CompactPreparation {
    Ready(Box<PreparedCompactInstallation>),
    CaughtUp(Box<ReplayCache>),
}

impl PreparedCompactInstallation {
    #[cfg(test)]
    pub(crate) fn reused_accepted_successor(&self) -> bool { self.reused_accepted }

    pub fn anchor(&self) -> &burst_consensus::checkpoint::CheckpointAnchor {
        self.next.continuation.anchor()
    }

    pub fn state(&self) -> &crate::confirmed_replay::ConfirmedReplay {
        self.next.continuation.state()
    }

    pub fn verify_unchanged(
        &self,
        transaction: &LmdbWriteTransaction<'_>,
    ) -> Result<(), StoreError> {
        transaction.require_clean_checkpoint_boundary()?;
        self.installed.verify_unchanged(transaction)?;
        self.observations.verify(transaction)?;
        self.cut.check_installed_cut(transaction)
    }

    #[cfg(test)]
    pub(crate) fn commit(
        self,
        store: &LmdbStore,
    ) -> Result<crate::confirmed_replay::installation::InstalledCheckpoint, StoreError> {
        self.commit_with_recovery(store)
            .map(|(installed, _, _)| installed)
    }

    /// Commit and force-sync before returning runtime state and cache entries.
    /// A sync error does not roll back the database commit.
    pub(crate) fn commit_with_recovery(
        self,
        store: &LmdbStore,
    ) -> Result<
        (
            crate::confirmed_replay::installation::InstalledCheckpoint,
            ReplayCache,
            super::super::CommittedProjection,
        ),
        StoreError,
    > {
        if !cfg!(test)
            && !burst_verification::checkpoint_selection_enabled(
                self.installed.recovered.continuation.anchor().network(),
            )
        {
            return Err(invalid(
                "compact live installation is behind the release gate",
            ));
        }
        let mut transaction = store.tx_begin_write()?;
        self.verify_unchanged(&transaction)?;
        let parent = self.installed.recovered.continuation.anchor().hash();
        // The exclusive writer is still clean. This read view sees the same
        // checked ledger while streamed rows are staged in the writer.
        let read = store.tx_begin_read()?;
        let mut supplied = std::collections::HashMap::new();
        let blocks = self
            .cut
            .installation_blocks(|hash| store.block_store().get_block_txn(&read, hash))
            .map(|row| {
                let row = row?;
                supplied.insert(
                    row.hash,
                    (
                        row.account.clone(),
                        row.height,
                        burst_crypto::blake2b_256(&row.bytes),
                    ),
                );
                Ok(row)
            });
        let anchor = self.next.continuation.anchor().clone();
        let state = self.next.continuation.state().clone();
        let projection = transaction.replace_compact_checkpoint_ledger_account_map(
            parent,
            blocks,
            &state.accounts,
            Some(&state.brn.wallets),
        )?;
        drop(read);
        crate::confirmed_replay::installation::validate_retired(
            &transaction,
            parent,
            &projection.retired_blocks,
        )?;
        let governance = state.persist_projection(&mut transaction, &anchor)?;
        // Absence from sparse membership is not proof of exclusion. Keep every
        // old promise without labelling unproven choices as minority votes.
        let retained_promises =
            transaction.advance_snapshot_payment_epoch(&parent, &anchor.hash())?;
        let ledger_counts = transaction.ledger_counts()?;
        let inventory = Arc::new(self.installed.inventory.capture_successor(
            &transaction,
            self.installed.recovered.continuation.state(),
            &supplied,
        )?);
        let cemented = Arc::new(super::super::super::tail::cemented::capture_installed::<
            true,
        >(
            &self.next.continuation,
            &transaction,
            store.env().path(),
            self.witnesses.clone(),
        )?);
        let applied = Arc::new(super::super::super::tail::cemented::capture_installed::<
            false,
        >(
            &self.next.continuation,
            &transaction,
            store.env().path(),
            self.witnesses,
        )?);
        let ordering = super::super::order::read(&transaction, &applied, &self.next.continuation)?;
        if !ordering.is_empty() {
            return Err(invalid(
                "installed checkpoint retains provisional application order",
            ));
        }
        let economics = Arc::new(super::super::economics::AppliedEconomics {
            state: state.clone(),
            ordering,
        });
        economics.verify_unchanged(&transaction)?;
        self.next
            .continuation
            .verify_installed_metadata_in(&transaction, &cemented)?;
        let handoff = super::super::CommittedProjection {
            network: anchor.network(),
            initial: self.installed.initial,
            epoch: anchor.hash(),
            projection: super::super::projection_cache::Projection {
                cemented,
                applied,
                inventory,
                economics,
            },
            replay: Arc::new(ReplayCache {
                base: self.installed.base_commitment.commitment(),
                recovered: self.next.fork(store.env().path())?,
            }),
        };
        transaction.commit()?;
        store.env().force_sync().map_err(invalid)?;
        let installed = crate::confirmed_replay::installation::InstalledCheckpoint {
            brn: state.brn,
            trst: state.trst,
            verification: state.verification,
            governance,
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
        };
        Ok((
            installed,
            ReplayCache {
                base: self.installed.base_commitment.commitment(),
                recovered: self.next,
            },
            handoff,
        ))
    }
}

impl RecoveredInstalledMetadata {
    /// Replay only the immediate accepted successor, never substitute the tip
    /// for installed authority. Peer waits hold neither a writer nor live locks.
    /// Dropping or cancelling preparation changes no durable state.
    pub async fn prepare_next_from_peers(
        self,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<ConnectionRegistry>>,
    ) -> Result<Option<PreparedCompactInstallation>, StoreError> {
        match self.prepare_next_with_recovery(store, registry).await? {
            CompactPreparation::Ready(prepared) => Ok(Some(*prepared)),
            CompactPreparation::CaughtUp(_) => Ok(None),
        }
    }

    pub(crate) async fn prepare_next_with_recovery(
        self,
        store: Arc<LmdbStore>,
        registry: Arc<RwLock<ConnectionRegistry>>,
    ) -> Result<CompactPreparation, StoreError> {
        let started = std::time::Instant::now();
        let source = store.clone();
        let planned = tokio::task::spawn_blocking(move || {
            let records = compact::read_after(
                &source,
                self.recovered.selections.authority.iter(),
                &self.recovered.selections,
            )?;
            let Some(record) = records.successors.first()? else {
                let transaction = source.tx_begin_write()?;
                self.verify_unchanged(&transaction)?;
                records.observations.verify(&transaction)?;
                return Ok((self, None));
            };
            let record = record.load(&source)?;
            self.recovered
                .continuation
                .anchor()
                .verify_certificate(&record.certificate)
                .map_err(invalid)?;
            let plan = self
                .recovered
                .continuation
                .plan_tail(
                    &source,
                    record.certificate.checkpoint.cutoff,
                    record.manifest.heads(),
                )
                .map_err(invalid)?;
            let wanted: std::collections::HashSet<_> =
                plan.witness_hashes().iter().copied().collect();
            let mut available = Vec::new();
            let mut seen = std::collections::HashSet::new();
            for proof in self
                .applied
                .tail()
                .retained()
                .iter()
                .chain(self.cemented.tail().retained().iter())
            {
                let proof = proof?;
                if wanted.contains(&proof.block().hash) && seen.insert(proof.block().hash) {
                    available.push(proof);
                }
            }
            Ok::<_, StoreError>((self, Some((record, records.observations, plan, available))))
        })
        .await
        .map_err(invalid)??;
        tracing::debug!(elapsed_ms = started.elapsed().as_millis(), stage = "plan",
            "checkpoint successor preparation timing");
        let (installed, next) = planned;
        let Some((record, observations, plan, available)) = next else {
            return Ok(CompactPreparation::CaughtUp(Box::new(ReplayCache {
                base: installed.base_commitment.commitment(),
                recovered: installed.recovered,
            })));
        };
        let started = std::time::Instant::now();
        let retained = witness::fetch_plan_reusing(
            &registry,
            &installed.recovered.continuation,
            &plan,
            available,
        )
        .await?;
        tracing::debug!(elapsed_ms = started.elapsed().as_millis(), stage = "witness",
            "checkpoint successor preparation timing");
        let started = std::time::Instant::now();
        let source = store.clone();
        let (installed, next, cut, observations, witness_plan, reused_accepted) =
            tokio::task::spawn_blocking(move || {
                let tail = plan.finish(retained).map_err(invalid)?;
                let (next, cut, reused_accepted) = installed.recovered.installation_successor(
                    &source,
                    record,
                    &observations,
                    tail,
                    installed.journals.accepted(),
                )?;
                let witness_plan = super::super::super::tail::cemented::plan_installed(
                    &next.continuation,
                    source.env().path(),
                )
                .map_err(invalid)?;
                Ok::<_, StoreError>((installed, next, cut, observations, witness_plan, reused_accepted))
            })
            .await
            .map_err(invalid)??;
        tracing::debug!(elapsed_ms = started.elapsed().as_millis(), stage = "replay", reused_accepted,
            "checkpoint successor preparation timing");
        let started = std::time::Instant::now();
        let witnesses = witness::fetch_plan(&registry, &next.continuation, &witness_plan).await?;
        tracing::debug!(elapsed_ms = started.elapsed().as_millis(), stage = "installed-witness",
            "checkpoint successor preparation timing");
        let started = std::time::Instant::now();
        let result = tokio::task::spawn_blocking(move || {
            let witnesses = Arc::new(super::super::super::history::HistoricalBlocks::stage(
                store.env().path(),
                witnesses,
            )?);
            let prepared = PreparedCompactInstallation {
                installed,
                next,
                cut,
                observations,
                witnesses,
                #[cfg(test)]
                reused_accepted,
            };
            let transaction = store.tx_begin_write()?;
            prepared.verify_unchanged(&transaction)?;
            Ok(CompactPreparation::Ready(Box::new(prepared)))
        })
        .await
        .map_err(invalid)?;
        tracing::debug!(elapsed_ms = started.elapsed().as_millis(), stage = "verify",
            "checkpoint successor preparation timing");
        result
    }
}
