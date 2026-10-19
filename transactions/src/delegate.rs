//! Delegation transactions: delegate and revoke voting power.
//!
//! These legacy transactions carry address pointers. They do not implement
//! the whitepaper's encrypted secondary-key handoff. Scoped grants and
//! revocations use the separately authenticated delegation block payload.

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A delegation transaction: `delegator` entrusts its vote to `delegate`.
/// Authority is proven by the delegator's own signature on this transaction;
/// no secondary/encrypted key is involved.
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

/// Revoke a previously delegated vote. Signed by the delegator's primary key,
/// which proves authority to revoke.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RevokeDelegationTx {
    pub hash: TxHash,
    pub delegator: WalletAddress,
    pub timestamp: Timestamp,
    pub work: u64,
    /// Signed by the primary private key (proves authority to revoke).
    pub signature: Signature,
}
