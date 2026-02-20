//! Vote rebroadcaster — relays important votes from other representatives.
//! Ensures votes reach all nodes even without direct connections.

use std::collections::HashSet;

/// Maximum recent vote hashes tracked (dedup).
const MAX_RECENT: usize = 65_536;

pub struct VoteRebroadcaster {
    /// Recently seen vote hashes (for dedup).
    recent: HashSet<[u8; 32]>,
    /// FIFO order for eviction.
    recent_order: Vec<[u8; 32]>,
    /// Minimum weight for rebroadcast (only rebroadcast principal rep votes).
    min_weight: u128,
    /// Maximum recent entries.
    max_recent: usize,
}

impl VoteRebroadcaster {
    pub fn new(min_weight: u128) -> Self {
        Self {
            recent: HashSet::new(),
            recent_order: Vec::new(),
            min_weight,
            max_recent: MAX_RECENT,
        }
    }

    /// Check if a vote should be rebroadcast.
    /// Returns true if the vote is from a sufficiently weighted rep
    /// and hasn't been seen recently.
    pub fn should_rebroadcast(&mut self, vote_hash: &[u8; 32], voter_weight: u128) -> bool {
        if voter_weight < self.min_weight {
            return false;
        }
        if self.recent.contains(vote_hash) {
            return false;
        }
        // Add to recent
        if self.recent.len() >= self.max_recent {
            if let Some(oldest) = self.recent_order.first().copied() {
                self.recent.remove(&oldest);
                self.recent_order.remove(0);
            }
        }
        self.recent.insert(*vote_hash);
        self.recent_order.push(*vote_hash);
        true
    }

    /// Number of recently seen votes.
    pub fn recent_count(&self) -> usize {
        self.recent.len()
    }

    /// Clear all state.
    pub fn clear(&mut self) {
        self.recent.clear();
        self.recent_order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vote_hash(n: u8) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[0] = n;
        bytes
    }

    #[test]
    fn rebroadcast_high_weight_vote() {
        let mut rb = VoteRebroadcaster::new(100);
        assert!(rb.should_rebroadcast(&vote_hash(1), 200));
        assert_eq!(rb.recent_count(), 1);
    }

    #[test]
    fn reject_low_weight_vote() {
        let mut rb = VoteRebroadcaster::new(100);
        assert!(!rb.should_rebroadcast(&vote_hash(1), 50));
        assert_eq!(rb.recent_count(), 0);
    }

    #[test]
    fn reject_exact_threshold() {
        let mut rb = VoteRebroadcaster::new(100);
        // Exact threshold should pass (not strictly less)
        assert!(rb.should_rebroadcast(&vote_hash(1), 100));
    }

    #[test]
    fn dedup_prevents_double_rebroadcast() {
        let mut rb = VoteRebroadcaster::new(100);
        let vh = vote_hash(1);
        assert!(rb.should_rebroadcast(&vh, 200));
        assert!(!rb.should_rebroadcast(&vh, 200));
    }

    #[test]
    fn different_votes_both_accepted() {
        let mut rb = VoteRebroadcaster::new(100);
        assert!(rb.should_rebroadcast(&vote_hash(1), 200));
        assert!(rb.should_rebroadcast(&vote_hash(2), 200));
        assert_eq!(rb.recent_count(), 2);
    }

    #[test]
    fn clear_resets_state() {
        let mut rb = VoteRebroadcaster::new(100);
        rb.should_rebroadcast(&vote_hash(1), 200);
        rb.should_rebroadcast(&vote_hash(2), 200);
        assert_eq!(rb.recent_count(), 2);

        rb.clear();
        assert_eq!(rb.recent_count(), 0);
        // Can rebroadcast previously seen votes after clear
        assert!(rb.should_rebroadcast(&vote_hash(1), 200));
    }

    #[test]
    fn eviction_when_at_capacity() {
        // Use a small capacity for testing
        let mut rb = VoteRebroadcaster {
            recent: HashSet::new(),
            recent_order: Vec::new(),
            min_weight: 0,
            max_recent: 3,
        };

        rb.should_rebroadcast(&vote_hash(1), 100);
        rb.should_rebroadcast(&vote_hash(2), 100);
        rb.should_rebroadcast(&vote_hash(3), 100);
        assert_eq!(rb.recent_count(), 3);

        // This should evict vote_hash(1)
        rb.should_rebroadcast(&vote_hash(4), 100);
        assert_eq!(rb.recent_count(), 3);

        // vote_hash(1) was evicted, so it should be accepted again
        assert!(rb.should_rebroadcast(&vote_hash(1), 100));
    }

    #[test]
    fn zero_weight_threshold() {
        let mut rb = VoteRebroadcaster::new(0);
        assert!(rb.should_rebroadcast(&vote_hash(1), 0));
        assert!(rb.should_rebroadcast(&vote_hash(2), 1));
    }

    #[test]
    fn high_weight_threshold() {
        let mut rb = VoteRebroadcaster::new(u128::MAX);
        assert!(!rb.should_rebroadcast(&vote_hash(1), u128::MAX - 1));
        assert!(rb.should_rebroadcast(&vote_hash(1), u128::MAX));
    }
}
