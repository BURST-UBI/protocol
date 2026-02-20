//! Burn transaction: consumer burns BRN → provider receives TRST.

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A burn transaction. The consumer destroys BRN; the provider receives fresh TRST.
///
/// The timestamp on this transaction determines the TRST expiry date.
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
