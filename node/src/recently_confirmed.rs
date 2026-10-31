//! Recently-confirmed block cache — prevents re-elections for blocks
//! that have already been confirmed by consensus.
//!
//! This is a bounded FIFO set: when full, the oldest entry is evicted to
//! make room for a new insertion. Hash-set lookups take expected O(1) time.

use burst_types::BlockHash;
use std::collections::{HashSet, VecDeque};

/// FIFO cache used to skip election work for recently cemented blocks.
/// Entries are supplied by the caller; the cache itself does not verify finality.
pub struct RecentlyConfirmed {
    set: HashSet<BlockHash>,
    order: VecDeque<BlockHash>,
    capacity: usize,
}

impl RecentlyConfirmed {
    pub fn new(capacity: usize) -> Self {
        Self {
            set: HashSet::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Insert, evicting the oldest entry if full. Duplicates do not refresh age;
    /// zero capacity disables the cache.
    pub fn insert(&mut self, hash: BlockHash) {
        if self.capacity == 0 {
            return;
        }
        if self.set.contains(&hash) {
            return;
        }
        if self.order.len() >= self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.set.remove(&evicted);
            }
        }
        self.set.insert(hash);
        self.order.push_back(hash);
    }

    /// Check whether a hash is in the recently-confirmed set.
    pub fn contains(&self, hash: &BlockHash) -> bool {
        self.set.contains(hash)
    }

    /// Number of entries in the cache.
    pub fn len(&self) -> usize {
        self.set.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hash(byte: u8) -> BlockHash {
        BlockHash::new([byte; 32])
    }

    #[test]
    fn insert_and_contains() {
        let mut rc = RecentlyConfirmed::new(10);
        let h = make_hash(1);
        assert!(!rc.contains(&h));
        rc.insert(h);
        assert!(rc.contains(&h));
        assert_eq!(rc.len(), 1);
    }

    #[test]
    fn duplicate_insert_is_noop() {
        let mut rc = RecentlyConfirmed::new(10);
        let h = make_hash(1);
        rc.insert(h);
        rc.insert(h);
        assert_eq!(rc.len(), 1);
    }

    #[test]
    fn eviction_at_capacity() {
        let mut rc = RecentlyConfirmed::new(3);
        rc.insert(make_hash(1));
        rc.insert(make_hash(2));
        rc.insert(make_hash(3));
        assert_eq!(rc.len(), 3);

        // Fourth insert should evict hash(1)
        rc.insert(make_hash(4));
        assert_eq!(rc.len(), 3);
        assert!(!rc.contains(&make_hash(1)));
        assert!(rc.contains(&make_hash(2)));
        assert!(rc.contains(&make_hash(3)));
        assert!(rc.contains(&make_hash(4)));
    }

    #[test]
    fn fifo_eviction_order() {
        let mut rc = RecentlyConfirmed::new(2);
        rc.insert(make_hash(1));
        rc.insert(make_hash(2));
        rc.insert(make_hash(3)); // evicts 1
        rc.insert(make_hash(4)); // evicts 2

        assert!(!rc.contains(&make_hash(1)));
        assert!(!rc.contains(&make_hash(2)));
        assert!(rc.contains(&make_hash(3)));
        assert!(rc.contains(&make_hash(4)));
    }

    #[test]
    fn empty_cache() {
        let rc = RecentlyConfirmed::new(10);
        assert!(rc.is_empty());
        assert_eq!(rc.len(), 0);
        assert!(!rc.contains(&make_hash(1)));
    }

    #[test]
    fn zero_capacity() {
        let mut rc = RecentlyConfirmed::new(0);
        rc.insert(make_hash(1));
        assert!(!rc.contains(&make_hash(1)));
        assert_eq!(rc.len(), 0);
    }
}
