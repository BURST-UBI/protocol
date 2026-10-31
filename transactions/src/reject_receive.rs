//! Reject-receive transaction: decline a pending TRST send.

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// Decline a pending payment. Acceptance creates a return receipt that the
/// sender must claim; it does not undo the original send or burn.
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
