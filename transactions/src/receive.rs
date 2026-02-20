//! Receive transaction — the receiver's side of the asynchronous send/receive.
//!
//! In the block-lattice, the sender publishes a Send block, and the receiver
//! publishes a Receive block to "pocket" the incoming TRST.

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A receive transaction that pockets an incoming TRST send.
///
/// The `send_block_hash` references the sender's Send block. The receiver
/// publishes this block on their own account chain to credit the TRST.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReceiveTx {
    pub hash: TxHash,
    pub receiver: WalletAddress,
    pub send_block_hash: TxHash,
    pub amount: u128,
    pub timestamp: Timestamp,
    pub work: u64,
    pub signature: Signature,
}
