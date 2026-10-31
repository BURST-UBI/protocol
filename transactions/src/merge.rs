//! Merge transaction: combine multiple TRST tokens into one.

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A TRST merge transaction.
///
/// Requests one holding from multiple sources, possibly with different origins.
/// Accepted merges preserve source contributions and the oldest origin time.
/// Source ownership and spendability require stateful validation.
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
