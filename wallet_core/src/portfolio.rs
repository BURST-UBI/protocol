//! TRST portfolio management — tracking transferable, expired, and revoked TRST.

use burst_types::{Timestamp, TrstState};
use serde::{Deserialize, Serialize};

mod review;
pub use review::{reviewed_merge_sources, select_transfer_origin, MergeSelection};

/// A TRST holding in the wallet's portfolio.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrstHolding {
    pub token_id: String,
    pub amount: u128,
    pub origin_wallet: String,
    pub origin_timestamp: Timestamp,
    pub state: TrstState,
    pub time_to_expiry_secs: Option<u64>,
}

/// Summary of a wallet's TRST portfolio.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortfolioSummary {
    pub transferable: u128,
    pub expired: u128,
    pub revoked: u128,
    pub total: u128,
    pub num_tokens: usize,
}

/// Auto-merge policy for grouping TRST tokens by expiry similarity.
///
/// The whitepaper states wallets auto-merge TRST with similar expiry dates
/// to maximize retained value. The user can configure aggressiveness via
/// a slider from 0 (never auto-merge) to 100 (merge everything).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AutoMergePolicy {
    /// Aggressiveness level (0-100). 0 = never auto-merge, 100 = merge all.
    pub aggressiveness: u8,
}

impl Default for AutoMergePolicy {
    fn default() -> Self {
        Self { aggressiveness: 50 }
    }
}

impl AutoMergePolicy {
    /// Create a new policy with given aggressiveness (0-100).
    pub fn new(aggressiveness: u8) -> Self {
        Self {
            aggressiveness: aggressiveness.min(100),
        }
    }

    /// Whether auto-merge is enabled at all.
    pub fn is_enabled(&self) -> bool {
        self.aggressiveness > 0
    }

    pub fn config(&self) -> crate::auto_merge::AutoMergeConfig {
        crate::auto_merge::AutoMergeConfig {
            similarity_pct: if self.aggressiveness >= 100 {
                100
            } else {
                self.aggressiveness / 5
            },
            enabled: self.is_enabled(),
            ..Default::default()
        }
    }

    /// Maximum loss relative to the longest remaining lifetime in a group.
    pub fn max_expiry_diff_secs(&self, longest_remaining_secs: u64) -> u64 {
        let config = self.config();
        if config.similarity_pct == 100 {
            return u64::MAX;
        }
        ((longest_remaining_secs as u128 * config.similarity_pct as u128) / 100) as u64
    }

    /// Find groups of tokens that should be merged together based on this policy.
    ///
    /// Returns groups of token IDs where each group shares similar expiry dates.
    /// Uses each holding's remaining lifetime, not its origin timestamp. The
    /// legacy expiry argument is retained for callers; it cannot override that data.
    pub fn find_merge_candidates(
        &self,
        holdings: &[TrstHolding],
        _trst_expiry_secs: u64,
    ) -> Vec<Vec<String>> {
        crate::auto_merge::compute_merge_groups(holdings, &self.config())
            .into_iter()
            .map(|group| group.token_ids)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_holding(id: &str, remaining_secs: u64) -> TrstHolding {
        TrstHolding {
            token_id: id.to_string(),
            amount: 100,
            origin_wallet: "brst_test".to_string(),
            origin_timestamp: Timestamp::new(1000),
            state: TrstState::Active,
            time_to_expiry_secs: Some(remaining_secs),
        }
    }

    #[test]
    fn auto_merge_disabled_at_zero() {
        let policy = AutoMergePolicy::new(0);
        assert!(!policy.is_enabled());
        let holdings = vec![make_holding("a", 1000), make_holding("b", 1001)];
        assert!(policy.find_merge_candidates(&holdings, 86400).is_empty());
    }

    #[test]
    fn auto_merge_groups_similar_expiry() {
        let policy = AutoMergePolicy::new(50);
        let holdings = vec![
            make_holding("a", 1000),
            make_holding("b", 1005),
            make_holding("c", 50000),
        ];
        let groups = policy.find_merge_candidates(&holdings, 86400);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], vec!["a", "b"]);
    }

    #[test]
    fn auto_merge_everything_at_100() {
        let policy = AutoMergePolicy::new(100);
        let holdings = vec![
            make_holding("a", 1000),
            make_holding("b", 50000),
            make_holding("c", 99999),
        ];
        let groups = policy.find_merge_candidates(&holdings, 86400);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 3);
    }

    #[test]
    fn auto_merge_skips_single_tokens() {
        let policy = AutoMergePolicy::new(50);
        let holdings = vec![make_holding("a", 1000)];
        assert!(policy.find_merge_candidates(&holdings, 86400).is_empty());
    }

    #[test]
    fn auto_merge_default_is_50() {
        let policy = AutoMergePolicy::default();
        assert_eq!(policy.aggressiveness, 50);
        assert!(policy.is_enabled());
    }
}
