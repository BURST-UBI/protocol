//! Block hash type for the DAG block-lattice.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A 32-byte block identifier. Holding a hash does not prove the block exists.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BlockHash([u8; 32]);

impl Default for BlockHash {
    fn default() -> Self {
        Self::ZERO
    }
}

impl BlockHash {
    pub const ZERO: Self = Self([0u8; 32]);

    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0u8; 32]
    }

    /// Reinterpret the same bytes as a TxHash, without hashing or ledger lookup.
    pub fn into_tx_hash(self) -> crate::transaction_hash::TxHash {
        crate::transaction_hash::TxHash::new(self.0)
    }
}

impl fmt::Debug for BlockHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlockHash(")?;
        for b in &self.0[..4] {
            write!(f, "{:02x}", b)?;
        }
        write!(f, "\u{2026})")
    }
}

impl fmt::Display for BlockHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0 {
            write!(f, "{:02x}", b)?;
        }
        Ok(())
    }
}
