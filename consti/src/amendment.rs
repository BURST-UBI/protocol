//! Constitutional amendments with diff-based operations.

use burst_governance::proposal::GovernancePhase;
use burst_types::{Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

pub use burst_governance::proposal::AmendmentOp;

/// A proposed constitutional amendment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Amendment {
    pub hash: TxHash,
    pub proposer: WalletAddress,
    pub title: String,
    pub text: String,
    pub phase: GovernancePhase,
    pub votes_yea: u32,
    pub votes_nay: u32,
    pub votes_abstain: u32,
    pub created_at: Timestamp,
    /// Diff-based operations to apply to the constitution.
    /// If empty, falls back to legacy behavior (add article from title/text).
    pub operations: Vec<AmendmentOp>,
}
