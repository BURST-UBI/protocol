//! Split transaction: divide TRST into multiple smaller tokens.

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A split output — one recipient and amount in a split.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SplitOutput {
    pub receiver: WalletAddress,
    pub amount: u128,
}

/// A TRST split transaction.
///
/// All outputs share the same `origin` and `link` from the parent.
/// The sum of output amounts must equal the parent token's amount.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SplitTx {
    pub hash: TxHash,
    pub sender: WalletAddress,
    pub timestamp: Timestamp,
    /// The parent token being split.
    pub parent_hash: TxHash,
    /// The origin from the parent token (copied forward).
    pub origin: TxHash,
    /// Individual outputs.
    pub outputs: Vec<SplitOutput>,
    pub work: u64,
    pub signature: Signature,
}
