//! Legacy selection of caller-filtered expiry and revocation candidates.
//!
//! These candidates are not deletion authority. Expiry and revocation can be
//! reversed, and account heads alone do not preserve provenance or recovery
//! dependencies. A caller must prove that authenticated replacement state and
//! retained history preserve those rights before removing any physical data.

use burst_types::{Timestamp, TxHash};

/// Operator policy for selecting pruning candidates.
pub struct PruningConfig {
    /// Whether candidate selection is enabled.
    pub enabled: bool,
    /// Age threshold for the caller's expiry filter, in seconds. Zero allows
    /// immediate selection. This helper does not apply the threshold itself.
    pub max_expired_age_secs: u64,
    /// Include revoked TRST chains in the candidates.
    pub prune_revoked: bool,
    /// Maximum number of candidates returned per batch.
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

/// Counts of selected candidates; no entries have been deleted.
pub struct CandidateCounts {
    /// Selected expired entries.
    pub expired: usize,
    /// Selected revoked entries.
    pub revoked: usize,
    /// Total selected entries (`expired + revoked`).
    pub total: usize,
}

/// Selects candidates under an operator policy; it does not prove them disposable.
pub struct PruningCandidates {
    config: PruningConfig,
}

impl PruningCandidates {
    /// Select under the supplied operator policy.
    pub fn new(config: PruningConfig) -> Self {
        Self { config }
    }

    /// Select TRST entries for a later dependency check.
    ///
    /// The caller must filter `expired_hashes` by expiry plus the configured age
    /// and supply revoked entries separately. This function does not inspect
    /// token state or use `now`.
    ///
    /// Takes expired entries first, then revoked entries if enabled, up to
    /// `batch_size`. Preserves input order and does not deduplicate hashes.
    pub fn select(
        &self,
        expired_hashes: &[TxHash],
        revoked_hashes: &[TxHash],
        _now: Timestamp,
    ) -> Vec<TxHash> {
        let mut candidates = Vec::new();

        if !self.config.enabled {
            return candidates;
        }

        candidates.extend(expired_hashes.iter().take(self.config.batch_size).cloned());

        if self.config.prune_revoked {
            let remaining = self.config.batch_size.saturating_sub(candidates.len());
            candidates.extend(revoked_hashes.iter().take(remaining).cloned());
        }

        candidates
    }

    /// Count selected candidates. No storage operation is performed.
    ///
    /// Selection alone never authorizes physical deletion.
    pub fn count_candidates(
        &self,
        expired_hashes: &[TxHash],
        revoked_hashes: &[TxHash],
        now: Timestamp,
    ) -> CandidateCounts {
        let candidates = self.select(expired_hashes, revoked_hashes, now);

        let expired_count = expired_hashes.len().min(candidates.len());
        let revoked_count = candidates.len().saturating_sub(expired_count);

        CandidateCounts {
            expired: expired_count,
            revoked: revoked_count,
            total: candidates.len(),
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
        let selector = PruningCandidates::new(PruningConfig {
            enabled: false,
            ..Default::default()
        });
        let expired = vec![tx(1), tx(2)];
        let revoked = vec![tx(3)];
        let result = selector.select(&expired, &revoked, Timestamp::new(100_000));
        assert!(result.is_empty());
    }

    #[test]
    fn test_enabled_pruning_collects_expired() {
        let selector = PruningCandidates::new(PruningConfig {
            enabled: true,
            prune_revoked: false,
            batch_size: 100,
            ..Default::default()
        });
        let expired = vec![tx(1), tx(2), tx(3)];
        let result = selector.select(&expired, &[], Timestamp::new(100_000));
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_revoked_pruning() {
        let selector = PruningCandidates::new(PruningConfig {
            enabled: true,
            prune_revoked: true,
            batch_size: 100,
            ..Default::default()
        });
        let revoked = vec![tx(10), tx(11)];
        let result = selector.select(&[], &revoked, Timestamp::new(100_000));
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_batch_size_limit() {
        let selector = PruningCandidates::new(PruningConfig {
            enabled: true,
            prune_revoked: true,
            batch_size: 2,
            ..Default::default()
        });
        let expired = vec![tx(1), tx(2), tx(3)];
        let revoked = vec![tx(10)];
        let result = selector.select(&expired, &revoked, Timestamp::new(100_000));
        // batch_size=2, so only 2 expired taken, no room for revoked.
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_batch_mixed_expired_and_revoked() {
        let selector = PruningCandidates::new(PruningConfig {
            enabled: true,
            prune_revoked: true,
            batch_size: 5,
            ..Default::default()
        });
        let expired = vec![tx(1), tx(2)];
        let revoked = vec![tx(10), tx(11), tx(12)];
        let result = selector.select(&expired, &revoked, Timestamp::new(100_000));
        // 2 expired + 3 revoked = 5, exactly the batch size.
        assert_eq!(result.len(), 5);
    }

    #[test]
    fn test_candidate_counts() {
        let selector = PruningCandidates::new(PruningConfig {
            enabled: true,
            prune_revoked: true,
            batch_size: 100,
            ..Default::default()
        });
        let expired = vec![tx(1), tx(2)];
        let revoked = vec![tx(10)];
        let result = selector.count_candidates(&expired, &revoked, Timestamp::new(100_000));
        assert_eq!(result.expired, 2);
        assert_eq!(result.revoked, 1);
        assert_eq!(result.total, 3);
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
