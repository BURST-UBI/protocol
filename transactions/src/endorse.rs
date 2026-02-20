//! Endorse transaction: endorser permanently burns BRN to vouch for a new wallet.

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// An endorsement transaction.
///
/// The endorser permanently burns their own BRN to vouch for a new wallet's humanity.
/// Once the endorsement threshold is met, the target wallet enters verification.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EndorseTx {
    pub hash: TxHash,
    /// The wallet doing the endorsing (burns their BRN).
    pub endorser: WalletAddress,
    /// The wallet being endorsed.
    pub target: WalletAddress,
    /// Amount of BRN permanently burned for this endorsement.
    pub burn_amount: u128,
    pub timestamp: Timestamp,
    pub work: u64,
    pub signature: Signature,
}
