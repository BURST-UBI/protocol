//! Per-account chain management.

use crate::error::LedgerError;
use crate::state_block::StateBlock;
use burst_types::{BlockHash, WalletAddress};

/// In-memory head and block count for one account chain.
pub struct AccountChain {
    pub account: WalletAddress,
    /// Hash of the most recent (head) block.
    pub head: BlockHash,
    /// Total number of blocks in this chain.
    pub block_count: u64,
}

impl AccountChain {
    /// Check that the block names this account and its current head.
    /// Signature, format and economic validation belong to the caller.
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

    /// Update head and count without validating the block. The caller must
    /// validate it first and ensure the block count can be incremented.
    pub fn append(&mut self, block: &StateBlock) {
        self.head = block.hash;
        self.block_count += 1;
    }
}
