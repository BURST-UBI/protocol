//! Active elections container — manages all ongoing consensus elections.
//!
//! This is the top-level coordinator for the ORV election system. It holds a
//! bounded set of active elections, routes incoming votes to the correct
//! election, and handles cleanup of expired elections.

use crate::election::{Election, ElectionState, ElectionStatus};
use crate::error::ConsensusError;
use crate::vote_info::VoteResult;
use burst_types::{BlockHash, Timestamp, WalletAddress};
use std::collections::HashMap;

/// Container for all active consensus elections.
///
/// Elections are keyed by their root block hash. The container enforces a
/// maximum capacity to prevent resource exhaustion during spam attacks.
#[derive(Clone, Debug)]
pub struct ActiveElections {
    /// Root block hash -> election.
    elections: HashMap<BlockHash, Election>,
    /// Maximum number of concurrent elections.
    max_elections: usize,
    /// Effective online voting weight used for new elections.
    /// Should be `max(current_online, trended_ema, min_floor)` to prevent
    /// quorum collapse on temporary online weight dips.
    effective_weight: u128,
}

impl ActiveElections {
    pub fn new(max_elections: usize, online_weight: u128) -> Self {
        Self {
            elections: HashMap::new(),
            max_elections,
            effective_weight: online_weight,
        }
    }

    /// Start a new election for the given root block.
    ///
    /// Returns an error if the container is at capacity or if an election
    /// for this root already exists.
    pub fn start_election(
        &mut self,
        root: BlockHash,
        now: Timestamp,
    ) -> Result<(), ConsensusError> {
        if self.elections.len() >= self.max_elections {
            return Err(ConsensusError::ElectionCapacityReached(self.max_elections));
        }
        if self.elections.contains_key(&root) {
            return Ok(());
        }

        let election = Election::new(root, self.effective_weight, now);
        self.elections.insert(root, election);
        Ok(())
    }

    /// Route a vote to the appropriate election.
    ///
    /// Returns the election status if the vote caused the election to confirm.
    pub fn process_vote(
        &mut self,
        root: &BlockHash,
        voter: &WalletAddress,
        block: BlockHash,
        weight: u128,
        is_final: bool,
        now: Timestamp,
    ) -> Result<Option<ElectionStatus>, ConsensusError> {
        let election = self
            .elections
            .get_mut(root)
            .ok_or_else(|| ConsensusError::ElectionNotFound(format!("{}", root)))?;

        if election.is_confirmed() {
            return Err(ConsensusError::ElectionAlreadyConfirmed);
        }

        let vote_result = election.vote(voter, block, weight, is_final, now);

        match vote_result {
            VoteResult::Error(msg) => {
                Err(ConsensusError::FinalVoteAlreadyCast(msg))
            }
            _ => {
                let status = election.try_confirm(now);
                Ok(status)
            }
        }
    }

    /// Remove all elections that have timed out.
    ///
    /// Returns the root hashes of expired elections.
    pub fn cleanup_expired(
        &mut self,
        timeout_ms: u64,
        now: Timestamp,
    ) -> Vec<BlockHash> {
        let mut expired = Vec::new();

        for (root, election) in &mut self.elections {
            if election.check_timeout(timeout_ms, now) {
                expired.push(*root);
            }
        }

        for root in &expired {
            self.elections.remove(root);
        }

        expired
    }

    /// Remove confirmed elections after their results have been processed.
    ///
    /// Should be called after `confirmed_elections()` so the caller can
    /// collect and cement the results, then free the slots.
    pub fn cleanup_confirmed(&mut self) -> Vec<BlockHash> {
        let confirmed: Vec<BlockHash> = self
            .elections
            .iter()
            .filter(|(_, e)| e.state == ElectionState::Confirmed)
            .map(|(root, _)| *root)
            .collect();

        for root in &confirmed {
            self.elections.remove(root);
        }

        confirmed
    }

    /// Get a reference to an election by its root.
    pub fn get_election(&self, root: &BlockHash) -> Option<&Election> {
        self.elections.get(root)
    }

    /// Get a mutable reference to an election by its root.
    pub fn get_election_mut(&mut self, root: &BlockHash) -> Option<&mut Election> {
        self.elections.get_mut(root)
    }

    /// Number of active elections.
    pub fn election_count(&self) -> usize {
        self.elections.len()
    }

    /// Collect status for all confirmed elections.
    pub fn confirmed_elections(&self) -> Vec<ElectionStatus> {
        self.elections
            .values()
            .filter(|e| e.state == ElectionState::Confirmed)
            .filter_map(|e| {
                let (winner, tally) = e.leading_block()?;
                let duration_ms = e
                    .state_changed_at
                    .as_secs()
                    .saturating_sub(e.created_at.as_secs())
                    .saturating_mul(1000);
                Some(ElectionStatus {
                    winner,
                    tally,
                    final_tally: tally,
                    election_duration_ms: duration_ms,
                })
            })
            .collect()
    }

    /// Update the effective weight used for new elections.
    ///
    /// Callers should pass `OnlineWeightSampler::effective_weight()` which is
    /// `max(current_online, trended_ema, min_floor)`. This prevents quorum
    /// collapse when online weight dips temporarily.
    pub fn set_online_weight(&mut self, weight: u128) {
        self.effective_weight = weight;
    }

    /// Whether the container has reached its capacity limit.
    pub fn is_at_capacity(&self) -> bool {
        self.elections.len() >= self.max_elections
    }

    /// Resolve a fork by expiring the losing block's election.
    ///
    /// When two blocks share the same previous (a fork), both enter elections.
    /// The losing block's election is cancelled and its data can be rolled back.
    /// Returns the fork block hash if the election was successfully expired.
    pub fn resolve_fork(
        &mut self,
        _confirmed_block: BlockHash,
        fork_block: BlockHash,
    ) -> Option<BlockHash> {
        if let Some(election) = self.elections.get_mut(&fork_block) {
            if !election.is_confirmed() {
                election.state = ElectionState::Expired;
                return Some(fork_block);
            }
        }
        None
    }

    /// Get all block hashes that lost to the confirmed winner in an election.
    ///
    /// These losers are candidates for rollback from the ledger.
    pub fn get_fork_losers(&self, confirmed_root: &BlockHash) -> Vec<BlockHash> {
        if let Some(election) = self.elections.get(confirmed_root) {
            if election.is_confirmed() {
                if let Some((winner, _)) = election.leading_block() {
                    return election
                        .tally
                        .keys()
                        .filter(|hash| **hash != winner)
                        .copied()
                        .collect();
                }
            }
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hash(byte: u8) -> BlockHash {
        BlockHash::new([byte; 32])
    }

    fn make_voter(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{name}"))
    }

    fn ts(secs: u64) -> Timestamp {
        Timestamp::new(secs)
    }

    #[test]
    fn start_election() {
        let mut ae = ActiveElections::new(10, 1000);
        assert!(ae.start_election(make_hash(1), ts(100)).is_ok());
        assert_eq!(ae.election_count(), 1);
        assert!(ae.get_election(&make_hash(1)).is_some());
    }

    #[test]
    fn duplicate_election_is_noop() {
        let mut ae = ActiveElections::new(10, 1000);
        ae.start_election(make_hash(1), ts(100)).unwrap();
        ae.start_election(make_hash(1), ts(101)).unwrap();
        assert_eq!(ae.election_count(), 1);
    }

    #[test]
    fn capacity_limit_enforced() {
        let mut ae = ActiveElections::new(2, 1000);
        ae.start_election(make_hash(1), ts(100)).unwrap();
        ae.start_election(make_hash(2), ts(101)).unwrap();

        let result = ae.start_election(make_hash(3), ts(102));
        assert!(result.is_err());
        match result.unwrap_err() {
            ConsensusError::ElectionCapacityReached(cap) => assert_eq!(cap, 2),
            e => panic!("unexpected error: {:?}", e),
        }
        assert!(ae.is_at_capacity());
    }

    #[test]
    fn process_vote_routes_to_election() {
        let mut ae = ActiveElections::new(10, 1000);
        ae.start_election(make_hash(1), ts(100)).unwrap();

        let result = ae.process_vote(
            &make_hash(1),
            &make_voter("alice"),
            make_hash(2),
            300,
            false,
            ts(101),
        );
        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // Not confirmed yet

        let election = ae.get_election(&make_hash(1)).unwrap();
        assert_eq!(election.tally.get(&make_hash(2)), Some(&300));
    }

    #[test]
    fn process_vote_election_not_found() {
        let mut ae = ActiveElections::new(10, 1000);
        let result = ae.process_vote(
            &make_hash(99),
            &make_voter("alice"),
            make_hash(2),
            100,
            false,
            ts(100),
        );
        assert!(matches!(result, Err(ConsensusError::ElectionNotFound(_))));
    }

    #[test]
    fn process_vote_confirms_election() {
        let mut ae = ActiveElections::new(10, 1000);
        ae.start_election(make_hash(1), ts(100)).unwrap();

        // Vote with enough weight to confirm (>= 670)
        let result = ae
            .process_vote(
                &make_hash(1),
                &make_voter("alice"),
                make_hash(2),
                700,
                true,
                ts(101),
            )
            .unwrap();

        assert!(result.is_some());
        let status = result.unwrap();
        assert_eq!(status.winner, make_hash(2));
        assert_eq!(status.tally, 700);
    }

    #[test]
    fn process_vote_on_confirmed_election_errors() {
        let mut ae = ActiveElections::new(10, 1000);
        ae.start_election(make_hash(1), ts(100)).unwrap();
        ae.process_vote(
            &make_hash(1),
            &make_voter("alice"),
            make_hash(2),
            700,
            true,
            ts(101),
        )
        .unwrap();

        let result = ae.process_vote(
            &make_hash(1),
            &make_voter("bob"),
            make_hash(3),
            100,
            false,
            ts(102),
        );
        assert!(matches!(result, Err(ConsensusError::ElectionAlreadyConfirmed)));
    }

    #[test]
    fn process_vote_final_vote_already_cast() {
        let mut ae = ActiveElections::new(10, 1000);
        ae.start_election(make_hash(1), ts(100)).unwrap();

        ae.process_vote(
            &make_hash(1),
            &make_voter("alice"),
            make_hash(2),
            300,
            true,
            ts(101),
        )
        .unwrap();

        let result = ae.process_vote(
            &make_hash(1),
            &make_voter("alice"),
            make_hash(3),
            300,
            false,
            ts(102),
        );
        assert!(matches!(result, Err(ConsensusError::FinalVoteAlreadyCast(_))));
    }

    #[test]
    fn cleanup_expired_elections() {
        let mut ae = ActiveElections::new(10, 1000);
        ae.start_election(make_hash(1), ts(100)).unwrap();
        ae.start_election(make_hash(2), ts(110)).unwrap();

        // 30s timeout: election 1 created at 100, now = 131 => expired
        // election 2 created at 110, now = 131 => 21s < 30s => not expired
        let expired = ae.cleanup_expired(30_000, ts(131));

        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0], make_hash(1));
        assert_eq!(ae.election_count(), 1);
        assert!(ae.get_election(&make_hash(1)).is_none());
        assert!(ae.get_election(&make_hash(2)).is_some());
    }

    #[test]
    fn cleanup_frees_capacity() {
        let mut ae = ActiveElections::new(2, 1000);
        ae.start_election(make_hash(1), ts(100)).unwrap();
        ae.start_election(make_hash(2), ts(101)).unwrap();

        assert!(ae.is_at_capacity());

        ae.cleanup_expired(1, ts(200)); // Expire all
        assert_eq!(ae.election_count(), 0);
        assert!(!ae.is_at_capacity());

        // Should be able to start new elections
        assert!(ae.start_election(make_hash(3), ts(201)).is_ok());
    }

    #[test]
    fn confirmed_elections_list() {
        let mut ae = ActiveElections::new(10, 1000);
        ae.start_election(make_hash(1), ts(100)).unwrap();
        ae.start_election(make_hash(2), ts(100)).unwrap();

        // Confirm election 1
        ae.process_vote(
            &make_hash(1),
            &make_voter("alice"),
            make_hash(10),
            700,
            true,
            ts(105),
        )
        .unwrap();

        // Election 2 stays unconfirmed
        ae.process_vote(
            &make_hash(2),
            &make_voter("bob"),
            make_hash(20),
            100,
            false,
            ts(105),
        )
        .unwrap();

        let confirmed = ae.confirmed_elections();
        assert_eq!(confirmed.len(), 1);
        assert_eq!(confirmed[0].winner, make_hash(10));
    }

    #[test]
    fn set_online_weight_affects_new_elections() {
        let mut ae = ActiveElections::new(10, 1000);
        ae.start_election(make_hash(1), ts(100)).unwrap();

        ae.set_online_weight(2000);
        ae.start_election(make_hash(2), ts(101)).unwrap();

        // Election 1: threshold = 670 (67% of 1000)
        let e1 = ae.get_election(&make_hash(1)).unwrap();
        assert_eq!(e1.confirmation_threshold(), 670);

        // Election 2: threshold = 1340 (67% of 2000)
        let e2 = ae.get_election(&make_hash(2)).unwrap();
        assert_eq!(e2.confirmation_threshold(), 1340);
    }

    #[test]
    fn multiple_votes_build_to_confirmation() {
        let mut ae = ActiveElections::new(10, 1000);
        ae.start_election(make_hash(1), ts(100)).unwrap();

        // Three voters, each insufficient alone
        let r1 = ae
            .process_vote(
                &make_hash(1),
                &make_voter("alice"),
                make_hash(2),
                250,
                true,
                ts(101),
            )
            .unwrap();
        assert!(r1.is_none());

        let r2 = ae
            .process_vote(
                &make_hash(1),
                &make_voter("bob"),
                make_hash(2),
                200,
                true,
                ts(102),
            )
            .unwrap();
        assert!(r2.is_none());

        // This should push it over 670
        let r3 = ae
            .process_vote(
                &make_hash(1),
                &make_voter("carol"),
                make_hash(2),
                250,
                true,
                ts(103),
            )
            .unwrap();
        assert!(r3.is_some());
        let status = r3.unwrap();
        assert_eq!(status.winner, make_hash(2));
        assert_eq!(status.tally, 700);
    }

    // --- Fork resolution tests ---

    #[test]
    fn resolve_fork_expires_unconfirmed_election() {
        let mut ae = ActiveElections::new(10, 1000);
        ae.start_election(make_hash(1), ts(100)).unwrap(); // confirmed block's election
        ae.start_election(make_hash(2), ts(100)).unwrap(); // fork block's election

        let result = ae.resolve_fork(make_hash(1), make_hash(2));
        assert_eq!(result, Some(make_hash(2)));

        let fork_election = ae.get_election(&make_hash(2)).unwrap();
        assert!(fork_election.is_expired());
    }

    #[test]
    fn resolve_fork_does_not_expire_confirmed_election() {
        let mut ae = ActiveElections::new(10, 1000);
        ae.start_election(make_hash(1), ts(100)).unwrap();

        // Confirm election 1
        ae.process_vote(
            &make_hash(1),
            &make_voter("alice"),
            make_hash(10),
            700,
            true,
            ts(101),
        )
        .unwrap();

        // Trying to resolve a fork against a confirmed election should be a no-op
        let result = ae.resolve_fork(make_hash(99), make_hash(1));
        assert!(result.is_none());

        let election = ae.get_election(&make_hash(1)).unwrap();
        assert!(election.is_confirmed());
    }

    #[test]
    fn resolve_fork_returns_none_for_missing_election() {
        let mut ae = ActiveElections::new(10, 1000);
        let result = ae.resolve_fork(make_hash(1), make_hash(99));
        assert!(result.is_none());
    }

    #[test]
    fn get_fork_losers_returns_losing_blocks() {
        let mut ae = ActiveElections::new(10, 1000);
        ae.start_election(make_hash(1), ts(100)).unwrap();

        // Vote for losing block first (below threshold, no auto-confirm)
        ae.process_vote(
            &make_hash(1),
            &make_voter("bob"),
            make_hash(20),
            100,
            false,
            ts(101),
        )
        .unwrap();

        // Vote for winning block with enough weight to confirm
        // tally=800, runner_up=100, margin=700 >= 670 → confirms
        let result = ae
            .process_vote(
                &make_hash(1),
                &make_voter("alice"),
                make_hash(10),
                800,
                true,
                ts(102),
            )
            .unwrap();
        assert!(result.is_some());

        let election = ae.get_election(&make_hash(1)).unwrap();
        assert!(election.is_confirmed());

        let losers = ae.get_fork_losers(&make_hash(1));
        assert_eq!(losers.len(), 1);
        assert_eq!(losers[0], make_hash(20));
    }

    #[test]
    fn get_fork_losers_empty_for_unconfirmed() {
        let mut ae = ActiveElections::new(10, 1000);
        ae.start_election(make_hash(1), ts(100)).unwrap();

        // Not enough weight to confirm
        ae.process_vote(
            &make_hash(1),
            &make_voter("alice"),
            make_hash(10),
            100,
            false,
            ts(101),
        )
        .unwrap();

        let losers = ae.get_fork_losers(&make_hash(1));
        assert!(losers.is_empty());
    }

    #[test]
    fn get_fork_losers_empty_for_missing_election() {
        let ae = ActiveElections::new(10, 1000);
        let losers = ae.get_fork_losers(&make_hash(99));
        assert!(losers.is_empty());
    }
}
