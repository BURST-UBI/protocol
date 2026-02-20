//! Block storage trait.

use crate::StoreError;
use burst_types::{BlockHash, WalletAddress};

/// Trait for block storage operations (the DAG block-lattice).
pub trait BlockStore {
    /// Store a block (serialized bytes keyed by hash).
    fn put_block(&self, hash: &BlockHash, block_bytes: &[u8]) -> Result<(), StoreError>;

    /// Store a block and update the per-account block index atomically.
    fn put_block_with_account(
        &self,
        hash: &BlockHash,
        block_bytes: &[u8],
        _account: &WalletAddress,
    ) -> Result<(), StoreError> {
        // Default: just put the block (backward compat for non-LMDB impls).
        self.put_block(hash, block_bytes)
    }

    /// Retrieve a block by hash.
    fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError>;

    /// Check if a block exists.
    fn exists(&self, hash: &BlockHash) -> Result<bool, StoreError>;

    /// Delete a block (for pruning).
    fn delete_block(&self, hash: &BlockHash) -> Result<(), StoreError>;

    /// Get all block hashes for an account (the account chain).
    fn get_account_blocks(&self, address: &WalletAddress) -> Result<Vec<BlockHash>, StoreError>;

    /// Total number of blocks in the store.
    fn block_count(&self) -> Result<u64, StoreError>;

    /// Get the block hash at a specific height in an account's chain.
    /// Height 1 is the first block (open block).
    fn block_at_height(
        &self,
        account: &WalletAddress,
        height: u64,
    ) -> Result<Option<BlockHash>, StoreError>;

    /// Get the height of a block in its account's chain.
    /// Returns `None` if the block is not found.
    fn height_of_block(&self, block_hash: &BlockHash) -> Result<Option<u64>, StoreError>;
}
