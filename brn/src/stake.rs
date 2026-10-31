//! BRN staking for verification voting and challenges.

use burst_types::{Timestamp, WalletAddress};
use serde::{Deserialize, Serialize};

/// Stake identifier allocated within a BRN engine.
pub type StakeId = u64;

/// What kind of action this BRN is staked for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StakeKind {
    /// Staked as a verifier voting on a wallet's humanity.
    Verification { target_wallet: WalletAddress },
    /// Staked as a challenger contesting another wallet.
    Challenge { target_wallet: WalletAddress },
}

/// A BRN stake and its settlement flag. Resolved records may still be retained.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Stake {
    pub id: StakeId,
    pub staker: WalletAddress,
    pub amount: u128,
    pub kind: StakeKind,
    pub created_at: Timestamp,
    /// Whether this stake has been resolved (returned or forfeited).
    pub resolved: bool,
}
