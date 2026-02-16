//! Constitutional amendments.

use burst_governance::proposal::GovernancePhase;
use burst_types::{Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A proposed constitutional amendment.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
}
