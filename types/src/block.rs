//! Block hash type for the DAG block-lattice.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A 32-byte block hash — identifies a block in an account's chain.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlockHash([u8; 32]);

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
}

impl fmt::Debug for BlockHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex: String = self.0[..4].iter().map(|b| format!("{:02x}", b)).collect();
        write!(f, "BlockHash({}…)", hex)
    }
}

impl fmt::Display for BlockHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hex: String = self.0.iter().map(|b| format!("{:02x}", b)).collect();
        write!(f, "{}", hex)
    }
}
