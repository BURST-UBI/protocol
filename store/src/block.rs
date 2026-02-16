//! Block storage trait.

use crate::StoreError;
use burst_types::{BlockHash, WalletAddress};

/// Trait for block storage operations (the DAG block-lattice).
pub trait BlockStore {
    /// Store a block (serialized bytes keyed by hash).
    fn put_block(&self, hash: &BlockHash, block_bytes: &[u8]) -> Result<(), StoreError>;

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
}
