//! Representative weights: one equal unit per verified human.
//!
//! Delegation assigns that unit; it does not create weight. BRN, TRST (including
//! expired reputation), and genesis status confer no extra votes. Verification
//! must enforce human uniqueness; this cache cannot establish it independently.

use burst_types::WalletAddress;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Fixed-point units per verified human, preserving quorum arithmetic precision.
pub const WEIGHT_PER_HUMAN: u128 = 10_000;

/// Cached representative weights, derived only from verified-person delegation.
pub struct RepWeightCache {
    /// Derived aggregates per representative (cleared and rebuilt from accounts).
    reps: Arc<HashMap<WalletAddress, u128>>,
    publication_identity: Option<usize>,
    /// Representatives excluded from current weight lookups and snapshots.
    /// Updating this set does not rewrite an election's frozen authority.
    evicted: Arc<HashSet<WalletAddress>>,
}

/// Immutable publication roots for reads outside the live cache lock.
#[derive(Clone)]
pub struct RepWeightSnapshot {
    reps: Arc<HashMap<WalletAddress, u128>>,
    evicted: Arc<HashSet<WalletAddress>>,
}

impl RepWeightSnapshot {
    pub fn weights(&self) -> impl Iterator<Item = (&WalletAddress, u128)> {
        self.reps.iter().filter(|(rep, _)| !self.evicted.contains(*rep))
            .map(|(rep, weight)| (rep, *weight))
    }
}

impl RepWeightCache {
    pub fn snapshot(&self) -> RepWeightSnapshot {
        RepWeightSnapshot { reps: self.reps.clone(), evicted: self.evicted.clone() }
    }

    /// Caller-supplied publication identity; this cache does not validate authority.
    pub fn publication_identity(&self) -> Option<usize> { self.publication_identity }

    pub fn mark_publication(&mut self, identity: Option<usize>) {
        self.publication_identity = identity;
    }

    /// Create an empty cache.
    pub fn new() -> Self {
        Self {
            reps: Arc::new(HashMap::new()),
            publication_identity: None,
            evicted: Arc::new(HashSet::new()),
        }
    }

    /// A representative's delegated verified-person weight.
    /// Returns zero for an unknown or evicted representative.
    pub fn weight(&self, rep: &WalletAddress) -> u128 {
        if self.evicted.contains(rep) {
            return 0;
        }
        self.reps.get(rep).copied().unwrap_or(0)
    }

    /// Materialize every representative's weight as a map (used by the online
    /// weight sampler). RPC uses shared snapshots. Evicted reps are omitted.
    pub fn all_weights(&self) -> HashMap<WalletAddress, u128> {
        let mut out: HashMap<WalletAddress, u128> = HashMap::with_capacity(self.reps.len());
        for (rep, e) in self.reps.iter() {
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
        self.publication_identity = None;
        self.evicted = Arc::new(evicted);
    }

    /// Whether a representative is currently evicted.
    pub fn is_evicted(&self, rep: &WalletAddress) -> bool {
        self.evicted.contains(rep)
    }

    /// Total consensus weight across all representatives.
    pub fn total_weight(&self) -> u128 {
        self.reps.iter().filter(|(rep, _)| !self.evicted.contains(*rep))
            .fold(0u128, |acc, (_, weight)| acc.saturating_add(*weight))
    }

    /// Number of representatives with active delegated weight.
    pub fn rep_count(&self) -> usize {
        self.reps.keys().filter(|rep| !self.evicted.contains(*rep)).count()
    }

    /// Rebuild from verification status and delegation. The caller must supply
    /// each account once; this iterator carries no account IDs for deduplication.
    /// Monetary/reputation fields are deliberately absent from this interface.
    pub fn rebuild_from_accounts(&mut self, accounts: impl Iterator<Item = (bool, WalletAddress)>) {
        self.publication_identity = None;
        // A rebuild replaces every weight. If a reader retains the old map,
        // start empty instead of copying values that would immediately be cleared.
        if Arc::get_mut(&mut self.reps).is_none() {
            self.reps = Arc::new(HashMap::with_capacity(self.reps.len()));
        }
        let reps = Arc::get_mut(&mut self.reps).expect("unique rebuild root");
        reps.clear();
        for (verified, rep) in accounts {
            if !verified {
                continue;
            }
            let e = reps.entry(rep).or_default();
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
    fn snapshots_share_roots_and_preserve_weights_through_rebuild_and_eviction() {
        let mut cache = RepWeightCache::new();
        cache.rebuild_from_accounts([(true, rep("a")), (true, rep("a")), (true, rep("b"))].into_iter());
        cache.mark_publication(Some(7));
        let before = cache.snapshot();
        assert!(Arc::ptr_eq(&before.reps, &cache.reps));
        assert!(Arc::ptr_eq(&before.evicted, &cache.evicted));
        let expected: HashMap<_, _> = before.weights().map(|(a,w)| (a.clone(),w)).collect();
        assert_eq!(expected, cache.all_weights());
        cache.set_evicted(HashSet::from([rep("a")]));
        assert_eq!(cache.publication_identity(), None);
        let evicted = cache.snapshot();
        assert_eq!(evicted.weights().count(), 1);
        cache.rebuild_from_accounts([(true, rep("c"))].into_iter());
        assert_eq!(before.weights().map(|(a,w)| (a.clone(),w)).collect::<HashMap<_,_>>(), expected);
        assert_eq!(evicted.weights().collect::<Vec<_>>(), vec![(&rep("b"), WEIGHT_PER_HUMAN)]);
        assert_eq!(cache.weight(&rep("c")), WEIGHT_PER_HUMAN);
        assert_eq!(cache.total_weight(), WEIGHT_PER_HUMAN);
        assert_eq!(cache.rep_count(), 1);
        cache.set_evicted(HashSet::new());
        assert_eq!(before.weights().count(), 2);
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
