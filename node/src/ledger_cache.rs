//! In-memory atomic counters for frequently-queried ledger statistics.
//!
//! Avoids repeated LMDB reads for values like block count, account count, and
//! pending count that are requested on every `node_info` RPC call.

use std::sync::atomic::{AtomicU64, Ordering};

/// Atomic cache of ledger counters. Initialized from LMDB at node startup
/// and kept in sync by incrementing/decrementing during block processing.
pub struct LedgerCache {
    block_count: AtomicU64,
    account_count: AtomicU64,
    pending_count: AtomicU64,
}

impl LedgerCache {
    /// Create a new cache seeded with the given initial values.
    pub fn new(block_count: u64, account_count: u64, pending_count: u64) -> Self {
        Self {
            block_count: AtomicU64::new(block_count),
            account_count: AtomicU64::new(account_count),
            pending_count: AtomicU64::new(pending_count),
        }
    }

    /// Current block count.
    pub fn block_count(&self) -> u64 {
        self.block_count.load(Ordering::Relaxed)
    }

    /// Current account count.
    pub fn account_count(&self) -> u64 {
        self.account_count.load(Ordering::Relaxed)
    }

    /// Current pending count.
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
