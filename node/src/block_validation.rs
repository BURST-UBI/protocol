//! Authentication and speculative payment preparation ahead of serialized commit.
//! Workers run concurrently; results are delivered in dequeue order. Worker and
//! ready-result counts are bounded here, not total engine memory or execution time.

use std::{collections::VecDeque, sync::Arc};

use tokio::task::{JoinError, JoinHandle};

use crate::{
    block_processor::{AuthenticationRules, BlockAuthentication},
    priority_queue::BlockPriorityQueue,
};

const PREPARATION_WINDOW: usize = 32;

// Keys live only as long as their worker/result/admission or unchecked owner. This is not an
// authentication cache: a later retry must validate against current rules again.
type InFlightPayloads = Arc<std::sync::Mutex<std::collections::BTreeSet<[u8; 32]>>>;

#[derive(Debug)]
pub(crate) struct InFlightPayload {
    payload: [u8; 32],
    owners: InFlightPayloads,
}

impl Drop for InFlightPayload {
    fn drop(&mut self) {
        self.owners
            .lock()
            .expect("in-flight payload lock poisoned")
            .remove(&self.payload);
    }
}

pub(crate) struct BlockValidationQueue {
    queue: Arc<BlockPriorityQueue>,
    stored: Option<Arc<dyn burst_store::block::BlockStore + Send + Sync>>,
    rules: AuthenticationRules,
    workers: usize,
    in_flight: InFlightPayloads,
    ready_limit: usize,
    gathering: Option<usize>,
    ready: VecDeque<Result<BlockAuthentication, JoinError>>,
    preparation_accounts: crate::account_work_pool::AccountWorkPool,
    preparation: Option<(
        Arc<burst_store_lmdb::LmdbStore>,
        burst_types::ProtocolParams,
    )>,
    #[cfg(test)]
    pub(crate) before_prepare: Option<Arc<dyn Fn() + Send + Sync>>,
    pending: VecDeque<JoinHandle<BlockAuthentication>>,
    #[cfg(test)]
    before_authenticate: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl BlockValidationQueue {
    pub(crate) fn new(
        queue: Arc<BlockPriorityQueue>,
        rules: AuthenticationRules,
        stored: Arc<dyn burst_store::block::BlockStore + Send + Sync>,
    ) -> Self {
        // Use one worker on a single core, otherwise half the available cores
        // clamped to 2–8. This caps jobs, not their individual memory use.
        let cores = std::thread::available_parallelism().map_or(1, usize::from);
        let mut pipeline = Self::with_workers(queue, rules, (cores / 2).clamp(2, 8).min(cores));
        pipeline.stored = Some(stored);
        pipeline
    }

    pub(crate) fn with_workers(
        queue: Arc<BlockPriorityQueue>,
        rules: AuthenticationRules,
        workers: usize,
    ) -> Self {
        assert!(workers > 0);
        Self {
            queue,
            stored: None,
            rules,
            workers,
            in_flight: Arc::default(),
            ready_limit: 1,
            gathering: None,
            ready: VecDeque::new(),
            pending: VecDeque::new(),
            preparation: None,
            // One delivered candidate retains its account reservation through
            // admission while up to `workers` jobs prepare the following work.
            preparation_accounts: crate::account_work_pool::AccountWorkPool::new(
                workers.checked_add(1).expect("worker count overflow"),
            ),
            #[cfg(test)]
            before_prepare: None,
            #[cfg(test)]
            before_authenticate: None,
        }
    }

    fn schedule(&mut self, retained: crate::block_retention::RetainedBlock) {
        let block = retained.block;
        // Drop hashes already stored before spending CPU on authentication.
        // A failed existence read continues validation; this is not a reusable proof.
        if self
            .stored
            .as_ref()
            .is_some_and(|store| store.exists(&block.hash).unwrap_or(false))
        {
            return;
        }
        // Include signatures and work, not just the claimed block hash: an
        // invalid variant must not suppress a correctly signed candidate.
        let in_flight = if let Ok(encoded) = bincode::serialize(&block) {
            let payload = burst_crypto::blake2b_256(&encoded);
            if !self
                .in_flight
                .lock()
                .expect("in-flight payload lock poisoned")
                .insert(payload)
            {
                return;
            }
            Some(Arc::new(InFlightPayload {
                payload,
                owners: self.in_flight.clone(),
            }))
        } else {
            None
        };
        let rules = self.rules.clone();
        let preparation = self.preparation.clone();
        // Reserve in dequeue order. Awaiting a same-account guard in a worker
        // could deadlock FIFO delivery if a later worker acquired it first.
        let reservation = preparation
            .as_ref()
            .filter(|_| {
                crate::economic_candidate::prepared::CapturedPayment::eligible(&block.block_type)
            })
            .and_then(|_| {
                self.preparation_accounts
                    .try_reserve(block.account.as_str())
            });
        #[cfg(test)]
        let prepare_hook = self.before_prepare.clone();
        #[cfg(test)]
        let hook = self.before_authenticate.clone();
        let dispatched =
            std::env::var_os("BURST_FINALITY_STAGE_LOG").map(|_| std::time::Instant::now());
        let queue_id = Arc::as_ptr(&self.queue) as usize;
        self.pending.push_back(tokio::task::spawn_blocking(move || {
            if let Some(dispatched) = dispatched {
                tracing::debug!(queue = queue_id, hash = %block.hash,
                    elapsed_us = dispatched.elapsed().as_micros(), "authentication worker dispatch");
            }
            #[cfg(test)]
            if let Some(hook) = hook {
                hook();
            }
            let mut authentication = rules.authenticate(block);
            authentication._in_flight_payload = in_flight;
            authentication.retention = retained.slot;
            let prepare_payment = reservation.as_ref().is_some_and(|(_, prepare)| *prepare);
            authentication._preparation_reservation = reservation.map(|(guard, _)| guard);
            if authentication.result().is_ok() && prepare_payment
            {
                if let Some((store, params)) = preparation {
                    use crate::economic_candidate::prepared::PreparedStageError;
                    let start = std::time::Instant::now();
                    let captured = crate::economic_candidate::prepared::CapturedPayment::capture_read(
                        &store, authentication.block(), &params);
                    match captured {
                        Ok(Some(captured)) => {
                            #[cfg(test)]
                            if let Some(hook) = prepare_hook { hook(); }
                            match captured.prepare() {
                                Ok(prepared) => authentication.prepared = Some(prepared),
                                Err(error) => tracing::debug!(%error, "payment preparation deferred to admission"),
                            }
                        }
                        Ok(None) => (),
                        Err(PreparedStageError::BeforeWrite(error)) =>
                            tracing::debug!(%error, "payment capture deferred to admission"),
                        Err(PreparedStageError::Abort(error)) => authentication.preparation_fault = Some(error),
                    }
                    authentication.preparation_time = start.elapsed();
                }
            }
            if let Some(dispatched) = dispatched {
                tracing::debug!(queue = queue_id, hash = %authentication.block().hash,
                    elapsed_us = dispatched.elapsed().as_micros(), "authentication worker complete");
            }
            authentication
        }));
    }

    pub(crate) fn set_preparation(
        &mut self,
        store: Arc<burst_store_lmdb::LmdbStore>,
        params: burst_types::ProtocolParams,
    ) {
        if self.preparation.is_none() {
            self.ready_limit = PREPARATION_WINDOW;
            self.preparation_accounts = crate::account_work_pool::AccountWorkPool::new(
                PREPARATION_WINDOW + self.workers + 1,
            );
        }
        // The admission loop refreshes parameters after checkpoint activation.
        // Pending/ready candidates keep their original reservations and captured
        // inputs; staging rechecks them against current authoritative state.
        self.preparation = Some((store, params));
    }

    /// Apply rules to future workers. Pending results retain their original rules;
    /// admission revalidates them when those rules no longer match.
    pub(crate) fn set_rules(&mut self, rules: AuthenticationRules) {
        self.rules = rules;
    }

    #[cfg(test)]
    pub(crate) fn install_completed_heads(&mut self, heads: Vec<JoinHandle<BlockAuthentication>>) {
        assert!(
            self.pending.is_empty() && self.ready.is_empty(),
            "probe must not reorder queued work"
        );
        assert!(
            heads.iter().all(JoinHandle::is_finished),
            "probe accepts completed authentication only"
        );
        self.pending.extend(heads);
    }

    fn refill_now(&mut self) {
        // Bound examined entries too: committed duplicates do not occupy worker
        // slots and must not turn an opportunistic refill into an ingress scan.
        let capacity = if self.ready_limit == 1 {
            self.workers
        } else {
            self.gathering.map_or(self.workers, |target| {
                self.workers.min(target.saturating_sub(self.ready.len()))
            })
        };
        for _ in 0..self.ready_limit.max(capacity) {
            if self.pending.len() >= capacity {
                break;
            }
            let Some(block) = self.queue.try_pop_retained_now() else {
                break;
            };
            self.schedule(block);
        }
    }

    /// Take only the completed FIFO head. This never waits for a later worker or
    /// removes an unfinished handle, so control messages can still win selection.
    pub(crate) fn try_next_ready(&mut self) -> Option<Result<BlockAuthentication, JoinError>> {
        if self.gathering.is_some() {
            return None;
        }
        if let Some(result) = self.ready.pop_front() {
            if self.ready.is_empty() && result.is_ok() {
                self.refill_now();
            }
            return Some(result);
        }
        use std::{
            future::Future,
            pin::Pin,
            task::{Context, Poll, Waker},
        };
        let head = self.pending.front_mut()?;
        if !head.is_finished() {
            return None;
        }
        let Poll::Ready(result) = Pin::new(head).poll(&mut Context::from_waker(Waker::noop()))
        else {
            return None;
        };
        self.pending.pop_front();
        if result.is_ok() {
            self.refill_now();
        }
        Some(result)
    }

    /// Cancellation preserves already selected work while the queue stays alive.
    /// A preparation window may wait for several FIFO workers before returning
    /// its first result; later arrivals do not increase the selected window size.
    pub(crate) async fn next(&mut self) -> Result<BlockAuthentication, JoinError> {
        if self.gathering.is_none() {
            if let Some(result) = self.ready.pop_front() {
                if self.ready.is_empty() && result.is_ok() {
                    self.refill_now();
                }
                return result;
            }
            while self.pending.is_empty() {
                let block = self.queue.pop_retained().await;
                self.schedule(block);
            }
            // Snapshot the amount of work already queued. New arrivals never
            // extend this window, and an idle node never waits for a batch.
            let available = self.pending.len().saturating_add(self.queue.len().await);
            self.gathering = Some(available.min(self.ready_limit).max(1));
        }
        while self.ready.len() < self.gathering.expect("active preparation window") {
            self.refill_now();
            let Some(head) = self.pending.front_mut() else {
                break;
            };
            // Completed results remain owned by the queue across cancellation.
            // At most `workers` blocking jobs run, even for a larger window.
            let result = head.await;
            self.pending.pop_front();
            let failed = result.is_err();
            self.ready.push_back(result);
            if failed {
                break;
            }
        }
        self.gathering = None;
        let result = self.ready.pop_front().expect("scheduled work");
        if self.ready.is_empty() && result.is_ok() {
            self.refill_now();
        }
        result
    }
}

#[cfg(test)]
mod tests;

impl Drop for BlockValidationQueue {
    fn drop(&mut self) {
        for task in &self.pending {
            // Abort queued jobs; already-running blocking jobs finish independently.
            // Capture uses a read snapshot. Jobs retain account/retention guards
            // until their authentication result is dropped.
            task.abort();
        }
    }
}
