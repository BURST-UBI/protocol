//! Representative — an account that votes on behalf of delegators for consensus.

use burst_types::WalletAddress;
use serde::{Deserialize, Serialize};

/// A consensus representative.
///
/// Consensus weight is the number of verified people delegating here, in fixed
/// units per person. Token balances and historical reputation confer no weight.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Representative {
    pub address: WalletAddress,
    /// Total weight delegated to this representative.
    pub delegated_weight: u128,
    /// Whether this representative is considered "online" (responsive).
    pub online: bool,
}
