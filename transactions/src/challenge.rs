//! Challenge transaction: challenger stakes BRN to contest a wallet's legitimacy.

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A challenge transaction.
///
/// Any verified wallet can challenge another by staking BRN.
/// Triggers a re-verification vote by new randomly selected verifiers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChallengeTx {
    pub hash: TxHash,
    /// The wallet initiating the challenge.
    pub challenger: WalletAddress,
    /// The wallet being challenged.
    pub target: WalletAddress,
    /// BRN staked by the challenger (lost if challenge fails).
    pub stake_amount: u128,
    pub timestamp: Timestamp,
    pub work: u64,
    pub signature: Signature,
}
