//! Election state machine — manages the lifecycle of a consensus election.
//!
//! Inspired by rsnano-node's election lifecycle. An election is created when a
//! fork is detected (two blocks sharing the same previous). Representatives
//! vote on which block to confirm. A block is confirmed when it accumulates
//! ≥ 67% of the total online voting weight.

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
    /// Reached quorum (≥ 67% of online weight). Terminal state.
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
    /// The final tally after all votes are counted.
    pub final_tally: u128,
    /// How long the election took, in milliseconds.
    pub election_duration_ms: u64,
}

/// A single consensus election for a root block.
///
/// Tracks votes from representatives and determines when a block reaches the
/// confirmation threshold (≥ 67% of total online voting weight).
#[derive(Clone, Debug)]
pub struct Election {
    /// The root block hash that triggered this election (e.g., the `previous` of forking blocks).
    pub id: BlockHash,
    /// Current lifecycle state.
    pub state: ElectionState,
    /// Latest vote per representative (keyed by voter address).
    pub last_votes: HashMap<WalletAddress, VoteInfo>,
    /// Per-block weight tally.
    pub tally: HashMap<BlockHash, u128>,
    /// When the election was created.
    pub created_at: Timestamp,
    /// When the state last changed.
    pub state_changed_at: Timestamp,
    /// Weight needed to confirm (67% of online weight).
    confirmation_threshold: u128,
}

impl Election {
    /// Create a new election in the Passive state.
    ///
    /// `online_weight` is the total voting weight of all online representatives.
    /// The confirmation threshold is set to 67% of that value.
    pub fn new(root: BlockHash, online_weight: u128, now: Timestamp) -> Self {
        let confirmation_threshold =
            online_weight.saturating_mul(QUORUM_BPS) / BPS_DENOMINATOR;

        Self {
            id: root,
            state: ElectionState::Passive,
            last_votes: HashMap::new(),
            tally: HashMap::new(),
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
        if self.state == ElectionState::Confirmed {
            return VoteResult::Ignored;
        }
        if self.state == ElectionState::Expired {
            return VoteResult::Ignored;
        }

        let election_age_secs = now
            .as_secs()
            .saturating_sub(self.created_at.as_secs());
        if election_age_secs > MAX_ELECTION_AGE_SECS {
            return VoteResult::Ignored;
        }

        if let Some(existing) = self.last_votes.get(voter) {
            if existing.is_final {
                return VoteResult::Error(format!(
                    "final vote already cast by {}",
                    voter.as_str()
                ));
            }

            // Replay protection: reject votes with timestamps not strictly newer
            if now.as_secs() <= existing.timestamp.as_secs() {
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

            let new_sequence = existing.sequence + 1;

            // Record the new vote
            let info = VoteInfo::new(
                voter.clone(),
                block,
                weight,
                is_final,
                now,
                new_sequence,
            );
            self.last_votes.insert(voter.clone(), info);
            *self.tally.entry(block).or_insert(0) += weight;

            // Transition from Passive to Active on first vote activity
            if self.state == ElectionState::Passive {
                self.state = ElectionState::Active;
                self.state_changed_at = now;
            }

            VoteResult::Updated
        } else {
            // First vote from this representative
            let info = VoteInfo::new(voter.clone(), block, weight, is_final, now, 1);
            self.last_votes.insert(voter.clone(), info);
            *self.tally.entry(block).or_insert(0) += weight;

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
    /// Returns `None` if no block has reached quorum yet.
    pub fn try_confirm(&mut self, now: Timestamp) -> Option<ElectionStatus> {
        if self.state == ElectionState::Confirmed {
            return None;
        }
        if self.state == ElectionState::Expired {
            return None;
        }

        let (winner, winner_tally) = self.leading_block()?;

        if winner_tally >= self.confirmation_threshold {
            self.state = ElectionState::Confirmed;
            self.state_changed_at = now;

            let duration_ms = now
                .as_secs()
                .saturating_sub(self.created_at.as_secs())
                .saturating_mul(1000);

            Some(ElectionStatus {
                winner,
                tally: winner_tally,
                final_tally: winner_tally,
                election_duration_ms: duration_ms,
            })
        } else {
            None
        }
    }

    /// Check if the election has timed out.
    ///
    /// If `now - created_at >= timeout_ms`, transitions to Expired and returns true.
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

    /// Returns the block with the most voting weight, along with its tally.
    pub fn leading_block(&self) -> Option<(BlockHash, u128)> {
        self.tally
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
        assert!(e.tally.get(&make_hash(2)).is_none());
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
        assert!(e.check_timeout(30_000, ts(131)));   // 31s elapsed >= 30s
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
        assert!(e.tally.get(&make_hash(3)).is_none());
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
        // Threshold is 0, so any vote should confirm
        assert_eq!(e.confirmation_threshold(), 0);
        e.vote(&make_voter("alice"), make_hash(2), 1, true, ts(101));
        let status = e.try_confirm(ts(102));
        assert!(status.is_some());
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

        let status = e.try_confirm(ts(103)).expect("should confirm at 68% quorum");
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

        let status = e.try_confirm(ts(102)).expect("single candidate should confirm");
        assert_eq!(status.winner, make_hash(2));
    }

    // --- Replay protection (election age) tests ---

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
}
