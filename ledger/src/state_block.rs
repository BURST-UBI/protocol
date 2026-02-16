//! State block — the unified block format for BURST's block-lattice.
//!
//! Inspired by Nano's state blocks: every block contains the full account state,
//! enabling efficient pruning without losing security.

use burst_types::{BlockHash, Signature, Timestamp, TxHash, WalletAddress};
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
}

/// A state block in BURST's block-lattice.
///
/// Each block contains the full account state after the operation,
/// enabling database pruning without losing balance information.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateBlock {
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

    /// The transaction contained in this block.
    pub transaction: TxHash,

    /// Block timestamp.
    pub timestamp: Timestamp,

    /// Proof-of-work nonce (anti-spam).
    pub work: u64,

    /// Signature by the account holder.
    pub signature: Signature,

    /// The computed hash of this block.
    pub hash: BlockHash,
}

impl StateBlock {
    /// Compute the hash of this block from its contents.
    pub fn compute_hash(&self) -> BlockHash {
        todo!("serialize block fields in canonical order -> Blake2b-256")
    }

    /// Verify this block's proof-of-work meets the minimum difficulty.
    pub fn verify_work(&self, _min_difficulty: u64) -> bool {
        todo!("check that hash(block_hash, work_nonce) meets difficulty threshold")
    }

    /// Whether this is the first block in an account chain.
    pub fn is_open(&self) -> bool {
        self.block_type == BlockType::Open
    }
}
