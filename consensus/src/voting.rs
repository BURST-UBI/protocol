//! Representative voting for conflict resolution.
//!
//! Tracks individual representative votes with support for final and non-final
//! votes. Final votes are locked and cannot be changed. Non-final votes can be
//! replaced (re-vote). The tally aggregates weight per block candidate.

use crate::representative::Representative;
use crate::vote_info::{VoteInfo, VoteResult};
use burst_types::{BlockHash, Timestamp, WalletAddress};
use std::collections::HashMap;

/// Manages representative votes for resolving block conflicts.
///
/// Unlike the old simple weight-accumulation approach, this tracks each
/// representative's individual vote, supports re-voting (non-final), and
/// locks final votes.
#[derive(Clone, Debug)]
pub struct RepresentativeVoting {
    /// Per-representative vote tracking (voter address -> vote info).
    votes: HashMap<WalletAddress, VoteInfo>,
    /// Total online voting weight.
    total_online_weight: u128,
}

impl RepresentativeVoting {
    pub fn new(total_online_weight: u128) -> Self {
        Self {
            votes: HashMap::new(),
            total_online_weight,
        }
    }

    /// Cast a vote from a representative.
    ///
    /// - New voters are accepted immediately.
    /// - Non-final votes can be replaced by subsequent votes.
    /// - Final votes are locked and reject further changes.
    pub fn cast_vote(
        &mut self,
        rep: &Representative,
        block: BlockHash,
        is_final: bool,
        now: Timestamp,
    ) -> VoteResult {
        let voter = &rep.address;

        if let Some(existing) = self.votes.get(voter) {
            if existing.is_final {
                return VoteResult::Error(format!("final vote already cast by {}", voter.as_str()));
            }

            let new_sequence = existing.sequence + 1;
            let info = VoteInfo::new(
                voter.clone(),
                block,
                rep.delegated_weight,
                is_final,
                now,
                new_sequence,
            );
            self.votes.insert(voter.clone(), info);
            VoteResult::Updated
        } else {
            let info = VoteInfo::new(voter.clone(), block, rep.delegated_weight, is_final, now, 1);
            self.votes.insert(voter.clone(), info);
            VoteResult::Accepted
        }
    }

    /// Compute the per-block weight tally from current votes.
    pub fn tally(&self) -> HashMap<BlockHash, u128> {
        let mut result: HashMap<BlockHash, u128> = HashMap::new();
        for vote in self.votes.values() {
            *result.entry(vote.block_hash).or_insert(0) += vote.weight;
        }
        result
    }

    /// Check if a block has reached the confirmation threshold (>= 67%).
    pub fn is_confirmed(&self, block: &BlockHash) -> bool {
        let tally = self.tally();
        let weight = tally.get(block).copied().unwrap_or(0);
        weight * 10_000 / self.total_online_weight.max(1) >= 6700
    }

    /// Get the winning block (if any has reached the confirmation threshold).
    pub fn winner(&self) -> Option<BlockHash> {
        let tally = self.tally();
        tally
            .iter()
            .find(|(hash, _)| self.is_confirmed_with_tally(hash, &tally))
            .map(|(hash, _)| *hash)
    }

    /// Get the vote info for a specific voter.
    pub fn get_vote(&self, voter: &WalletAddress) -> Option<&VoteInfo> {
        self.votes.get(voter)
    }

    /// Number of distinct voters.
    pub fn voter_count(&self) -> usize {
        self.votes.len()
    }

    fn is_confirmed_with_tally(&self, block: &BlockHash, tally: &HashMap<BlockHash, u128>) -> bool {
        let weight = tally.get(block).copied().unwrap_or(0);
        weight * 10_000 / self.total_online_weight.max(1) >= 6700
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rep(name: &str, weight: u128) -> Representative {
        Representative {
            address: WalletAddress::new(format!("brst_{name}")),
            delegated_weight: weight,
            online: true,
        }
    }

    fn make_hash(byte: u8) -> BlockHash {
        BlockHash::new([byte; 32])
    }

    fn ts(secs: u64) -> Timestamp {
        Timestamp::new(secs)
    }

    #[test]
    fn basic_vote_and_tally() {
        let mut voting = RepresentativeVoting::new(1000);
        let alice = make_rep("alice", 300);
        let bob = make_rep("bob", 400);

        voting.cast_vote(&alice, make_hash(1), false, ts(100));
        voting.cast_vote(&bob, make_hash(1), false, ts(101));

        let tally = voting.tally();
        assert_eq!(tally.get(&make_hash(1)), Some(&700));
    }

    #[test]
    fn non_final_vote_can_be_updated() {
        let mut voting = RepresentativeVoting::new(1000);
        let alice = make_rep("alice", 300);

        voting.cast_vote(&alice, make_hash(1), false, ts(100));
        let result = voting.cast_vote(&alice, make_hash(2), false, ts(101));
        assert_eq!(result, VoteResult::Updated);

        let tally = voting.tally();
        assert!(tally.get(&make_hash(1)).is_none() || *tally.get(&make_hash(1)).unwrap() == 0);
        assert_eq!(tally.get(&make_hash(2)), Some(&300));
    }

    #[test]
    fn final_vote_cannot_be_changed() {
        let mut voting = RepresentativeVoting::new(1000);
        let alice = make_rep("alice", 300);

        voting.cast_vote(&alice, make_hash(1), true, ts(100));
        let result = voting.cast_vote(&alice, make_hash(2), false, ts(101));
        assert!(matches!(result, VoteResult::Error(_)));

        // Original vote should remain
        let tally = voting.tally();
        assert_eq!(tally.get(&make_hash(1)), Some(&300));
    }

    #[test]
    fn confirmation_threshold() {
        let mut voting = RepresentativeVoting::new(1000);
        let alice = make_rep("alice", 670);

        voting.cast_vote(&alice, make_hash(1), false, ts(100));
        assert!(voting.is_confirmed(&make_hash(1)));
        assert_eq!(voting.winner(), Some(make_hash(1)));
    }

    #[test]
    fn below_threshold_not_confirmed() {
        let mut voting = RepresentativeVoting::new(1000);
        let alice = make_rep("alice", 669);

        voting.cast_vote(&alice, make_hash(1), false, ts(100));
        assert!(!voting.is_confirmed(&make_hash(1)));
        assert!(voting.winner().is_none());
    }

    #[test]
    fn competing_blocks() {
        let mut voting = RepresentativeVoting::new(1000);
        let alice = make_rep("alice", 400);
        let bob = make_rep("bob", 350);
        let carol = make_rep("carol", 250);

        voting.cast_vote(&alice, make_hash(1), false, ts(100));
        voting.cast_vote(&bob, make_hash(2), false, ts(101));
        voting.cast_vote(&carol, make_hash(1), false, ts(102));

        let tally = voting.tally();
        assert_eq!(tally.get(&make_hash(1)), Some(&650));
        assert_eq!(tally.get(&make_hash(2)), Some(&350));
    }

    #[test]
    fn voter_count() {
        let mut voting = RepresentativeVoting::new(1000);
        let alice = make_rep("alice", 300);
        let bob = make_rep("bob", 400);

        voting.cast_vote(&alice, make_hash(1), false, ts(100));
        voting.cast_vote(&bob, make_hash(1), false, ts(101));
        assert_eq!(voting.voter_count(), 2);

        // Re-vote doesn't add a new voter
        voting.cast_vote(&alice, make_hash(2), false, ts(102));
        assert_eq!(voting.voter_count(), 2);
    }

    #[test]
    fn get_vote_info() {
        let mut voting = RepresentativeVoting::new(1000);
        let alice = make_rep("alice", 300);

        voting.cast_vote(&alice, make_hash(1), true, ts(100));

        let info = voting.get_vote(&alice.address).unwrap();
        assert_eq!(info.block_hash, make_hash(1));
        assert_eq!(info.weight, 300);
        assert!(info.is_final);
        assert_eq!(info.sequence, 1);
    }

    #[test]
    fn sequence_increments_on_re_vote() {
        let mut voting = RepresentativeVoting::new(1000);
        let alice = make_rep("alice", 300);

        voting.cast_vote(&alice, make_hash(1), false, ts(100));
        voting.cast_vote(&alice, make_hash(2), false, ts(101));
        voting.cast_vote(&alice, make_hash(3), false, ts(102));

        let info = voting.get_vote(&alice.address).unwrap();
        assert_eq!(info.sequence, 3);
    }
}
