//! Bounded backlog — limits unconfirmed blocks to prevent DoS.
//!
//! Tracks unconfirmed blocks with priority-based eviction. When the backlog
//! exceeds its capacity, the lowest-priority blocks (by PoW difficulty) that
//! are not in active elections are candidates for proactive rollback.

use burst_types::{BlockHash, WalletAddress};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Maximum unconfirmed blocks before proactive rollback.
const DEFAULT_MAX_BACKLOG: usize = 100_000;

/// Maximum unconfirmed blocks per account (prevents single-account spam).
const MAX_BACKLOG_PER_ACCOUNT: usize = 128;

/// Tracks unconfirmed blocks with priority-based eviction.
pub struct BoundedBacklog {
    /// All unconfirmed blocks: hash → entry metadata.
    entries: HashMap<BlockHash, BacklogEntry>,
    /// Priority index: (priority, hash) for efficient lowest-priority lookup.
    by_priority: BTreeMap<(u64, BlockHash), WalletAddress>,
    /// Per-account unconfirmed block counts.
    per_account: HashMap<WalletAddress, usize>,
    /// Max entries.
    max_size: usize,
    /// Hashes currently in active elections (cannot be rolled back).
    protected: HashSet<BlockHash>,
}

struct BacklogEntry {
    #[allow(dead_code)] // stored for future per-account eviction policies
    account: WalletAddress,
    priority: u64,
    #[allow(dead_code)]
    inserted_at: u64,
}

impl BoundedBacklog {
    pub fn new(max_size: usize) -> Self {
        Self {
            entries: HashMap::new(),
            by_priority: BTreeMap::new(),
            per_account: HashMap::new(),
            max_size,
            protected: HashSet::new(),
        }
    }

    pub fn with_default_size() -> Self {
        Self::new(DEFAULT_MAX_BACKLOG)
    }

    /// Check if an account can accept another unconfirmed block.
    pub fn can_accept(&self, account: &WalletAddress) -> bool {
        if self.entries.len() >= self.max_size {
            return false;
        }
        self.per_account.get(account).copied().unwrap_or(0) < MAX_BACKLOG_PER_ACCOUNT
    }

    /// Number of unconfirmed blocks for a specific account.
    pub fn account_backlog_count(&self, account: &WalletAddress) -> usize {
        self.per_account.get(account).copied().unwrap_or(0)
    }

    /// Add an unconfirmed block to the backlog.
    pub fn insert(
        &mut self,
        hash: BlockHash,
        account: WalletAddress,
        priority: u64,
        now: u64,
    ) {
        if self.entries.contains_key(&hash) {
            return;
        }
        self.entries.insert(
            hash,
            BacklogEntry {
                account: account.clone(),
                priority,
                inserted_at: now,
            },
        );
        self.by_priority.insert((priority, hash), account.clone());
        *self.per_account.entry(account).or_insert(0) += 1;
    }

    /// Remove a block (confirmed or rolled back).
    pub fn remove(&mut self, hash: &BlockHash) {
        if let Some(entry) = self.entries.remove(hash) {
            self.by_priority.remove(&(entry.priority, *hash));
            self.protected.remove(hash);
            if let Some(count) = self.per_account.get_mut(&entry.account) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    self.per_account.remove(&entry.account);
                }
            }
        }
    }

    /// Mark a block as protected (in active election).
    pub fn protect(&mut self, hash: &BlockHash) {
        self.protected.insert(*hash);
    }

    /// Unprotect a block.
    pub fn unprotect(&mut self, hash: &BlockHash) {
        self.protected.remove(hash);
    }

    /// Get lowest-priority blocks that should be rolled back to stay within bounds.
    /// Returns block hashes to evict (lowest priority first), skipping protected ones.
    pub fn blocks_to_evict(&self) -> Vec<BlockHash> {
        if self.entries.len() <= self.max_size {
            return Vec::new();
        }
        let overage = self.entries.len() - self.max_size;
        let mut to_evict = Vec::with_capacity(overage);
        for ((_, hash), _) in self.by_priority.iter() {
            if to_evict.len() >= overage {
                break;
            }
            if !self.protected.contains(hash) {
                to_evict.push(*hash);
            }
        }
        to_evict
    }

    /// Current backlog size.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the backlog is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether backlog is over limit.
    pub fn is_over_limit(&self) -> bool {
        self.entries.len() > self.max_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{BlockHash, WalletAddress};

    fn hash(n: u8) -> BlockHash {
        BlockHash::new([n; 32])
    }

    fn account(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{name}"))
    }

    #[test]
    fn new_backlog_is_empty() {
        let bl = BoundedBacklog::new(100);
        assert_eq!(bl.len(), 0);
        assert!(bl.is_empty());
        assert!(!bl.is_over_limit());
    }

    #[test]
    fn insert_and_remove() {
        let mut bl = BoundedBacklog::new(100);
        bl.insert(hash(1), account("alice"), 10, 1000);
        assert_eq!(bl.len(), 1);
        assert!(!bl.is_empty());

        bl.remove(&hash(1));
        assert_eq!(bl.len(), 0);
        assert!(bl.is_empty());
    }

    #[test]
    fn duplicate_insert_ignored() {
        let mut bl = BoundedBacklog::new(100);
        bl.insert(hash(1), account("alice"), 10, 1000);
        bl.insert(hash(1), account("alice"), 20, 2000); // duplicate
        assert_eq!(bl.len(), 1);
    }

    #[test]
    fn remove_nonexistent_is_noop() {
        let mut bl = BoundedBacklog::new(100);
        bl.remove(&hash(42));
        assert_eq!(bl.len(), 0);
    }

    #[test]
    fn no_eviction_under_limit() {
        let mut bl = BoundedBacklog::new(5);
        for i in 0..5 {
            bl.insert(hash(i), account("a"), i as u64, 1000);
        }
        assert!(!bl.is_over_limit());
        assert!(bl.blocks_to_evict().is_empty());
    }

    #[test]
    fn eviction_when_over_limit() {
        let mut bl = BoundedBacklog::new(3);
        bl.insert(hash(1), account("a"), 100, 1000); // high priority
        bl.insert(hash(2), account("b"), 50, 1001); // medium priority
        bl.insert(hash(3), account("c"), 10, 1002); // low priority
        bl.insert(hash(4), account("d"), 200, 1003); // highest priority

        assert!(bl.is_over_limit());
        let evict = bl.blocks_to_evict();
        assert_eq!(evict.len(), 1); // overage = 4 - 3 = 1

        // Lowest priority (10) should be evicted first
        assert_eq!(evict[0], hash(3));
    }

    #[test]
    fn eviction_skips_protected_blocks() {
        let mut bl = BoundedBacklog::new(2);
        bl.insert(hash(1), account("a"), 10, 1000); // lowest priority
        bl.insert(hash(2), account("b"), 20, 1001);
        bl.insert(hash(3), account("c"), 30, 1002);

        // Protect the lowest-priority block
        bl.protect(&hash(1));

        let evict = bl.blocks_to_evict();
        assert_eq!(evict.len(), 1);
        // hash(1) is protected, so hash(2) (next lowest) is evicted
        assert_eq!(evict[0], hash(2));
    }

    #[test]
    fn protect_and_unprotect() {
        let mut bl = BoundedBacklog::new(2);
        bl.insert(hash(1), account("a"), 10, 1000);
        bl.insert(hash(2), account("b"), 20, 1001);
        bl.insert(hash(3), account("c"), 30, 1002);

        bl.protect(&hash(1));
        let evict = bl.blocks_to_evict();
        assert_eq!(evict[0], hash(2)); // hash(1) protected

        bl.unprotect(&hash(1));
        let evict = bl.blocks_to_evict();
        assert_eq!(evict[0], hash(1)); // hash(1) no longer protected
    }

    #[test]
    fn eviction_returns_correct_overage_count() {
        let mut bl = BoundedBacklog::new(2);
        bl.insert(hash(1), account("a"), 10, 1000);
        bl.insert(hash(2), account("b"), 20, 1001);
        bl.insert(hash(3), account("c"), 30, 1002);
        bl.insert(hash(4), account("d"), 40, 1003);
        bl.insert(hash(5), account("e"), 50, 1004);

        // 5 entries, max 2 → need to evict 3
        let evict = bl.blocks_to_evict();
        assert_eq!(evict.len(), 3);
        // Lowest priorities first: 10, 20, 30
        assert_eq!(evict[0], hash(1));
        assert_eq!(evict[1], hash(2));
        assert_eq!(evict[2], hash(3));
    }

    #[test]
    fn remove_cleans_up_all_indices() {
        let mut bl = BoundedBacklog::new(10);
        bl.insert(hash(1), account("a"), 10, 1000);
        bl.protect(&hash(1));

        bl.remove(&hash(1));

        assert_eq!(bl.len(), 0);
        assert!(bl.is_empty());
        // Priority index and protected set should also be cleaned
        assert!(bl.by_priority.is_empty());
        assert!(bl.protected.is_empty());
    }

    #[test]
    fn with_default_size_uses_constant() {
        let bl = BoundedBacklog::with_default_size();
        assert_eq!(bl.max_size, DEFAULT_MAX_BACKLOG);
    }

    #[test]
    fn large_backlog_eviction_performance() {
        let mut bl = BoundedBacklog::new(100);
        for i in 0u32..200 {
            let mut bytes = [0u8; 32];
            bytes[0..4].copy_from_slice(&i.to_le_bytes());
            bl.insert(BlockHash::new(bytes), account("x"), i as u64, i as u64);
        }

        assert!(bl.is_over_limit());
        let evict = bl.blocks_to_evict();
        assert_eq!(evict.len(), 100);

        // Verify eviction order: lowest priorities first (0, 1, 2, ...)
        for (idx, h) in evict.iter().enumerate() {
            let expected_priority = idx as u32;
            let mut expected_bytes = [0u8; 32];
            expected_bytes[0..4].copy_from_slice(&expected_priority.to_le_bytes());
            assert_eq!(*h, BlockHash::new(expected_bytes));
        }
    }

    #[test]
    fn all_protected_prevents_eviction() {
        let mut bl = BoundedBacklog::new(2);
        bl.insert(hash(1), account("a"), 10, 1000);
        bl.insert(hash(2), account("b"), 20, 1001);
        bl.insert(hash(3), account("c"), 30, 1002);

        // Protect everything
        bl.protect(&hash(1));
        bl.protect(&hash(2));
        bl.protect(&hash(3));

        let evict = bl.blocks_to_evict();
        // Can't evict anything — all protected
        assert!(evict.is_empty());
    }
}
