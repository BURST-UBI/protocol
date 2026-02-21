//! Frontier storage trait.

use crate::StoreError;
use burst_types::{BlockHash, WalletAddress};

/// Trait for tracking account-chain frontiers.
///
/// A frontier is the head block of each account's chain in the DAG. This store
/// maps each account address to its latest block hash.
pub trait FrontierStore {
    /// Set or update the frontier (head block) for an account.
    fn put_frontier(&self, account: &WalletAddress, head: &BlockHash) -> Result<(), StoreError>;

    /// Get the frontier (head block) for an account.
    fn get_frontier(&self, account: &WalletAddress) -> Result<BlockHash, StoreError>;

    /// Delete the frontier entry for an account.
    fn delete_frontier(&self, account: &WalletAddress) -> Result<(), StoreError>;

    /// Iterate over all frontiers, returning (account, head) pairs.
    fn iter_frontiers(&self) -> Result<Vec<(WalletAddress, BlockHash)>, StoreError>;

    /// Total number of frontier entries.
    fn frontier_count(&self) -> Result<u64, StoreError>;
}
