//! Receive payload for claiming an incoming TRST receipt.

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// Claim a receipt identified by `send_block_hash`. The node checks its owner,
/// amount and availability before accepting the corresponding account block.
/// A receipt can originate from a burn, send or return; its stored hash field
/// does not restrict it to a Send block.
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
