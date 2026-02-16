//! Delegation transactions: delegate and revoke voting power.

use burst_types::{Signature, Timestamp, TxHash, WalletAddress};
use serde::{Deserialize, Serialize};

/// A delegation transaction.
///
/// The delegator generates a secondary key pair and encrypts the private key
/// with the delegate's public key, then broadcasts it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DelegateTx {
    pub hash: TxHash,
    /// The wallet delegating its vote.
    pub delegator: WalletAddress,
    /// The wallet receiving delegation authority.
    pub delegate: WalletAddress,
    /// The delegation public key (secondary key pair).
    pub delegation_public_key: Vec<u8>,
    /// The delegation private key, encrypted with the delegate's public key.
    pub encrypted_delegation_key: Vec<u8>,
    pub timestamp: Timestamp,
    pub signature: Signature,
}

/// Revoke a previously delegated vote.
///
/// Broadcasts a new delegation key signed by the primary private key,
/// invalidating the previous delegation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RevokeDelegationTx {
    pub hash: TxHash,
    pub delegator: WalletAddress,
    /// New delegation public key (invalidates the old one).
    pub new_delegation_public_key: Vec<u8>,
    pub timestamp: Timestamp,
    /// Signed by the primary private key (proves authority to revoke).
    pub signature: Signature,
}
