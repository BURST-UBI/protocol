//! Work pre-computation cache and priority queue.
//!
//! After a block is confirmed, we can pre-compute PoW on its hash so the
//! *next* transaction from that account has zero latency. The priority queue
//! orders incoming blocks by PoW difficulty so higher-effort blocks get
//! processed first, providing natural spam resistance.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use burst_types::BlockHash;

// ---------------------------------------------------------------------------
// Work Cache — pre-computed PoW nonces
// ---------------------------------------------------------------------------

/// Cache for pre-computed PoW nonces, keyed by block hash.
///
/// Once a block is confirmed its hash becomes the input for the next
/// transaction's PoW. Pre-computing that nonce eliminates wait time when the
/// user initiates a new transaction.
pub struct WorkCache {
    cache: HashMap<[u8; 32], u64>,
    max_entries: usize,
}

impl WorkCache {
    /// Create a new work cache with the given capacity.
    pub fn new(max_entries: usize) -> Self {
        Self {
            cache: HashMap::with_capacity(max_entries),
            max_entries,
        }
    }

    /// Insert a pre-computed nonce for a block hash.
    ///
    /// If the cache is full the entry is silently dropped (FIFO eviction
    /// would add complexity; callers should size the cache appropriately).
    pub fn insert(&mut self, block_hash: &BlockHash, nonce: u64) {
        if self.cache.len() >= self.max_entries {
            // Evict an arbitrary entry to make room.
            if let Some(&key) = self.cache.keys().next() {
                self.cache.remove(&key);
            }
        }
        self.cache.insert(*block_hash.as_bytes(), nonce);
    }

    /// Retrieve the cached nonce for a block hash, if any.
    pub fn get(&self, block_hash: &BlockHash) -> Option<u64> {
        self.cache.get(block_hash.as_bytes()).copied()
    }

    /// Remove a cached entry (e.g. after the nonce has been consumed).
    pub fn remove(&mut self, block_hash: &BlockHash) {
        self.cache.remove(block_hash.as_bytes());
    }

    /// Number of entries currently cached.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Priority Queue — order blocks by PoW difficulty
// ---------------------------------------------------------------------------

/// A block waiting in the processing queue, ranked by PoW difficulty.
pub struct PriorityBlock {
    /// Serialized block bytes.
    pub block_bytes: Vec<u8>,
    /// PoW difficulty value (higher = more effort spent).
    pub difficulty: u64,
    /// Unix timestamp (seconds) when this block was received.
    pub received_at: u64,
}

impl PartialEq for PriorityBlock {
    fn eq(&self, other: &Self) -> bool {
        self.difficulty == other.difficulty && self.received_at == other.received_at
    }
}

impl Eq for PriorityBlock {}

impl PartialOrd for PriorityBlock {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PriorityBlock {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher difficulty = higher priority. Break ties by earlier arrival.
        self.difficulty
            .cmp(&other.difficulty)
            .then_with(|| other.received_at.cmp(&self.received_at))
    }
}

/// A bounded max-heap that prioritizes blocks with higher PoW difficulty.
pub struct WorkPriorityQueue {
    queue: BinaryHeap<PriorityBlock>,
    max_size: usize,
}

impl WorkPriorityQueue {
    /// Create a new priority queue with the given capacity.
    pub fn new(max_size: usize) -> Self {
        Self {
            queue: BinaryHeap::with_capacity(max_size),
            max_size,
        }
    }

    /// Push a block onto the queue. Returns `false` if the queue is full.
    pub fn push(&mut self, block: PriorityBlock) -> bool {
        if self.queue.len() >= self.max_size {
            return false;
        }
        self.queue.push(block);
        true
    }

    /// Pop the highest-priority block (highest difficulty, then earliest arrival).
    pub fn pop(&mut self) -> Option<PriorityBlock> {
        self.queue.pop()
    }

    /// Number of blocks currently queued.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::BlockHash;

    // --- WorkCache tests ---

    #[test]
    fn test_cache_insert_and_get() {
        let mut cache = WorkCache::new(10);
        let hash = BlockHash::new([1u8; 32]);
        cache.insert(&hash, 42);
        assert_eq!(cache.get(&hash), Some(42));
    }

    #[test]
    fn test_cache_remove() {
        let mut cache = WorkCache::new(10);
        let hash = BlockHash::new([2u8; 32]);
        cache.insert(&hash, 99);
        cache.remove(&hash);
        assert_eq!(cache.get(&hash), None);
    }

    #[test]
    fn test_cache_miss() {
        let cache = WorkCache::new(10);
        let hash = BlockHash::new([3u8; 32]);
        assert_eq!(cache.get(&hash), None);
    }

    #[test]
    fn test_cache_eviction_at_capacity() {
        let mut cache = WorkCache::new(2);
        let h1 = BlockHash::new([1u8; 32]);
        let h2 = BlockHash::new([2u8; 32]);
        let h3 = BlockHash::new([3u8; 32]);

        cache.insert(&h1, 1);
        cache.insert(&h2, 2);
        assert_eq!(cache.len(), 2);

        // Inserting a third should evict one to stay at capacity.
        cache.insert(&h3, 3);
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.get(&h3), Some(3));
    }

    #[test]
    fn test_cache_is_empty() {
        let cache = WorkCache::new(5);
        assert!(cache.is_empty());
    }

    // --- WorkPriorityQueue tests ---

    fn block(difficulty: u64, received_at: u64) -> PriorityBlock {
        PriorityBlock {
            block_bytes: vec![0u8; 8],
            difficulty,
            received_at,
        }
    }

    #[test]
    fn test_priority_ordering() {
        let mut q = WorkPriorityQueue::new(10);
        q.push(block(100, 1));
        q.push(block(500, 2));
        q.push(block(200, 3));

        // Highest difficulty first.
        assert_eq!(q.pop().unwrap().difficulty, 500);
        assert_eq!(q.pop().unwrap().difficulty, 200);
        assert_eq!(q.pop().unwrap().difficulty, 100);
    }

    #[test]
    fn test_priority_tiebreak_by_arrival() {
        let mut q = WorkPriorityQueue::new(10);
        q.push(block(100, 10)); // arrived later
        q.push(block(100, 5)); // arrived earlier

        // Same difficulty — earlier arrival wins.
        assert_eq!(q.pop().unwrap().received_at, 5);
        assert_eq!(q.pop().unwrap().received_at, 10);
    }

    #[test]
    fn test_queue_full() {
        let mut q = WorkPriorityQueue::new(2);
        assert!(q.push(block(10, 1)));
        assert!(q.push(block(20, 2)));
        assert!(!q.push(block(30, 3))); // full
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn test_queue_empty() {
        let mut q = WorkPriorityQueue::new(5);
        assert!(q.is_empty());
        assert!(q.pop().is_none());
    }
}
