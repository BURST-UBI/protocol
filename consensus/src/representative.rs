//! Representative — an account that votes on behalf of delegators for consensus.

use burst_types::WalletAddress;
use serde::{Deserialize, Serialize};

/// A consensus representative.
///
/// Unlike governance delegation (one-person-one-vote), consensus weight is
/// proportional to delegated TRST balance (similar to Nano's ORV).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Representative {
    pub address: WalletAddress,
    /// Total weight delegated to this representative.
    pub delegated_weight: u128,
    /// Whether this representative is considered "online" (responsive).
    pub online: bool,
}
