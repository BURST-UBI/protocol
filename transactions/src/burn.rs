//! Burn payload for creating a pending TRST payment from BRN.

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// Request to burn BRN for a recipient. Acceptance creates pending TRST; the
/// recipient must claim it with a receive. The burn time starts its expiry age.
/// This payload alone does not establish acceptance or available funds.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BurnTx {
    pub hash: TxHash,
    pub sender: WalletAddress,
    pub receiver: WalletAddress,
    pub amount: u128,
    pub timestamp: Timestamp,
    pub work: u64,
    pub signature: Signature,
}
