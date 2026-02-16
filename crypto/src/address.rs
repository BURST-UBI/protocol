//! Wallet address derivation from public keys.
//!
//! Address format: `brst_` + base32-encoded Blake2b hash of the public key + checksum.

use burst_types::{PublicKey, WalletAddress};

/// Derive a `brst_`-prefixed wallet address from a public key.
///
/// Process: Blake2b-256(public_key) -> base32 encode -> prepend `brst_` -> append checksum.
pub fn derive_address(_public_key: &PublicKey) -> WalletAddress {
    todo!("Blake2b hash of public key -> base32 encode -> prepend brst_ prefix")
}

/// Validate that an address string is well-formed and its checksum is correct.
pub fn validate_address(_address: &str) -> bool {
    todo!("decode base32, verify checksum, check prefix")
}
