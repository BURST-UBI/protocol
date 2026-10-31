//! Delegation transactions: delegate and revoke voting power.
//!
//! These payloads carry wallet-address pointers. Scoped signing-key grants and
//! revocations use a separate authenticated delegation block payload.

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A delegation transaction: `delegator` entrusts its vote to `delegate`.
/// The node must authenticate the resulting account block before changing
/// delegation. Storing a signature here does not establish authority.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DelegateTx {
    pub hash: TxHash,
    /// The wallet delegating its vote.
    pub delegator: WalletAddress,
    /// The wallet receiving delegation authority.
    pub delegate: WalletAddress,
    pub timestamp: Timestamp,
    pub work: u64,
    pub signature: Signature,
}

/// Request removal of an address-based delegation. Admission authenticates
/// the account's authority to revoke it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RevokeDelegationTx {
    pub hash: TxHash,
    pub delegator: WalletAddress,
    pub timestamp: Timestamp,
    pub work: u64,
    /// Supplied signature; validation belongs to the admission path.
    pub signature: Signature,
}
