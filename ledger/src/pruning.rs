//! Ledger pruning — remove expired and revoked TRST history.
//!
//! Once TRST is non-transferable (expired or revoked), its full transaction
//! chain can be pruned. The state block model ensures balance info is preserved
//! in the head block. Pruning reduces storage requirements for full nodes
//! without losing any information needed for current account state.
//!
//! The pruning engine is deliberately stateless with respect to the storage
//! backend: callers pass in lists of candidate hashes and the engine decides
//! which to prune. The actual deletion is performed by the storage layer.

use burst_types::{Timestamp, TxHash};

/// Configuration for ledger pruning.
pub struct PruningConfig {
    /// Whether pruning is enabled.
    pub enabled: bool,
    /// Maximum age of expired TRST to keep (in seconds). `0` = prune immediately.
    pub max_expired_age_secs: u64,
    /// Whether to prune revoked TRST chains.
    pub prune_revoked: bool,
    /// Maximum number of entries to prune per batch (limits I/O per cycle).
    pub batch_size: usize,
}

impl Default for PruningConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_expired_age_secs: 30 * 24 * 3600, // 30 days
            prune_revoked: true,
            batch_size: 1000,
        }
    }
}

/// Result of a pruning operation.
pub struct PruneResult {
    /// Number of expired TRST entries pruned.
    pub expired_pruned: usize,
    /// Number of revoked TRST entries pruned.
    pub revoked_pruned: usize,
    /// Total entries pruned (`expired_pruned + revoked_pruned`).
    pub total_pruned: usize,
}

/// Pruning engine — decides which TRST entries should be removed.
pub struct LedgerPruner {
    config: PruningConfig,
}

impl LedgerPruner {
    /// Create a new pruner with the given configuration.
    pub fn new(config: PruningConfig) -> Self {
        Self { config }
    }

    /// Identify TRST entries eligible for pruning.
    ///
    /// `expired_hashes` should be pre-filtered by the storage layer to only
    /// include TRST entries whose expiry timestamp plus `max_expired_age_secs`
    /// has passed. `revoked_hashes` are TRST entries that were revoked
    /// (sybil detection).
    ///
    /// The result is bounded by `batch_size`.
    pub fn find_pruneable(
        &self,
        expired_hashes: &[TxHash],
        revoked_hashes: &[TxHash],
        _now: Timestamp,
    ) -> Vec<TxHash> {
        let mut to_prune = Vec::new();

        if !self.config.enabled {
            return to_prune;
        }

        // Add expired entries (up to batch_size).
        to_prune.extend(expired_hashes.iter().take(self.config.batch_size).cloned());

        // Add revoked entries if configured, filling remaining batch capacity.
        if self.config.prune_revoked {
            let remaining = self.config.batch_size.saturating_sub(to_prune.len());
            to_prune.extend(revoked_hashes.iter().take(remaining).cloned());
        }

        to_prune
    }

    /// Execute pruning — returns a summary of what was pruned.
    ///
    /// This method identifies pruneable entries and returns the counts.
    /// The actual deletion from the storage backend is the caller's
    /// responsibility (pass the result of `find_pruneable` to the store).
    pub fn prune(
        &self,
        expired_hashes: &[TxHash],
        revoked_hashes: &[TxHash],
        now: Timestamp,
    ) -> PruneResult {
        let pruneable = self.find_pruneable(expired_hashes, revoked_hashes, now);

        // Count how many expired vs revoked were selected.
        let expired_count = expired_hashes.len().min(pruneable.len());
        let revoked_count = pruneable.len().saturating_sub(expired_count);

        PruneResult {
            expired_pruned: expired_count,
            revoked_pruned: revoked_count,
            total_pruned: pruneable.len(),
        }
    }

    /// Access the current configuration.
    pub fn config(&self) -> &PruningConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{Timestamp, TxHash};

    fn tx(val: u8) -> TxHash {
        TxHash::new([val; 32])
    }

    #[test]
    fn test_disabled_pruning_returns_empty() {
        let pruner = LedgerPruner::new(PruningConfig {
            enabled: false,
            ..Default::default()
        });
        let expired = vec![tx(1), tx(2)];
        let revoked = vec![tx(3)];
        let result = pruner.find_pruneable(&expired, &revoked, Timestamp::new(100_000));
        assert!(result.is_empty());
    }

    #[test]
    fn test_enabled_pruning_collects_expired() {
        let pruner = LedgerPruner::new(PruningConfig {
            enabled: true,
            prune_revoked: false,
            batch_size: 100,
            ..Default::default()
        });
        let expired = vec![tx(1), tx(2), tx(3)];
        let result = pruner.find_pruneable(&expired, &[], Timestamp::new(100_000));
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_revoked_pruning() {
        let pruner = LedgerPruner::new(PruningConfig {
            enabled: true,
            prune_revoked: true,
            batch_size: 100,
            ..Default::default()
        });
        let revoked = vec![tx(10), tx(11)];
        let result = pruner.find_pruneable(&[], &revoked, Timestamp::new(100_000));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_batch_size_limit() {
        let pruner = LedgerPruner::new(PruningConfig {
            enabled: true,
            prune_revoked: true,
            batch_size: 2,
            ..Default::default()
        });
        let expired = vec![tx(1), tx(2), tx(3)];
        let revoked = vec![tx(10)];
        let result = pruner.find_pruneable(&expired, &revoked, Timestamp::new(100_000));
        // batch_size=2, so only 2 expired taken, no room for revoked.
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_batch_mixed_expired_and_revoked() {
        let pruner = LedgerPruner::new(PruningConfig {
            enabled: true,
            prune_revoked: true,
            batch_size: 5,
            ..Default::default()
        });
        let expired = vec![tx(1), tx(2)];
        let revoked = vec![tx(10), tx(11), tx(12)];
        let result = pruner.find_pruneable(&expired, &revoked, Timestamp::new(100_000));
        // 2 expired + 3 revoked = 5, exactly the batch size.
        assert_eq!(result.len(), 5);
    }

    #[test]
    fn test_prune_result_counts() {
        let pruner = LedgerPruner::new(PruningConfig {
            enabled: true,
            prune_revoked: true,
            batch_size: 100,
            ..Default::default()
        });
        let expired = vec![tx(1), tx(2)];
        let revoked = vec![tx(10)];
        let result = pruner.prune(&expired, &revoked, Timestamp::new(100_000));
        assert_eq!(result.expired_pruned, 2);
        assert_eq!(result.revoked_pruned, 1);
        assert_eq!(result.total_pruned, 3);
    }

    #[test]
    fn test_default_config() {
        let config = PruningConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.max_expired_age_secs, 30 * 24 * 3600);
        assert!(config.prune_revoked);
        assert_eq!(config.batch_size, 1000);
    }
}
