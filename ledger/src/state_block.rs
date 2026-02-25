//! State block — the unified block format for BURST's block-lattice.
//!
//! Inspired by Nano's state blocks: every block contains the full account state,
//! enabling efficient pruning without losing security.

use burst_crypto::blake2b_256;
use burst_types::{BlockHash, Signature, Timestamp, TxHash, WalletAddress};
use burst_work::validate_work;
use serde::{Deserialize, Serialize};

/// The type of operation this block represents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockType {
    /// Account opening block (first block in the chain).
    Open,
    /// BRN burn → TRST mint.
    Burn,
    /// TRST send to another account.
    Send,
    /// Receive TRST from another account.
    Receive,
    /// Split TRST into multiple outputs.
    Split,
    /// Merge multiple TRST tokens.
    Merge,
    /// Endorse a new wallet.
    Endorse,
    /// Challenge another wallet.
    Challenge,
    /// Governance proposal submission.
    GovernanceProposal,
    /// Governance vote.
    GovernanceVote,
    /// Delegate voting power.
    Delegate,
    /// Revoke delegation.
    RevokeDelegation,
    /// Change consensus representative.
    ChangeRepresentative,
    /// Epoch block (protocol upgrade marker).
    Epoch,
    /// Reject a pending TRST receive (returns to sender).
    RejectReceive,
    /// Verification vote — verifier casts a vote on a wallet's humanity.
    VerificationVote,
    /// Governance activation block — records an on-chain parameter change
    /// (Tezos-style self-amendment). Placed on the genesis account's chain.
    GovernanceActivation,
}

/// Current state block version.
pub const CURRENT_BLOCK_VERSION: u8 = 1;

/// A state block in BURST's block-lattice.
///
/// Each block contains the full account state after the operation,
/// enabling database pruning without losing balance information.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateBlock {
    /// Block format version (currently 1). Allows future upgrades to the
    /// block structure without hard forks.
    pub version: u8,

    /// The block type / operation.
    pub block_type: BlockType,

    /// The account this block belongs to.
    pub account: WalletAddress,

    /// Hash of the previous block in this account's chain (zero for Open).
    pub previous: BlockHash,

    /// The account's consensus representative.
    pub representative: WalletAddress,

    /// BRN balance after this block (available, not total accrued).
    pub brn_balance: u128,

    /// TRST balance after this block (transferable only).
    pub trst_balance: u128,

    /// Link field — context-dependent:
    /// - For Burn: unused (zero)
    /// - For Send: destination account's block hash (pairing)
    /// - For Receive: the send block hash being received
    /// - For Endorse: the target wallet's pending verification
    /// - For GovernanceVote: the proposal hash
    pub link: BlockHash,

    /// Origin burn transaction hash for TRST provenance tracking.
    pub origin: TxHash,

    /// The transaction contained in this block.
    pub transaction: TxHash,

    /// Block timestamp.
    pub timestamp: Timestamp,

    /// Deterministic hash of the ProtocolParams this block was validated under.
    /// Equivalent to Tezos's protocol hash in block headers.
    pub params_hash: BlockHash,

    /// Proof-of-work nonce (anti-spam).
    pub work: u64,

    /// Signature by the account holder.
    pub signature: Signature,

    /// The computed hash of this block.
    pub hash: BlockHash,
}

impl StateBlock {
    /// Compute the hash of this block from its contents.
    ///
    /// Serializes all fields except `signature` and `work` in canonical order,
    /// then hashes with Blake2b-256.
    pub fn compute_hash(&self) -> BlockHash {
        // Serialize fields in canonical order:
        // 0. version (1 byte)
        // 1. block_type (u8 enum discriminant)
        // 2. account (string bytes)
        // 3. previous (32 bytes)
        // 4. representative (string bytes)
        // 5. brn_balance (16 bytes, big-endian u128)
        // 6. trst_balance (16 bytes, big-endian u128)
        // 7. link (32 bytes)
        // 7b. origin (32 bytes)
        // 8. transaction (32 bytes)
        // 9. timestamp (8 bytes, big-endian u64)

        let mut buffer = Vec::with_capacity(256);

        // 0. version
        buffer.push(self.version);

        // 1. block_type as u8 discriminant
        let block_type_byte = match self.block_type {
            BlockType::Open => 0,
            BlockType::Burn => 1,
            BlockType::Send => 2,
            BlockType::Receive => 3,
            BlockType::Split => 4,
            BlockType::Merge => 5,
            BlockType::Endorse => 6,
            BlockType::Challenge => 7,
            BlockType::GovernanceProposal => 8,
            BlockType::GovernanceVote => 9,
            BlockType::Delegate => 10,
            BlockType::RevokeDelegation => 11,
            BlockType::ChangeRepresentative => 12,
            BlockType::Epoch => 13,
            BlockType::RejectReceive => 14,
            BlockType::VerificationVote => 15,
            BlockType::GovernanceActivation => 16,
        };
        buffer.push(block_type_byte);

        // 2. account (string bytes)
        buffer.extend_from_slice(self.account.as_str().as_bytes());

        // 3. previous (32 bytes)
        buffer.extend_from_slice(self.previous.as_bytes());

        // 4. representative (string bytes)
        buffer.extend_from_slice(self.representative.as_str().as_bytes());

        // 5. brn_balance (16 bytes, big-endian u128)
        buffer.extend_from_slice(&self.brn_balance.to_be_bytes());

        // 6. trst_balance (16 bytes, big-endian u128)
        buffer.extend_from_slice(&self.trst_balance.to_be_bytes());

        // 7. link (32 bytes)
        buffer.extend_from_slice(self.link.as_bytes());

        // 7b. origin (32 bytes)
        buffer.extend_from_slice(self.origin.as_bytes());

        // 8. transaction (32 bytes)
        buffer.extend_from_slice(self.transaction.as_bytes());

        // 9. timestamp (8 bytes, big-endian u64)
        buffer.extend_from_slice(&self.timestamp.as_secs().to_be_bytes());

        // 10. params_hash (32 bytes)
        buffer.extend_from_slice(self.params_hash.as_bytes());

        // Hash the concatenated bytes
        let hash_bytes = blake2b_256(&buffer);
        BlockHash::new(hash_bytes)
    }

    /// Verify this block's proof-of-work meets the minimum difficulty.
    pub fn verify_work(&self, min_difficulty: u64) -> bool {
        validate_work(&self.hash, self.work, min_difficulty)
    }

    /// Whether this is the first block in an account chain.
    pub fn is_open(&self) -> bool {
        self.block_type == BlockType::Open
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{BlockHash, Signature, Timestamp, TxHash, WalletAddress};
    use burst_work::WorkGenerator;

    fn create_test_block() -> StateBlock {
        StateBlock {
            version: CURRENT_BLOCK_VERSION,
            block_type: BlockType::Open,
            account: WalletAddress::new(
                "brst_1111111111111111111111111111111111111111111111111111111111111111111",
            ),
            previous: BlockHash::ZERO,
            representative: WalletAddress::new(
                "brst_2222222222222222222222222222222222222222222222222222222222222222222",
            ),
            brn_balance: 1000,
            trst_balance: 500,
            link: BlockHash::ZERO,
            origin: TxHash::ZERO,
            transaction: TxHash::ZERO,
            timestamp: Timestamp::new(1234567890),
            params_hash: BlockHash::ZERO,
            work: 0,
            signature: Signature([0u8; 64]),
            hash: BlockHash::ZERO,
        }
    }

    #[test]
    fn test_compute_hash_deterministic() {
        let block = create_test_block();
        let hash1 = block.compute_hash();
        let hash2 = block.compute_hash();

        // Same block should produce same hash
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_compute_hash_different_blocks() {
        let block1 = create_test_block();
        let mut block2 = create_test_block();

        // Change one field
        block2.brn_balance = 2000;

        let hash1 = block1.compute_hash();
        let hash2 = block2.compute_hash();

        // Different blocks should produce different hashes
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_hash_different_block_types() {
        let mut block1 = create_test_block();
        let mut block2 = create_test_block();

        block1.block_type = BlockType::Open;
        block2.block_type = BlockType::Burn;

        let hash1 = block1.compute_hash();
        let hash2 = block2.compute_hash();

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_hash_different_accounts() {
        let mut block1 = create_test_block();
        let mut block2 = create_test_block();

        block1.account = WalletAddress::new(
            "brst_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        block2.account = WalletAddress::new(
            "brst_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        );

        let hash1 = block1.compute_hash();
        let hash2 = block2.compute_hash();

        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_compute_hash_excludes_signature_and_work() {
        let block1 = create_test_block();
        let mut block2 = create_test_block();

        // Change signature and work (should not affect hash)
        block2.signature = Signature([0xFFu8; 64]);
        block2.work = 999999;

        let hash1 = block1.compute_hash();
        let hash2 = block2.compute_hash();

        // Hash should be the same since signature and work are excluded
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_verify_work_with_valid_nonce() {
        let mut block = create_test_block();

        // Compute hash first
        block.hash = block.compute_hash();

        // Generate valid work for a low difficulty
        let min_difficulty = 1000;
        let generator = WorkGenerator;
        let work_nonce = generator.generate(&block.hash, min_difficulty).unwrap();

        // Set the work nonce
        block.work = work_nonce.0;

        // Verify work should pass
        assert!(block.verify_work(min_difficulty));
    }

    #[test]
    fn test_verify_work_with_invalid_nonce() {
        let mut block = create_test_block();
        block.hash = block.compute_hash();

        // Use a random nonce that likely won't meet high difficulty
        block.work = 12345;

        // With very high difficulty, this should fail
        assert!(!block.verify_work(u64::MAX));
    }

    #[test]
    fn test_verify_work_zero_difficulty() {
        let mut block = create_test_block();
        block.hash = block.compute_hash();
        block.work = 0;

        // Zero difficulty should always pass
        assert!(block.verify_work(0));
    }
}
