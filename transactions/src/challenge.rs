//! Challenge transaction: challenger stakes BRN to contest a wallet's legitimacy.

use burst_types::{ChallengeReason, Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A challenge transaction.
///
/// Requests a challenge panel with a BRN stake. Eligibility, active challenges,
/// panel selection and settlement require stateful protocol validation.
/// This payload has no evidence attachment field.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChallengeTx {
    pub hash: TxHash,
    /// The wallet initiating the challenge.
    pub challenger: WalletAddress,
    /// The wallet being challenged.
    pub target: WalletAddress,
    /// BRN offered as a stake, subject to admission and outcome settlement.
    pub stake_amount: u128,
    /// Why the wallet is being challenged. Fraud (default) → revoke TRST on
    /// upheld; Inactivity → benign deactivation with no revocation. Carried into
    /// the Challenge block's `origin` field by the transaction builder.
    #[serde(default)]
    pub reason: ChallengeReason,
    /// Canonical source of the recorded outcome being appealed. None is an
    /// ordinary challenge; a supplied source must be nonzero and owner-signed.
    #[serde(default)]
    pub appeal_of: Option<burst_types::BlockHash>,
    pub timestamp: Timestamp,
    pub work: u64,
    pub signature: Signature,
}
