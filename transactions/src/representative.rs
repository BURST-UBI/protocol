//! Change representative transaction (for consensus voting weight delegation).

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// Change the consensus representative for an account.
///
/// Requests a new consensus representative. The certified authority transition
/// determines when the change affects voting weight; this payload alone does
/// not change the installed electorate.
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
