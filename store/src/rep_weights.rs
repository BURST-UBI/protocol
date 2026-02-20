//! Representative weight storage trait.

use crate::StoreError;
use burst_types::WalletAddress;

/// Persistent storage for representative weights and online weight samples.
pub trait RepWeightStore {
    /// Store a representative's weight.
    fn put_rep_weight(&self, rep: &WalletAddress, weight: u128) -> Result<(), StoreError>;

    /// Get a representative's weight.
    fn get_rep_weight(&self, rep: &WalletAddress) -> Result<Option<u128>, StoreError>;

    /// Delete a representative's weight entry.
    fn delete_rep_weight(&self, rep: &WalletAddress) -> Result<(), StoreError>;

    /// Get all representative weights.
    fn iter_rep_weights(&self) -> Result<Vec<(WalletAddress, u128)>, StoreError>;

    /// Store an online weight sample at the given timestamp.
    fn put_online_weight_sample(&self, timestamp: u64, weight: u128) -> Result<(), StoreError>;

    /// Get the most recent online weight samples, up to `limit` entries.
    /// Returned in descending timestamp order (newest first).
    fn get_online_weight_samples(&self, limit: usize) -> Result<Vec<(u64, u128)>, StoreError>;
}
