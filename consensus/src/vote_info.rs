//! Vote data — represents a single representative's vote on a block.

use burst_types::{BlockHash, Timestamp, WalletAddress};
use serde::{Deserialize, Serialize};

/// The result of processing a vote in an election.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VoteResult {
    /// Vote was accepted (new voter, or first vote).
    Accepted,
    /// Vote replaced a previous non-final vote (re-vote).
    Updated,
    /// Vote was ignored (duplicate, or lower sequence, or election already confirmed).
    Ignored,
    /// Vote processing encountered an error.
    Error(String),
}

/// Information about a single vote cast by a representative.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoteInfo {
    /// The representative who cast this vote.
    pub voter: WalletAddress,
    /// The block hash being voted for.
    pub block_hash: BlockHash,
    /// Voting weight of this representative.
    pub weight: u128,
    /// Whether this is a final vote (cannot be changed once cast).
    pub is_final: bool,
    /// When the vote was cast.
    pub timestamp: Timestamp,
    /// Monotonically increasing sequence number per voter.
    pub sequence: u64,
}

impl VoteInfo {
    pub fn new(
        voter: WalletAddress,
        block_hash: BlockHash,
        weight: u128,
        is_final: bool,
        timestamp: Timestamp,
        sequence: u64,
    ) -> Self {
        Self {
            voter,
            block_hash,
            weight,
            is_final,
            timestamp,
            sequence,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_voter(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{name}"))
    }

    fn make_hash(byte: u8) -> BlockHash {
        BlockHash::new([byte; 32])
    }

    #[test]
    fn vote_info_creation() {
        let voter = make_voter("alice");
        let hash = make_hash(1);
        let info = VoteInfo::new(voter.clone(), hash, 100, false, Timestamp::new(1000), 1);

        assert_eq!(info.voter, voter);
        assert_eq!(info.block_hash, hash);
        assert_eq!(info.weight, 100);
        assert!(!info.is_final);
        assert_eq!(info.timestamp, Timestamp::new(1000));
        assert_eq!(info.sequence, 1);
    }

    #[test]
    fn vote_result_variants() {
        assert_eq!(VoteResult::Accepted, VoteResult::Accepted);
        assert_eq!(VoteResult::Updated, VoteResult::Updated);
        assert_eq!(VoteResult::Ignored, VoteResult::Ignored);
        assert_eq!(
            VoteResult::Error("test".to_string()),
            VoteResult::Error("test".to_string())
        );
        assert_ne!(VoteResult::Accepted, VoteResult::Updated);
    }
}
