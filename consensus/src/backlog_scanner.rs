//! Backlog scanner — tracks unconfirmed blocks for periodic re-checking.
//!
//! Blocks that arrive but don't immediately enter an election (e.g. because the
//! election container is at capacity, or a dependency is missing) are placed in
//! the backlog. A background task periodically drains batches from the backlog
//! and re-evaluates election eligibility.

use burst_types::BlockHash;
use std::collections::VecDeque;

/// Tracks unconfirmed blocks that should be periodically re-checked
/// for election eligibility.
pub struct BacklogScanner {
    /// Queue of block hashes to re-check (FIFO order).
    backlog: VecDeque<BlockHash>,
    /// Maximum backlog size. New entries are dropped when at capacity.
    capacity: usize,
    /// Minimum age (seconds) before a block is eligible for re-scan.
    /// Used by the caller to filter, not enforced internally.
    min_age_secs: u64,
}

impl BacklogScanner {
    /// Create a new scanner with the given capacity and minimum age.
    pub fn new(capacity: usize, min_age_secs: u64) -> Self {
        Self {
            backlog: VecDeque::with_capacity(capacity.min(1024)),
            capacity,
            min_age_secs,
        }
    }

    /// Add a block to the backlog. Silently dropped if at capacity.
    pub fn add(&mut self, hash: BlockHash) {
        if self.backlog.len() < self.capacity {
            self.backlog.push_back(hash);
        }
    }

    /// Remove a specific block from the backlog (e.g., it was confirmed).
    ///
    /// O(n) scan; acceptable because confirmations are relatively infrequent
    /// compared to the backlog size, and the backlog is bounded.
    pub fn remove(&mut self, hash: &BlockHash) {
        self.backlog.retain(|h| h != hash);
    }

    /// Get the next batch of blocks to re-scan.
    ///
    /// Drains up to `count` entries from the front of the queue. The caller
    /// is responsible for re-adding blocks that still need scanning.
    pub fn next_batch(&mut self, count: usize) -> Vec<BlockHash> {
        let n = count.min(self.backlog.len());
        self.backlog.drain(..n).collect()
    }

    /// Current backlog size.
    pub fn len(&self) -> usize {
        self.backlog.len()
    }

    /// Whether the backlog is empty.
    pub fn is_empty(&self) -> bool {
        self.backlog.is_empty()
    }

    /// Maximum capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The configured minimum age (seconds).
    pub fn min_age_secs(&self) -> u64 {
        self.min_age_secs
    }

    /// Whether the backlog is at capacity.
    pub fn is_full(&self) -> bool {
        self.backlog.len() >= self.capacity
    }

    /// Peek at the next hash without removing it.
    pub fn peek(&self) -> Option<&BlockHash> {
        self.backlog.front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(byte: u8) -> BlockHash {
        BlockHash::new([byte; 32])
    }

    #[test]
    fn new_scanner_is_empty() {
        let scanner = BacklogScanner::new(100, 30);
        assert!(scanner.is_empty());
        assert_eq!(scanner.len(), 0);
        assert_eq!(scanner.capacity(), 100);
        assert_eq!(scanner.min_age_secs(), 30);
    }

    #[test]
    fn add_and_len() {
        let mut scanner = BacklogScanner::new(100, 30);
        scanner.add(hash(1));
        scanner.add(hash(2));
        scanner.add(hash(3));

        assert_eq!(scanner.len(), 3);
        assert!(!scanner.is_empty());
    }

    #[test]
    fn capacity_enforcement() {
        let mut scanner = BacklogScanner::new(2, 0);
        scanner.add(hash(1));
        scanner.add(hash(2));
        scanner.add(hash(3)); // should be dropped

        assert_eq!(scanner.len(), 2);
        assert!(scanner.is_full());
    }

    #[test]
    fn next_batch_drains_from_front() {
        let mut scanner = BacklogScanner::new(100, 0);
        scanner.add(hash(1));
        scanner.add(hash(2));
        scanner.add(hash(3));
        scanner.add(hash(4));

        let batch = scanner.next_batch(2);
        assert_eq!(batch, vec![hash(1), hash(2)]);
        assert_eq!(scanner.len(), 2);

        let batch2 = scanner.next_batch(10);
        assert_eq!(batch2, vec![hash(3), hash(4)]);
        assert!(scanner.is_empty());
    }

    #[test]
    fn next_batch_clamped_to_len() {
        let mut scanner = BacklogScanner::new(100, 0);
        scanner.add(hash(1));

        let batch = scanner.next_batch(100);
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0], hash(1));
    }

    #[test]
    fn next_batch_empty() {
        let mut scanner = BacklogScanner::new(100, 0);
        let batch = scanner.next_batch(10);
        assert!(batch.is_empty());
    }

    #[test]
    fn remove_specific_block() {
        let mut scanner = BacklogScanner::new(100, 0);
        scanner.add(hash(1));
        scanner.add(hash(2));
        scanner.add(hash(3));

        scanner.remove(&hash(2));

        assert_eq!(scanner.len(), 2);
        let batch = scanner.next_batch(10);
        assert_eq!(batch, vec![hash(1), hash(3)]);
    }

    #[test]
    fn remove_nonexistent_is_noop() {
        let mut scanner = BacklogScanner::new(100, 0);
        scanner.add(hash(1));
        scanner.remove(&hash(99));
        assert_eq!(scanner.len(), 1);
    }

    #[test]
    fn peek_returns_front() {
        let mut scanner = BacklogScanner::new(100, 0);
        assert!(scanner.peek().is_none());

        scanner.add(hash(5));
        scanner.add(hash(6));

        assert_eq!(scanner.peek(), Some(&hash(5)));
        // peek doesn't remove
        assert_eq!(scanner.len(), 2);
    }

    #[test]
    fn fifo_ordering_preserved() {
        let mut scanner = BacklogScanner::new(100, 0);
        for i in 0..10 {
            scanner.add(hash(i));
        }

        let all = scanner.next_batch(10);
        let expected: Vec<BlockHash> = (0..10).map(hash).collect();
        assert_eq!(all, expected);
    }

    #[test]
    fn add_after_drain_works() {
        let mut scanner = BacklogScanner::new(3, 0);
        scanner.add(hash(1));
        scanner.add(hash(2));
        scanner.add(hash(3));

        let _ = scanner.next_batch(2);
        assert_eq!(scanner.len(), 1);

        // Capacity freed up
        assert!(!scanner.is_full());
        scanner.add(hash(4));
        scanner.add(hash(5));

        assert_eq!(scanner.len(), 3);
        let batch = scanner.next_batch(3);
        assert_eq!(batch, vec![hash(3), hash(4), hash(5)]);
    }
}
