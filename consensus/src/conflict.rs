//! Conflict detection — identifies forks in account chains.

use burst_types::{BlockHash, WalletAddress};

/// Detects conflicting blocks (forks) in account chains.
pub struct ConflictDetector;

impl ConflictDetector {
    /// Check if two blocks represent a fork (same account, same previous, different hash).
    pub fn is_fork(
        &self,
        _account: &WalletAddress,
        _block_a: &BlockHash,
        _block_b: &BlockHash,
        _previous_a: &BlockHash,
        _previous_b: &BlockHash,
    ) -> bool {
        todo!("check if both blocks claim the same previous block")
    }
}
