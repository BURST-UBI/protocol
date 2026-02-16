//! Wallet address type with `brst_` prefix.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A BURST wallet address, always prefixed with `brst_`.
///
/// Derived from the wallet's public key via Blake2b hashing + base32 encoding.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WalletAddress(String);

impl WalletAddress {
    /// The standard prefix for all BURST wallet addresses.
    pub const PREFIX: &'static str = "brst_";

    /// Create a new wallet address from a raw string.
    ///
    /// # Panics
    /// Panics if the string does not start with `brst_`.
    pub fn new(raw: impl Into<String>) -> Self {
        let s = raw.into();
        assert!(s.starts_with(Self::PREFIX), "address must start with brst_");
        Self(s)
    }

    /// Create a wallet address from a public key.
    pub fn from_public_key(_public_key: &crate::keys::PublicKey) -> Self {
        todo!("derive address from public key via Blake2b + base32 encoding")
    }

    /// Return the raw address string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Validate that this address is well-formed.
    pub fn is_valid(&self) -> bool {
        self.0.starts_with(Self::PREFIX) && self.0.len() > Self::PREFIX.len()
    }
}

impl fmt::Display for WalletAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for WalletAddress {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}
