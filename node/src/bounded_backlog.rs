//! Optional in-memory index of unconfirmed blocks.
//!
//! Callers check capacity before insertion. Overflow selection returns hashes;
//! it neither removes entries nor rolls back ledger state. The node can recover
//! untracked work through its durable scan when this index is full.

use burst_types::{BlockHash, WalletAddress};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Default capacity used by admission checks.
const DEFAULT_MAX_BACKLOG: usize = 65_536;

/// Per-account tracking limit used by admission checks.
const MAX_BACKLOG_PER_ACCOUNT: usize = 128;

/// Tracks unconfirmed blocks with priority-based eviction.
pub struct BoundedBacklog {
    /// Tracked unconfirmed blocks: hash → entry metadata.
    entries: HashMap<BlockHash, BacklogEntry>,
    /// Priority index: (priority, hash) for efficient lowest-priority lookup.
    by_priority: BTreeMap<(u64, BlockHash), WalletAddress>,
    /// Per-account unconfirmed block counts.
    per_account: HashMap<WalletAddress, usize>,
    /// Max entries.
    max_size: usize,
    /// Caller-protected hashes excluded from overflow selection.
    protected: HashSet<BlockHash>,
}

struct BacklogEntry {
    account: WalletAddress,
    priority: u64,
    #[allow(dead_code)]
    inserted_at: u64,
}

impl BoundedBacklog {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.by_priority.clear();
        self.per_account.clear();
        self.protected.clear();
    }
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

    /// Insert without enforcing capacity; duplicate hashes leave metadata unchanged.
    /// Priority is an opaque caller value. The node currently supplies the work
    /// nonce, not its computed PoW difficulty.
    pub fn insert(&mut self, hash: BlockHash, account: WalletAddress, priority: u64, now: u64) {
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

    /// Select up to the global overflow count in ascending (priority, hash) order.
    /// May scan the whole index and return fewer hashes when entries are protected.
    /// Does not mutate the backlog or enforce per-account limits.
    pub fn blocks_to_evict(&self) -> Vec<BlockHash> {
        if self.entries.len() <= self.max_size {
            return Vec::new();
        }
        let overage = self.entries.len() - self.max_size;
        let mut to_evict = Vec::with_capacity(overage);
        for (_, hash) in self.by_priority.keys() {
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
        let backlog = BoundedBacklog::new(100);
        assert_eq!(backlog.len(), 0);
        assert!(backlog.is_empty());
        assert!(!backlog.is_over_limit());
    }

    #[test]
    fn insert_and_remove() {
        let mut backlog = BoundedBacklog::new(100);
        backlog.insert(hash(1), account("alice"), 10, 1000);
        assert_eq!(backlog.len(), 1);
        assert!(!backlog.is_empty());

        backlog.remove(&hash(1));
        assert_eq!(backlog.len(), 0);
        assert!(backlog.is_empty());
    }

    #[test]
    fn duplicate_insert_ignored() {
        let mut backlog = BoundedBacklog::new(100);
        backlog.insert(hash(1), account("alice"), 10, 1000);
        backlog.insert(hash(1), account("alice"), 20, 2000); // duplicate
        assert_eq!(backlog.len(), 1);
    }

    #[test]
    fn remove_nonexistent_is_noop() {
        let mut backlog = BoundedBacklog::new(100);
        backlog.remove(&hash(42));
        assert_eq!(backlog.len(), 0);
    }

    #[test]
    fn no_eviction_under_limit() {
        let mut backlog = BoundedBacklog::new(5);
        for i in 0..5 {
            backlog.insert(hash(i), account("a"), i as u64, 1000);
        }
        assert!(!backlog.is_over_limit());
        assert!(backlog.blocks_to_evict().is_empty());
    }

    #[test]
    fn eviction_when_over_limit() {
        let mut backlog = BoundedBacklog::new(3);
        backlog.insert(hash(1), account("a"), 100, 1000); // high priority
        backlog.insert(hash(2), account("b"), 50, 1001); // medium priority
        backlog.insert(hash(3), account("c"), 10, 1002); // low priority
        backlog.insert(hash(4), account("d"), 200, 1003); // highest priority

        assert!(backlog.is_over_limit());
        let evict = backlog.blocks_to_evict();
        assert_eq!(evict.len(), 1); // overage = 4 - 3 = 1

        // Lowest priority (10) should be evicted first
        assert_eq!(evict[0], hash(3));
    }

    #[test]
    fn eviction_skips_protected_blocks() {
        let mut backlog = BoundedBacklog::new(2);
        backlog.insert(hash(1), account("a"), 10, 1000); // lowest priority
        backlog.insert(hash(2), account("b"), 20, 1001);
        backlog.insert(hash(3), account("c"), 30, 1002);

        // Protect the lowest-priority block
        backlog.protect(&hash(1));

        let evict = backlog.blocks_to_evict();
        assert_eq!(evict.len(), 1);
        // hash(1) is protected, so hash(2) (next lowest) is evicted
        assert_eq!(evict[0], hash(2));
    }

    #[test]
    fn protect_and_unprotect() {
        let mut backlog = BoundedBacklog::new(2);
        backlog.insert(hash(1), account("a"), 10, 1000);
        backlog.insert(hash(2), account("b"), 20, 1001);
        backlog.insert(hash(3), account("c"), 30, 1002);

        backlog.protect(&hash(1));
        let evict = backlog.blocks_to_evict();
        assert_eq!(evict[0], hash(2)); // hash(1) protected

        backlog.unprotect(&hash(1));
        let evict = backlog.blocks_to_evict();
        assert_eq!(evict[0], hash(1)); // hash(1) no longer protected
    }

    #[test]
    fn eviction_returns_correct_overage_count() {
        let mut backlog = BoundedBacklog::new(2);
        backlog.insert(hash(1), account("a"), 10, 1000);
        backlog.insert(hash(2), account("b"), 20, 1001);
        backlog.insert(hash(3), account("c"), 30, 1002);
        backlog.insert(hash(4), account("d"), 40, 1003);
        backlog.insert(hash(5), account("e"), 50, 1004);

        // 5 entries, max 2 → need to evict 3
        let evict = backlog.blocks_to_evict();
        assert_eq!(evict.len(), 3);
        // Lowest priorities first: 10, 20, 30
        assert_eq!(evict[0], hash(1));
        assert_eq!(evict[1], hash(2));
        assert_eq!(evict[2], hash(3));
    }

    #[test]
    fn remove_cleans_up_all_indices() {
        let mut backlog = BoundedBacklog::new(10);
        backlog.insert(hash(1), account("a"), 10, 1000);
        backlog.protect(&hash(1));

        backlog.remove(&hash(1));

        assert_eq!(backlog.len(), 0);
        assert!(backlog.is_empty());
        // Priority index and protected set should also be cleaned
        assert!(backlog.by_priority.is_empty());
        assert!(backlog.protected.is_empty());
    }

    #[test]
    fn with_default_size_uses_constant() {
        let backlog = BoundedBacklog::with_default_size();
        assert_eq!(backlog.max_size, DEFAULT_MAX_BACKLOG);
    }

    #[test]
    fn eviction_orders_overflow_by_priority() {
        let mut backlog = BoundedBacklog::new(100);
        for i in 0u32..200 {
            let mut bytes = [0u8; 32];
            bytes[0..4].copy_from_slice(&i.to_le_bytes());
            backlog.insert(BlockHash::new(bytes), account("x"), i as u64, i as u64);
        }

        assert!(backlog.is_over_limit());
        let evict = backlog.blocks_to_evict();
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
        let mut backlog = BoundedBacklog::new(2);
        backlog.insert(hash(1), account("a"), 10, 1000);
        backlog.insert(hash(2), account("b"), 20, 1001);
        backlog.insert(hash(3), account("c"), 30, 1002);

        // Protect everything
        backlog.protect(&hash(1));
        backlog.protect(&hash(2));
        backlog.protect(&hash(3));

        let evict = backlog.blocks_to_evict();
        // Can't evict anything — all protected
        assert!(evict.is_empty());
    }
}
