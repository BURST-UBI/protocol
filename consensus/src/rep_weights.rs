//! Representative weights: one equal unit per verified human.
//!
//! Delegation assigns that unit; it does not create weight. BRN, TRST (including
//! expired reputation), and genesis status confer no extra votes. Verification
//! must enforce human uniqueness; this cache cannot establish it independently.

use burst_types::WalletAddress;
use std::collections::{HashMap, HashSet};

/// Fixed-point units per verified human, preserving quorum arithmetic precision.
pub const WEIGHT_PER_HUMAN: u128 = 10_000;

/// Cached representative weights, derived only from verified-person delegation.
pub struct RepWeightCache {
    /// Derived aggregates per representative (cleared and rebuilt from accounts).
    reps: HashMap<WalletAddress, u128>,
    /// Representatives EVICTED from the ORV set by governance. An evicted rep
    /// reports ZERO weight, so its votes stop counting
    /// toward quorum. Set by [`RepWeightCache::set_evicted`] from the on-chain
    /// `orv_evicted` flags each rebuild.
    evicted: HashSet<WalletAddress>,
}

impl RepWeightCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self {
            reps: HashMap::new(),
            evicted: HashSet::new(),
        }
    }

    /// A representative's delegated verified-person weight.
    /// Returns 0 for an unknown OR evicted representative.
    pub fn weight(&self, rep: &WalletAddress) -> u128 {
        if self.evicted.contains(rep) {
            return 0;
        }
        self.reps.get(rep).copied().unwrap_or(0)
    }

    /// Materialize every representative's weight as a map (used by the online
    /// weight sampler and the `representatives` RPC). Evicted reps are omitted.
    pub fn all_weights(&self) -> HashMap<WalletAddress, u128> {
        let mut out: HashMap<WalletAddress, u128> = HashMap::with_capacity(self.reps.len());
        for (rep, e) in &self.reps {
            if self.evicted.contains(rep) {
                continue;
            }
            out.insert(rep.clone(), *e);
        }
        out
    }

    /// Replace the set of representatives evicted from the ORV consensus set.
    /// Called from the rebuild with the current on-chain `orv_evicted` flags.
    pub fn set_evicted(&mut self, evicted: HashSet<WalletAddress>) {
        self.evicted = evicted;
    }

    /// Whether a representative is currently evicted.
    pub fn is_evicted(&self, rep: &WalletAddress) -> bool {
        self.evicted.contains(rep)
    }

    /// Total consensus weight across all representatives.
    pub fn total_weight(&self) -> u128 {
        self.all_weights()
            .values()
            .fold(0u128, |acc, w| acc.saturating_add(*w))
    }

    /// Number of representatives with active delegated weight.
    pub fn rep_count(&self) -> usize {
        self.all_weights().len()
    }

    /// Rebuild from each unique account's verification status and representative.
    /// Monetary/reputation fields are deliberately absent from this interface.
    pub fn rebuild_from_accounts(&mut self, accounts: impl Iterator<Item = (bool, WalletAddress)>) {
        self.reps.clear();
        for (verified, rep) in accounts {
            if !verified {
                continue;
            }
            let e = self.reps.entry(rep).or_default();
            *e = e.saturating_add(WEIGHT_PER_HUMAN);
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
    fn rep(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{name}"))
    }

    #[test]
    fn empty_cache_has_no_privileged_weight() {
        let cache = RepWeightCache::new();
        assert_eq!(cache.total_weight(), 0);
        assert_eq!(cache.rep_count(), 0);
        assert_eq!(cache.weight(&rep("genesis")), 0);
    }

    #[test]
    fn each_verified_person_counts_equally_including_genesis() {
        let mut cache = RepWeightCache::new();
        cache.rebuild_from_accounts(
            ["genesis", "alice", "bob"]
                .map(|n| (true, rep(n)))
                .into_iter(),
        );
        for n in ["genesis", "alice", "bob"] {
            assert_eq!(cache.weight(&rep(n)), WEIGHT_PER_HUMAN);
        }
        assert_eq!(cache.total_weight(), 3 * WEIGHT_PER_HUMAN);
    }

    #[test]
    fn delegation_aggregates_people_not_representative_count() {
        let mut cache = RepWeightCache::new();
        cache.rebuild_from_accounts((0..100).map(|_| (true, rep("a"))).chain([(true, rep("b"))]));
        assert_eq!(cache.weight(&rep("a")), 100 * WEIGHT_PER_HUMAN);
        assert_eq!(cache.weight(&rep("b")), WEIGHT_PER_HUMAN);
        assert_eq!(cache.rep_count(), 2);
        assert_eq!(cache.total_weight(), 101 * WEIGHT_PER_HUMAN);
    }

    #[test]
    fn unverified_accounts_never_add_weight() {
        let mut cache = RepWeightCache::new();
        cache.rebuild_from_accounts(
            (0..1000)
                .map(|_| (false, rep("attacker")))
                .chain([(true, rep("human"))]),
        );
        assert_eq!(cache.weight(&rep("attacker")), 0);
        assert_eq!(cache.rep_count(), 1);
        assert_eq!(cache.total_weight(), WEIGHT_PER_HUMAN);
    }

    #[test]
    fn rebuild_redelegates_without_duplicating_weight() {
        let mut cache = RepWeightCache::new();
        cache.rebuild_from_accounts([(true, rep("a")), (true, rep("a"))].into_iter());
        for _ in 0..3 {
            cache.rebuild_from_accounts([(true, rep("a")), (true, rep("b"))].into_iter());
            assert_eq!(cache.weight(&rep("a")), WEIGHT_PER_HUMAN);
            assert_eq!(cache.weight(&rep("b")), WEIGHT_PER_HUMAN);
            assert_eq!(cache.total_weight(), 2 * WEIGHT_PER_HUMAN);
        }
        cache.rebuild_from_accounts([(false, rep("a")), (true, rep("b"))].into_iter());
        assert_eq!(cache.total_weight(), WEIGHT_PER_HUMAN);
        cache.rebuild_from_accounts(std::iter::empty());
        assert_eq!(cache.total_weight(), 0);
    }

    #[test]
    fn eviction_and_reinstatement_preserve_equal_units() {
        let mut cache = RepWeightCache::new();
        cache.rebuild_from_accounts([(true, rep("a")), (true, rep("a"))].into_iter());
        cache.set_evicted(HashSet::from([rep("a")]));
        assert!(cache.is_evicted(&rep("a")));
        assert_eq!(cache.weight(&rep("a")), 0);
        assert_eq!(cache.rep_count(), 0);
        cache.set_evicted(HashSet::new());
        assert_eq!(cache.weight(&rep("a")), 2 * WEIGHT_PER_HUMAN);
        assert_eq!(
            cache.all_weights().get(&rep("a")),
            Some(&(2 * WEIGHT_PER_HUMAN))
        );
    }
}
