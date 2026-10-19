//! Bounded CPU authentication ahead of the serialized economic commit path.

use std::{collections::VecDeque, sync::Arc};

use tokio::task::{JoinError, JoinHandle};

use crate::{
    block_processor::{AuthenticatedBlock, AuthenticationRules},
    priority_queue::BlockPriorityQueue,
};

pub(crate) struct BlockValidationQueue {
    queue: Arc<BlockPriorityQueue>,
    stored: Option<Arc<dyn burst_store::block::BlockStore + Send + Sync>>,
    rules: AuthenticationRules,
    workers: usize,
    pending: VecDeque<JoinHandle<AuthenticatedBlock>>,
    #[cfg(test)]
    before_authenticate: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl BlockValidationQueue {
    pub(crate) fn new(
        queue: Arc<BlockPriorityQueue>,
        rules: AuthenticationRules,
        stored: Arc<dyn burst_store::block::BlockStore + Send + Sync>,
    ) -> Self {
        // Use two workers on small multicore devices; reserve half the cores
        // on larger hosts and cap speculative CPU/memory use.
        let cores = std::thread::available_parallelism().map_or(1, usize::from);
        let mut pipeline = Self::with_workers(queue, rules, (cores / 2).clamp(2, 8).min(cores));
        pipeline.stored = Some(stored);
        pipeline
    }

    fn with_workers(
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
            pending: VecDeque::new(),
            #[cfg(test)]
            before_authenticate: None,
        }
    }

    fn schedule(&mut self, retained: crate::block_retention::RetainedBlock) {
        let block = retained.block;
        // Match the existing ingestion dedup before spending CPU on signatures.
        // This is only a drop, never an authentication proof used after rollback.
        if self
            .stored
            .as_ref()
            .is_some_and(|store| store.exists(&block.hash).unwrap_or(false))
        {
            return;
        }
        let rules = self.rules.clone();
        #[cfg(test)]
        let hook = self.before_authenticate.clone();
        self.pending.push_back(tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            if let Some(hook) = hook {
                hook();
            }
            let mut authenticated = rules.authenticate(block);
            authenticated.retention = retained.slot;
            authenticated
        }));
    }

    pub(crate) fn set_rules(&mut self, rules: AuthenticationRules) {
        self.rules = rules;
    }

    /// Cancellation-safe in the node's shutdown/checkpoint select. Selected
    /// work stays in `pending` until delivered; no block is lost on a timer tick.
    pub(crate) async fn next(&mut self) -> Result<AuthenticatedBlock, JoinError> {
        while self.pending.is_empty() {
            let block = self.queue.pop_retained().await;
            self.schedule(block);
        }
        while self.pending.len() < self.workers {
            let Some(block) = self.queue.try_pop_retained().await else {
                break;
            };
            self.schedule(block);
        }
        // Preserve priority dequeue order even when a later signature finishes
        // first. Ledger dependencies are still checked at admission/commit.
        let result = self.pending.front_mut().expect("scheduled work").await;
        self.pending.pop_front();
        result
    }
}

#[cfg(test)]
mod tests;

impl Drop for BlockValidationQueue {
    fn drop(&mut self) {
        for task in &self.pending {
            // Running blocking jobs cannot be cancelled, but they own only a
            // bounded block and immutable rules, never ledger or node locks.
            task.abort();
        }
    }
}
