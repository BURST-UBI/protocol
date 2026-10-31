//! Stored amendment metadata and explicit article operations.

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
    /// Ordered article operations. An empty list adds one article using this
    /// amendment's title and text.
    pub operations: Vec<AmendmentOp>,
}
