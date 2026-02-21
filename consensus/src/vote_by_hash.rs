//! Vote-by-hash — a compact vote message that references blocks by hash.
//!
//! Instead of including full block data in every vote message, representatives
//! send only the hashes of the blocks they're voting for. This reduces vote
//! message bandwidth by ~95%. The receiving node must already have the block
//! (or can request it separately).

use burst_types::{BlockHash, Signature, WalletAddress};
use serde::{Deserialize, Serialize};

/// A vote message that references blocks by hash instead of including full
/// block data. This is the primary vote encoding on the wire.
///
/// The signature covers `voter || timestamp || sequence || is_final || block_hashes...`
/// to prevent replay and tampering.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VoteByHash {
    /// The voting representative's address.
    pub voter: WalletAddress,
    /// Block hashes being voted for (one per election).
    pub block_hashes: Vec<BlockHash>,
    /// Whether this is a final (irrevocable) vote.
    pub is_final: bool,
    /// UNIX timestamp (seconds) when the vote was created.
    pub timestamp: u64,
    /// Monotonically increasing sequence number per voter.
    /// Higher sequence numbers supersede lower ones (for non-final votes).
    pub sequence: u64,
    /// Ed25519 signature over the vote contents.
    pub signature: Signature,
}

impl VoteByHash {
    /// Create a new vote-by-hash message (unsigned — caller must set signature).
    pub fn new(
        voter: WalletAddress,
        block_hashes: Vec<BlockHash>,
        is_final: bool,
        timestamp: u64,
        sequence: u64,
    ) -> Self {
        Self {
            voter,
            block_hashes,
            is_final,
            timestamp,
            sequence,
            signature: Signature([0u8; 64]),
        }
    }

    /// Compute the bytes that should be signed.
    ///
    /// Format: `voter_bytes || timestamp_le || sequence_le || is_final_byte || hash_1 || hash_2 || ...`
    pub fn signing_data(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(
            self.voter.as_str().len() + 8 + 8 + 1 + self.block_hashes.len() * 32,
        );
        data.extend_from_slice(self.voter.as_str().as_bytes());
        data.extend_from_slice(&self.timestamp.to_le_bytes());
        data.extend_from_slice(&self.sequence.to_le_bytes());
        data.push(if self.is_final { 1 } else { 0 });
        for hash in &self.block_hashes {
            data.extend_from_slice(hash.as_bytes());
        }
        data
    }

    /// Number of block hashes in this vote.
    pub fn hash_count(&self) -> usize {
        self.block_hashes.len()
    }

    /// Whether the vote contains any block hashes.
    pub fn is_empty(&self) -> bool {
        self.block_hashes.is_empty()
    }
}

impl PartialEq for VoteByHash {
    fn eq(&self, other: &Self) -> bool {
        self.voter == other.voter
            && self.block_hashes == other.block_hashes
            && self.is_final == other.is_final
            && self.timestamp == other.timestamp
            && self.sequence == other.sequence
    }
}

impl Eq for VoteByHash {}

#[cfg(test)]
mod tests {
    use super::*;

    fn voter(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{name}"))
    }

    fn hash(byte: u8) -> BlockHash {
        BlockHash::new([byte; 32])
    }

    #[test]
    fn create_vote() {
        let vote = VoteByHash::new(voter("alice"), vec![hash(1), hash(2)], false, 1000, 1);

        assert_eq!(vote.voter, voter("alice"));
        assert_eq!(vote.hash_count(), 2);
        assert!(!vote.is_final);
        assert_eq!(vote.timestamp, 1000);
        assert_eq!(vote.sequence, 1);
        assert!(!vote.is_empty());
    }

    #[test]
    fn empty_vote() {
        let vote = VoteByHash::new(voter("alice"), vec![], false, 1000, 1);
        assert!(vote.is_empty());
        assert_eq!(vote.hash_count(), 0);
    }

    #[test]
    fn final_vote() {
        let vote = VoteByHash::new(voter("alice"), vec![hash(1)], true, 1000, 5);
        assert!(vote.is_final);
        assert_eq!(vote.sequence, 5);
    }

    #[test]
    fn signing_data_deterministic() {
        let vote = VoteByHash::new(voter("alice"), vec![hash(1), hash(2)], true, 1000, 42);

        let data1 = vote.signing_data();
        let data2 = vote.signing_data();
        assert_eq!(data1, data2);
    }

    #[test]
    fn signing_data_different_for_different_votes() {
        let vote1 = VoteByHash::new(voter("alice"), vec![hash(1)], false, 1000, 1);
        let vote2 = VoteByHash::new(voter("alice"), vec![hash(2)], false, 1000, 1);
        let vote3 = VoteByHash::new(voter("alice"), vec![hash(1)], true, 1000, 1);
        let vote4 = VoteByHash::new(voter("bob"), vec![hash(1)], false, 1000, 1);
        let vote5 = VoteByHash::new(voter("alice"), vec![hash(1)], false, 1001, 1);
        let vote6 = VoteByHash::new(voter("alice"), vec![hash(1)], false, 1000, 2);

        let d1 = vote1.signing_data();
        assert_ne!(d1, vote2.signing_data()); // different hash
        assert_ne!(d1, vote3.signing_data()); // different is_final
        assert_ne!(d1, vote4.signing_data()); // different voter
        assert_ne!(d1, vote5.signing_data()); // different timestamp
        assert_ne!(d1, vote6.signing_data()); // different sequence
    }

    #[test]
    fn signing_data_includes_all_hashes() {
        let vote_1hash = VoteByHash::new(voter("alice"), vec![hash(1)], false, 0, 0);
        let vote_2hash = VoteByHash::new(voter("alice"), vec![hash(1), hash(2)], false, 0, 0);

        assert_ne!(vote_1hash.signing_data(), vote_2hash.signing_data());
        // Two hashes → signing data is 32 bytes longer
        assert_eq!(
            vote_2hash.signing_data().len() - vote_1hash.signing_data().len(),
            32
        );
    }

    #[test]
    fn equality_ignores_signature() {
        let mut vote1 = VoteByHash::new(voter("alice"), vec![hash(1)], false, 1000, 1);
        let mut vote2 = VoteByHash::new(voter("alice"), vec![hash(1)], false, 1000, 1);

        vote1.signature = Signature([1u8; 64]);
        vote2.signature = Signature([2u8; 64]);

        assert_eq!(vote1, vote2);
    }

    #[test]
    fn serde_roundtrip() {
        let vote = VoteByHash::new(
            voter("alice"),
            vec![hash(1), hash(2), hash(3)],
            true,
            999,
            7,
        );

        let bytes = bincode::serialize(&vote).unwrap();
        let deserialized: VoteByHash = bincode::deserialize(&bytes).unwrap();

        assert_eq!(deserialized.voter, vote.voter);
        assert_eq!(deserialized.block_hashes, vote.block_hashes);
        assert_eq!(deserialized.is_final, vote.is_final);
        assert_eq!(deserialized.timestamp, vote.timestamp);
        assert_eq!(deserialized.sequence, vote.sequence);
    }
}
