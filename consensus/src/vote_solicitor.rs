//! Vote solicitation — actively request representatives to vote on elections.
//!
//! When an election starts, the node should not passively wait for votes. This
//! module tracks which elections need solicitation, which reps have already
//! responded, and enforces a re-solicitation interval with a maximum retry count.

use burst_types::{BlockHash, WalletAddress};
use std::collections::{HashMap, HashSet};

/// Per-election solicitation state.
struct SolicitationState {
    /// The block hash we're asking representatives to vote on.
    block_hash: BlockHash,
    /// UNIX timestamp (seconds) of the last solicitation round.
    last_solicited: u64,
    /// How many solicitation rounds have been sent.
    solicitation_count: u32,
    /// Maximum number of solicitation rounds before giving up.
    max_solicitations: u32,
    /// Representatives that have already responded with a vote.
    responded_reps: HashSet<WalletAddress>,
}

/// Tracks which elections need vote solicitation and when to re-request.
pub struct VoteSolicitor {
    /// Elections that need votes: election_root → solicitation state.
    pending: HashMap<BlockHash, SolicitationState>,
    /// Minimum interval between re-solicitations (seconds).
    interval_secs: u64,
}

impl VoteSolicitor {
    /// Create a new solicitor with the given re-solicitation interval.
    pub fn new(interval_secs: u64) -> Self {
        Self {
            pending: HashMap::new(),
            interval_secs,
        }
    }

    /// Register an election that needs vote solicitation.
    ///
    /// `root` is the election root (e.g. the `previous` hash of forking blocks).
    /// `block_hash` is the specific block we're asking reps to vote on.
    pub fn add_election(&mut self, root: BlockHash, block_hash: BlockHash) {
        self.pending.entry(root).or_insert_with(|| SolicitationState {
            block_hash,
            last_solicited: 0,
            solicitation_count: 0,
            max_solicitations: 10,
            responded_reps: HashSet::new(),
        });
    }

    /// Register an election with custom max solicitations.
    pub fn add_election_with_max(
        &mut self,
        root: BlockHash,
        block_hash: BlockHash,
        max_solicitations: u32,
    ) {
        self.pending.entry(root).or_insert_with(|| SolicitationState {
            block_hash,
            last_solicited: 0,
            solicitation_count: 0,
            max_solicitations,
            responded_reps: HashSet::new(),
        });
    }

    /// Remove an election (confirmed or expired).
    pub fn remove_election(&mut self, root: &BlockHash) {
        self.pending.remove(root);
    }

    /// Record that a representative has voted (don't re-solicit them).
    pub fn record_vote(&mut self, root: &BlockHash, rep: &WalletAddress) {
        if let Some(state) = self.pending.get_mut(root) {
            state.responded_reps.insert(rep.clone());
        }
    }

    /// Get elections that need solicitation at the current time.
    ///
    /// Returns a list of `(block_hash, target_reps)` for each election
    /// that hasn't been solicited recently and hasn't exceeded its retry limit.
    /// `target_reps` excludes representatives that have already responded.
    pub fn elections_needing_solicitation(
        &mut self,
        now: u64,
        all_reps: &[WalletAddress],
    ) -> Vec<(BlockHash, Vec<WalletAddress>)> {
        let interval = self.interval_secs;
        let mut results = Vec::new();

        for state in self.pending.values_mut() {
            // Skip if we've exceeded max solicitations
            if state.solicitation_count >= state.max_solicitations {
                continue;
            }

            // Skip if not enough time has elapsed since last solicitation
            if state.last_solicited > 0 && now.saturating_sub(state.last_solicited) < interval {
                continue;
            }

            // Determine which reps haven't responded yet
            let target_reps: Vec<WalletAddress> = all_reps
                .iter()
                .filter(|r| !state.responded_reps.contains(r))
                .cloned()
                .collect();

            if !target_reps.is_empty() {
                state.last_solicited = now;
                state.solicitation_count += 1;
                results.push((state.block_hash, target_reps));
            }
        }

        results
    }

    /// Number of active solicitations.
    pub fn active_count(&self) -> usize {
        self.pending.len()
    }

    /// Check if an election is being solicited.
    pub fn contains(&self, root: &BlockHash) -> bool {
        self.pending.contains_key(root)
    }

    /// How many reps have responded for a given election.
    pub fn responded_count(&self, root: &BlockHash) -> usize {
        self.pending
            .get(root)
            .map(|s| s.responded_reps.len())
            .unwrap_or(0)
    }

    /// How many solicitation rounds have been sent for a given election.
    pub fn solicitation_count(&self, root: &BlockHash) -> u32 {
        self.pending
            .get(root)
            .map(|s| s.solicitation_count)
            .unwrap_or(0)
    }

    /// Remove elections that have exceeded their max solicitations.
    /// Returns the roots of pruned elections.
    pub fn prune_exhausted(&mut self) -> Vec<BlockHash> {
        let exhausted: Vec<BlockHash> = self
            .pending
            .iter()
            .filter(|(_, s)| s.solicitation_count >= s.max_solicitations)
            .map(|(root, _)| *root)
            .collect();

        for root in &exhausted {
            self.pending.remove(root);
        }

        exhausted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(byte: u8) -> BlockHash {
        BlockHash::new([byte; 32])
    }

    fn block(byte: u8) -> BlockHash {
        BlockHash::new([byte + 100; 32])
    }

    fn rep(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{name}"))
    }

    #[test]
    fn add_and_remove_election() {
        let mut solicitor = VoteSolicitor::new(5);
        solicitor.add_election(root(1), block(1));

        assert_eq!(solicitor.active_count(), 1);
        assert!(solicitor.contains(&root(1)));

        solicitor.remove_election(&root(1));
        assert_eq!(solicitor.active_count(), 0);
        assert!(!solicitor.contains(&root(1)));
    }

    #[test]
    fn duplicate_add_is_noop() {
        let mut solicitor = VoteSolicitor::new(5);
        solicitor.add_election(root(1), block(1));
        solicitor.add_election(root(1), block(2)); // should not overwrite

        assert_eq!(solicitor.active_count(), 1);
    }

    #[test]
    fn solicitation_returns_all_reps_initially() {
        let mut solicitor = VoteSolicitor::new(5);
        solicitor.add_election(root(1), block(1));

        let reps = vec![rep("alice"), rep("bob"), rep("carol")];
        let results = solicitor.elections_needing_solicitation(100, &reps);

        assert_eq!(results.len(), 1);
        let (hash, target) = &results[0];
        assert_eq!(*hash, block(1));
        assert_eq!(target.len(), 3);
    }

    #[test]
    fn solicitation_excludes_responded_reps() {
        let mut solicitor = VoteSolicitor::new(5);
        solicitor.add_election(root(1), block(1));
        solicitor.record_vote(&root(1), &rep("alice"));

        let reps = vec![rep("alice"), rep("bob"), rep("carol")];
        let results = solicitor.elections_needing_solicitation(100, &reps);

        assert_eq!(results.len(), 1);
        let (_, target) = &results[0];
        assert_eq!(target.len(), 2);
        assert!(!target.contains(&rep("alice")));
    }

    #[test]
    fn solicitation_respects_interval() {
        let mut solicitor = VoteSolicitor::new(10);
        solicitor.add_election(root(1), block(1));

        let reps = vec![rep("alice")];

        // First solicitation at t=100
        let r1 = solicitor.elections_needing_solicitation(100, &reps);
        assert_eq!(r1.len(), 1);

        // Too early (t=105, only 5s elapsed, interval=10)
        let r2 = solicitor.elections_needing_solicitation(105, &reps);
        assert_eq!(r2.len(), 0);

        // Ready (t=110, 10s elapsed)
        let r3 = solicitor.elections_needing_solicitation(110, &reps);
        assert_eq!(r3.len(), 1);
    }

    #[test]
    fn solicitation_respects_max_count() {
        let mut solicitor = VoteSolicitor::new(1);
        solicitor.add_election_with_max(root(1), block(1), 2);

        let reps = vec![rep("alice")];

        // Round 1 at t=0
        assert_eq!(
            solicitor.elections_needing_solicitation(0, &reps).len(),
            1
        );

        // Round 2 at t=10
        assert_eq!(
            solicitor.elections_needing_solicitation(10, &reps).len(),
            1
        );

        // Round 3 — exceeded max (2), should be empty
        assert_eq!(
            solicitor.elections_needing_solicitation(20, &reps).len(),
            0
        );

        assert_eq!(solicitor.solicitation_count(&root(1)), 2);
    }

    #[test]
    fn no_solicitation_when_all_reps_responded() {
        let mut solicitor = VoteSolicitor::new(1);
        solicitor.add_election(root(1), block(1));
        solicitor.record_vote(&root(1), &rep("alice"));
        solicitor.record_vote(&root(1), &rep("bob"));

        let reps = vec![rep("alice"), rep("bob")];
        let results = solicitor.elections_needing_solicitation(100, &reps);
        assert_eq!(results.len(), 0);

        assert_eq!(solicitor.responded_count(&root(1)), 2);
    }

    #[test]
    fn record_vote_for_unknown_election_is_noop() {
        let mut solicitor = VoteSolicitor::new(5);
        solicitor.record_vote(&root(99), &rep("alice"));
        assert_eq!(solicitor.active_count(), 0);
    }

    #[test]
    fn prune_exhausted() {
        let mut solicitor = VoteSolicitor::new(0);
        solicitor.add_election_with_max(root(1), block(1), 1);
        solicitor.add_election_with_max(root(2), block(2), 10);

        let reps = vec![rep("alice")];

        // Exhaust root(1)
        solicitor.elections_needing_solicitation(0, &reps);

        let pruned = solicitor.prune_exhausted();
        assert_eq!(pruned.len(), 1);
        assert_eq!(pruned[0], root(1));
        assert_eq!(solicitor.active_count(), 1);
    }

    #[test]
    fn multiple_elections_solicited_independently() {
        let mut solicitor = VoteSolicitor::new(5);
        solicitor.add_election(root(1), block(1));
        solicitor.add_election(root(2), block(2));

        let reps = vec![rep("alice"), rep("bob")];
        let results = solicitor.elections_needing_solicitation(100, &reps);
        assert_eq!(results.len(), 2);

        // Record alice for election 1 only
        solicitor.record_vote(&root(1), &rep("alice"));

        let results2 = solicitor.elections_needing_solicitation(110, &reps);
        assert_eq!(results2.len(), 2);

        // Election 1 should only target bob
        for (hash, targets) in &results2 {
            if *hash == block(1) {
                assert_eq!(targets.len(), 1);
                assert_eq!(targets[0], rep("bob"));
            } else {
                assert_eq!(targets.len(), 2);
            }
        }
    }
}
