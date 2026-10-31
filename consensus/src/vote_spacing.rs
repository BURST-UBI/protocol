//! Rate-limit changes of candidate at one election root.
//!
//! A different candidate must wait 1.5 seconds after the last recorded vote;
//! repeating the same candidate is always allowed. The node supplies a root
//! derived from the account and previous block, so forks share a slot while
//! successive account positions have independent slots. The caller must check
//! `votable` before `record`; recording alone does not enforce the interval.

use burst_types::BlockHash;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const MIN_VOTE_SPACING: Duration = Duration::from_millis(1500);

/// Tracks per-root vote timing to prevent rapid vote flipping.
pub struct VoteSpacing {
    last_vote: HashMap<BlockHash, (Instant, BlockHash)>,
}

impl VoteSpacing {
    pub fn new() -> Self {
        Self {
            last_vote: HashMap::new(),
        }
    }

    /// Check if a vote can be generated for this election root.
    /// Returns true if enough time has passed since the last vote on this root,
    /// or if the candidate is the same block (reconfirmation is always OK).
    pub fn votable(&self, root: &BlockHash, candidate: &BlockHash) -> bool {
        match self.last_vote.get(root) {
            None => true,
            Some((last_time, last_hash)) => {
                if last_hash == candidate {
                    return true;
                }
                last_time.elapsed() >= MIN_VOTE_SPACING
            }
        }
    }

    /// Record that a vote was cast for this root.
    pub fn record(&mut self, root: BlockHash, hash: BlockHash) {
        self.last_vote.insert(root, (Instant::now(), hash));
    }

    /// Cleanup old entries (older than 2x spacing to prevent memory growth).
    pub fn cleanup(&mut self) {
        let cutoff = Instant::now() - (MIN_VOTE_SPACING * 2);
        self.last_vote.retain(|_, (t, _)| *t > cutoff);
    }

    /// Number of tracked roots.
    pub fn len(&self) -> usize {
        self.last_vote.len()
    }

    /// Whether there are no tracked roots.
    pub fn is_empty(&self) -> bool {
        self.last_vote.is_empty()
    }
}

impl Default for VoteSpacing {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn make_root(name: &str) -> BlockHash {
        // Distinct root per name (roots are frontier-position block hashes).
        let seed = name.bytes().next().unwrap_or(0);
        BlockHash::new([seed.wrapping_add(100); 32])
    }

    fn make_hash(byte: u8) -> BlockHash {
        BlockHash::new([byte; 32])
    }

    #[test]
    fn first_vote_always_allowed() {
        let spacing = VoteSpacing::new();
        assert!(spacing.votable(&make_root("alice"), &make_hash(1)));
    }

    #[test]
    fn same_candidate_always_allowed() {
        let mut spacing = VoteSpacing::new();
        let root = make_root("alice");
        let hash = make_hash(1);

        spacing.record(root, hash);
        // Same candidate, reconfirmation — always OK regardless of timing
        assert!(spacing.votable(&root, &hash));
    }

    #[test]
    fn different_candidate_blocked_immediately() {
        let mut spacing = VoteSpacing::new();
        let root = make_root("alice");

        spacing.record(root, make_hash(1));
        // Different candidate immediately — should be blocked
        assert!(!spacing.votable(&root, &make_hash(2)));
    }

    #[test]
    fn different_candidate_allowed_after_spacing() {
        let mut spacing = VoteSpacing::new();
        let root = make_root("alice");

        spacing.record(root, make_hash(1));
        // Sleep longer than MIN_VOTE_SPACING (1500ms)
        thread::sleep(Duration::from_millis(1600));
        assert!(spacing.votable(&root, &make_hash(2)));
    }

    #[test]
    fn multiple_roots_independent() {
        let mut spacing = VoteSpacing::new();
        let root_a = make_root("alice");
        let root_b = make_root("bob");

        spacing.record(root_a, make_hash(1));
        // root_b has no record, so first-vote logic applies
        assert!(spacing.votable(&root_b, &make_hash(2)));
        // root_a switching to different candidate immediately — blocked
        assert!(!spacing.votable(&root_a, &make_hash(2)));
    }

    #[test]
    fn record_overwrites_previous() {
        let mut spacing = VoteSpacing::new();
        let root = make_root("alice");

        spacing.record(root, make_hash(1));
        spacing.record(root, make_hash(2));

        // After re-recording, hash(2) is now the last candidate
        assert!(spacing.votable(&root, &make_hash(2)));
        // hash(3) is different from the latest (hash(2)) — blocked
        assert!(!spacing.votable(&root, &make_hash(3)));
    }

    #[test]
    fn cleanup_removes_old_entries() {
        let mut spacing = VoteSpacing::new();
        spacing.record(make_root("alice"), make_hash(1));
        assert_eq!(spacing.len(), 1);

        // Sleep past 2x spacing (3000ms)
        thread::sleep(Duration::from_millis(3100));
        spacing.cleanup();
        assert_eq!(spacing.len(), 0);
    }

    #[test]
    fn cleanup_keeps_recent_entries() {
        let mut spacing = VoteSpacing::new();
        spacing.record(make_root("alice"), make_hash(1));
        // Don't sleep — entry is fresh
        spacing.cleanup();
        assert_eq!(spacing.len(), 1);
    }

    #[test]
    fn is_empty_and_len() {
        let mut spacing = VoteSpacing::new();
        assert!(spacing.is_empty());
        assert_eq!(spacing.len(), 0);

        spacing.record(make_root("alice"), make_hash(1));
        assert!(!spacing.is_empty());
        assert_eq!(spacing.len(), 1);
    }

    #[test]
    fn default_impl() {
        let spacing = VoteSpacing::default();
        assert!(spacing.is_empty());
    }

    #[test]
    fn sequential_chain_all_votable_back_to_back() {
        // Distinct synthetic roots stand for successive account positions.
        // Spacing on one position must not delay a vote on another.
        let mut spacing = VoteSpacing::new();
        let b1 = make_hash(1);
        let b2 = make_hash(2);
        let b3 = make_hash(3);
        // root of b2 is b1's hash; root of b3 is b2's hash; etc.
        assert!(spacing.votable(&b1, &b2));
        spacing.record(b1, b2);
        assert!(spacing.votable(&b2, &b3)); // immediately votable — different root
        spacing.record(b2, b3);
        assert!(spacing.votable(&b3, &make_hash(4)));
    }

    #[test]
    fn fork_at_same_root_is_rate_limited() {
        // Two competing blocks at the SAME frontier position (shared root) —
        // this is the flip-flop we throttle.
        let mut spacing = VoteSpacing::new();
        let root = make_hash(10);
        let fork_a = make_hash(11);
        let fork_b = make_hash(12);
        assert!(spacing.votable(&root, &fork_a));
        spacing.record(root, fork_a);
        // Switching to the competing fork at the same root immediately: blocked.
        assert!(!spacing.votable(&root, &fork_b));
    }
}
