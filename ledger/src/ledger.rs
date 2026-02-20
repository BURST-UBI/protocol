//! Unified Ledger trait — a coherent abstraction for all ledger operations.
//!
//! Provides accessor methods for the four core storage components and
//! higher-level operations that coordinate across them.

use burst_store::account::AccountStore;
use burst_store::block::BlockStore;
use burst_store::frontier::FrontierStore;
use burst_store::pending::PendingStore;
use burst_store::trst_index::TrstIndexStore;
use burst_store::StoreError;
use burst_types::{BlockHash, WalletAddress};

/// Unified ledger interface providing access to all storage subsystems.
///
/// Implementors expose the four core stores plus optional indices.
/// Higher-level operations coordinate across stores.
pub trait Ledger {
    type Accounts: AccountStore;
    type Blocks: BlockStore;
    type Frontiers: FrontierStore;
    type Pending: PendingStore;
    type TrstIndex: TrstIndexStore;

    fn account_store(&self) -> &Self::Accounts;
    fn block_store(&self) -> &Self::Blocks;
    fn frontier_store(&self) -> &Self::Frontiers;
    fn pending_store(&self) -> &Self::Pending;
    fn trst_index_store(&self) -> &Self::TrstIndex;

    /// Check whether an account exists and has at least one block.
    fn account_exists(&self, address: &WalletAddress) -> Result<bool, StoreError> {
        self.account_store().exists(address)
    }

    /// Get the head block hash for an account (from the frontier).
    fn head_block(&self, address: &WalletAddress) -> Result<BlockHash, StoreError> {
        self.frontier_store().get_frontier(address)
    }

    /// Ledger summary statistics.
    fn summary(&self) -> Result<LedgerSummary, StoreError> {
        Ok(LedgerSummary {
            accounts: self.account_store().account_count()?,
            blocks: self.block_store().block_count()?,
            pending: self.pending_store().pending_count()?,
            frontiers: self.frontier_store().frontier_count()?,
        })
    }

    /// Check if a block is confirmed by comparing its height against the
    /// account's confirmation height — O(1) via `height_of_block` index.
    fn is_block_confirmed(
        &self,
        block_hash: &BlockHash,
        account: &WalletAddress,
    ) -> Result<bool, StoreError> {
        let acct = self.account_store().get_account(account)?;
        if acct.confirmation_height == 0 {
            return Ok(false);
        }
        match self.block_store().height_of_block(block_hash)? {
            Some(height) => Ok(height <= acct.confirmation_height),
            None => Err(StoreError::NotFound("block not found in height index".into())),
        }
    }
}

/// Summary statistics for the ledger.
#[derive(Clone, Debug)]
pub struct LedgerSummary {
    pub accounts: u64,
    pub blocks: u64,
    pub pending: u64,
    pub frontiers: u64,
}
