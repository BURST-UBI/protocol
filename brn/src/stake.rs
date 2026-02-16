//! BRN staking for verification voting and challenges.

use burst_types::Timestamp;
use serde::{Deserialize, Serialize};

/// Unique identifier for an active stake.
pub type StakeId = u64;

/// What kind of action this BRN is staked for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StakeKind {
    /// Staked as a verifier voting on a wallet's humanity.
    Verification { target_wallet: String },
    /// Staked as a challenger contesting another wallet.
    Challenge { target_wallet: String },
}

/// An active BRN stake.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stake {
    pub id: StakeId,
    pub amount: u128,
    pub kind: StakeKind,
    pub created_at: Timestamp,
    /// Whether this stake has been resolved (returned or forfeited).
    pub resolved: bool,
}
