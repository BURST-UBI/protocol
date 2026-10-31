//! Candidate queues for election scheduling.
//!
//! The node uses `PriorityScheduler` with each block's recorded TRST balance.
//! Queue priority does not confer consensus voting weight. `HintedScheduler` is
//! a standalone helper that ranks cached votes; the node does not currently use it.

use burst_types::{BlockHash, WalletAddress};
use std::collections::BTreeMap;

use crate::vote_cache::VoteCache;

/// Election scheduling behaviors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ElectionBehavior {
    /// Based on account balance.
    Priority,
    /// Based on accumulated vote weight in cache.
    Hinted,
    /// RPC-triggered.
    Manual,
}

// ── Hinted scheduler ────────────────────────────────────────────────────

/// Select candidate hashes whose cached vote weight reaches a threshold.
/// This helper neither validates candidates nor starts elections.
pub struct HintedScheduler {
    /// Minimum accumulated weight to trigger an election.
    min_weight_threshold: u128,
    /// Maximum elections to start per check cycle.
    max_per_cycle: usize,
}

impl HintedScheduler {
    pub fn new(min_weight_threshold: u128, max_per_cycle: usize) -> Self {
        Self {
            min_weight_threshold,
            max_per_cycle,
        }
    }

    /// Find blocks in the vote cache with enough accumulated weight
    /// to warrant starting an election proactively.
    ///
    /// Returns candidate block hashes, not account-position election roots.
    /// The caller handles cache expiry and validates candidates before registration.
    pub fn check(&self, cache: &VoteCache) -> Vec<BlockHash> {
        let top = cache.top(self.max_per_cycle);
        top.into_iter()
            .filter(|(_, weight)| *weight >= self.min_weight_threshold)
            .map(|(root, _)| root)
            .collect()
    }
}

// ── Priority scheduler ──────────────────────────────────────────────────

/// Orders candidates by descending supplied balance, then descending hash.
/// Both indexes stay bounded by `max_queue`; individual mutations are O(log n).
pub struct PriorityScheduler {
    queue: BTreeMap<(u128, BlockHash), WalletAddress>,
    priorities: BTreeMap<BlockHash, u128>,
    max_queue: usize,
}

impl PriorityScheduler {
    pub fn clear(&mut self) {
        self.queue.clear();
        self.priorities.clear();
    }

    pub fn remove(&mut self, hash: &BlockHash) {
        if let Some(priority) = self.priorities.remove(hash) {
            self.queue.remove(&(priority, *hash));
        }
    }

    pub fn new(max_queue: usize) -> Self {
        Self {
            queue: BTreeMap::new(),
            priorities: BTreeMap::new(),
            max_queue,
        }
    }

    /// Duplicate candidate hashes retain their original account and priority. At capacity,
    /// only a strictly higher balance can replace the lowest-priority entry.
    pub fn push(&mut self, root: BlockHash, account: WalletAddress, balance: u128) {
        if self.max_queue == 0 || self.priorities.contains_key(&root) {
            return;
        }
        if self.queue.len() == self.max_queue {
            let (&(lowest, hash), _) = self.queue.first_key_value().expect("full queue");
            if balance <= lowest {
                return;
            }
            self.remove(&hash);
        }
        self.queue.insert((balance, root), account);
        self.priorities.insert(root, balance);
    }

    pub fn pop(&mut self) -> Option<(BlockHash, WalletAddress)> {
        let ((_, root), account) = self.queue.pop_last()?;
        self.priorities.remove(&root);
        Some((root, account))
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hash(byte: u8) -> BlockHash {
        BlockHash::new([byte; 32])
    }

    fn make_addr(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{name}"))
    }

    #[test]
    fn full_width_balances_preserve_order_and_capacity_replacement() {
        let mut scheduler = PriorityScheduler::new(2);
        let small = u64::MAX as u128 + 1;
        scheduler.push(make_hash(3), make_addr("small"), small);
        scheduler.push(make_hash(2), make_addr("large"), small * 50);
        scheduler.push(make_hash(1), make_addr("maximum"), u128::MAX);
        // A duplicate cannot change its existing priority.
        scheduler.push(make_hash(2), make_addr("duplicate"), u128::MAX);
        assert_eq!(scheduler.pop(), Some((make_hash(1), make_addr("maximum"))));
        assert_eq!(scheduler.pop(), Some((make_hash(2), make_addr("large"))));
        assert!(scheduler.is_empty());
    }

    // ── HintedScheduler tests ───────────────────────────────────────────

    #[test]
    fn hinted_returns_roots_above_threshold() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_addr("alice"), 500, 1000, false);
        cache.insert(make_hash(1), make_addr("bob"), 300, 1001, false);
        cache.insert(make_hash(2), make_addr("carol"), 100, 1002, false);

        let scheduler = HintedScheduler::new(700, 10);
        let roots = scheduler.check(&cache);

        // Root 1 has 800 weight (>= 700), root 2 has 100 (< 700)
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0], make_hash(1));
    }

    #[test]
    fn hinted_respects_max_per_cycle() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_addr("a"), 1000, 100, false);
        cache.insert(make_hash(2), make_addr("b"), 900, 101, false);
        cache.insert(make_hash(3), make_addr("c"), 800, 102, false);

        let scheduler = HintedScheduler::new(100, 2);
        let roots = scheduler.check(&cache);

        // Should return at most 2 even though 3 qualify
        assert!(roots.len() <= 2);
    }

    #[test]
    fn hinted_empty_cache_returns_empty() {
        let cache = VoteCache::new();
        let scheduler = HintedScheduler::new(100, 10);
        let roots = scheduler.check(&cache);
        assert!(roots.is_empty());
    }

    #[test]
    fn hinted_no_roots_above_threshold() {
        let mut cache = VoteCache::new();
        cache.insert(make_hash(1), make_addr("alice"), 50, 1000, false);

        let scheduler = HintedScheduler::new(100, 10);
        let roots = scheduler.check(&cache);
        assert!(roots.is_empty());
    }

    // ── PriorityScheduler tests ─────────────────────────────────────────

    #[test]
    fn priority_push_and_pop() {
        let mut sched = PriorityScheduler::new(10);
        sched.push(make_hash(1), make_addr("alice"), 100);
        sched.push(make_hash(2), make_addr("bob"), 500);
        sched.push(make_hash(3), make_addr("carol"), 200);

        assert_eq!(sched.len(), 3);

        // Highest priority (500) should come first
        let (root, account) = sched.pop().unwrap();
        assert_eq!(root, make_hash(2));
        assert_eq!(account, make_addr("bob"));

        // Next: 200
        let (root, _) = sched.pop().unwrap();
        assert_eq!(root, make_hash(3));

        // Last: 100
        let (root, _) = sched.pop().unwrap();
        assert_eq!(root, make_hash(1));

        assert!(sched.pop().is_none());
    }

    #[test]
    fn priority_duplicate_root_ignored() {
        let mut sched = PriorityScheduler::new(10);
        sched.push(make_hash(1), make_addr("alice"), 100);
        sched.push(make_hash(1), make_addr("bob"), 500);

        assert_eq!(sched.len(), 1);
        let (_, account) = sched.pop().unwrap();
        // Original entry should remain
        assert_eq!(account, make_addr("alice"));
    }

    #[test]
    fn priority_respects_max_queue() {
        let mut sched = PriorityScheduler::new(2);
        sched.push(make_hash(1), make_addr("alice"), 100);
        sched.push(make_hash(2), make_addr("bob"), 200);

        // Queue is full. This has higher priority than the lowest (100),
        // so it should replace it.
        sched.push(make_hash(3), make_addr("carol"), 300);

        assert_eq!(sched.len(), 2);

        let (root1, _) = sched.pop().unwrap();
        let (root2, _) = sched.pop().unwrap();

        // Should have carol (300) and bob (200), alice (100) was evicted
        assert_eq!(root1, make_hash(3));
        assert_eq!(root2, make_hash(2));
    }

    #[test]
    fn priority_low_entry_dropped_when_full() {
        let mut sched = PriorityScheduler::new(2);
        sched.push(make_hash(1), make_addr("alice"), 200);
        sched.push(make_hash(2), make_addr("bob"), 300);

        // This has lower priority than everything in the queue
        sched.push(make_hash(3), make_addr("carol"), 100);

        assert_eq!(sched.len(), 2);
        // Carol should NOT be in the queue
        let (r1, _) = sched.pop().unwrap();
        let (r2, _) = sched.pop().unwrap();
        assert_eq!(r1, make_hash(2));
        assert_eq!(r2, make_hash(1));
    }

    #[test]
    fn priority_empty_queue() {
        let sched = PriorityScheduler::new(10);
        assert!(sched.is_empty());
        assert_eq!(sched.len(), 0);
    }

    #[test]
    fn priority_pop_empty_returns_none() {
        let mut sched = PriorityScheduler::new(10);
        assert!(sched.pop().is_none());
    }
    #[test]
    fn priority_indexes_survive_eviction_removal_clear_and_reinsertion() {
        let mut disabled = PriorityScheduler::new(0);
        disabled.push(make_hash(1), make_addr("a"), 100);
        assert!(disabled.is_empty());
        let mut sched = PriorityScheduler::new(2);
        sched.push(make_hash(1), make_addr("a"), 100);
        sched.push(make_hash(2), make_addr("b"), 100);
        sched.push(make_hash(3), make_addr("c"), 100);
        assert!(!sched.priorities.contains_key(&make_hash(3)));
        sched.push(make_hash(3), make_addr("c"), 200);
        assert_eq!(sched.queue.len(), sched.priorities.len());
        sched.remove(&make_hash(3));
        sched.push(make_hash(3), make_addr("new"), 300);
        assert_eq!(sched.pop(), Some((make_hash(3), make_addr("new"))));
        sched.clear();
        assert!(sched.priorities.is_empty());
        sched.push(make_hash(1), make_addr("again"), 1);
        assert_eq!(sched.pop(), Some((make_hash(1), make_addr("again"))));
        assert!(sched.priorities.is_empty());
    }
}
