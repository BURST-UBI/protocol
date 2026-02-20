//! Verification vote transaction: verifier casts a vote on a wallet's humanity.

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A verification vote transaction.
///
/// A selected verifier stakes BRN and casts a vote on whether a target wallet
/// is a legitimate unique human. Vote values: 1 = Legitimate, 2 = Illegitimate,
/// 3 = Neither.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationVoteTx {
    pub hash: TxHash,
    /// The verifier casting the vote.
    pub voter: WalletAddress,
    /// The wallet being verified.
    pub target_wallet: WalletAddress,
    /// Vote: 1 = Legitimate, 2 = Illegitimate, 3 = Neither.
    pub vote: u8,
    /// Amount of BRN staked on this vote.
    pub stake_amount: u128,
    pub timestamp: Timestamp,
    pub work: u64,
    pub signature: Signature,
}
