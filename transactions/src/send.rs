//! Send transaction: transfer TRST between wallets.

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A TRST send transaction.
///
/// Carries `link` (previous tx) and `origin` (original burn tx) for provenance tracking.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SendTx {
    pub hash: TxHash,
    pub sender: WalletAddress,
    pub receiver: WalletAddress,
    pub amount: u128,
    pub timestamp: Timestamp,
    /// Hash of the immediately preceding transaction this TRST was derived from.
    pub link: TxHash,
    /// Hash of the original burn transaction that created this TRST.
    pub origin: TxHash,
    pub work: u64,
    pub signature: Signature,
}
