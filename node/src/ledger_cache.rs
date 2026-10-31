//! Cached ledger counters for RPC queries.
//!
//! Relaxed atomic reads are independent: the three values do not form one ledger
//! snapshot. The node seeds and updates them; they are not recovery authority.

use std::sync::atomic::{AtomicU64, Ordering};

/// Caller-maintained counters initialized from storage. Block count tracks
/// retained bodies and decreases on retirement. Arithmetic is unchecked; callers
/// must prevent underflow and serialize replacement with additive updates.
pub struct LedgerCache {
    block_count: AtomicU64,
    account_count: AtomicU64,
    pending_count: AtomicU64,
}

impl LedgerCache {
    pub(crate) fn reset(&self, counts: (u64, u64, u64)) {
        self.block_count.store(counts.0, Ordering::Relaxed);
        self.account_count.store(counts.1, Ordering::Relaxed);
        self.pending_count.store(counts.2, Ordering::Relaxed);
    }
    /// Create a new cache seeded with the given initial values.
    pub fn new(block_count: u64, account_count: u64, pending_count: u64) -> Self {
        Self {
            block_count: AtomicU64::new(block_count),
            account_count: AtomicU64::new(account_count),
            pending_count: AtomicU64::new(pending_count),
        }
    }

    /// Cached retained-block count.
    pub fn block_count(&self) -> u64 {
        self.block_count.load(Ordering::Relaxed)
    }

    /// Cached account count.
    pub fn account_count(&self) -> u64 {
        self.account_count.load(Ordering::Relaxed)
    }

    /// Cached pending-entry count.
    pub fn pending_count(&self) -> u64 {
        self.pending_count.load(Ordering::Relaxed)
    }

    /// Increment block count by 1 (called after a block is persisted).
    pub fn inc_block_count(&self) {
        self.block_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement block count by 1 (called on rollback).
    pub fn dec_block_count(&self) {
        self.block_count.fetch_sub(1, Ordering::Relaxed);
    }

    /// Publish a committed physical-body retirement without overwriting unrelated
    /// account/pending counts or concurrent additive block publications. The
    /// coordinator must serialize this with full cache replacement.
    pub(crate) fn retire_blocks(&self, count: u64) {
        self.block_count.fetch_sub(count, Ordering::Relaxed);
    }

    /// Increment account count by 1 (new account opened).
    pub fn inc_account_count(&self) {
        self.account_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Increment pending count by 1 (send created a pending entry).
    pub fn inc_pending_count(&self) {
        self.pending_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement pending count by 1 (receive consumed a pending entry).
    pub fn dec_pending_count(&self) {
        self.pending_count.fetch_sub(1, Ordering::Relaxed);
    }
}

impl burst_rpc::LedgerCacheView for LedgerCache {
    fn block_count(&self) -> u64 {
        self.block_count()
    }

    fn account_count(&self) -> u64 {
        self.account_count()
    }

    fn pending_count(&self) -> u64 {
        self.pending_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_values() {
        let cache = LedgerCache::new(10, 5, 3);
        assert_eq!(cache.block_count(), 10);
        assert_eq!(cache.account_count(), 5);
        assert_eq!(cache.pending_count(), 3);
    }

    #[test]
    fn increment_decrement() {
        let cache = LedgerCache::new(0, 0, 0);
        cache.inc_block_count();
        cache.inc_block_count();
        assert_eq!(cache.block_count(), 2);
        cache.dec_block_count();
        assert_eq!(cache.block_count(), 1);

        cache.inc_account_count();
        assert_eq!(cache.account_count(), 1);

        cache.inc_pending_count();
        cache.inc_pending_count();
        cache.dec_pending_count();
        assert_eq!(cache.pending_count(), 1);
    }
}
