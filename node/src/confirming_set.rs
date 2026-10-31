//! Standalone in-memory cementation queue and counter. The running node uses
//! only the counter here; its worker consumes the durable confirmation queue.

use burst_types::BlockHash;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Main-queue admission limit for add; deferred retries bypass it.
const MAX_CONFIRMING: usize = 16_384;
/// Batch size for cementation.
const CEMENT_BATCH_SIZE: usize = 256;

/// Manages the set of blocks waiting to be cemented.
pub struct ConfirmingSet {
    /// Blocks waiting to be cemented.
    queue: VecDeque<BlockHash>,
    /// Blocks that failed cementation (retry later).
    deferred: VecDeque<(BlockHash, u32)>,
    /// Upper bound checked against the caller-supplied retry count.
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

    /// Append without deduplication or checking confirmation. Returns false
    /// when the main queue is full; deferred entries do not count toward capacity.
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

    /// Append to the unbounded deferred queue if the supplied retry count permits.
    pub fn defer(&mut self, hash: BlockHash, retry_count: u32) {
        if retry_count < self.max_retries {
            self.deferred.push_back((hash, retry_count + 1));
        } else {
            tracing::warn!(%hash, "block exceeded max cementation retries, dropping");
        }
    }

    /// Move all deferred hashes to the main queue, discarding retry counts.
    /// This bypasses the capacity check and does not update near_full.
    pub fn retry_deferred(&mut self) {
        while let Some((hash, _count)) = self.deferred.pop_front() {
            self.queue.push_back(hash);
        }
    }

    /// Hysteresis signal maintained by add and next_batch: set at 80% full,
    /// cleared below 60%. Deferred entries and retries do not update it.
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
        let set = ConfirmingSet::new(5);
        assert_eq!(set.pending_count(), 0);
        assert_eq!(set.cemented_count(), 0);
        assert!(!set.is_near_full());
    }

    #[test]
    fn add_and_retrieve_batch() {
        let mut set = ConfirmingSet::new(5);
        assert!(set.add(hash(1)));
        assert!(set.add(hash(2)));
        assert!(set.add(hash(3)));

        assert_eq!(set.pending_count(), 3);

        let batch = set.next_batch();
        assert_eq!(batch.len(), 3);
        assert_eq!(batch[0], hash(1));
        assert_eq!(batch[1], hash(2));
        assert_eq!(batch[2], hash(3));
        assert_eq!(set.pending_count(), 0);
    }

    #[test]
    fn batch_size_limited() {
        let mut set = ConfirmingSet::new(5);
        for i in 0..=255 {
            set.add(hash(i));
        }
        // Add more via a different byte pattern
        for i in 0..100u8 {
            let mut bytes = [0u8; 32];
            bytes[0] = i;
            bytes[1] = 1;
            set.add(BlockHash::new(bytes));
        }
        assert_eq!(set.pending_count(), 356);

        let batch = set.next_batch();
        assert_eq!(batch.len(), CEMENT_BATCH_SIZE);
        assert_eq!(set.pending_count(), 356 - CEMENT_BATCH_SIZE);
    }

    #[test]
    fn backpressure_at_capacity() {
        let mut set = ConfirmingSet::new(5);
        for i in 0..MAX_CONFIRMING {
            let mut bytes = [0u8; 32];
            bytes[0..4].copy_from_slice(&(i as u32).to_le_bytes());
            assert!(set.add(BlockHash::new(bytes)));
        }
        // Queue is now full — next add should fail
        assert!(!set.add(hash(0xFF)));
        assert!(set.is_near_full());
    }

    #[test]
    fn near_full_signal_at_80_percent() {
        let mut set = ConfirmingSet::new(5);
        let threshold = MAX_CONFIRMING * 80 / 100;

        for i in 0..threshold {
            let mut bytes = [0u8; 32];
            bytes[0..4].copy_from_slice(&(i as u32).to_le_bytes());
            set.add(BlockHash::new(bytes));
        }
        assert!(set.is_near_full());
    }

    #[test]
    fn near_full_clears_below_60_percent() {
        let mut set = ConfirmingSet::new(5);
        let threshold = MAX_CONFIRMING * 80 / 100;

        for i in 0..threshold {
            let mut bytes = [0u8; 32];
            bytes[0..4].copy_from_slice(&(i as u32).to_le_bytes());
            set.add(BlockHash::new(bytes));
        }
        assert!(set.is_near_full());

        // Drain enough to get below 60%
        let target = MAX_CONFIRMING * 60 / 100;
        let drain_count = threshold - target + 1;
        for _ in 0..drain_count.div_ceil(CEMENT_BATCH_SIZE) {
            set.next_batch();
        }
        assert!(!set.is_near_full());
    }

    #[test]
    fn defer_and_retry() {
        let mut set = ConfirmingSet::new(3);
        set.defer(hash(1), 0);
        set.defer(hash(2), 1);

        assert_eq!(set.pending_count(), 2); // 2 deferred

        set.retry_deferred();
        assert_eq!(set.pending_count(), 2); // moved to main queue

        let batch = set.next_batch();
        assert_eq!(batch.len(), 2);
    }

    #[test]
    fn defer_exceeds_max_retries() {
        let mut set = ConfirmingSet::new(3);

        set.defer(hash(1), 2);
        assert_eq!(set.pending_count(), 1); // 2 < 3, so added with count 3

        // retry_count = 3 → 3 >= 3 → dropped
        set.defer(hash(2), 3);
        assert_eq!(set.pending_count(), 1); // not added
    }

    #[test]
    fn record_cemented_count() {
        let set = ConfirmingSet::new(5);
        assert_eq!(set.cemented_count(), 0);

        set.record_cemented(10);
        assert_eq!(set.cemented_count(), 10);

        set.record_cemented(5);
        assert_eq!(set.cemented_count(), 15);
    }

    #[test]
    fn empty_batch_from_empty_set() {
        let mut set = ConfirmingSet::new(5);
        let batch = set.next_batch();
        assert!(batch.is_empty());
    }

    #[test]
    fn fifo_ordering_preserved() {
        let mut set = ConfirmingSet::new(5);
        set.add(hash(10));
        set.add(hash(20));
        set.add(hash(30));

        let batch = set.next_batch();
        assert_eq!(batch, vec![hash(10), hash(20), hash(30)]);
    }
}
