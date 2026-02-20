//! Vote spacing — prevents rapid vote flip-flopping during fork resolution.
//!
//! When the leading candidate changes in an election, a node should not
//! immediately flip its vote. Vote spacing enforces a minimum time gap
//! between vote changes for the same account root, preventing vote
//! oscillation attacks where an adversary rapidly alternates the winner.

use burst_types::{BlockHash, WalletAddress};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const MIN_VOTE_SPACING: Duration = Duration::from_millis(1500);

/// Tracks per-root vote timing to prevent rapid vote flipping.
pub struct VoteSpacing {
    last_vote: HashMap<WalletAddress, (Instant, BlockHash)>,
}

impl VoteSpacing {
    pub fn new() -> Self {
        Self {
            last_vote: HashMap::new(),
        }
    }

    /// Check if a vote can be generated for this account root.
    /// Returns true if enough time has passed since the last vote on this root,
    /// or if the candidate is the same block (reconfirmation is always OK).
    pub fn votable(&self, root: &WalletAddress, candidate: &BlockHash) -> bool {
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
    pub fn record(&mut self, root: WalletAddress, hash: BlockHash) {
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

    fn make_root(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{name}"))
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

        spacing.record(root.clone(), hash);
        // Same candidate, reconfirmation — always OK regardless of timing
        assert!(spacing.votable(&root, &hash));
    }

    #[test]
    fn different_candidate_blocked_immediately() {
        let mut spacing = VoteSpacing::new();
        let root = make_root("alice");

        spacing.record(root.clone(), make_hash(1));
        // Different candidate immediately — should be blocked
        assert!(!spacing.votable(&root, &make_hash(2)));
    }

    #[test]
    fn different_candidate_allowed_after_spacing() {
        let mut spacing = VoteSpacing::new();
        let root = make_root("alice");

        spacing.record(root.clone(), make_hash(1));
        // Sleep longer than MIN_VOTE_SPACING (1500ms)
        thread::sleep(Duration::from_millis(1600));
        assert!(spacing.votable(&root, &make_hash(2)));
    }

    #[test]
    fn multiple_roots_independent() {
        let mut spacing = VoteSpacing::new();
        let root_a = make_root("alice");
        let root_b = make_root("bob");

        spacing.record(root_a.clone(), make_hash(1));
        // root_b has no record, so first-vote logic applies
        assert!(spacing.votable(&root_b, &make_hash(2)));
        // root_a switching to different candidate immediately — blocked
        assert!(!spacing.votable(&root_a, &make_hash(2)));
    }

    #[test]
    fn record_overwrites_previous() {
        let mut spacing = VoteSpacing::new();
        let root = make_root("alice");

        spacing.record(root.clone(), make_hash(1));
        spacing.record(root.clone(), make_hash(2));

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
}
