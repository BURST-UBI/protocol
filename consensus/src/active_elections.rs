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
use std::sync::Arc;

/// Container for all active consensus elections.
///
/// Elections are keyed by their root block hash. The container enforces a
/// maximum capacity to prevent resource exhaustion during spam attacks.
#[derive(Clone, Debug)]
pub struct ActiveElections {
    /// Root block hash -> election.
    elections: HashMap<BlockHash, Election>,
    /// Validated candidate hash -> shared conflict root. Wire votes carry hashes,
    /// not roots; never let a peer choose the election a candidate belongs to.
    candidate_roots: HashMap<BlockHash, BlockHash>,
    candidate_counts: HashMap<BlockHash, usize>,
    /// Maximum number of concurrent elections.
    max_elections: usize,
    /// Total frozen authority for new elections. Legacy component callers may
    /// supply a weight directly; the node sets the complete confirmed map.
    effective_weight: u128,
    authority: Option<Arc<HashMap<WalletAddress, u128>>>,
    electorates: HashMap<BlockHash, Arc<HashMap<WalletAddress, u128>>>,
    authority_epoch: Option<BlockHash>,
    quorum_bps: u16,
}

impl ActiveElections {
    pub fn new(max_elections: usize, online_weight: u128) -> Self {
        Self {
            elections: HashMap::new(),
            candidate_roots: HashMap::new(),
            candidate_counts: HashMap::new(),
            max_elections,
            effective_weight: online_weight,
            authority: None,
            electorates: HashMap::new(),
            authority_epoch: None,
            quorum_bps: 6700,
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
        if self.elections.contains_key(&root) {
            return Ok(());
        }
        if self.elections.len() >= self.max_elections {
            return Err(ConsensusError::ElectionCapacityReached(self.max_elections));
        }

        let election = Election::with_quorum(root, self.effective_weight, now, self.quorum_bps);
        if let Some(authority) = &self.authority {
            self.electorates.insert(root, Arc::clone(authority));
        }
        self.elections.insert(root, election);
        Ok(())
    }

    /// Register a locally validated candidate before accepting votes for it.
    /// Competing candidates must use the same account-position root.
    pub fn register_candidate(
        &mut self,
        root: BlockHash,
        candidate: BlockHash,
        now: Timestamp,
    ) -> Result<(), ConsensusError> {
        if let Some(existing) = self.candidate_roots.get(&candidate) {
            return if *existing == root {
                Ok(())
            } else {
                Err(ConsensusError::Other(
                    "candidate already belongs to another election".into(),
                ))
            };
        }
        // Bound candidate routing independently of the number of elections.
        if self.candidate_counts.get(&root).copied().unwrap_or(0) >= 16 {
            return Err(ConsensusError::Other(
                "election candidate limit reached".into(),
            ));
        }
        self.start_election(root, now)?;
        self.candidate_roots.insert(candidate, root);
        *self.candidate_counts.entry(root).or_default() += 1;
        Ok(())
    }

    pub fn candidate_root(&self, candidate: &BlockHash) -> Option<BlockHash> {
        self.candidate_roots.get(candidate).copied()
    }

    pub fn unconfirmed_candidates(&self) -> Vec<BlockHash> {
        let mut hashes: Vec<_> = self
            .candidate_roots
            .iter()
            .filter(|(_, root)| {
                self.elections
                    .get(root)
                    .is_some_and(|e| !e.is_confirmed() && !e.is_expired())
            })
            .map(|(hash, _)| *hash)
            .collect();
        hashes.sort_by_key(|hash| *hash.as_bytes());
        hashes
    }

    fn prune_candidate_routes(&mut self) {
        self.candidate_roots
            .retain(|_, root| self.elections.contains_key(root));
        self.candidate_counts
            .retain(|root, _| self.elections.contains_key(root));
        self.electorates
            .retain(|root, _| self.elections.contains_key(root));
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
        let weight = self
            .electorates
            .get(root)
            .map_or(weight, |roster| roster.get(voter).copied().unwrap_or(0));
        let election = self
            .elections
            .get_mut(root)
            .ok_or_else(|| ConsensusError::ElectionNotFound(format!("{}", root)))?;

        if election.is_confirmed() {
            return Err(ConsensusError::ElectionAlreadyConfirmed);
        }

        let vote_result = election.vote(voter, block, weight, is_final, now);

        match vote_result {
            VoteResult::Error(msg) => Err(ConsensusError::FinalVoteAlreadyCast(msg)),
            _ => {
                let status = election.try_confirm(now);
                Ok(status)
            }
        }
    }

    /// Process an authenticated wire vote using its signed ordering metadata.
    pub fn process_signed_vote(
        &mut self,
        root: &BlockHash,
        mut vote: crate::vote_info::VoteInfo,
        received_at: Timestamp,
    ) -> Result<Option<ElectionStatus>, ConsensusError> {
        if self.candidate_root(&vote.block_hash) != Some(*root) {
            return Err(ConsensusError::Other("unregistered vote candidate".into()));
        }
        vote.weight = self.electorates.get(root).map_or(vote.weight, |roster| {
            roster.get(&vote.voter).copied().unwrap_or(0)
        });
        let election = self
            .elections
            .get_mut(root)
            .ok_or_else(|| ConsensusError::ElectionNotFound(root.to_string()))?;
        match election.vote_signed(vote, received_at) {
            VoteResult::Error(message) => Err(ConsensusError::FinalVoteAlreadyCast(message)),
            _ => Ok(election.try_confirm(received_at)),
        }
    }

    /// `(root, winner)` for active elections that have reached SOFT quorum (a
    /// supermajority margin over all votes — see [`Election::has_quorum`]) but
    /// where `our_rep` has not yet cast a FINAL vote. The node emits a final
    /// vote for each so the election can progress soft-quorum → final-quorum →
    /// confirmation (rsnano's two-phase model). Confirmation itself is gated on
    /// FINAL votes, so without this step an election that reached soft quorum
    /// would never cement.
    pub fn soft_quorum_needing_final(
        &self,
        our_rep: &WalletAddress,
    ) -> Vec<(BlockHash, BlockHash)> {
        self.elections
            .iter()
            .filter(|(_, e)| !e.is_confirmed() && !e.is_expired() && e.has_quorum())
            .filter(|(_, e)| e.last_votes.get(our_rep).map(|v| v.is_final) != Some(true))
            .filter_map(|(root, e)| e.leading_block().map(|(winner, _)| (*root, winner)))
            .collect()
    }

    /// Remove all elections that have timed out.
    ///
    /// Returns the root hashes of expired elections.
    pub fn cleanup_expired(&mut self, timeout_ms: u64, now: Timestamp) -> Vec<BlockHash> {
        let mut expired = Vec::new();

        for (root, election) in &mut self.elections {
            if election.check_timeout(timeout_ms, now) {
                expired.push(*root);
            }
        }

        for root in &expired {
            self.elections.remove(root);
        }
        self.prune_candidate_routes();

        expired
    }

    /// Remove confirmed elections after their results have been processed.
    ///
    /// Should be called after `confirmed_elections()` so the caller can
    /// collect and cement the results, then free the slots.
    pub fn cleanup_confirmed(&mut self) -> Vec<BlockHash> {
        self.cleanup_confirmed_where(|_| true)
    }

    /// Keep a confirmed election until its winner has been handed to the ledger.
    pub fn cleanup_confirmed_where(
        &mut self,
        processed: impl Fn(&BlockHash) -> bool,
    ) -> Vec<BlockHash> {
        let confirmed: Vec<BlockHash> = self
            .elections
            .iter()
            .filter(|(_, e)| e.state == ElectionState::Confirmed)
            .filter(|(_, e)| {
                e.leading_final_block()
                    .is_some_and(|(winner, _)| processed(&winner))
            })
            .map(|(root, _)| *root)
            .collect();

        for root in &confirmed {
            self.elections.remove(root);
        }
        self.prune_candidate_routes();

        confirmed
    }

    /// Get a reference to an election by its root.
    pub fn get_election(&self, root: &BlockHash) -> Option<&Election> {
        self.elections.get(root)
    }

    /// Retire an election whose account position was removed by ledger replacement.
    pub fn remove_root(&mut self, root: &BlockHash) {
        self.elections.remove(root);
        self.prune_candidate_routes();
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
                // The confirmed winner is the FINAL-tally leader (that's what
                // cements a block), not the combined-tally leader.
                let (winner, final_tally) = e.leading_final_block()?;
                let tally = e.tally.get(&winner).copied().unwrap_or(final_tally);
                let duration_ms = e
                    .state_changed_at
                    .as_secs()
                    .saturating_sub(e.created_at.as_secs())
                    .saturating_mul(1000);
                Some(ElectionStatus {
                    winner,
                    tally,
                    final_tally,
                    election_duration_ms: duration_ms,
                })
            })
            .collect()
    }

    /// Legacy component configuration. Once an authority map is installed,
    /// attendance samples cannot change the denominator for new elections.
    pub fn set_online_weight(&mut self, weight: u128) {
        if self.authority.is_none() {
            self.effective_weight = weight;
        }
    }

    /// Freeze confirmed representative weights when each election starts.
    /// Online attendance and later delegation cannot rewrite an active tally.
    pub fn set_authority(
        &mut self,
        weights: HashMap<WalletAddress, u128>,
    ) -> Result<(), ConsensusError> {
        if self.authority_epoch.is_some() && self.authority.as_deref() != Some(&weights) {
            return Err(ConsensusError::Other(
                "certified authority requires an epoch handoff".into(),
            ));
        }
        let total = weights
            .values()
            .try_fold(0u128, |sum, weight| sum.checked_add(*weight))
            .ok_or_else(|| ConsensusError::Other("authority weight overflow".into()))?;
        self.authority = Some(Arc::new(weights));
        self.effective_weight = total;
        Ok(())
    }

    pub fn authority_epoch(&self) -> Option<BlockHash> {
        self.authority_epoch
    }

    /// The caller authenticates the installed checkpoint. A new epoch retires
    /// every old election and candidate route; attendance cannot reopen them.
    /// Repeating the same epoch is allowed only with identical authority.
    pub fn install_authority(
        &mut self,
        epoch: BlockHash,
        weights: HashMap<WalletAddress, u128>,
        quorum_bps: u16,
    ) -> Result<(), ConsensusError> {
        let total = weights
            .values()
            .try_fold(0u128, |sum, weight| sum.checked_add(*weight))
            .ok_or_else(|| ConsensusError::Other("authority weight overflow".into()))?;
        if epoch.is_zero()
            || !(6667..=10000).contains(&quorum_bps)
            || total == 0
            || weights
                .values()
                .any(|weight| *weight == 0 || *weight % crate::rep_weights::WEIGHT_PER_HUMAN != 0)
        {
            return Err(ConsensusError::Other(
                "invalid certified payment authority".into(),
            ));
        }
        if self.authority_epoch == Some(epoch) {
            if self.authority.as_deref() != Some(&weights) || self.quorum_bps != quorum_bps {
                return Err(ConsensusError::Other(
                    "installed payment authority changed".into(),
                ));
            }
            return Ok(());
        }
        self.elections.clear();
        self.candidate_roots.clear();
        self.candidate_counts.clear();
        self.electorates.clear();
        self.authority = Some(Arc::new(weights));
        self.effective_weight = total;
        self.authority_epoch = Some(epoch);
        self.quorum_bps = quorum_bps;
        Ok(())
    }

    pub fn has_authorized_voter(&self, voter: &WalletAddress) -> bool {
        self.authority
            .as_ref()
            .is_some_and(|r| r.get(voter).is_some_and(|w| *w > 0))
            || self
                .electorates
                .values()
                .any(|r| r.get(voter).is_some_and(|w| *w > 0))
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
                // Losers = every candidate block except the FINAL-tally winner
                // (the one that actually cemented). Using the combined-tally
                // leader here could roll back the truly-confirmed block.
                if let Some((winner, _)) = election.leading_final_block() {
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

    #[test]
    fn certified_epoch_retires_old_routes_and_freezes_its_quorum() {
        let old = WalletAddress::new("brst_old");
        let next = WalletAddress::new("brst_next");
        let mut elections = ActiveElections::new(10, 0);
        let first = BlockHash::new([1; 32]);
        let second = BlockHash::new([2; 32]);
        let root = BlockHash::new([3; 32]);
        let candidate = BlockHash::new([4; 32]);
        elections
            .install_authority(first, HashMap::from([(old.clone(), 100_000)]), 6700)
            .unwrap();
        elections
            .register_candidate(root, candidate, Timestamp::new(1))
            .unwrap();
        assert_eq!(
            elections
                .get_election(&root)
                .unwrap()
                .confirmation_threshold(),
            67_000
        );
        elections
            .install_authority(second, HashMap::from([(next.clone(), 100_000)]), 8000)
            .unwrap();
        assert_eq!(elections.election_count(), 0);
        assert_eq!(elections.candidate_root(&candidate), None);
        assert!(!elections.has_authorized_voter(&old));
        assert!(elections.has_authorized_voter(&next));
        elections.set_online_weight(1);
        elections
            .register_candidate(root, BlockHash::new([5; 32]), Timestamp::new(2))
            .unwrap();
        assert_eq!(
            elections
                .get_election(&root)
                .unwrap()
                .confirmation_threshold(),
            80_000
        );
        elections
            .install_authority(second, HashMap::from([(next.clone(), 100_000)]), 8000)
            .unwrap();
        assert_eq!(elections.election_count(), 1);
        assert!(elections
            .install_authority(second, HashMap::from([(next, 100_000)]), 6700)
            .is_err());
        assert!(elections
            .set_authority(HashMap::from([(old, 100_000)]))
            .is_err());
        assert_eq!(
            elections
                .get_election(&root)
                .unwrap()
                .confirmation_threshold(),
            80_000
        );
    }

    #[test]
    fn invalid_certified_authority_preserves_current_elections() {
        let voter = WalletAddress::new("brst_human");
        let mut elections = ActiveElections::new(10, 0);
        let epoch = BlockHash::new([1; 32]);
        elections
            .install_authority(epoch, HashMap::from([(voter.clone(), 10_000)]), 6700)
            .unwrap();
        elections
            .register_candidate(epoch, epoch, Timestamp::new(1))
            .unwrap();
        for (next, weight, quorum) in [
            (BlockHash::ZERO, 10_000, 6700),
            (BlockHash::new([2; 32]), 1, 6700),
            (BlockHash::new([2; 32]), 10_000, 6666),
            (BlockHash::new([2; 32]), 0, 6700),
        ] {
            assert!(elections
                .install_authority(next, HashMap::from([(voter.clone(), weight)]), quorum)
                .is_err());
            assert_eq!(elections.authority_epoch(), Some(epoch));
            assert_eq!(elections.candidate_root(&epoch), Some(epoch));
        }
    }

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
    fn frozen_authority_ignores_claimed_weights_attendance_and_later_delegation() {
        let alice = make_voter("alice");
        let bob = make_voter("bob");
        let carol = make_voter("carol");
        let mallory = make_voter("mallory");
        let mut elections = ActiveElections::new(10, 0);
        elections
            .set_authority(HashMap::from([
                (alice.clone(), 10_000),
                (bob.clone(), 10_000),
                (carol.clone(), 10_000),
            ]))
            .unwrap();
        let root = make_hash(1);
        let block = make_hash(2);
        elections.register_candidate(root, block, ts(100)).unwrap();
        elections.set_online_weight(1);
        elections
            .set_authority(HashMap::from([(mallory.clone(), 30_000)]))
            .unwrap();
        assert!(elections
            .process_vote(&root, &mallory, block, u128::MAX, true, ts(101))
            .unwrap()
            .is_none());
        assert!(elections
            .process_vote(&root, &alice, block, u128::MAX, true, ts(102))
            .unwrap()
            .is_none());
        assert!(elections
            .process_vote(&root, &bob, block, 0, true, ts(102))
            .unwrap()
            .is_none());
        assert_eq!(
            elections.get_election(&root).unwrap().final_tally[&block],
            20_000
        );
        assert_eq!(
            elections
                .get_election(&root)
                .unwrap()
                .confirmation_threshold(),
            20_100
        );
        assert!(elections
            .process_vote(&root, &carol, block, 0, true, ts(102))
            .unwrap()
            .is_some());
        let next = make_hash(3);
        elections
            .register_candidate(next, make_hash(4), ts(103))
            .unwrap();
        assert_eq!(
            elections
                .get_election(&next)
                .unwrap()
                .confirmation_threshold(),
            20_100
        );
        assert!(elections
            .process_vote(&next, &alice, make_hash(4), u128::MAX, true, ts(104))
            .unwrap()
            .is_none());
        assert!(elections
            .process_vote(&next, &mallory, make_hash(4), 0, true, ts(104))
            .unwrap()
            .is_some());
        elections.cleanup_confirmed();
        assert!(elections.electorates.is_empty());
    }

    #[test]
    fn empty_or_overflowing_authority_cannot_manufacture_a_quorum() {
        let mut elections = ActiveElections::new(10, 1);
        assert!(elections
            .set_authority(HashMap::from([
                (make_voter("a"), u128::MAX),
                (make_voter("b"), 1)
            ]))
            .is_err());
        elections.set_authority(HashMap::new()).unwrap();
        elections
            .register_candidate(make_hash(1), make_hash(2), ts(100))
            .unwrap();
        assert!(elections
            .process_vote(
                &make_hash(1),
                &make_voter("a"),
                make_hash(2),
                u128::MAX,
                true,
                ts(101)
            )
            .unwrap()
            .is_none());
    }

    #[test]
    fn duplicate_election_is_noop() {
        let mut ae = ActiveElections::new(10, 1000);
        ae.start_election(make_hash(1), ts(100)).unwrap();
        ae.start_election(make_hash(1), ts(101)).unwrap();
        assert_eq!(ae.election_count(), 1);
    }

    #[test]
    fn competing_candidates_share_one_election_even_at_capacity() {
        let mut ae = ActiveElections::new(1, 3);
        let root = make_hash(1);
        ae.register_candidate(root, make_hash(2), ts(100)).unwrap();
        ae.register_candidate(root, make_hash(3), ts(101)).unwrap();
        assert_eq!(ae.election_count(), 1);
        assert_eq!(ae.candidate_root(&make_hash(2)), Some(root));
        assert_eq!(ae.candidate_root(&make_hash(3)), Some(root));
        assert_eq!(ae.candidate_root(&make_hash(9)), None);
        assert!(ae
            .register_candidate(make_hash(4), make_hash(2), ts(101))
            .is_err());
        assert!(ae
            .register_candidate(make_hash(4), make_hash(5), ts(101))
            .is_err());
        assert_eq!(ae.candidate_root(&make_hash(5)), None);
        ae.process_vote(&root, &make_voter("alice"), make_hash(2), 1, true, ts(102))
            .unwrap();
        assert!(ae
            .process_vote(&root, &make_voter("alice"), make_hash(3), 1, true, ts(103))
            .is_err());
        assert_eq!(
            ae.get_election(&root)
                .unwrap()
                .final_tally
                .get(&make_hash(3)),
            None
        );
    }

    #[test]
    fn candidate_routes_are_bounded_and_removed_with_elections() {
        let mut ae = ActiveElections::new(1, 1);
        let root = make_hash(1);
        for n in 2..18 {
            ae.register_candidate(root, make_hash(n), ts(100)).unwrap();
        }
        assert!(ae.register_candidate(root, make_hash(18), ts(100)).is_err());
        ae.register_candidate(root, make_hash(2), ts(100)).unwrap();
        ae.cleanup_expired(1000, ts(102));
        assert!(ae.candidate_roots.is_empty());
        assert!(ae.candidate_counts.is_empty());
        ae.register_candidate(root, make_hash(2), ts(103)).unwrap();
        ae.process_vote(&root, &make_voter("alice"), make_hash(2), 1, true, ts(104))
            .unwrap();
        ae.cleanup_confirmed();
        assert!(ae.candidate_roots.is_empty());
        assert!(ae.candidate_counts.is_empty());
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
    fn soft_quorum_needing_final_flags_then_clears() {
        let mut ae = ActiveElections::new(10, 1000);
        let root = make_hash(1);
        let block = make_hash(2);
        let our_rep = make_voter("self");
        ae.start_election(root, ts(100)).unwrap();

        // No votes → nothing at soft quorum.
        assert!(ae.soft_quorum_needing_final(&our_rep).is_empty());

        // A non-final supermajority (700 ≥ 670 margin over 0) → soft quorum,
        // and we haven't cast a final vote → flagged for us to finalize.
        ae.process_vote(&root, &make_voter("alice"), block, 700, false, ts(101))
            .unwrap();
        assert_eq!(ae.soft_quorum_needing_final(&our_rep), vec![(root, block)]);

        // We cast a small FINAL vote (100 < 670 → does NOT confirm), but now we
        // HAVE final-voted → no longer flagged (excluded by the is_final check,
        // not by confirmation).
        ae.process_vote(&root, &our_rep, block, 100, true, ts(102))
            .unwrap();
        assert!(!ae.get_election(&root).unwrap().is_confirmed());
        assert!(ae.soft_quorum_needing_final(&our_rep).is_empty());
    }

    #[test]
    fn fork_resolution_confirms_accepted_and_identifies_loser() {
        // End-to-end ORV fork resolution at the election layer: an attacker
        // votes for a fork block; we vote for the block we accepted with
        // majority weight; the accepted block confirms on final votes and the
        // fork is identified as the rollback loser.
        let mut ae = ActiveElections::new(10, 1000);
        let root = make_hash(1);
        let accepted = make_hash(2);
        let fork = make_hash(3);
        let our_rep = make_voter("self");
        ae.start_election(root, ts(100)).unwrap();

        // Minority vote for the fork; our majority (soft) vote for accepted.
        ae.process_vote(&root, &make_voter("attacker"), fork, 100, false, ts(101))
            .unwrap();
        ae.process_vote(&root, &our_rep, accepted, 800, false, ts(102))
            .unwrap();

        // Accepted leads by 700 ≥ 670 → soft quorum; we haven't finalized yet.
        assert_eq!(
            ae.soft_quorum_needing_final(&our_rep),
            vec![(root, accepted)]
        );

        // The confirmation task's job: finalize. 800 final ≥ 670 → confirm.
        let status = ae
            .process_vote(&root, &our_rep, accepted, 800, true, ts(103))
            .unwrap()
            .expect("accepted block should confirm on final quorum");
        assert_eq!(status.winner, accepted);

        // The fork is the loser (rollback candidate); the accepted block is not.
        let losers = ae.get_fork_losers(&root);
        assert!(
            losers.contains(&fork),
            "fork block must be a rollback loser"
        );
        assert!(
            !losers.contains(&accepted),
            "accepted block must not be rolled back"
        );
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
        assert!(matches!(
            result,
            Err(ConsensusError::ElectionAlreadyConfirmed)
        ));
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
        assert!(matches!(
            result,
            Err(ConsensusError::FinalVoteAlreadyCast(_))
        ));
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
