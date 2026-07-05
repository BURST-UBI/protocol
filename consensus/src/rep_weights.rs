//! Cached representative weights for ORV consensus.
//!
//! Avoids scanning all accounts to compute a representative's total weight.
//! The cache is rebuilt at node startup from the full account set, then
//! maintained incrementally as accounts accrue weight and change rep.
//!
//! ## Weight = EXPIRED TRST (not spendable balance)
//!
//! An account's consensus weight equals its `expired_trst` — the cumulative
//! amount of its TRST that has passed its expiry and become **non-transferable
//! "virtue points"**. Spendable (transferable) balance carries ZERO consensus
//! weight. This is deliberate and is the core of BURST's anti-plutocracy ORV:
//!
//! - **Un-buyable.** Expired TRST cannot be transferred, so consensus influence
//!   cannot be bought, borrowed, or moved — only *earned* and held.
//! - **Egalitarian baseline.** Anyone can accrue weight by letting their own
//!   birthright-minted TRST expire in their ledger; since birthright accrues at
//!   a per-human rate, this trends toward one-human-one-baseline over time.
//! - **Meritocratic bonus.** Others route their soon-to-expire TRST to servers
//!   they trust; that value expires in the recipient's ledger and lifts its
//!   weight — trust, demonstrated by real economic flow, becomes consensus say.
//!
//! Weight is denominated in raw TRST units (u128). It changes when TRST expires
//! (the expiry flush accrues `expired_trst`) and when an account changes its
//! representative; it does NOT change on ordinary balance movements.

use burst_types::WalletAddress;
use std::collections::HashMap;

/// Cached representative weights for ORV (weight = delegated expired TRST).
pub struct RepWeightCache {
    /// representative_address → total delegated *expired* TRST.
    weights: HashMap<WalletAddress, u128>,
    /// Total weight across all representatives.
    total_weight: u128,
}

impl RepWeightCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self {
            weights: HashMap::new(),
            total_weight: 0,
        }
    }

    /// Add weight to a representative (when an account delegates to them).
    pub fn add_weight(&mut self, rep: &WalletAddress, weight: u128) {
        let entry = self.weights.entry(rep.clone()).or_insert(0);
        *entry = entry.saturating_add(weight);
        self.total_weight = self.total_weight.saturating_add(weight);
    }

    /// Remove weight from a representative (when an account changes rep).
    pub fn remove_weight(&mut self, rep: &WalletAddress, weight: u128) {
        if let Some(entry) = self.weights.get_mut(rep) {
            let removed = weight.min(*entry);
            *entry -= removed;
            self.total_weight = self.total_weight.saturating_sub(removed);
            if *entry == 0 {
                self.weights.remove(rep);
            }
        }
    }

    /// Process a ChangeRepresentative: remove weight from old rep, add to new rep.
    pub fn change_rep(&mut self, old_rep: &WalletAddress, new_rep: &WalletAddress, weight: u128) {
        self.remove_weight(old_rep, weight);
        self.add_weight(new_rep, weight);
    }

    /// Get a representative's current weight. Returns 0 if not found.
    pub fn weight(&self, rep: &WalletAddress) -> u128 {
        self.weights.get(rep).copied().unwrap_or(0)
    }

    /// Total weight across all representatives.
    pub fn total_weight(&self) -> u128 {
        self.total_weight
    }

    /// Get all representatives with their weights.
    pub fn all_weights(&self) -> &HashMap<WalletAddress, u128> {
        &self.weights
    }

    /// Number of representatives in the cache.
    pub fn rep_count(&self) -> usize {
        self.weights.len()
    }

    /// Rebuild cache from a full account iterator.
    ///
    /// Called during node startup. Each item yields
    /// `(account_address, representative_address, expired_trst)` — every
    /// account delegates its *expired* TRST (its consensus weight) to its
    /// representative. Spendable balance is intentionally NOT included.
    pub fn rebuild_from_accounts(
        &mut self,
        accounts: impl Iterator<Item = (WalletAddress, WalletAddress, u128)>,
    ) {
        self.weights.clear();
        self.total_weight = 0;
        for (_account, rep, expired_weight) in accounts {
            *self.weights.entry(rep).or_insert(0) += expired_weight;
            self.total_weight = self.total_weight.saturating_add(expired_weight);
        }
    }
}

impl Default for RepWeightCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::WalletAddress;

    fn rep(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{name}"))
    }

    fn account(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_acct_{name}"))
    }

    #[test]
    fn new_cache_is_empty() {
        let cache = RepWeightCache::new();
        assert_eq!(cache.total_weight(), 0);
        assert_eq!(cache.rep_count(), 0);
        assert_eq!(cache.weight(&rep("alice")), 0);
    }

    #[test]
    fn add_weight_single_rep() {
        let mut cache = RepWeightCache::new();
        cache.add_weight(&rep("alice"), 10_000);
        assert_eq!(cache.weight(&rep("alice")), 10_000);
        assert_eq!(cache.total_weight(), 10_000);
    }

    #[test]
    fn add_weight_multiple_reps() {
        let mut cache = RepWeightCache::new();
        cache.add_weight(&rep("alice"), 10_000);
        cache.add_weight(&rep("bob"), 20_000);
        cache.add_weight(&rep("alice"), 5_000);

        assert_eq!(cache.weight(&rep("alice")), 15_000);
        assert_eq!(cache.weight(&rep("bob")), 20_000);
        assert_eq!(cache.total_weight(), 35_000);
        assert_eq!(cache.rep_count(), 2);
    }

    #[test]
    fn remove_weight() {
        let mut cache = RepWeightCache::new();
        cache.add_weight(&rep("alice"), 10_000);
        cache.remove_weight(&rep("alice"), 3_000);

        assert_eq!(cache.weight(&rep("alice")), 7_000);
        assert_eq!(cache.total_weight(), 7_000);
    }

    #[test]
    fn remove_weight_clears_zero_entries() {
        let mut cache = RepWeightCache::new();
        cache.add_weight(&rep("alice"), 5_000);
        cache.remove_weight(&rep("alice"), 5_000);

        assert_eq!(cache.weight(&rep("alice")), 0);
        assert_eq!(cache.rep_count(), 0);
        assert_eq!(cache.total_weight(), 0);
    }

    #[test]
    fn remove_weight_clamped_to_zero() {
        let mut cache = RepWeightCache::new();
        cache.add_weight(&rep("alice"), 3_000);
        cache.remove_weight(&rep("alice"), 100_000);

        assert_eq!(cache.weight(&rep("alice")), 0);
        assert_eq!(cache.total_weight(), 0);
    }

    #[test]
    fn remove_weight_nonexistent_rep_is_noop() {
        let mut cache = RepWeightCache::new();
        cache.remove_weight(&rep("ghost"), 10_000);
        assert_eq!(cache.total_weight(), 0);
    }

    #[test]
    fn change_rep() {
        let mut cache = RepWeightCache::new();
        cache.add_weight(&rep("alice"), 10_000);
        cache.add_weight(&rep("bob"), 5_000);

        cache.change_rep(&rep("alice"), &rep("bob"), 4_000);

        assert_eq!(cache.weight(&rep("alice")), 6_000);
        assert_eq!(cache.weight(&rep("bob")), 9_000);
        assert_eq!(cache.total_weight(), 15_000);
    }

    #[test]
    fn change_rep_to_new_rep() {
        let mut cache = RepWeightCache::new();
        cache.add_weight(&rep("alice"), 10_000);

        cache.change_rep(&rep("alice"), &rep("carol"), 10_000);

        assert_eq!(cache.weight(&rep("alice")), 0);
        assert_eq!(cache.weight(&rep("carol")), 10_000);
        assert_eq!(cache.total_weight(), 10_000);
        assert_eq!(cache.rep_count(), 1);
    }

    #[test]
    fn change_rep_same_rep_is_identity() {
        let mut cache = RepWeightCache::new();
        cache.add_weight(&rep("alice"), 10_000);

        cache.change_rep(&rep("alice"), &rep("alice"), 5_000);

        assert_eq!(cache.weight(&rep("alice")), 10_000);
        assert_eq!(cache.total_weight(), 10_000);
    }

    #[test]
    fn rebuild_from_accounts() {
        let mut cache = RepWeightCache::new();
        // Pre-existing state should be cleared
        cache.add_weight(&rep("stale"), 999_000);

        let accounts = vec![
            (account("1"), rep("alice"), 1_000_000),
            (account("2"), rep("alice"), 2_500_000),
            (account("3"), rep("bob"), 500_000),
            (account("4"), rep("alice"), 750_000),
            (account("5"), rep("carol"), 3_000_000),
        ];

        cache.rebuild_from_accounts(accounts.into_iter());

        assert_eq!(cache.weight(&rep("alice")), 4_250_000);
        assert_eq!(cache.weight(&rep("bob")), 500_000);
        assert_eq!(cache.weight(&rep("carol")), 3_000_000);
        assert_eq!(cache.weight(&rep("stale")), 0);
        assert_eq!(cache.total_weight(), 7_750_000);
        assert_eq!(cache.rep_count(), 3);
    }

    #[test]
    fn rebuild_clears_previous_state() {
        let mut cache = RepWeightCache::new();
        cache.add_weight(&rep("alice"), 100_000);

        cache.rebuild_from_accounts(std::iter::empty());

        assert_eq!(cache.total_weight(), 0);
        assert_eq!(cache.rep_count(), 0);
    }

    #[test]
    fn all_weights_returns_complete_map() {
        let mut cache = RepWeightCache::new();
        cache.add_weight(&rep("alice"), 10_000);
        cache.add_weight(&rep("bob"), 20_000);

        let all = cache.all_weights();
        assert_eq!(all.len(), 2);
        assert_eq!(all.get(&rep("alice")), Some(&10_000u128));
        assert_eq!(all.get(&rep("bob")), Some(&20_000u128));
    }

    #[test]
    fn saturation_add_weight() {
        let mut cache = RepWeightCache::new();
        cache.add_weight(&rep("alice"), u128::MAX);
        cache.add_weight(&rep("alice"), 1);

        assert_eq!(cache.weight(&rep("alice")), u128::MAX);
    }

    #[test]
    fn rebuild_with_zero_balance_accounts() {
        let mut cache = RepWeightCache::new();
        let accounts = vec![
            (account("1"), rep("alice"), 1_000),
            (account("2"), rep("alice"), 0),
            (account("3"), rep("bob"), 0),
        ];

        cache.rebuild_from_accounts(accounts.into_iter());

        assert_eq!(cache.weight(&rep("alice")), 1_000);
        assert_eq!(cache.weight(&rep("bob")), 0);
        assert_eq!(cache.total_weight(), 1_000);
    }

    #[test]
    fn expired_trst_models_baseline_plus_trust_bonus() {
        // Documents the ORV weight semantics: the third tuple field is each
        // account's EXPIRED TRST. Two humans who each let their own birthright-
        // minted TRST expire accrue an equal baseline; a trusted server that
        // *also* received others' expiring TRST accrues a meritocratic bonus on
        // top. All self-represent (rep == account).
        let mut cache = RepWeightCache::new();
        let alice = account("alice");
        let bob = account("bob");
        let server = account("server");
        cache.rebuild_from_accounts(
            vec![
                // baseline: own birthright TRST that expired in-place
                (alice.clone(), alice.clone(), 100),
                (bob.clone(), bob.clone(), 100),
                // baseline + trust routed in from others that expired here
                (server.clone(), server.clone(), 100 + 900),
            ]
            .into_iter(),
        );
        assert_eq!(cache.weight(&alice), 100); // equal baseline
        assert_eq!(cache.weight(&bob), 100); // equal baseline
        assert_eq!(cache.weight(&server), 1000); // baseline + earned trust
        assert_eq!(cache.total_weight(), 1200);
    }

    #[test]
    fn large_balance_values() {
        let mut cache = RepWeightCache::new();
        let large_balance: u128 = 1_000_000_000_000_000_000; // 10^18
        cache.add_weight(&rep("alice"), large_balance);
        cache.add_weight(&rep("bob"), large_balance * 2);

        assert_eq!(cache.weight(&rep("alice")), large_balance);
        assert_eq!(cache.weight(&rep("bob")), large_balance * 2);
        assert_eq!(cache.total_weight(), large_balance * 3);
    }
}
