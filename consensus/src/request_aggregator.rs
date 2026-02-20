//! Request aggregator — batches inbound vote requests to minimize vote generation.
//!
//! When multiple peers request votes for the same block, the aggregator
//! batches them so we only generate one vote and send copies to all requesters.
//! This is critical for performance under high load.

use burst_types::BlockHash;
use std::collections::{HashMap, VecDeque};

/// Aggregates inbound vote requests to minimize vote generation.
///
/// When multiple peers request votes for the same block, the aggregator
/// batches them so we only generate one vote and send copies to all requesters.
pub struct RequestAggregator {
    /// Pending requests: block_hash -> list of requesting peer IDs
    pending: HashMap<BlockHash, Vec<String>>,
    /// Processing order (FIFO)
    queue: VecDeque<BlockHash>,
    /// Maximum pending requests
    max_pending: usize,
    /// Batch size for processing
    batch_size: usize,
}

impl RequestAggregator {
    /// Create a new request aggregator.
    ///
    /// # Arguments
    /// - `max_pending` — maximum number of distinct block hashes that can be queued
    /// - `batch_size` — number of items to dequeue per `next_batch` call
    pub fn new(max_pending: usize, batch_size: usize) -> Self {
        Self {
            pending: HashMap::new(),
            queue: VecDeque::new(),
            max_pending,
            batch_size,
        }
    }

    /// Add a vote request from a peer.
    ///
    /// If the block hash is already pending, the peer is added to the list of
    /// requesters. If at capacity and the block hash is new, the request is dropped.
    pub fn add_request(&mut self, block_hash: BlockHash, peer_id: String) {
        if self.pending.len() >= self.max_pending && !self.pending.contains_key(&block_hash) {
            return; // Drop if at capacity
        }
        let entry = self.pending.entry(block_hash).or_insert_with(|| {
            self.queue.push_back(block_hash);
            Vec::new()
        });
        if !entry.contains(&peer_id) {
            entry.push(peer_id);
        }
    }

    /// Get the next batch of requests to process.
    /// Returns `(block_hash, requesting_peer_ids)` pairs.
    ///
    /// Items are dequeued in FIFO order, up to `batch_size` items.
    pub fn next_batch(&mut self) -> Vec<(BlockHash, Vec<String>)> {
        let count = self.batch_size.min(self.queue.len());
        let mut batch = Vec::with_capacity(count);
        for _ in 0..count {
            if let Some(hash) = self.queue.pop_front() {
                if let Some(peers) = self.pending.remove(&hash) {
                    batch.push((hash, peers));
                }
            }
        }
        batch
    }

    /// Number of pending request entries (distinct block hashes).
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Whether there are requests waiting to be processed.
    pub fn has_pending(&self) -> bool {
        !self.queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_hash(byte: u8) -> BlockHash {
        BlockHash::new([byte; 32])
    }

    #[test]
    fn new_aggregator_is_empty() {
        let agg = RequestAggregator::new(100, 10);
        assert_eq!(agg.pending_count(), 0);
        assert!(!agg.has_pending());
    }

    #[test]
    fn add_single_request() {
        let mut agg = RequestAggregator::new(100, 10);
        agg.add_request(test_hash(1), "peer1".to_string());
        assert_eq!(agg.pending_count(), 1);
        assert!(agg.has_pending());
    }

    #[test]
    fn add_duplicate_peer_for_same_block() {
        let mut agg = RequestAggregator::new(100, 10);
        agg.add_request(test_hash(1), "peer1".to_string());
        agg.add_request(test_hash(1), "peer1".to_string());
        assert_eq!(agg.pending_count(), 1);

        let batch = agg.next_batch();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].1.len(), 1); // peer1 only once
    }

    #[test]
    fn aggregate_multiple_peers_same_block() {
        let mut agg = RequestAggregator::new(100, 10);
        agg.add_request(test_hash(1), "peer1".to_string());
        agg.add_request(test_hash(1), "peer2".to_string());
        agg.add_request(test_hash(1), "peer3".to_string());
        assert_eq!(agg.pending_count(), 1);

        let batch = agg.next_batch();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].1.len(), 3);
    }

    #[test]
    fn fifo_ordering() {
        let mut agg = RequestAggregator::new(100, 10);
        agg.add_request(test_hash(1), "peer1".to_string());
        agg.add_request(test_hash(2), "peer2".to_string());
        agg.add_request(test_hash(3), "peer3".to_string());

        let batch = agg.next_batch();
        assert_eq!(batch.len(), 3);
        assert_eq!(batch[0].0, test_hash(1));
        assert_eq!(batch[1].0, test_hash(2));
        assert_eq!(batch[2].0, test_hash(3));
    }

    #[test]
    fn batch_size_limits_output() {
        let mut agg = RequestAggregator::new(100, 2);
        agg.add_request(test_hash(1), "peer1".to_string());
        agg.add_request(test_hash(2), "peer2".to_string());
        agg.add_request(test_hash(3), "peer3".to_string());

        let batch1 = agg.next_batch();
        assert_eq!(batch1.len(), 2);

        let batch2 = agg.next_batch();
        assert_eq!(batch2.len(), 1);

        let batch3 = agg.next_batch();
        assert_eq!(batch3.len(), 0);
    }

    #[test]
    fn max_pending_drops_new_entries() {
        let mut agg = RequestAggregator::new(2, 10);
        agg.add_request(test_hash(1), "peer1".to_string());
        agg.add_request(test_hash(2), "peer2".to_string());
        agg.add_request(test_hash(3), "peer3".to_string()); // should be dropped
        assert_eq!(agg.pending_count(), 2);
    }

    #[test]
    fn max_pending_allows_existing_hash() {
        let mut agg = RequestAggregator::new(2, 10);
        agg.add_request(test_hash(1), "peer1".to_string());
        agg.add_request(test_hash(2), "peer2".to_string());
        // At capacity but hash(1) already exists — should be accepted
        agg.add_request(test_hash(1), "peer3".to_string());
        assert_eq!(agg.pending_count(), 2);

        let batch = agg.next_batch();
        // hash(1) should have both peer1 and peer3
        let h1_entry = batch.iter().find(|(h, _)| *h == test_hash(1)).unwrap();
        assert_eq!(h1_entry.1.len(), 2);
    }

    #[test]
    fn next_batch_clears_state() {
        let mut agg = RequestAggregator::new(100, 10);
        agg.add_request(test_hash(1), "peer1".to_string());
        agg.add_request(test_hash(2), "peer2".to_string());

        let batch = agg.next_batch();
        assert_eq!(batch.len(), 2);
        assert_eq!(agg.pending_count(), 0);
        assert!(!agg.has_pending());
    }

    #[test]
    fn empty_batch_returns_empty_vec() {
        let mut agg = RequestAggregator::new(100, 10);
        let batch = agg.next_batch();
        assert!(batch.is_empty());
    }

    #[test]
    fn interleaved_add_and_batch() {
        let mut agg = RequestAggregator::new(100, 2);

        agg.add_request(test_hash(1), "peer1".to_string());
        agg.add_request(test_hash(2), "peer2".to_string());

        let batch1 = agg.next_batch();
        assert_eq!(batch1.len(), 2);

        // Add more after draining
        agg.add_request(test_hash(3), "peer3".to_string());
        assert_eq!(agg.pending_count(), 1);

        let batch2 = agg.next_batch();
        assert_eq!(batch2.len(), 1);
        assert_eq!(batch2[0].0, test_hash(3));
    }
}
