//! Change representative transaction (for consensus voting weight delegation).

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// Change the consensus representative for an account.
///
/// Similar to Nano's representative change — delegates ORV voting weight
/// for double-spend resolution.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChangeRepresentativeTx {
    pub hash: TxHash,
    pub account: WalletAddress,
    /// The new representative to delegate consensus weight to.
    pub new_representative: WalletAddress,
    pub timestamp: Timestamp,
    pub work: u64,
    pub signature: Signature,
}
