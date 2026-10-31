//! Recovery from a pinned database view for authority handoff and body retirement.
use super::super::{economics::AppliedEconomics, journals::JournalGuard};
use super::*;
use burst_store_lmdb::LmdbReadTransaction;

fn log_stage(epoch: BlockHash, started: std::time::Instant, name: &str) {
    let elapsed_us = started.elapsed().as_micros();
    tracing::debug!(%epoch, elapsed_us, stage = name, "captured retirement recovery");
    #[cfg(test)]
    if std::env::var_os("BURST_FINALITY_STAGE_LOG").is_some() {
        eprintln!("RETIREMENT_CAPTURE epoch={epoch} stage={name} elapsed_us={elapsed_us}");
    }
}

pub(crate) async fn recover_retirement_catalogue(
    store: Arc<LmdbStore>,
    network: NetworkId,
    params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
    shutdown: Arc<crate::shutdown::ShutdownController>,
    durable_inputs: (crate::confirmed_replay::scratch_file::ScratchLimit, usize),
    retain_seconds: u64,
    after: Option<BlockHash>,
    limits: crate::body_retirement::capture_budget::CaptureLimits,
) -> Result<Option<(OwnedRetirement, BlockHash)>, StoreError> {
    let result = recover_bounded(
        store,
        network,
        params,
        registry,
        shutdown,
        Some(durable_inputs),
        limits,
        CapturePurpose::Retirement {
            retain_seconds,
            after,
        },
        None,
    )
    .await?;
    match result {
        Some(CaptureResult::Retirement(catalogue, epoch)) => Ok(Some((catalogue, epoch))),
        None | Some(CaptureResult::Deferred) => Ok(None),
        Some(CaptureResult::Authority(_)) => unreachable!("retirement capture target"),
    }
}

struct CaptureGuard(Arc<crate::shutdown::ShutdownController>);

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

enum CapturePurpose {
    Retirement {
        retain_seconds: u64,
        after: Option<BlockHash>,
    },
    Authority {
        previous: Option<burst_consensus::checkpoint::CheckpointAnchor>,
    },
}

enum CaptureResult {
    Retirement(OwnedRetirement, BlockHash),
    Authority(ReplayCache),
    Deferred,
}

pub(crate) async fn recover_checkpoint_authority(
    store: Arc<LmdbStore>,
    network: NetworkId,
    params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
    previous: Option<burst_consensus::checkpoint::CheckpointAnchor>,
    progress: &mut Option<ReplayCache>,
) -> Result<Option<ReplayCache>, StoreError> {
    let limits = crate::config::BodyRetirementConfig::default().capture_limits();
    match recover_bounded(
        store,
        network,
        params,
        registry,
        Arc::new(crate::shutdown::ShutdownController::new()),
        None,
        limits,
        CapturePurpose::Authority { previous },
        Some(progress),
    )
    .await?
    {
        Some(CaptureResult::Authority(cache)) => Ok(Some(cache)),
        None => Ok(None),
        Some(CaptureResult::Deferred) => Err(StoreError::Backend(
            "checkpoint authority capture deferred by storage budget".into(),
        )),
        Some(CaptureResult::Retirement(..)) => unreachable!("authority capture target"),
    }
}

async fn recover_bounded(
    store: Arc<LmdbStore>,
    network: NetworkId,
    params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
    shutdown: Arc<crate::shutdown::ShutdownController>,
    durable_inputs: Option<(crate::confirmed_replay::scratch_file::ScratchLimit, usize)>,
    limits: crate::body_retirement::capture_budget::CaptureLimits,
    purpose: CapturePurpose,
    mut progress: Option<&mut Option<ReplayCache>>,
) -> Result<Option<CaptureResult>, StoreError> {
    // Authentication/replay can be history-sized. Skip it when capture already
    // has to defer; the authoritative writer-side acquisition below closes races.
    if !store.env().may_limit_file_extent() {
        return Ok(Some(CaptureResult::Deferred));
    }
    let control = RecoveryControl {
        shutdown: Some(shutdown.clone()),
        durable_inputs,
        parent_shutdown: None,
        capture_limits: Some(limits),
        #[cfg(test)]
        probe: registry.read().await.recovery_probe,
    };
    let cached = progress.as_mut().and_then(|progress| progress.take());
    let Some(RecoverySeed {
        recovered,
        base_commitment,
        epoch,
        initial_hash,
        journal_params,
        projections,
        started,
    }) = Box::pin(authenticate_metadata(
        store.clone(),
        network,
        params,
        registry.clone(),
        cached,
        progress,
        control.clone(),
    ))
    .await?
    else {
        return Ok(None);
    };
    let Some((recovered, accepted)) = Box::pin(recover_journal_authority(
        recovered,
        &store,
        network,
        journal_params,
        &registry,
        base_commitment,
        &control,
    ))
    .await?
    else {
        return Ok(None);
    };
    let runtime = tokio::runtime::Handle::current();
    let local_stop = Arc::new(crate::shutdown::ShutdownController::new());
    let _owner = CaptureGuard(local_stop.clone());
    let worker_stop = local_stop.clone();
    let monitor_store = store.clone();
    let budget = Arc::new(std::sync::OnceLock::<
        crate::body_retirement::capture_budget::CaptureBudget,
    >::new());
    let worker_budget = budget.clone();
    let pressure = Arc::new(std::sync::OnceLock::<burst_store_lmdb::GrowthWatch>::new());
    let worker_pressure = pressure.clone();
    let mut control = control;
    control.parent_shutdown = Some(shutdown.clone());
    control.shutdown = Some(worker_stop.clone());
    // The worker owns its native reader. Handled shutdown and budget exhaustion
    // await worker cleanup; dropping this future only requests cancellation.
    let mut worker = tokio::task::spawn_blocking(move || {
        if control.checkpoint("retirement-capture:before-reader") {
            return Ok(Some(CaptureResult::Deferred));
        }
        let mut writer = store.tx_begin_write()?;
        let capture_budget =
            crate::body_retirement::capture_budget::CaptureBudget::start(&store, limits)?;
        if let Some(reason) = capture_budget.exceeded(&store)? {
            tracing::debug!(
                reason,
                "retirement capture declined before reader acquisition"
            );
            return Ok(Some(CaptureResult::Deferred));
        }
        // Declare the lease before the native reader: every exit drops the
        // reader first, so waiting writers wake only after its pages are unpinned.
        let Some(growth_limit) =
            writer.try_limit_file_extent(capture_budget.file_ceiling(&store))?
        else {
            return Ok(Some(CaptureResult::Deferred)); // Another capture or an admitted retry owns priority.
        };
        let watch = growth_limit.watch();
        let _ = worker_pressure.set(watch.clone());
        let _ = worker_budget.set(capture_budget);
        if worker_stop.is_requested() {
            return Ok(Some(CaptureResult::Deferred));
        }
        let changes = if matches!(&purpose, CapturePurpose::Retirement { .. }) {
            Some(writer.watch_body_changes(65_536, 64 << 20)?)
        } else {
            None
        };
        let reader = store.tx_begin_read()?;
        let cut = reader
            .clean_storage_cut()
            .ok_or_else(|| invalid("retirement capture has no storage identity"))?;
        if !writer.matches_clean_storage_cut(&cut) {
            return Err(invalid("retirement reader differs from watch boundary"));
        }
        drop(writer);
        let view = store.read_view(&reader)?;
        base_commitment.verify_view(&view)?;
        let interrupted = std::cell::Cell::new(false);
        let poll = |stage| {
            if control.checkpoint(stage) {
                interrupted.set(true);
                return Err(invalid("captured record verification cancelled"));
            }
            Ok(())
        };
        let records = (|| {
            recovered.verify_records_view_with_poll(&view, &poll)?;
            if view.payment_epoch()? != epoch {
                return Err(invalid("installed epoch changed before retirement capture"));
            }
            if let Some(accepted) = &accepted {
                accepted.verify_records_view_with_poll(&view, &poll)?;
            }
            Ok(())
        })();
        if interrupted.get() {
            return Ok(Some(CaptureResult::Deferred));
        }
        records?;
        log_stage(epoch, started, "storage-boundary");
        if control.checkpoint("retirement-capture:reader") {
            return Ok(Some(CaptureResult::Deferred));
        }
        let metadata = runtime.block_on(recover_captured(
            &store,
            &reader,
            network,
            initial_hash,
            epoch,
            recovered,
            accepted,
            base_commitment,
            &projections,
            &registry,
            &control,
        ))?;
        let Some(metadata) = metadata else {
            return Ok(Some(CaptureResult::Deferred));
        };
        log_stage(epoch, started, "metadata-complete");
        let (retain_seconds, after) = match purpose {
            CapturePurpose::Authority { previous } => {
                // Validate all economics against the same captured view. Only
                // certified authority must still be current at worker handoff;
                // subsequent discovery/signing rechecks live payment finality.
                let cache = metadata.into_checkpoint_recovery_view(&view, previous.as_ref())?;
                drop(view);
                drop(reader);
                let live_reader = store.tx_begin_read()?;
                let live = store.read_view(&live_reader)?;
                base_commitment.verify_view(&live)?;
                cache
                    .recovered
                    .verify_records_view_with_poll(&live, &|_| Ok(()))?;
                cache
                    .recovered
                    .continuation
                    .verify_installed_authority_view(&live)?;
                if worker_stop.is_requested()
                    || watch.was_exceeded()
                    || worker_budget
                        .get()
                        .expect("started capture budget")
                        .exceeded(&store)?
                        .is_some()
                {
                    return Ok(Some(CaptureResult::Deferred));
                }
                return Ok(Some(CaptureResult::Authority(cache)));
            }
            CapturePurpose::Retirement {
                retain_seconds,
                after,
            } => (retain_seconds, after),
        };
        let changes = changes.expect("retirement owns body mutation watch");
        changes.check_unconsumed_since(&store, &cut)?;
        let cutoff = metadata.recovered().continuation.anchor().cutoff();
        if control.checkpoint("retirement-capture:before-roots") {
            return Ok(Some(CaptureResult::Deferred));
        }
        let Some(projection) = metadata.prepare_token_indexes_view_until(
            &store,
            &reader,
            Timestamp::new(cutoff),
            &|stage| control.checkpoint(stage),
        )?
        else {
            return Ok(Some(CaptureResult::Deferred));
        };
        log_stage(epoch, started, "rollback-prepared");
        let compaction_control = control.clone();
        let Some(required) = projection.body_requirements_view_until(
            &store,
            &reader,
            || control.checkpoint("retirement-capture:root-page"),
            Arc::new(move |stage| compaction_control.checkpoint(stage)),
        )?
        else {
            return Ok(Some(CaptureResult::Deferred));
        };
        log_stage(epoch, started, "roots-complete");
        let mut stop = worker_stop.subscribe();
        let closed = runtime.block_on(async {
            tokio::select! {
                biased;
                _ = stop.recv() => Ok(None),
                closed = required.close_retained_from_peers(Default::default(), &registry) =>
                    closed.map(Some).map_err(|error| invalid(format!("retirement body closure: {error:?}"))),
            }
        })?;
        let Some(closed) = closed else {
            return Ok(Some(CaptureResult::Deferred));
        };
        if control.checkpoint("retirement-capture:before-transfer") {
            return Ok(Some(CaptureResult::Deferred));
        }
        if watch.was_exceeded()
            || worker_budget
                .get()
                .expect("started capture budget")
                .exceeded(&store)?
                .is_some()
        {
            worker_stop.shutdown();
            return Ok(Some(CaptureResult::Deferred));
        }
        let catalogue = closed.own_captured_retirement(
            Timestamp::new(cutoff.saturating_sub(retain_seconds)),
            after,
            || worker_stop.is_requested(),
            changes,
        )?;
        if worker_stop.is_requested()
            || watch.was_exceeded()
            || worker_budget
                .get()
                .expect("started capture budget")
                .exceeded(&store)?
                .is_some()
        {
            return Ok(Some(CaptureResult::Deferred));
        }
        log_stage(epoch, started, "catalogue-transferred");
        // All proofs and native readers drop here. Reconciliation still checks
        // every observed write and the exact live publication cut before deletion.
        Ok(Some(CaptureResult::Retirement(catalogue, epoch)))
    });
    let mut parent_stop = shutdown.subscribe();
    let mut poll = tokio::time::interval(std::time::Duration::from_millis(10));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let joined = loop {
        tokio::select! {
            biased;
            _ = parent_stop.recv() => {
                local_stop.shutdown();
                break worker.await;
            }
            result = &mut worker => break result,
            _ = poll.tick() => {
                if pressure.get().is_some_and(|watch| watch.was_exceeded()) {
                    local_stop.shutdown();
                    break worker.await;
                }
                if let Some(budget) = budget.get() {
                    match budget.exceeded(&monitor_store) {
                        Ok(None) => {},
                        Ok(Some(reason)) => {
                            tracing::debug!(reason, "retirement capture budget exhausted; awaiting owned cleanup");
                            local_stop.shutdown();
                            break worker.await;
                        }
                        Err(error) => {
                            local_stop.shutdown();
                            worker.await.map_err(invalid)??;
                            return Err(error);
                        }
                    }
                }
            }
        }
    };
    let result = joined.map_err(invalid)?;
    if local_stop.is_requested() || pressure.get().is_some_and(|watch| watch.was_exceeded()) {
        result?; // Preserve storage errors even when they race cancellation.
        Ok(Some(CaptureResult::Deferred))
    } else {
        result
    }
}

async fn recover_captured(
    store: &Arc<LmdbStore>,
    reader: &LmdbReadTransaction<'_>,
    network: NetworkId,
    initial: BlockHash,
    epoch: BlockHash,
    recovered: RecoveredSnapshot,
    accepted: Option<RecoveredSnapshot>,
    base_commitment: RecoveryBase,
    projections: &super::super::projection_cache::ProjectionCache,
    registry: &Arc<RwLock<ConnectionRegistry>>,
    control: &RecoveryControl,
) -> Result<Option<RecoveredInstalledMetadata>, StoreError> {
    let started = std::time::Instant::now();
    let view = store.read_view(reader)?;
    let cached = projections.get_view(
        store,
        network,
        initial,
        epoch,
        &recovered.continuation,
        &view,
    )?;
    if control.checkpoint("private-recovery:projection-cache") {
        return Ok(None);
    }
    let (recovered, cemented, applied, inventory, cached_economics) = match cached {
        Some(cached) => (
            recovered,
            cached.cemented,
            cached.applied,
            cached.inventory,
            Some(cached.economics),
        ),
        None => {
            let (plan, applied_plan, authenticated) = recovered
                .continuation
                .plan_installed_tails_view(store, reader)
                .map_err(invalid)?;
            log_stage(epoch, started, "tails-planned");
            if control.checkpoint("private-recovery:tails-planned") {
                return Ok(None);
            }
            let Some(retained) = control
                .peer_wait(witness::fetch_plan_reusing(
                    registry,
                    &recovered.continuation,
                    plan.witness_plan(),
                    authenticated,
                ))
                .await?
            else {
                return Ok(None);
            };
            if control.checkpoint("private-recovery:confirmed-witnesses") {
                return Ok(None);
            }
            let applied_retained = if plan.witness_plan().witness_hashes()
                == applied_plan.witness_plan().witness_hashes()
            {
                None
            } else {
                let Some(proofs) = control
                    .peer_wait(witness::fetch_plan_reusing(
                        registry,
                        &recovered.continuation,
                        applied_plan.witness_plan(),
                        retained.clone(),
                    ))
                    .await?
                else {
                    return Ok(None);
                };
                Some(proofs)
            };
            if control.checkpoint("private-recovery:applied-witnesses") {
                return Ok(None);
            }
            let Some((recovered, inventory)) = super::super::retained::verify_captured(
                store.clone(),
                recovered,
                reader,
                registry,
                control.clone(),
            )
            .await?
            else {
                return Ok(None);
            };
            log_stage(epoch, started, "inventory-verified");
            if control.checkpoint("private-recovery:inventory") {
                return Ok(None);
            }
            let staged = Arc::new(super::super::super::history::HistoricalBlocks::stage(
                store.env().path(),
                retained,
            )?);
            let cemented = Arc::new(plan.finish_staged(staged.clone()).map_err(invalid)?);
            let applied = Arc::new(match applied_retained {
                None => applied_plan.finish_staged(staged).map_err(invalid)?,
                Some(proofs) => applied_plan.finish(proofs).map_err(invalid)?,
            });
            (recovered, cemented, applied, Arc::new(inventory), None)
        }
    };
    if control.checkpoint("private-recovery:tails-finished") {
        return Ok(None);
    }
    if control.checkpoint("private-recovery:before-economics") {
        return Ok(None);
    }
    let ordering = match &cached_economics {
        Some(economics) => economics.ordering.clone(),
        None => super::super::order::read_view(&view, &applied, &recovered.continuation)?,
    };
    if control.checkpoint("private-recovery:replay-start") {
        return Ok(None);
    }
    let economics = match cached_economics {
        Some(economics) => economics,
        None => {
            let Some(state) =
                AppliedEconomics::replay_tail_until(&recovered, &applied, &ordering, || {
                    control.checkpoint("private-recovery:applied-transition")
                })?
            else {
                return Ok(None);
            };
            if control.checkpoint("private-recovery:replayed-economics") {
                return Ok(None);
            }
            Arc::new(AppliedEconomics::materialize_view(
                &view, &recovered, state, ordering,
            )?)
        }
    };
    log_stage(epoch, started, "economics-verified");
    if control.checkpoint("private-recovery:economics") {
        return Ok(None);
    }
    let journal_stopped = std::cell::Cell::new(false);
    let journals = JournalGuard::capture_view_with_poll(&view, &recovered, accepted, &|stage| {
        if control.checkpoint(stage) {
            journal_stopped.set(true);
            return Err(invalid("captured journal validation cancelled"));
        }
        Ok(())
    });
    if journal_stopped.get() {
        return Ok(None);
    }
    let journals = journals?;
    let metadata = RecoveredInstalledMetadata {
        verified_cut: std::sync::Mutex::new(None),
        verified_view: std::sync::Mutex::new(None),
        initial,
        recovered,
        cemented,
        base_commitment,
        inventory,
        journals,
        applied,
        economics,
    };
    if !metadata.verify_view_until(&view, &|stage| control.checkpoint(stage))? {
        return Ok(None);
    }
    if control.checkpoint("private-recovery:publication-guards") {
        return Ok(None);
    }
    // Historical validity must not overwrite a newer shared projection cache.
    Ok(Some(metadata))
}
