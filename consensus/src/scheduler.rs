//! Election schedulers — decide which blocks should have elections started.
//!
//! Two scheduler strategies:
//!
//! - **Hinted**: Starts elections for blocks that have already accumulated
//!   significant vote weight in the vote cache (i.e. representatives voted
//!   before the node saw the fork).
//!
//! - **Priority**: Schedules elections based on account importance (balance),
//!   ensuring that high-value accounts get elections resolved first during
//!   contention.

use burst_types::{BlockHash, WalletAddress};

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

/// Starts elections for blocks that already have significant vote weight
/// in the cache.
///
/// This is useful when votes arrive before the node has detected the fork.
/// Instead of waiting for the block processor to see the fork, the hinted
/// scheduler proactively starts elections based on vote-cache evidence.
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
    /// Returns roots (block hashes) that should have elections started.
    pub fn check(&self, cache: &VoteCache) -> Vec<BlockHash> {
        let top = cache.top(self.max_per_cycle);
        top.into_iter()
            .filter(|(_, weight)| *weight >= self.min_weight_threshold)
            .map(|(root, _)| root)
            .collect()
    }
}

// ── Priority scheduler ──────────────────────────────────────────────────

/// Schedules elections based on account importance (balance).
///
/// Higher-balance accounts get their elections resolved first. This is a
/// simple max-heap (sorted Vec) of pending election requests.
pub struct PriorityScheduler {
    /// Pending accounts needing election, maintained in sorted order.
    queue: Vec<PriorityEntry>,
    /// Maximum queue size.
    max_queue: usize,
}

struct PriorityEntry {
    root: BlockHash,
    account: WalletAddress,
    priority: u64,
}

impl PriorityScheduler {
    pub fn new(max_queue: usize) -> Self {
        Self {
            queue: Vec::new(),
            max_queue,
        }
    }

    /// Add a block that needs an election, with its account balance as priority.
    ///
    /// If the queue is full, the entry is dropped (lower priority than
    /// everything already queued). Duplicate roots are silently ignored.
    pub fn push(&mut self, root: BlockHash, account: WalletAddress, balance: u64) {
        // Deduplicate by root
        if self.queue.iter().any(|e| e.root == root) {
            return;
        }

        if self.queue.len() >= self.max_queue {
            // Only insert if higher priority than the lowest entry
            if let Some(min) = self.queue.last() {
                if balance <= min.priority {
                    return;
                }
                self.queue.pop();
            }
        }

        let entry = PriorityEntry {
            root,
            account,
            priority: balance,
        };
        // Insert in sorted order (highest priority first)
        let pos = self
            .queue
            .binary_search_by(|e| balance.cmp(&e.priority))
            .unwrap_or_else(|pos| pos);
        self.queue.insert(pos, entry);
    }

    /// Pop the highest-priority entry.
    pub fn pop(&mut self) -> Option<(BlockHash, WalletAddress)> {
        if self.queue.is_empty() {
            None
        } else {
            let entry = self.queue.remove(0);
            Some((entry.root, entry.account))
        }
    }

    /// Current queue length.
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

    fn make_hash(byte: u8) -> BlockHash {
        BlockHash::new([byte; 32])
    }

    fn make_addr(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{name}"))
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
}
