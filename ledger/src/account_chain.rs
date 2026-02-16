//! Per-account chain management.

use crate::error::LedgerError;
use crate::state_block::StateBlock;
use burst_types::{BlockHash, WalletAddress};

/// Represents a single account's chain in the block-lattice.
pub struct AccountChain {
    pub account: WalletAddress,
    /// Hash of the most recent (head) block.
    pub head: BlockHash,
    /// Total number of blocks in this chain.
    pub block_count: u64,
}

impl AccountChain {
    /// Validate that a new block can be appended to this chain.
    pub fn validate_append(&self, block: &StateBlock) -> Result<(), LedgerError> {
        if block.previous != self.head {
            return Err(LedgerError::BlockGap {
                previous: format!("{}", block.previous),
            });
        }
        if block.account != self.account {
            return Err(LedgerError::InvalidBlock {
                reason: "block account does not match chain account".into(),
            });
        }
        Ok(())
    }

    /// Append a validated block, updating the chain head.
    pub fn append(&mut self, block: &StateBlock) {
        self.head = block.hash;
        self.block_count += 1;
    }
}
