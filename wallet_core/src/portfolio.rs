//! TRST portfolio management — tracking transferable, expired, and revoked TRST.

use burst_types::{Timestamp, TrstState};
use serde::{Deserialize, Serialize};

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

    /// Compute the maximum expiry difference (in seconds) that tokens
    /// must be within to be considered merge candidates.
    ///
    /// At aggressiveness 0: no merging.
    /// At aggressiveness 50: merge tokens within 10% expiry difference.
    /// At aggressiveness 100: merge everything regardless of expiry.
    pub fn max_expiry_diff_secs(&self, trst_expiry_secs: u64) -> u64 {
        if self.aggressiveness == 0 {
            return 0;
        }
        if self.aggressiveness >= 100 {
            return u64::MAX;
        }
        // Scale linearly: at 50 aggressiveness, allow 10% of total expiry
        // At 100, allow 100% (everything). Formula: (aggressiveness/100) * expiry * 0.2
        // This means at 50, it's 10% of expiry; at 100, it's 20% of expiry.
        // But we want 100 to merge everything, so use a different scale:
        // max_diff = (aggressiveness / 50) * (expiry * 0.1)
        // So at 50 -> 10%, at 100 -> 20%. For "merge all", aggressiveness=100 returns MAX above.
        let pct = (self.aggressiveness as u64 * 20) / 100; // 0-20%
        (trst_expiry_secs * pct) / 100
    }

    /// Find groups of tokens that should be merged together based on this policy.
    ///
    /// Returns groups of token IDs where each group shares similar expiry dates.
    /// Each group has at least 2 tokens (since merging a single token is a no-op).
    pub fn find_merge_candidates(
        &self,
        holdings: &[TrstHolding],
        trst_expiry_secs: u64,
    ) -> Vec<Vec<String>> {
        if !self.is_enabled() || holdings.len() < 2 {
            return Vec::new();
        }

        let max_diff = self.max_expiry_diff_secs(trst_expiry_secs);

        // Sort holdings by origin timestamp (proxy for expiry date)
        let mut sorted: Vec<&TrstHolding> = holdings
            .iter()
            .filter(|h| h.state == TrstState::Active)
            .collect();
        sorted.sort_by_key(|h| h.origin_timestamp.as_secs());

        // Group tokens with origin timestamps within max_diff of each other
        let mut groups: Vec<Vec<String>> = Vec::new();
        let mut current_group: Vec<String> = Vec::new();
        let mut group_start_ts: u64 = 0;

        for holding in sorted {
            let ts = holding.origin_timestamp.as_secs();
            if current_group.is_empty() {
                current_group.push(holding.token_id.clone());
                group_start_ts = ts;
            } else if ts.saturating_sub(group_start_ts) <= max_diff {
                current_group.push(holding.token_id.clone());
            } else {
                if current_group.len() >= 2 {
                    groups.push(current_group);
                }
                current_group = vec![holding.token_id.clone()];
                group_start_ts = ts;
            }
        }
        if current_group.len() >= 2 {
            groups.push(current_group);
        }

        groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_holding(id: &str, origin_ts: u64) -> TrstHolding {
        TrstHolding {
            token_id: id.to_string(),
            amount: 100,
            origin_wallet: "brst_test".to_string(),
            origin_timestamp: Timestamp::new(origin_ts),
            state: TrstState::Active,
            time_to_expiry_secs: Some(3600),
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
