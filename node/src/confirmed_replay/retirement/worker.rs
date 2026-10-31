//! Sequential, operator-budgeted retirement of eligible snapshot-prefix bodies.
pub(crate) mod capture_budget;
use crate::{
    block_processor::BlockProcessor, config::BodyRetirementConfig,
    confirmed_replay::snapshot::installed, connection_registry::ConnectionRegistry,
    ledger_cache::LedgerCache, shutdown::ShutdownController,
};
use burst_store::StoreError;
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, NetworkId, ProtocolParams};
use std::{sync::Arc, time::Duration};
use tokio::sync::{Mutex, RwLock};

pub(crate) struct Worker {
    pub store: Arc<LmdbStore>,
    pub network: NetworkId,
    pub params: ProtocolParams,
    pub registry: Arc<RwLock<ConnectionRegistry>>,
    pub processor: Arc<Mutex<BlockProcessor>>,
    pub counts: Arc<LedgerCache>,
    pub shutdown: Arc<ShutdownController>,
    #[cfg(test)]
    pub prepared_probe: Option<Arc<dyn Fn(Probe) + Send + Sync>>,
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) enum Probe {
    Prepared(bool),
    Deferred,
    Finished {
        elapsed: Duration,
        examined: usize,
        deleted: usize,
        bytes: u64,
        error: Option<String>,
    },
}

pub(crate) struct Report {
    pub epoch: BlockHash,
    pub resume_after: Option<BlockHash>,
    pub complete: bool,
    pub examined: usize,
    pub hashes: Vec<BlockHash>,
    pub bytes: u64,
    pub blocked_body_bytes: Option<u64>,
}

impl Report {
    fn can_continue_scan(&self, after: Option<BlockHash>, max_bytes: u64) -> bool {
        !self.complete
            && self.examined > 0
            && self
                .resume_after
                .is_some_and(|next| after.is_none_or(|previous| next > previous))
            && self
                .blocked_body_bytes
                .is_none_or(|bytes| bytes <= max_bytes)
    }
}

struct PreparedRetirement {
    catalogue: installed::OwnedRetirement,
    epoch: BlockHash,
    _scratch_limit: crate::confirmed_replay::scratch_file::ScratchLimit,
}

struct IdleScan<C> {
    cycle: Option<C>,
    complete: Option<C>,
}

impl<C: Clone> IdleScan<C> {
    fn new() -> Self {
        Self {
            cycle: None,
            complete: None,
        }
    }

    fn skip(&self, current: Option<&C>, compatible: &impl Fn(&C, &C) -> bool) -> bool {
        self.complete
            .as_ref()
            .zip(current)
            .is_some_and(|(before, after)| compatible(before, after))
    }

    fn begin(
        &mut self,
        current: Option<C>,
        after: Option<BlockHash>,
        compatible: &impl Fn(&C, &C) -> bool,
    ) {
        self.complete = None;
        if after.is_none() {
            self.cycle = current;
        } else if !self
            .cycle
            .as_ref()
            .zip(current.as_ref())
            .is_some_and(|(before, after)| compatible(before, after))
        {
            self.cycle = None;
        }
    }

    fn finish(
        &mut self,
        current: Option<C>,
        complete: bool,
        deleted: bool,
        compatible: &impl Fn(&C, &C) -> bool,
    ) {
        if deleted
            || !self
                .cycle
                .as_ref()
                .zip(current.as_ref())
                .is_some_and(|(before, after)| compatible(before, after))
        {
            self.cycle = None;
        }
        if complete {
            self.complete = self.cycle.clone();
        }
    }
}

impl Worker {
    pub(crate) fn start(
        self: Arc<Self>,
        policy: BodyRetirementConfig,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut stop = self.shutdown.subscribe();
            let interval = Duration::from_secs(policy.interval_secs);
            let mut cursor = None;
            let mut prepared = None;
            let mut idle = IdleScan::new();
            let mut continue_scan = false;
            let compatible =
                |before: &burst_store_lmdb::write_batch::CleanStorageCut,
                 after: &burst_store_lmdb::write_batch::CleanStorageCut| {
                    before
                        .body_dependencies_unchanged(after, self.store.as_ref())
                        .unwrap_or(false)
                };
            loop {
                tokio::select! {
                    biased;
                    _ = stop.recv() => break,
                    _ = async {
                        if continue_scan { tokio::task::yield_now().await; }
                        else { tokio::time::sleep(interval).await; }
                    } => {},
                }
                // Errors, idle scans and non-progressing pages wait again. Only
                // a productive page with an owned catalogue drains immediately.
                continue_scan = false;
                let cut = self.storage_cut();
                if idle.skip(cut.as_ref(), &compatible) {
                    continue;
                }
                idle.begin(cut, cursor, &compatible);
                // Await the owned attempt even during shutdown. Its blocking
                // worker checks shutdown before commit and publishes counts
                // synchronously afterward. This loop awaits each attempt.
                match self
                    .clone()
                    .run_cached(policy.clone(), cursor, prepared.take())
                    .await
                {
                    Ok((Some(report), next)) => {
                        continue_scan =
                            next.is_some() && report.can_continue_scan(cursor, policy.max_bytes);
                        prepared = next;
                        tracing::debug!(epoch = %report.epoch, deleted = report.hashes.len(), bytes = report.bytes,
                            examined = report.examined, blocked_bytes = ?report.blocked_body_bytes,
                            "physical retirement attempt complete");
                        // Age is relative to the certified cutoff, not wall time.
                        // Only a whole no-deletion cycle with unchanged body
                        // dependencies can suppress later idle preparation.
                        idle.finish(
                            self.storage_cut(),
                            report.complete,
                            !report.hashes.is_empty(),
                            &compatible,
                        );
                        cursor = if report.complete {
                            None
                        } else {
                            report.resume_after
                        };
                    }
                    Ok((None, next)) => {
                        prepared = next;
                        if prepared.is_none() {
                            cursor = None;
                        }
                        idle = IdleScan::new();
                    }
                    Err(error) => {
                        idle = IdleScan::new();
                        tracing::debug!(%error, "physical retirement deferred until next interval");
                    }
                }
                if self.shutdown.is_requested() {
                    break;
                }
            }
        })
    }

    fn storage_cut(&self) -> Option<burst_store_lmdb::write_batch::CleanStorageCut> {
        // Drop the read transaction immediately: idle reuse must not pin old
        // LMDB pages. Read errors bypass the shortcut and normal recovery reports them.
        self.store
            .tx_begin_read()
            .ok()
            .and_then(|view| view.clean_storage_cut())
    }

    #[cfg(test)]
    pub(crate) async fn run_once(
        self: Arc<Self>,
        policy: BodyRetirementConfig,
        cursor: Option<BlockHash>,
    ) -> Result<Option<Report>, StoreError> {
        self.run_cached(policy, cursor, None)
            .await
            .map(|(report, _)| report)
    }

    async fn run_cached(
        self: Arc<Self>,
        policy: BodyRetirementConfig,
        cursor: Option<BlockHash>,
        prepared: Option<PreparedRetirement>,
    ) -> Result<(Option<Report>, Option<PreparedRetirement>), StoreError> {
        // The task owns its proof and scratch lease even if its waiter is dropped.
        #[cfg(test)]
        let probe = self.prepared_probe.clone();
        #[cfg(test)]
        let started = std::time::Instant::now();
        let result = tokio::spawn(self.run_once_owned(policy, cursor, prepared))
            .await
            .map_err(|error| StoreError::Corruption(format!("retirement task failed: {error}")))?;
        #[cfg(test)]
        if let Some(probe) = probe {
            let (examined, deleted, bytes, error) = match &result {
                Ok((Some(report), _)) => (report.examined, report.hashes.len(), report.bytes, None),
                Ok((None, _)) => (0, 0, 0, None),
                Err(error) => (0, 0, 0, Some(error.to_string())),
            };
            probe(Probe::Finished {
                elapsed: started.elapsed(),
                examined,
                deleted,
                bytes,
                error,
            });
        }
        result
    }

    async fn run_once_owned(
        self: Arc<Self>,
        policy: BodyRetirementConfig,
        cursor: Option<BlockHash>,
        prepared: Option<PreparedRetirement>,
    ) -> Result<(Option<Report>, Option<PreparedRetirement>), StoreError> {
        policy.validate().map_err(StoreError::Corruption)?;
        if !policy.enabled || self.shutdown.is_requested() {
            return Ok((None, None));
        }
        let prepared = match prepared {
            Some(mut prepared) => {
                let store = self.store.clone();
                let shutdown = self.shutdown.clone();
                let limits = policy.capture_limits();
                let (prepared, exhausted) = tokio::task::spawn_blocking(move || {
                    let poll = capture_budget::CapturePoll::start(&store, limits)?;
                    let result = prepared
                        .catalogue
                        .reconcile_until(&store, || shutdown.is_requested() || poll.cancelled());
                    let exhausted = poll.finish()?;
                    Ok::<_, StoreError>((
                        if result.is_ok() { Some(prepared) } else { None },
                        exhausted,
                    ))
                })
                .await
                .map_err(|error| {
                    StoreError::Corruption(format!(
                        "retirement reconciliation task failed: {error}"
                    ))
                })??;
                if exhausted {
                    return Ok((None, None));
                }
                prepared
            }
            None => None,
        };
        let recovery = if prepared.is_none() {
            let budget = policy
                .max_scratch_bytes
                .expect("validated retirement budget");
            let map_bytes = ((budget / 4).min(16 << 20) & !4095) as usize;
            if map_bytes < 1 << 20 {
                return Err(StoreError::Corruption(
                    "retirement durable inputs need at least4MiB aggregate scratch budget".into(),
                ));
            }
            let reserved =
                crate::confirmed_replay::snapshot::history::DurableHistoryInputs::reservation(
                    map_bytes,
                )?;
            let limit =
                crate::confirmed_replay::scratch_file::ScratchLimit::acquire_shared_reserved(
                    self.store.env().path(),
                    budget,
                    reserved,
                )
                .map_err(|error| StoreError::Corruption(error.to_string()))?;
            let captured = Box::pin(installed::recover_retirement_catalogue(
                self.store.clone(),
                self.network,
                self.params.clone(),
                self.registry.clone(),
                self.shutdown.clone(),
                (limit.clone(), map_bytes),
                policy.retain_seconds,
                cursor,
                policy.capture_limits(),
            ))
            .await?;
            let Some((catalogue, epoch)) = captured else {
                return Ok((None, None));
            };
            Some(PreparedRetirement {
                catalogue,
                epoch,
                _scratch_limit: limit,
            })
        } else {
            None
        };
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            if self.shutdown.is_requested() {
                return Ok((None, None));
            }
            #[cfg(test)]
            let built_catalogue = prepared.is_none();
            let mut prepared = match prepared {
                Some(prepared) => prepared,
                None => recovery.expect("missing retirement recovery"),
            };
            let poll = capture_budget::CapturePoll::start(&self.store, policy.capture_limits())?;
            let page = prepared.catalogue.prepare_page_until(
                &self.store,
                policy.max_bodies,
                policy.max_bytes,
                policy.max_examined,
                || self.shutdown.is_requested() || poll.cancelled(),
            );
            if poll.finish()? {
                return Ok((None, None));
            }
            let plan = page?;
            let progress = plan.progress();
            let hashes = plan.hashes().to_vec();
            #[cfg(test)]
            if let Some(probe) = &self.prepared_probe {
                probe(Probe::Prepared(built_catalogue));
            }
            let mut stop = self.shutdown.subscribe();
            let publication = runtime.block_on(async {
                tokio::select! {
                    biased;
                    _ = stop.recv() => None,
                    guard = self.processor.clone().lock_owned() => Some(guard),
                }
            });
            let Some(_publication) = publication else {
                return Ok((None, None));
            };
            if self.shutdown.is_requested() {
                return Ok((None, None));
            }
            let Some((_, bytes)) = plan.try_commit_counted(&self.store, &self.counts)? else {
                #[cfg(test)]
                if let Some(probe) = &self.prepared_probe {
                    probe(Probe::Deferred);
                }
                return Ok((None, Some(prepared)));
            };
            let report = Report {
                epoch: prepared.epoch,
                resume_after: progress.resume_after,
                complete: progress.complete,
                examined: progress.examined,
                hashes,
                bytes,
                blocked_body_bytes: progress.blocked_body_bytes,
            };
            let next = if progress.complete {
                None
            } else {
                Some(prepared)
            };
            Ok((Some(report), next))
        })
        .await
        .map_err(|error| StoreError::Corruption(format!("retirement worker failed: {error}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_continuation_requires_progress_and_a_body_that_fits() {
        let first = BlockHash::new([1; 32]);
        let second = BlockHash::new([2; 32]);
        let mut report = Report {
            epoch: first,
            resume_after: Some(second),
            complete: false,
            examined: 1,
            hashes: Vec::new(),
            bytes: 0,
            blocked_body_bytes: None,
        };
        assert!(report.can_continue_scan(None, 100));
        assert!(report.can_continue_scan(Some(first), 100));
        assert!(!report.can_continue_scan(Some(second), 100));
        assert!(!report.can_continue_scan(Some(BlockHash::new([3; 32])), 100));
        report.blocked_body_bytes = Some(100);
        assert!(
            report.can_continue_scan(Some(first), 100),
            "next empty page fits the body"
        );
        report.blocked_body_bytes = Some(101);
        assert!(
            !report.can_continue_scan(Some(first), 100),
            "oversized body must not spin"
        );
        report.blocked_body_bytes = None;
        report.examined = 0;
        assert!(!report.can_continue_scan(Some(first), 100));
        report.examined = 1;
        report.resume_after = None;
        assert!(!report.can_continue_scan(None, 100));
        report.resume_after = Some(second);
        report.complete = true;
        assert!(!report.can_continue_scan(Some(first), 100));
    }

    #[test]
    fn idle_scan_requires_a_complete_unchanged_cycle() {
        let cursor = Some(BlockHash::new([1; 32]));
        let same = |before: &usize, after: &usize| before == after;
        let mut scan = IdleScan::new();
        assert!(!scan.skip(Some(&1), &same));
        scan.begin(Some(1), None, &same);
        scan.finish(Some(1), false, false, &same);
        assert!(!scan.skip(Some(&1), &same));
        scan.begin(Some(1), cursor, &same);
        scan.finish(Some(1), true, false, &same);
        assert!(scan.skip(Some(&1), &same));
        assert!(!scan.skip(None, &same));
        assert!(!scan.skip(Some(&2), &same));
        scan.begin(Some(2), None, &same);
        scan.finish(Some(2), false, false, &same);
        scan.begin(Some(3), cursor, &same);
        scan.finish(Some(3), true, false, &same);
        assert!(!scan.skip(Some(&3), &same));
        scan.begin(Some(3), None, &same);
        scan.finish(Some(3), true, false, &same);
        assert!(scan.skip(Some(&3), &same));
        for (before, after, deleted) in [
            (Some(4), Some(5), false),
            (Some(5), Some(5), true),
            (None, None, false),
            (Some(5), None, false),
        ] {
            scan.begin(before, None, &same);
            scan.finish(after, true, deleted, &same);
            assert!(!scan.skip(after.as_ref(), &same));
        }
    }

    #[test]
    fn idle_scan_reuses_only_a_whole_compatible_cycle() {
        // Different hundreds model dependency changes; within a hundred only
        // increasing sequence-only generations are compatible. Storage tests
        // exercise the real typed commit coverage and environment identity.
        let compatible =
            |before: &usize, after: &usize| before <= after && before / 100 == after / 100;
        let mut scan = IdleScan::new();
        let cursor = Some(BlockHash::new([1; 32]));
        scan.begin(Some(1), None, &compatible);
        scan.finish(Some(2), false, false, &compatible);
        scan.begin(Some(3), cursor, &compatible);
        scan.finish(Some(4), true, false, &compatible);
        assert!(scan.skip(Some(&99), &compatible));
        assert!(!scan.skip(Some(&100), &compatible));
        assert!(!scan.skip(Some(&0), &compatible));
        scan.begin(Some(100), cursor, &compatible);
        scan.finish(Some(101), true, false, &compatible);
        assert!(!scan.skip(Some(&101), &compatible));
        scan.begin(Some(102), None, &compatible);
        scan.finish(Some(103), true, true, &compatible);
        assert!(!scan.skip(Some(&103), &compatible));
    }
}
