//! Merge transaction: combine multiple TRST tokens into one.

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A TRST merge transaction.
///
/// Combines multiple tokens (potentially from different origins) into one.
/// The merged token's expiry is the **earliest** expiry among all inputs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeTx {
    pub hash: TxHash,
    pub sender: WalletAddress,
    pub timestamp: Timestamp,
    /// Hashes of the tokens being merged.
    pub source_hashes: Vec<TxHash>,
    pub work: u64,
    pub signature: Signature,
}
