//! Reject-receive transaction: decline a pending TRST send.

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A transaction that rejects a pending TRST receive, returning it to the sender.
///
/// The `send_block_hash` references the original send block being declined.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RejectReceiveTx {
    pub hash: TxHash,
    pub rejecter: WalletAddress,
    pub send_block_hash: TxHash,
    pub timestamp: Timestamp,
    pub work: u64,
    pub signature: Signature,
}
