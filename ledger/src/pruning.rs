//! Ledger pruning — remove expired and revoked TRST history.
//!
//! Once TRST is non-transferable (expired or revoked), its full transaction
//! chain can be pruned. The state block model ensures balance info is preserved.

use burst_types::Timestamp;

/// Configuration for the ledger pruner.
pub struct PruningConfig {
    /// Only prune TRST that expired before this timestamp.
    pub prune_before: Timestamp,
    /// Whether to prune revoked TRST chains.
    pub prune_revoked: bool,
    /// Maximum number of blocks to prune per batch (to limit I/O).
    pub batch_size: usize,
}

/// Prune expired and/or revoked TRST transaction chains from the ledger.
///
/// Returns the number of blocks pruned.
pub fn prune_ledger(_config: &PruningConfig) -> u64 {
    todo!("iterate expired/revoked origins, follow transaction chains, delete blocks")
}
