//! Election voting and final-vote confirmation for competing blocks at one root.

use crate::vote_info::{VoteInfo, VoteResult};
use burst_types::{BlockHash, Timestamp, WalletAddress};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Confirmation threshold: 67% expressed as basis points (6700 / 10000).
const QUORUM_BPS: u128 = 6700;
const BPS_DENOMINATOR: u128 = 10_000;

/// Maximum age of an election (in seconds) before new votes are rejected.
const MAX_ELECTION_AGE_SECS: u64 = 300;

/// The lifecycle state of an election.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ElectionState {
    /// Created, waiting for votes to arrive.
    Passive,
    /// Actively soliciting votes from representatives.
    Active,
    /// Reached the final-vote confirmation threshold. Terminal state.
    Confirmed,
    /// Timed out without reaching confirmation. Terminal state.
    Expired,
}

/// Summary of a confirmed election.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ElectionStatus {
    /// The winning block hash.
    pub winner: BlockHash,
    /// The weight tally of the winning block at confirmation time.
    pub tally: u128,
    /// The weight of final votes for the winning block at confirmation.
    pub final_tally: u128,
    /// How long the election took, in milliseconds.
    pub election_duration_ms: u64,
}

/// A single consensus election for a root block.
///
/// Tracks votes from representatives and determines when a block reaches the
/// configured final-vote confirmation threshold. The caller authenticates votes,
/// supplies electorate weights, and ensures candidates belong to this root.
#[derive(Clone, Debug)]
pub struct Election {
    /// Caller-supplied conflict root. The node derives it from account and previous hash.
    pub id: BlockHash,
    /// Current lifecycle state.
    pub state: ElectionState,
    /// Latest vote per representative (keyed by voter address).
    pub last_votes: HashMap<WalletAddress, VoteInfo>,
    /// Combined soft and final vote weight per block. The lead over the next
    /// candidate determines whether final voting can begin.
    pub tally: HashMap<BlockHash, u128>,
    /// Final vote weight per block. Only this tally can confirm the election;
    /// durable cementation is handled by the node.
    pub final_tally: HashMap<BlockHash, u128>,
    /// When the election was created.
    pub created_at: Timestamp,
    /// When the state last changed.
    pub state_changed_at: Timestamp,
    /// Final-vote weight needed to confirm.
    confirmation_threshold: u128,
}

impl Election {
    /// Create a new election in the Passive state.
    ///
    /// Sets the threshold to 67% of the supplied electorate weight, rounded up.
    /// This helper does not discover online peers or authenticate the electorate.
    pub fn new(root: BlockHash, online_weight: u128, now: Timestamp) -> Self {
        Self::with_quorum(root, online_weight, now, QUORUM_BPS as u16)
    }

    pub(crate) fn with_quorum(
        root: BlockHash,
        weight: u128,
        now: Timestamp,
        quorum_bps: u16,
    ) -> Self {
        let quorum = u128::from(quorum_bps);
        let confirmation_threshold = (weight / BPS_DENOMINATOR) * quorum
            + ((weight % BPS_DENOMINATOR) * quorum).div_ceil(BPS_DENOMINATOR);

        Self {
            id: root,
            state: ElectionState::Passive,
            last_votes: HashMap::new(),
            tally: HashMap::new(),
            final_tally: HashMap::new(),
            created_at: now,
            state_changed_at: now,
            confirmation_threshold,
        }
    }

    /// Process a vote from a representative.
    ///
    /// Rules:
    /// - If the election is already confirmed or expired, the vote is ignored.
    /// - If the voter already cast a final vote, the new vote is rejected.
    /// - If the voter already cast a non-final vote, it can be replaced (re-vote).
    /// - Final votes cannot be changed once cast.
    pub fn vote(
        &mut self,
        voter: &WalletAddress,
        block: BlockHash,
        weight: u128,
        is_final: bool,
        now: Timestamp,
    ) -> VoteResult {
        if !is_final
            && self
                .last_votes
                .get(voter)
                .is_some_and(|old| now <= old.timestamp)
        {
            return VoteResult::Ignored;
        }
        let sequence = self
            .last_votes
            .get(voter)
            .map_or(1, |old| old.sequence.saturating_add(1));
        self.vote_signed(
            VoteInfo::new(voter.clone(), block, weight, is_final, now, sequence),
            now,
        )
    }

    /// The caller verifies signatures, assigns weight from the frozen electorate,
    /// and validates candidate membership. Soft votes are ordered by their signed
    /// sequence. A final vote supersedes a soft vote regardless of sequence, but
    /// no vote can replace an existing final vote.
    pub fn vote_signed(&mut self, vote: VoteInfo, received_at: Timestamp) -> VoteResult {
        let voter = &vote.voter;
        let block = vote.block_hash;
        let weight = vote.weight;
        let is_final = vote.is_final;
        let now = received_at;
        if vote.sequence == 0 {
            return VoteResult::Ignored;
        }
        if weight == 0 {
            return VoteResult::Ignored;
        }
        if self.state == ElectionState::Confirmed {
            return VoteResult::Ignored;
        }
        if self.state == ElectionState::Expired {
            return VoteResult::Ignored;
        }

        let election_age_secs = now.as_secs().saturating_sub(self.created_at.as_secs());
        if election_age_secs > MAX_ELECTION_AGE_SECS {
            return VoteResult::Ignored;
        }

        if let Some(existing) = self.last_votes.get(voter) {
            if existing.is_final {
                return VoteResult::Error(format!("final vote already cast by {}", voter.as_str()));
            }

            // A final vote supersedes any soft vote. A prior final vote is
            // immutable, regardless of timestamp or sequence.
            if !is_final && vote.sequence <= existing.sequence {
                return VoteResult::Ignored;
            }

            // Re-vote: subtract the old vote's weight from its block tally
            let old_block = existing.block_hash;
            let old_weight = existing.weight;
            if let Some(w) = self.tally.get_mut(&old_block) {
                *w = w.saturating_sub(old_weight);
                if *w == 0 {
                    self.tally.remove(&old_block);
                }
            }

            // Record the new vote. The old vote was necessarily non-final (a
            // final vote can't be re-cast — guarded above), so only the
            // combined `tally` needed the subtraction; `final_tally` gains the
            // new weight iff this vote is final.
            self.last_votes.insert(voter.clone(), vote);
            *self.tally.entry(block).or_insert(0) += weight;
            if is_final {
                *self.final_tally.entry(block).or_insert(0) += weight;
            }

            // Transition from Passive to Active on first vote activity
            if self.state == ElectionState::Passive {
                self.state = ElectionState::Active;
                self.state_changed_at = now;
            }

            VoteResult::Updated
        } else {
            // First vote from this representative
            self.last_votes.insert(voter.clone(), vote);
            *self.tally.entry(block).or_insert(0) += weight;
            if is_final {
                *self.final_tally.entry(block).or_insert(0) += weight;
            }

            if self.state == ElectionState::Passive {
                self.state = ElectionState::Active;
                self.state_changed_at = now;
            }

            VoteResult::Accepted
        }
    }

    /// Check if any block has reached the confirmation threshold.
    ///
    /// If so, transitions the election to Confirmed and returns the status.
    /// Returns `None` if no block has reached quorum or the election is terminal.
    /// This method does not check elapsed time; the caller schedules expiration.
    pub fn try_confirm(&mut self, now: Timestamp) -> Option<ElectionStatus> {
        // An empty electorate cannot confirm a block.
        if self.confirmation_threshold == 0 {
            return None;
        }
        if self.state == ElectionState::Confirmed {
            return None;
        }
        if self.state == ElectionState::Expired {
            return None;
        }

        // Soft votes remain replaceable and cannot establish confirmation.
        let (winner, winner_final_tally) = self.leading_final_block()?;

        if winner_final_tally >= self.confirmation_threshold {
            self.state = ElectionState::Confirmed;
            self.state_changed_at = now;

            let duration_ms = now
                .as_secs()
                .saturating_sub(self.created_at.as_secs())
                .saturating_mul(1000);

            // Report the winner's total tally alongside its (confirming) final tally.
            let winner_total = self
                .tally
                .get(&winner)
                .copied()
                .unwrap_or(winner_final_tally);

            Some(ElectionStatus {
                winner,
                tally: winner_total,
                final_tally: winner_final_tally,
                election_duration_ms: duration_ms,
            })
        } else {
            None
        }
    }

    /// Whether the leading combined tally exceeds the runner-up by at least the
    /// confirmation threshold. The node uses this margin to consider final voting;
    /// it does not confirm the election or check its lifecycle state.
    pub fn has_quorum(&self) -> bool {
        if self.confirmation_threshold == 0 {
            return false;
        }
        let (mut first, mut second) = (0, 0);
        for &weight in self.tally.values() {
            if weight >= first {
                second = first;
                first = weight;
            } else if weight > second {
                second = weight;
            }
        }
        first.saturating_sub(second) >= self.confirmation_threshold
    }

    /// Check if the election has timed out.
    ///
    /// Converts elapsed whole seconds to milliseconds and expires the election
    /// when that duration reaches `timeout_ms`. Terminal elections are unchanged.
    pub fn check_timeout(&mut self, timeout_ms: u64, now: Timestamp) -> bool {
        if self.state == ElectionState::Confirmed || self.state == ElectionState::Expired {
            return false;
        }

        let elapsed_ms = now
            .as_secs()
            .saturating_sub(self.created_at.as_secs())
            .saturating_mul(1000);

        if elapsed_ms >= timeout_ms {
            self.state = ElectionState::Expired;
            self.state_changed_at = now;
            true
        } else {
            false
        }
    }

    /// Whether the election has been confirmed.
    pub fn is_confirmed(&self) -> bool {
        self.state == ElectionState::Confirmed
    }

    /// Whether the election has expired.
    pub fn is_expired(&self) -> bool {
        self.state == ElectionState::Expired
    }

    /// Returns the block with the most voting weight (all votes), along with
    /// its combined tally. Equal weights have no deterministic hash tie-break.
    pub fn leading_block(&self) -> Option<(BlockHash, u128)> {
        self.tally
            .iter()
            .max_by_key(|(_, w)| *w)
            .map(|(hash, w)| (*hash, *w))
    }

    /// Returns the block with the most final voting weight. Equal weights have
    /// no deterministic hash tie-break. Confirmation uses this final tally.
    pub fn leading_final_block(&self) -> Option<(BlockHash, u128)> {
        self.final_tally
            .iter()
            .max_by_key(|(_, w)| *w)
            .map(|(hash, w)| (*hash, *w))
    }

    /// Returns the confirmation threshold for this election.
    pub fn confirmation_threshold(&self) -> u128 {
        self.confirmation_threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticated_sequence_orders_soft_votes_even_with_identical_or_reversed_clocks() {
        let voter = WalletAddress::new("brst_rep");
        let a = BlockHash::new([1; 32]);
        let b = BlockHash::new([2; 32]);
        let now = Timestamp::new(100);
        let mut election = Election::new(a, 30_000, now);
        let vote = |hash, sequence, time, final_vote| {
            VoteInfo::new(
                voter.clone(),
                hash,
                10_000,
                final_vote,
                Timestamp::new(time),
                sequence,
            )
        };
        assert_eq!(
            election.vote_signed(vote(a, 10, 500, false), now),
            VoteResult::Accepted
        );
        assert_eq!(
            election.vote_signed(vote(b, 11, 400, false), now),
            VoteResult::Updated
        );
        assert_eq!(
            election.vote_signed(vote(a, 10, 500, false), Timestamp::new(101)),
            VoteResult::Ignored
        );
        assert_eq!(
            election.vote_signed(vote(a, 11, 600, false), Timestamp::new(102)),
            VoteResult::Ignored
        );
        assert_eq!(election.last_votes[&voter].block_hash, b);
        assert_eq!(election.last_votes[&voter].sequence, 11);
        assert_eq!(election.tally.get(&a), None);
        assert_eq!(election.tally.get(&b), Some(&10_000));
        assert_eq!(
            election.vote_signed(vote(b, 12, 400, true), now),
            VoteResult::Updated
        );
        assert!(matches!(
            election.vote_signed(vote(a, u64::MAX, 900, true), now),
            VoteResult::Error(_)
        ));
        assert_eq!(election.final_tally.get(&b), Some(&10_000));
    }

    #[test]
    fn final_vote_in_same_second_as_soft_vote_is_not_lost_or_double_counted() {
        let voter = WalletAddress::new("brst_voter");
        let root = BlockHash::new([1; 32]);
        let block = BlockHash::new([2; 32]);
        let time = Timestamp::new(100);
        let mut election = Election::new(root, 10_000, time);
        election.vote(&voter, block, 10_000, false, time);
        assert!(election.try_confirm(time).is_none());
        election.vote(&voter, block, 10_000, true, time);
        assert_eq!(election.tally[&block], 10_000);
        assert_eq!(election.final_tally[&block], 10_000);
        assert!(election.try_confirm(time).is_some());
        election.vote(&voter, block, 10_000, true, time);
        assert_eq!(election.final_tally[&block], 10_000);
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
    fn new_election_is_passive() {
        let e = Election::new(make_hash(1), 1000, ts(100));
        assert_eq!(e.state, ElectionState::Passive);
        assert_eq!(e.id, make_hash(1));
        assert!(e.last_votes.is_empty());
        assert!(e.tally.is_empty());
        // 67% of 1000 = 670
        assert_eq!(e.confirmation_threshold(), 670);
    }

    #[test]
    fn quorum_rounds_up_without_overflow() {
        for (weight, threshold) in [(0, 0), (1, 1), (2, 2), (3, 3), (100, 67), (101, 68)] {
            assert_eq!(
                Election::new(make_hash(1), weight, ts(100)).confirmation_threshold(),
                threshold
            );
        }
        let large = Election::new(make_hash(1), u128::MAX, ts(100)).confirmation_threshold();
        assert!(large > u128::MAX / 2);
        assert!(large < u128::MAX);
    }

    #[test]
    fn first_vote_transitions_to_active() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        let result = e.vote(&make_voter("alice"), make_hash(2), 100, false, ts(101));

        assert_eq!(result, VoteResult::Accepted);
        assert_eq!(e.state, ElectionState::Active);
        assert_eq!(e.tally.get(&make_hash(2)), Some(&100));
    }

    #[test]
    fn multiple_votes_accumulate_tally() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        e.vote(&make_voter("alice"), make_hash(2), 300, false, ts(101));
        e.vote(&make_voter("bob"), make_hash(2), 400, false, ts(102));

        assert_eq!(e.tally.get(&make_hash(2)), Some(&700));
        assert_eq!(e.leading_block(), Some((make_hash(2), 700)));
    }

    #[test]
    fn non_final_vote_can_be_updated() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        e.vote(&make_voter("alice"), make_hash(2), 300, false, ts(101));

        // Alice changes her vote from block 2 to block 3
        let result = e.vote(&make_voter("alice"), make_hash(3), 300, false, ts(102));

        assert_eq!(result, VoteResult::Updated);
        // Block 2 should have been removed (tally dropped to 0)
        assert!(!e.tally.contains_key(&make_hash(2)));
        assert_eq!(e.tally.get(&make_hash(3)), Some(&300));
        // Sequence should have incremented
        assert_eq!(e.last_votes.get(&make_voter("alice")).unwrap().sequence, 2);
    }

    #[test]
    fn final_vote_cannot_be_changed() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        e.vote(&make_voter("alice"), make_hash(2), 300, true, ts(101));

        let result = e.vote(&make_voter("alice"), make_hash(3), 300, false, ts(102));

        match result {
            VoteResult::Error(msg) => assert!(msg.contains("final vote already cast")),
            other => panic!("expected Error, got {:?}", other),
        }
        // Original vote should remain
        assert_eq!(e.tally.get(&make_hash(2)), Some(&300));
    }

    #[test]
    fn non_final_upgraded_to_final() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        e.vote(&make_voter("alice"), make_hash(2), 300, false, ts(101));
        let result = e.vote(&make_voter("alice"), make_hash(2), 300, true, ts(102));

        assert_eq!(result, VoteResult::Updated);
        assert!(e.last_votes.get(&make_voter("alice")).unwrap().is_final);
    }

    #[test]
    fn try_confirm_reaches_quorum() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        // Need 670 to confirm
        e.vote(&make_voter("alice"), make_hash(2), 400, true, ts(101));
        assert!(e.try_confirm(ts(102)).is_none());

        e.vote(&make_voter("bob"), make_hash(2), 300, true, ts(103));
        // Now at 700 >= 670
        let status = e.try_confirm(ts(104)).expect("should confirm");

        assert_eq!(status.winner, make_hash(2));
        assert_eq!(status.tally, 700);
        assert_eq!(e.state, ElectionState::Confirmed);
    }

    #[test]
    fn try_confirm_returns_none_when_already_confirmed() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        e.vote(&make_voter("alice"), make_hash(2), 700, true, ts(101));
        e.try_confirm(ts(102));

        assert!(e.try_confirm(ts(103)).is_none());
    }

    #[test]
    fn votes_ignored_on_confirmed_election() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        e.vote(&make_voter("alice"), make_hash(2), 700, true, ts(101));
        e.try_confirm(ts(102));

        let result = e.vote(&make_voter("bob"), make_hash(3), 200, false, ts(103));
        assert_eq!(result, VoteResult::Ignored);
    }

    #[test]
    fn check_timeout_expires_election() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        e.vote(&make_voter("alice"), make_hash(2), 100, false, ts(101));

        // 30 seconds = 30_000ms timeout, now at 100 + 31 = 131
        assert!(!e.check_timeout(30_000, ts(120))); // only 20s elapsed
        assert!(e.check_timeout(30_000, ts(131))); // 31s elapsed >= 30s
        assert_eq!(e.state, ElectionState::Expired);
    }

    #[test]
    fn check_timeout_noop_on_confirmed() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        e.vote(&make_voter("alice"), make_hash(2), 700, true, ts(101));
        e.try_confirm(ts(102));

        assert!(!e.check_timeout(1, ts(200)));
        assert_eq!(e.state, ElectionState::Confirmed);
    }

    #[test]
    fn votes_ignored_on_expired_election() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        e.check_timeout(1, ts(200)); // Force expire

        let result = e.vote(&make_voter("alice"), make_hash(2), 500, false, ts(201));
        assert_eq!(result, VoteResult::Ignored);
    }

    #[test]
    fn leading_block_returns_highest_tally() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        e.vote(&make_voter("alice"), make_hash(2), 300, false, ts(101));
        e.vote(&make_voter("bob"), make_hash(3), 500, false, ts(102));

        let (leader, weight) = e.leading_block().unwrap();
        assert_eq!(leader, make_hash(3));
        assert_eq!(weight, 500);
    }

    #[test]
    fn leading_block_none_on_empty() {
        let e = Election::new(make_hash(1), 1000, ts(100));
        assert!(e.leading_block().is_none());
    }

    #[test]
    fn competing_blocks_tracked_separately() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        e.vote(&make_voter("alice"), make_hash(2), 300, false, ts(101));
        e.vote(&make_voter("bob"), make_hash(3), 200, false, ts(102));
        e.vote(&make_voter("carol"), make_hash(2), 100, false, ts(103));

        assert_eq!(e.tally.get(&make_hash(2)), Some(&400));
        assert_eq!(e.tally.get(&make_hash(3)), Some(&200));
    }

    #[test]
    fn re_vote_to_different_block() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        e.vote(&make_voter("alice"), make_hash(2), 300, false, ts(101));
        e.vote(&make_voter("bob"), make_hash(3), 400, false, ts(102));

        // Bob switches from block 3 to block 2
        let result = e.vote(&make_voter("bob"), make_hash(2), 400, false, ts(103));
        assert_eq!(result, VoteResult::Updated);

        assert_eq!(e.tally.get(&make_hash(2)), Some(&700));
        assert!(!e.tally.contains_key(&make_hash(3)));
    }

    #[test]
    fn election_duration_calculated_correctly() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        e.vote(&make_voter("alice"), make_hash(2), 700, true, ts(105));
        let status = e.try_confirm(ts(110)).unwrap();

        // 110 - 100 = 10 seconds = 10_000 ms
        assert_eq!(status.election_duration_ms, 10_000);
    }

    #[test]
    fn zero_online_weight_election() {
        let mut e = Election::new(make_hash(1), 0, ts(100));
        // No eligible electorate must fail closed, even with an inconsistent
        // positive-weight vote supplied by the caller.
        assert_eq!(e.confirmation_threshold(), 0);
        e.vote(&make_voter("alice"), make_hash(2), 1, true, ts(101));
        assert!(!e.has_quorum());
        assert!(e.try_confirm(ts(102)).is_none());
    }

    #[test]
    fn weightless_votes_do_not_enter_tallies() {
        for online in [0, 10_000] {
            let mut e = Election::new(make_hash(1), online, ts(100));
            assert_eq!(
                e.vote(&make_voter("unverified"), make_hash(2), 0, true, ts(101)),
                VoteResult::Ignored
            );
            assert!(e.last_votes.is_empty());
            assert!(e.tally.is_empty());
            assert!(e.final_tally.is_empty());
            assert!(!e.has_quorum());
            assert!(e.try_confirm(ts(102)).is_none());
        }
    }

    #[test]
    fn re_vote_with_different_weight() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        e.vote(&make_voter("alice"), make_hash(2), 300, false, ts(101));

        // Alice re-votes for the same block but with different weight
        let result = e.vote(&make_voter("alice"), make_hash(2), 500, false, ts(102));
        assert_eq!(result, VoteResult::Updated);
        assert_eq!(e.tally.get(&make_hash(2)), Some(&500));
    }

    // --- Quorum margin tests ---

    #[test]
    fn quorum_confirms_when_winner_exceeds_threshold() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));

        // Block A gets 680 >= threshold 670 → confirms regardless of runner-up
        e.vote(&make_voter("alice"), make_hash(2), 680, true, ts(101));
        e.vote(&make_voter("bob"), make_hash(3), 200, true, ts(102));

        let status = e
            .try_confirm(ts(103))
            .expect("should confirm at 68% quorum");
        assert_eq!(status.winner, make_hash(2));
        assert_eq!(status.tally, 680);
    }

    #[test]
    fn quorum_does_not_confirm_below_threshold() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));

        // Block A gets 660 < threshold 670 → does NOT confirm
        e.vote(&make_voter("alice"), make_hash(2), 660, true, ts(101));

        assert!(e.try_confirm(ts(102)).is_none());
        assert_ne!(e.state, ElectionState::Confirmed);
    }

    #[test]
    fn quorum_single_candidate_confirms_normally() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        e.vote(&make_voter("alice"), make_hash(2), 700, true, ts(101));

        let status = e
            .try_confirm(ts(102))
            .expect("single candidate should confirm");
        assert_eq!(status.winner, make_hash(2));
    }

    // Vote admission age limit.

    #[test]
    fn vote_rejected_after_max_election_age() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));

        // Vote at 100 + 301 = 401 seconds → age = 301 > 300
        let result = e.vote(&make_voter("alice"), make_hash(2), 500, false, ts(401));
        assert_eq!(result, VoteResult::Ignored);
        assert!(e.tally.is_empty());
    }

    #[test]
    fn vote_accepted_within_max_election_age() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));

        // Vote at 100 + 300 = 400 seconds → age = 300, NOT > 300
        let result = e.vote(&make_voter("alice"), make_hash(2), 500, false, ts(400));
        assert_eq!(result, VoteResult::Accepted);
        assert_eq!(e.tally.get(&make_hash(2)), Some(&500));
    }

    #[test]
    fn vote_rejected_just_past_max_election_age() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));

        // First vote within window
        let r1 = e.vote(&make_voter("alice"), make_hash(2), 500, false, ts(200));
        assert_eq!(r1, VoteResult::Accepted);

        // Second vote just past the 300s window
        let r2 = e.vote(&make_voter("bob"), make_hash(2), 300, false, ts(401));
        assert_eq!(r2, VoteResult::Ignored);
        // Only alice's vote should be tallied
        assert_eq!(e.tally.get(&make_hash(2)), Some(&500));
    }

    // --- Final-tally confirmation (soft votes must NOT cement) ---

    #[test]
    fn soft_votes_do_not_confirm_even_above_threshold() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        // 900 of soft (non-final) weight — well over the 670 threshold.
        e.vote(&make_voter("alice"), make_hash(2), 500, false, ts(101));
        e.vote(&make_voter("bob"), make_hash(2), 400, false, ts(102));

        // Combined tally is over threshold...
        assert_eq!(e.tally.get(&make_hash(2)), Some(&900));
        // ...but there are no FINAL votes, so it must NOT confirm.
        assert!(e.try_confirm(ts(103)).is_none());
        assert_ne!(e.state, ElectionState::Confirmed);
        assert!(e.final_tally.is_empty());
    }

    #[test]
    fn only_final_weight_counts_toward_confirmation() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        // 400 soft + 300 final on block 2 = 700 combined, but only 300 final.
        e.vote(&make_voter("alice"), make_hash(2), 400, false, ts(101));
        e.vote(&make_voter("bob"), make_hash(2), 300, true, ts(102));

        assert_eq!(e.tally.get(&make_hash(2)), Some(&700));
        assert_eq!(e.final_tally.get(&make_hash(2)), Some(&300));
        // 300 final < 670 → no confirmation despite 700 combined.
        assert!(e.try_confirm(ts(103)).is_none());

        // Carol adds 400 final → 700 final ≥ 670 → confirms.
        e.vote(&make_voter("carol"), make_hash(2), 400, true, ts(104));
        let status = e
            .try_confirm(ts(105))
            .expect("should confirm on final quorum");
        assert_eq!(status.winner, make_hash(2));
        assert_eq!(status.final_tally, 700);
    }

    #[test]
    fn soft_upgraded_to_final_then_confirms() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        e.vote(&make_voter("alice"), make_hash(2), 700, false, ts(101));
        assert!(e.try_confirm(ts(102)).is_none()); // soft only
        // Upgrade to final
        e.vote(&make_voter("alice"), make_hash(2), 700, true, ts(103));
        assert_eq!(e.final_tally.get(&make_hash(2)), Some(&700));
        assert!(e.try_confirm(ts(104)).is_some());
    }

    // --- has_quorum margin signal ---

    #[test]
    fn has_quorum_true_only_with_supermajority_margin() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        // Single leader with 700 vs nothing → margin 700 ≥ 670.
        e.vote(&make_voter("alice"), make_hash(2), 700, false, ts(101));
        assert!(e.has_quorum());
    }

    #[test]
    fn has_quorum_false_for_contested_forks() {
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        // Two forks near 50/50 — neither leads by the threshold margin.
        e.vote(&make_voter("alice"), make_hash(2), 500, false, ts(101));
        e.vote(&make_voter("bob"), make_hash(3), 480, false, ts(102));
        // margin = 20 < 670 → no soft quorum, so neither fork can go final.
        assert!(!e.has_quorum());
        // And of course nothing confirms.
        assert!(e.try_confirm(ts(103)).is_none());
    }

    #[test]
    fn has_quorum_matches_sorted_margin_at_ties_and_integer_boundaries() {
        let weights = [0, 1, 669, 670, 671, u128::MAX];
        for a in weights {
            for b in weights {
                for c in weights {
                    for d in weights {
                        let mut e = Election::new(make_hash(1), 1000, ts(100));
                        for (index, weight) in [a, b, c, d].into_iter().enumerate() {
                            e.tally.insert(make_hash(index as u8 + 2), weight);
                        }
                        let mut sorted = [a, b, c, d];
                        sorted.sort_unstable_by(|a, b| b.cmp(a));
                        assert_eq!(e.has_quorum(), sorted[0].saturating_sub(sorted[1]) >= 670);
                        // A soft margin never supplies final-vote authority.
                        assert!(e.try_confirm(ts(101)).is_none());
                    }
                }
            }
        }
        let mut e = Election::new(make_hash(1), 1000, ts(100));
        assert!(!e.has_quorum());
        e.tally.insert(make_hash(2), 670);
        assert!(e.has_quorum());
        e.tally.insert(make_hash(3), 1);
        assert!(!e.has_quorum());
        e.tally.insert(make_hash(3), 0);
        assert!(e.has_quorum());
        e.confirmation_threshold = 0;
        assert!(!e.has_quorum());
    }
}
