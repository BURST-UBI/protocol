//! Fork cache — persistent memory of fork blocks.
//! When a fork is detected, both competing blocks are cached so the
//! election can consider all candidates.

use burst_types::BlockHash;
use std::collections::{HashMap, VecDeque};

/// Maximum entries in the fork cache.
const MAX_ENTRIES: usize = 16_384;
/// Maximum forks tracked per root.
const MAX_FORKS_PER_ROOT: usize = 10;

/// Caches fork blocks keyed by root (the `previous` hash they share).
#[derive(Default)]
pub struct ForkCache {
    entries: HashMap<BlockHash, Vec<BlockHash>>,
    total_count: usize,
    /// FIFO order containing only live roots.
    insertion_order: VecDeque<BlockHash>,
}

impl ForkCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Cache a fork block.
    pub fn insert(&mut self, root: BlockHash, fork_hash: BlockHash) {
        if self
            .entries
            .get(&root)
            .is_some_and(|forks| forks.contains(&fork_hash) || forks.len() >= MAX_FORKS_PER_ROOT)
        {
            return;
        }
        while self.total_count >= MAX_ENTRIES {
            self.evict_oldest();
        }

        let is_new = !self.entries.contains_key(&root);
        let entry = self.entries.entry(root).or_default();
        if is_new {
            self.insertion_order.push_back(root);
        }

        if entry.len() >= MAX_FORKS_PER_ROOT {
            return;
        }

        if !entry.contains(&fork_hash) {
            entry.push(fork_hash);
            self.total_count += 1;
        }
    }

    /// Get all fork block hashes for a root.
    pub fn get_forks(&self, root: &BlockHash) -> Option<&[BlockHash]> {
        self.entries.get(root).map(|v| v.as_slice())
    }

    /// Remove a root and its eviction entry together.
    pub fn remove(&mut self, root: &BlockHash) {
        if let Some(forks) = self.entries.remove(root) {
            self.total_count = self.total_count.saturating_sub(forks.len());
            self.insertion_order.retain(|entry| entry != root);
        }
    }

    /// Evict the oldest live root.
    fn evict_oldest(&mut self) {
        while let Some(oldest) = self.insertion_order.pop_front() {
            if self.entries.contains_key(&oldest) {
                self.remove(&oldest);
                return;
            }
        }
    }

    pub fn len(&self) -> usize {
        self.total_count
    }

    pub fn root_count(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.total_count == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(n: u8) -> BlockHash {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        BlockHash::new(bytes)
    }

    #[test]
    fn insert_and_retrieve() {
        let mut cache = ForkCache::new();
        let root = hash(1);
        let fork_a = hash(10);
        let fork_b = hash(11);

        cache.insert(root, fork_a);
        cache.insert(root, fork_b);

        let forks = cache.get_forks(&root).unwrap();
        assert_eq!(forks.len(), 2);
        assert!(forks.contains(&fork_a));
        assert!(forks.contains(&fork_b));
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.root_count(), 1);
    }

    #[test]
    fn duplicate_insert_ignored() {
        let mut cache = ForkCache::new();
        let root = hash(1);
        let fork = hash(10);

        cache.insert(root, fork);
        cache.insert(root, fork);

        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get_forks(&root).unwrap().len(), 1);
    }

    #[test]
    fn max_forks_per_root() {
        let mut cache = ForkCache::new();
        let root = hash(1);

        for i in 0..15 {
            cache.insert(root, hash(100 + i));
        }

        assert_eq!(cache.get_forks(&root).unwrap().len(), 10);
        assert_eq!(cache.len(), 10);
    }

    #[test]
    fn remove_root() {
        let mut cache = ForkCache::new();
        let root = hash(1);
        cache.insert(root, hash(10));
        cache.insert(root, hash(11));

        assert_eq!(cache.len(), 2);
        cache.remove(&root);
        assert_eq!(cache.len(), 0);
        assert!(cache.get_forks(&root).is_none());
        assert!(cache.is_empty());
    }

    #[test]
    fn remove_nonexistent_root_is_noop() {
        let mut cache = ForkCache::new();
        cache.remove(&hash(99));
        assert!(cache.is_empty());
    }

    #[test]
    fn multiple_roots() {
        let mut cache = ForkCache::new();
        let root_a = hash(1);
        let root_b = hash(2);

        cache.insert(root_a, hash(10));
        cache.insert(root_a, hash(11));
        cache.insert(root_b, hash(20));

        assert_eq!(cache.root_count(), 2);
        assert_eq!(cache.len(), 3);
        assert_eq!(cache.get_forks(&root_a).unwrap().len(), 2);
        assert_eq!(cache.get_forks(&root_b).unwrap().len(), 1);
    }

    #[test]
    fn is_empty_reflects_state() {
        let mut cache = ForkCache::new();
        assert!(cache.is_empty());

        let root = hash(1);
        cache.insert(root, hash(10));
        assert!(!cache.is_empty());

        cache.remove(&root);
        assert!(cache.is_empty());
    }

    #[test]
    fn eviction_removes_oldest_root() {
        let mut cache = ForkCache::new();

        let root_old = hash(1);
        cache.insert(root_old, hash(10));

        let root_new = hash(2);
        for i in 0..10 {
            cache.insert(root_new, hash(100 + i));
        }

        cache.evict_oldest();
        assert!(cache.get_forks(&root_old).is_none());
        assert!(cache.get_forks(&root_new).is_some());
    }

    #[test]
    fn get_forks_returns_none_for_unknown_root() {
        let cache = ForkCache::new();
        assert!(cache.get_forks(&hash(42)).is_none());
    }

    #[test]
    fn eviction_after_removal_tracks_only_live_roots() {
        let mut cache = ForkCache::new();
        let root_a = hash(1);
        let root_b = hash(2);
        let root_c = hash(3);

        cache.insert(root_a, hash(10));
        cache.insert(root_b, hash(20));
        cache.insert(root_c, hash(30));

        cache.remove(&root_a);
        cache.remove(&root_b);
        assert_eq!(cache.insertion_order.len(), 1);

        cache.evict_oldest();
        assert!(cache.get_forks(&root_c).is_none());
        assert!(cache.is_empty());
    }

    #[test]
    fn repeated_removal_does_not_accumulate_eviction_metadata() {
        let mut cache = ForkCache::new();
        for _ in 0..MAX_ENTRIES * 2 {
            cache.insert(hash(1), hash(10));
            cache.remove(&hash(1));
            assert!(cache.insertion_order.is_empty());
            assert!(cache.entries.is_empty());
            assert_eq!(cache.total_count, 0);
        }
        cache.insert(hash(2), hash(20));
        cache.insert(hash(1), hash(10));
        cache.evict_oldest();
        assert!(cache.get_forks(&hash(2)).is_none());
        assert_eq!(cache.get_forks(&hash(1)), Some([hash(10)].as_slice()));
    }

    #[test]
    fn duplicate_at_capacity_does_not_evict_an_unrelated_root() {
        let mut cache = ForkCache::new();
        for index in 0..MAX_ENTRIES {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&(index as u64).to_le_bytes());
            let root = BlockHash::new(bytes);
            cache.insert(root, root);
        }
        cache.insert(BlockHash::ZERO, BlockHash::ZERO);
        assert_eq!(cache.len(), MAX_ENTRIES);
        assert_eq!(cache.root_count(), MAX_ENTRIES);
        assert_eq!(cache.insertion_order.len(), MAX_ENTRIES);
        assert_eq!(
            cache.get_forks(&BlockHash::ZERO),
            Some([BlockHash::ZERO].as_slice())
        );
    }
}
