//! Auto-merge logic for TRST tokens in the wallet.

use crate::portfolio::TrstHolding;
use burst_types::TrstState;

/// Configuration for the auto-merge behavior.
///
/// The `max_lifetime_loss_pct` controls how aggressive merging is:
/// - 0 = never auto-merge
/// - 100 = group all eligible tokens, subject to the input limit
/// - 50 = merge tokens whose expiry dates are within 50% of each other
#[derive(Clone, Debug)]
pub struct AutoMergeConfig {
    /// Maximum lifetime loss allowed when grouping tokens (0..=100).
    /// Tokens are merged if the difference between their expiry times
    /// is within this percentage of the longer remaining lifetime.
    pub max_lifetime_loss_pct: u8,
    /// Per-group input limit, defaulting to the protocol maximum of 256.
    /// This public field is not clamped here; callers must keep it within bounds.
    pub max_merge_inputs: usize,
    /// Whether auto-merge is enabled.
    pub enabled: bool,
}

impl Default for AutoMergeConfig {
    fn default() -> Self {
        Self {
            max_lifetime_loss_pct: 10,
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
    /// Remaining lifetime of the merged output, in seconds.
    pub remaining_lifetime_secs: u64,
}

/// Analyze a portfolio of TRST holdings and return merge groups.
///
/// Selects holdings reported Active with a known remaining lifetime, sorts by
/// that lifetime, and groups against each group's earliest expiry. Does not
/// validate identifiers, deduplicate inputs, sum amounts or submit a merge.
pub fn compute_merge_groups(holdings: &[TrstHolding], config: &AutoMergeConfig) -> Vec<MergeGroup> {
    if !config.enabled || config.max_lifetime_loss_pct == 0 {
        return Vec::new();
    }

    let mut active: Vec<&TrstHolding> = holdings
        .iter()
        .filter(|h| h.state == TrstState::Active && h.time_to_expiry_secs.is_some())
        .collect();

    if active.len() < 2 {
        return Vec::new();
    }

    active.sort_by_key(|h| h.time_to_expiry_secs.unwrap_or(u64::MAX));

    let merge_all = config.max_lifetime_loss_pct >= 100;
    let mut groups: Vec<MergeGroup> = Vec::new();
    let mut current_group: Vec<&TrstHolding> = vec![active[0]];
    let mut group_anchor_expiry = active[0].time_to_expiry_secs.unwrap_or(0);

    for holding in &active[1..] {
        let in_range = if merge_all {
            true
        } else {
            let expiry = holding.time_to_expiry_secs.unwrap_or(0);
            let diff = expiry.abs_diff(group_anchor_expiry);
            let max_diff = (expiry as u128 * config.max_lifetime_loss_pct as u128 / 100) as u64;
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

    if current_group.len() >= 2 {
        groups.push(make_group(&current_group));
    }

    groups
}

fn make_group(holdings: &[&TrstHolding]) -> MergeGroup {
    let token_ids = holdings.iter().map(|h| h.token_id.clone()).collect();
    let remaining_lifetime_secs = holdings
        .iter()
        .filter_map(|h| h.time_to_expiry_secs)
        .min()
        .unwrap_or(0);
    MergeGroup {
        token_ids,
        remaining_lifetime_secs,
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
    fn default_matches_ten_percent_requirement_and_handles_unlimited_expiry() {
        let config = AutoMergeConfig::default();
        assert_eq!(config.max_lifetime_loss_pct, 10);
        assert_eq!(
            compute_merge_groups(
                &[
                    holding("a", 1, 1000),
                    holding("b", 1, 1100),
                    holding("c", 1, 1300)
                ],
                &config
            )[0]
            .token_ids,
            vec!["a", "b"]
        );
        assert_eq!(
            compute_merge_groups(
                &[holding("a", 1, u64::MAX), holding("b", 1, u64::MAX)],
                &config
            )
            .len(),
            1
        );
    }

    #[test]
    fn groups_ninety_to_one_hundred_days_without_chaining() {
        let day = 86_400;
        let groups = compute_merge_groups(
            &[
                holding("latest", 3, 100 * day),
                holding("earliest", 2, 90 * day),
                holding("too_late", 5, 100 * day + 1),
                holding("chained", 7, 110 * day),
            ],
            &AutoMergeConfig::default(),
        );
        assert_eq!(groups[0].token_ids, vec!["earliest", "latest"]);
        assert_eq!(groups[0].remaining_lifetime_secs, 90 * day);
        assert_eq!(groups[1].token_ids, vec!["too_late", "chained"]);
        assert!(compute_merge_groups(
            &[holding("a", 1, 90 * day - 1), holding("b", 1, 100 * day),],
            &AutoMergeConfig::default()
        )
        .is_empty());
    }

    #[test]
    fn grouping_does_not_sum_untrusted_amounts() {
        let groups = compute_merge_groups(
            &[holding("a", u128::MAX, 1000), holding("b", 1, 1000)],
            &AutoMergeConfig::default(),
        );
        assert_eq!(groups[0].token_ids, vec!["a", "b"]);
    }

    #[test]
    fn zero_lifetime_loss_returns_empty() {
        let config = AutoMergeConfig {
            max_lifetime_loss_pct: 0,
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
            max_lifetime_loss_pct: 50,
            ..Default::default()
        };
        // The earliest lifetime retains more than half of the latest.
        let holdings = vec![holding("a", 100, 1000), holding("b", 200, 1200)];
        let groups = compute_merge_groups(&holdings, &config);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].token_ids, vec!["a", "b"]);
        assert_eq!(groups[0].remaining_lifetime_secs, 1000);
    }

    #[test]
    fn dissimilar_expiry_tokens_separate() {
        let config = AutoMergeConfig {
            max_lifetime_loss_pct: 10,
            ..Default::default()
        };
        // The lifetime loss is 80%, above the 10% threshold.
        let holdings = vec![holding("a", 100, 1000), holding("b", 200, 5000)];
        let groups = compute_merge_groups(&holdings, &config);
        assert!(groups.is_empty()); // neither group has 2+ tokens
    }

    #[test]
    fn full_merge_groups_everything() {
        let config = AutoMergeConfig {
            max_lifetime_loss_pct: 100,
            ..Default::default()
        };
        let holdings = vec![
            holding("a", 100, 1000),
            holding("b", 200, 5000),
            holding("c", 300, 9000),
        ];
        let groups = compute_merge_groups(&holdings, &config);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].token_ids, vec!["a", "b", "c"]);
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
