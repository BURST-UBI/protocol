//! Auto-merge logic for TRST tokens in the wallet.
//!
//! The whitepaper specifies: "The wallet software handles [merging] automatically
//! by default, grouping tokens with similar expiry dates to maximize retained value."
//! Implementation decisions say: "Auto merge with user-set limits, slider from 0 to 100."

use crate::portfolio::TrstHolding;
use burst_types::TrstState;

/// Configuration for the auto-merge behavior.
///
/// The `similarity_pct` controls how aggressive merging is:
/// - 0 = never auto-merge
/// - 100 = merge everything into one token (earliest expiry wins)
/// - 50 = merge tokens whose expiry dates are within 50% of each other
#[derive(Clone, Debug)]
pub struct AutoMergeConfig {
    /// Similarity threshold for expiry grouping (0..=100).
    /// Tokens are merged if the difference between their expiry times
    /// is within this percentage of the shorter expiry.
    pub similarity_pct: u8,
    /// Maximum number of tokens to merge at once (protocol max is 256).
    pub max_merge_inputs: usize,
    /// Whether auto-merge is enabled.
    pub enabled: bool,
}

impl Default for AutoMergeConfig {
    fn default() -> Self {
        Self {
            similarity_pct: 50,
            max_merge_inputs: 256,
            enabled: true,
        }
    }
}

/// A group of tokens that should be merged together.
#[derive(Clone, Debug)]
pub struct MergeGroup {
    /// Token IDs in this group.
    pub token_ids: Vec<String>,
    /// Total amount of all tokens in this group.
    pub total_amount: u128,
    /// The earliest expiry in this group (seconds remaining).
    pub earliest_expiry_secs: u64,
}

/// Analyze a portfolio of TRST holdings and return merge groups.
///
/// Only considers active (transferable) tokens. Tokens are grouped by
/// expiry similarity based on the configured threshold.
pub fn compute_merge_groups(holdings: &[TrstHolding], config: &AutoMergeConfig) -> Vec<MergeGroup> {
    if !config.enabled || config.similarity_pct == 0 {
        return Vec::new();
    }

    // Filter to active tokens with known expiry
    let mut active: Vec<&TrstHolding> = holdings
        .iter()
        .filter(|h| h.state == TrstState::Active && h.time_to_expiry_secs.is_some())
        .collect();

    if active.len() < 2 {
        return Vec::new();
    }

    // Sort by time-to-expiry ascending
    active.sort_by_key(|h| h.time_to_expiry_secs.unwrap_or(u64::MAX));

    let merge_all = config.similarity_pct >= 100;
    let threshold = config.similarity_pct as f64 / 100.0;
    let mut groups: Vec<MergeGroup> = Vec::new();
    let mut current_group: Vec<&TrstHolding> = vec![active[0]];
    let mut group_anchor_expiry = active[0].time_to_expiry_secs.unwrap_or(0);

    for holding in &active[1..] {
        let in_range = if merge_all {
            true
        } else {
            let expiry = holding.time_to_expiry_secs.unwrap_or(0);
            let diff = expiry.abs_diff(group_anchor_expiry);
            let max_diff = (group_anchor_expiry as f64 * threshold) as u64;
            diff <= max_diff
        };

        if in_range && current_group.len() < config.max_merge_inputs {
            current_group.push(holding);
        } else {
            if current_group.len() >= 2 {
                groups.push(make_group(&current_group));
            }
            current_group = vec![holding];
            group_anchor_expiry = holding.time_to_expiry_secs.unwrap_or(0);
        }
    }

    // Don't forget the last group
    if current_group.len() >= 2 {
        groups.push(make_group(&current_group));
    }

    groups
}

fn make_group(holdings: &[&TrstHolding]) -> MergeGroup {
    let token_ids = holdings.iter().map(|h| h.token_id.clone()).collect();
    let total_amount = holdings.iter().map(|h| h.amount).sum();
    let earliest_expiry_secs = holdings
        .iter()
        .filter_map(|h| h.time_to_expiry_secs)
        .min()
        .unwrap_or(0);
    MergeGroup {
        token_ids,
        total_amount,
        earliest_expiry_secs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::Timestamp;

    fn holding(id: &str, amount: u128, expiry_secs: u64) -> TrstHolding {
        TrstHolding {
            token_id: id.to_string(),
            amount,
            origin_wallet: "brst_test".to_string(),
            origin_timestamp: Timestamp::new(1000),
            state: TrstState::Active,
            time_to_expiry_secs: Some(expiry_secs),
        }
    }

    #[test]
    fn disabled_returns_empty() {
        let config = AutoMergeConfig {
            enabled: false,
            ..Default::default()
        };
        let holdings = vec![holding("a", 100, 1000), holding("b", 200, 1100)];
        assert!(compute_merge_groups(&holdings, &config).is_empty());
    }

    #[test]
    fn zero_similarity_returns_empty() {
        let config = AutoMergeConfig {
            similarity_pct: 0,
            ..Default::default()
        };
        let holdings = vec![holding("a", 100, 1000), holding("b", 200, 1100)];
        assert!(compute_merge_groups(&holdings, &config).is_empty());
    }

    #[test]
    fn single_token_returns_empty() {
        let config = AutoMergeConfig::default();
        let holdings = vec![holding("a", 100, 1000)];
        assert!(compute_merge_groups(&holdings, &config).is_empty());
    }

    #[test]
    fn similar_expiry_tokens_grouped() {
        let config = AutoMergeConfig {
            similarity_pct: 50,
            ..Default::default()
        };
        // 1000 and 1200 differ by 200, which is 20% of 1000 — within 50% threshold
        let holdings = vec![holding("a", 100, 1000), holding("b", 200, 1200)];
        let groups = compute_merge_groups(&holdings, &config);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].token_ids.len(), 2);
        assert_eq!(groups[0].total_amount, 300);
        assert_eq!(groups[0].earliest_expiry_secs, 1000);
    }

    #[test]
    fn dissimilar_expiry_tokens_separate() {
        let config = AutoMergeConfig {
            similarity_pct: 10,
            ..Default::default()
        };
        // 1000 and 5000 differ by 4000, which is 400% of 1000 — way above 10%
        let holdings = vec![holding("a", 100, 1000), holding("b", 200, 5000)];
        let groups = compute_merge_groups(&holdings, &config);
        assert!(groups.is_empty()); // neither group has 2+ tokens
    }

    #[test]
    fn full_merge_groups_everything() {
        let config = AutoMergeConfig {
            similarity_pct: 100,
            ..Default::default()
        };
        let holdings = vec![
            holding("a", 100, 1000),
            holding("b", 200, 5000),
            holding("c", 300, 9000),
        ];
        let groups = compute_merge_groups(&holdings, &config);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].token_ids.len(), 3);
        assert_eq!(groups[0].total_amount, 600);
    }

    #[test]
    fn expired_tokens_excluded() {
        let config = AutoMergeConfig::default();
        let mut h = holding("a", 100, 1000);
        h.state = TrstState::Expired;
        let holdings = vec![h, holding("b", 200, 1100)];
        assert!(compute_merge_groups(&holdings, &config).is_empty());
    }
}
