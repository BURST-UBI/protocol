//! Wallet address storage; full encoding validation lives in burst-crypto.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Stored wallet address text. Construction checks only the `brst_` prefix;
/// derived deserialization does not perform even that check.
/// Validate untrusted values with `burst_crypto::decode_address` before use.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WalletAddress(String);

impl WalletAddress {
    /// The standard prefix for all BURST wallet addresses.
    pub const PREFIX: &'static str = "brst_";

    /// Store a string after checking its prefix, without checking the payload.
    ///
    /// # Panics
    /// Panics if the string does not start with `brst_`.
    pub fn new(raw: impl Into<String>) -> Self {
        let s = raw.into();
        assert!(s.starts_with(Self::PREFIX), "address must start with brst_");
        Self(s)
    }

    /// Return the raw address string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Check the prefix and presence of a payload; does not verify encoding or checksum.
    pub fn has_prefix_and_payload(&self) -> bool {
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
