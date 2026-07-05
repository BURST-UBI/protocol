//! Cached representative weights for ORV consensus — the **hybrid** model.
//!
//! A representative's consensus weight has two parts:
//!
//! ```text
//!   weight(rep) = (verified humans delegating to rep) × (1 + contribution_boost)
//!                 + raw_bonus(rep)
//! ```
//!
//! ## Why hybrid
//!
//! BURST verifies unique humans (PoUH), so — unlike Nano, which weights by
//! balance purely to resist Sybils it cannot otherwise stop — BURST does NOT
//! need economic weight for Sybil resistance. That frees the weighting to be a
//! *values* choice rather than a security necessity. The hybrid encodes:
//!
//! - **Identity baseline (egalitarian, un-buyable):** each *verified* human
//!   contributes one equal unit of weight, delegated to their representative.
//!   Sybil resistance comes from verification, not from money.
//! - **Contribution multiplier (meritocratic, bounded):** a rep's accumulated
//!   *expired* TRST (the whitepaper's non-transferable "virtue points" — a
//!   record of value provided) amplifies its democratic mandate, but only up to
//!   a hard cap. Crucially it is a MULTIPLIER on identity, never an addend:
//!   contribution × zero delegators = zero. You cannot buy weight from nothing;
//!   the most money can do is give an already-trusted rep a bounded edge.
//!
//! At launch (expiry disabled → nothing has expired) every boost is zero, so the
//! hybrid degenerates cleanly to one-verified-human-one-vote. The contribution
//! term only engages once TRST begins to expire.
//!
//! ## Derivation, not incremental hooks
//!
//! Weight is a pure function of confirmed account state (who is verified, who
//! they delegate to, how much has expired for each). The cache is therefore
//! REBUILT from the account set — at startup and periodically — rather than
//! patched at every verification / delegation / expiry event, which would be a
//! consensus-corruption hazard. The one non-derived piece, `raw_bonus` (the
//! genesis bootstrap bridge), is preserved across rebuilds.

use burst_types::WalletAddress;
use std::collections::HashMap;

/// Base consensus weight contributed by one verified human, before the
/// contribution multiplier. Large enough to give the boost bps resolution.
pub const WEIGHT_PER_HUMAN: u128 = 10_000;

/// Maximum contribution boost, in basis points of the identity baseline.
/// 5_000 bps = +50%: a maximally-contributing rep's human mandate is amplified
/// by at most half. Bounded so accumulated wealth can never dominate identity.
pub const CONTRIBUTION_CAP_BPS: u128 = 5_000;

/// Expired-TRST amount at which the boost reaches HALF the cap, on the saturating
/// curve `boost = CAP · E / (E + SCALE)`. Mis-calibration degrades gracefully:
/// too low → real contributors cluster near the cap (≈ a flat bonus); too high →
/// boost ≈ 0 (≈ pure one-human-one-vote). Both remain safe because the identity
/// baseline dominates and the boost is capped regardless.
pub const CONTRIBUTION_SCALE: u128 = 1_000 * burst_types::TRST_UNIT;

/// Per-representative aggregates, rebuilt from the account set.
#[derive(Default, Clone)]
struct RepEntry {
    /// Number of VERIFIED humans delegating their consensus vote to this rep.
    delegators: u64,
    /// Sum of those delegators' expired TRST (their contribution reputation).
    expired: u128,
}

/// Cached representative weights for ORV (hybrid identity + bounded contribution).
pub struct RepWeightCache {
    /// Derived aggregates per representative (cleared and rebuilt from accounts).
    reps: HashMap<WalletAddress, RepEntry>,
    /// Non-derived additive weight (the genesis bootstrap bridge). Preserved
    /// across rebuilds; see [`RepWeightCache::set_bonus`].
    bonus: HashMap<WalletAddress, u128>,
}

/// The contribution boost in basis points for a given total expired TRST.
/// Saturating curve in `[0, CONTRIBUTION_CAP_BPS]`.
fn contribution_boost_bps(expired: u128) -> u128 {
    if expired == 0 {
        return 0;
    }
    // boost = CAP · E / (E + SCALE), computed as CAP − CAP·SCALE/(E + SCALE) so
    // the numerator can't overflow for large E (CAP·SCALE is a small constant;
    // CAP·E is not). `denom ≥ SCALE` ⇒ `reducer ≤ CAP`, so the subtraction is
    // safe and the result stays in [0, CAP]. Monotonic 0 → CAP.
    let denom = expired.saturating_add(CONTRIBUTION_SCALE);
    let reducer = CONTRIBUTION_CAP_BPS.saturating_mul(CONTRIBUTION_SCALE) / denom;
    CONTRIBUTION_CAP_BPS.saturating_sub(reducer)
}

/// Identity-baseline weight amplified by the bounded contribution multiplier.
/// `delegators == 0` ⇒ 0, regardless of `expired` (money buys nothing alone).
fn hybrid_weight(delegators: u64, expired: u128) -> u128 {
    (delegators as u128).saturating_mul(WEIGHT_PER_HUMAN + contribution_boost_bps(expired))
}

impl RepWeightCache {
    /// Create an empty cache.
    pub fn new() -> Self {
        Self {
            reps: HashMap::new(),
            bonus: HashMap::new(),
        }
    }

    /// A representative's current consensus weight (hybrid derived + bonus).
    /// Returns 0 for an unknown representative.
    pub fn weight(&self, rep: &WalletAddress) -> u128 {
        let derived = self
            .reps
            .get(rep)
            .map(|e| hybrid_weight(e.delegators, e.expired))
            .unwrap_or(0);
        derived.saturating_add(self.bonus.get(rep).copied().unwrap_or(0))
    }

    /// Materialize every representative's weight as a map (used by the online
    /// weight sampler and the `representatives` RPC).
    pub fn all_weights(&self) -> HashMap<WalletAddress, u128> {
        let mut out: HashMap<WalletAddress, u128> = HashMap::with_capacity(self.reps.len());
        for (rep, e) in &self.reps {
            out.insert(rep.clone(), hybrid_weight(e.delegators, e.expired));
        }
        for (rep, b) in &self.bonus {
            let entry = out.entry(rep.clone()).or_insert(0);
            *entry = entry.saturating_add(*b);
        }
        out
    }

    /// Total consensus weight across all representatives.
    pub fn total_weight(&self) -> u128 {
        self.all_weights()
            .values()
            .fold(0u128, |acc, w| acc.saturating_add(*w))
    }

    /// Number of representatives with any weight (derived or bonus).
    pub fn rep_count(&self) -> usize {
        self.all_weights().len()
    }

    /// Set a representative's non-derived additive weight (the genesis bootstrap
    /// bridge). Preserved across [`RepWeightCache::rebuild_from_accounts`].
    pub fn set_bonus(&mut self, rep: &WalletAddress, amount: u128) {
        if amount == 0 {
            self.bonus.remove(rep);
        } else {
            self.bonus.insert(rep.clone(), amount);
        }
    }

    /// Rebuild the derived aggregates from a full account iterator.
    ///
    /// Each item is `(is_verified, representative, expired_trst)`. Only VERIFIED
    /// accounts count toward the identity baseline — verification is what makes
    /// each counted account a unique human (Sybil resistance). The `bonus` map is
    /// left untouched.
    pub fn rebuild_from_accounts(
        &mut self,
        accounts: impl Iterator<Item = (bool, WalletAddress, u128)>,
    ) {
        self.reps.clear();
        for (verified, rep, expired) in accounts {
            if !verified {
                continue;
            }
            let e = self.reps.entry(rep).or_default();
            e.delegators += 1;
            e.expired = e.expired.saturating_add(expired);
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
    fn baseline_is_one_vote_per_verified_human() {
        // Three verified humans, all self-representing, nothing expired → each
        // rep has exactly one delegator and weight == WEIGHT_PER_HUMAN.
        let mut cache = RepWeightCache::new();
        cache.rebuild_from_accounts(
            vec![
                (true, account("a"), 0),
                (true, account("b"), 0),
                (true, account("c"), 0),
            ]
            .into_iter(),
        );
        assert_eq!(cache.weight(&account("a")), WEIGHT_PER_HUMAN);
        assert_eq!(cache.total_weight(), 3 * WEIGHT_PER_HUMAN);
    }

    #[test]
    fn delegation_aggregates_humans_under_one_rep() {
        // Five verified humans all delegate to one rep → rep weight = 5 votes.
        let mut cache = RepWeightCache::new();
        let r = rep("server");
        let accounts: Vec<(bool, WalletAddress, u128)> =
            (0..5).map(|_| (true, r.clone(), 0)).collect();
        cache.rebuild_from_accounts(accounts.into_iter());
        assert_eq!(cache.weight(&r), 5 * WEIGHT_PER_HUMAN);
    }

    #[test]
    fn unverified_accounts_carry_no_weight() {
        // Unverified accounts are not humans yet — excluded from the baseline.
        let mut cache = RepWeightCache::new();
        cache.rebuild_from_accounts(
            vec![
                (true, account("real"), 0),
                (false, account("sybil1"), 0),
                (false, account("sybil2"), 0),
            ]
            .into_iter(),
        );
        assert_eq!(cache.weight(&account("real")), WEIGHT_PER_HUMAN);
        assert_eq!(cache.weight(&account("sybil1")), 0);
        assert_eq!(cache.total_weight(), WEIGHT_PER_HUMAN);
    }

    #[test]
    fn money_times_zero_delegators_is_zero() {
        // The core anti-plutocracy property: contribution with no delegators is
        // worthless. A would-be attacker who expired a fortune but whom no human
        // delegates to has zero weight.
        assert_eq!(hybrid_weight(0, u128::MAX / 2), 0);
        let cache = RepWeightCache::new();
        assert_eq!(cache.weight(&rep("rich_but_untrusted")), 0);
    }

    #[test]
    fn contribution_boost_is_capped() {
        // A single delegator with an enormous expired stake: weight is bounded
        // at one human × (1 + CAP), never more.
        let mut cache = RepWeightCache::new();
        cache.rebuild_from_accounts(vec![(true, account("whale"), u128::MAX / 4)].into_iter());
        let w = cache.weight(&account("whale"));
        assert_eq!(w, WEIGHT_PER_HUMAN + CONTRIBUTION_CAP_BPS);
        // i.e. +50% over the plain baseline, and no more.
        assert!(w <= WEIGHT_PER_HUMAN + CONTRIBUTION_CAP_BPS);
    }

    #[test]
    fn contribution_boost_saturates_at_half_cap_at_scale() {
        // At expired == SCALE the boost is exactly half the cap.
        assert_eq!(
            contribution_boost_bps(CONTRIBUTION_SCALE),
            CONTRIBUTION_CAP_BPS / 2
        );
        assert_eq!(contribution_boost_bps(0), 0);
        // Monotonic: more expired → more boost, up to the cap.
        assert!(
            contribution_boost_bps(CONTRIBUTION_SCALE * 10)
                > contribution_boost_bps(CONTRIBUTION_SCALE)
        );
        assert!(contribution_boost_bps(CONTRIBUTION_SCALE * 1_000) <= CONTRIBUTION_CAP_BPS);
    }

    #[test]
    fn contribution_amplifies_but_never_replaces_identity() {
        // Two reps with the same number of delegators; the contributor beats the
        // freeloader, but by a bounded margin, not by orders of magnitude.
        let mut cache = RepWeightCache::new();
        cache.rebuild_from_accounts(
            vec![
                (true, rep("contributor"), CONTRIBUTION_SCALE * 1_000),
                (true, rep("freeloader"), 0),
            ]
            .into_iter(),
        );
        let c = cache.weight(&rep("contributor"));
        let f = cache.weight(&rep("freeloader"));
        assert!(c > f);
        // Bounded edge: at most +50%.
        assert!(c <= f + f * CONTRIBUTION_CAP_BPS / WEIGHT_PER_HUMAN);
    }

    #[test]
    fn bonus_is_additive_and_survives_rebuild() {
        let mut cache = RepWeightCache::new();
        let g = rep("genesis");
        cache.set_bonus(&g, 1_000_000);
        assert_eq!(cache.weight(&g), 1_000_000); // bonus only, no delegators yet

        // Genesis self-reps (verified) → +1 human baseline on top of the bonus.
        cache.rebuild_from_accounts(vec![(true, g.clone(), 0)].into_iter());
        assert_eq!(cache.weight(&g), 1_000_000 + WEIGHT_PER_HUMAN);

        // A second rebuild must NOT wipe the bonus.
        cache.rebuild_from_accounts(vec![(true, g.clone(), 0)].into_iter());
        assert_eq!(cache.weight(&g), 1_000_000 + WEIGHT_PER_HUMAN);
    }

    #[test]
    fn set_bonus_zero_clears() {
        let mut cache = RepWeightCache::new();
        let g = rep("genesis");
        cache.set_bonus(&g, 500);
        assert_eq!(cache.weight(&g), 500);
        cache.set_bonus(&g, 0);
        assert_eq!(cache.weight(&g), 0);
    }

    #[test]
    fn all_weights_includes_bonus_only_reps() {
        let mut cache = RepWeightCache::new();
        cache.rebuild_from_accounts(vec![(true, account("human"), 0)].into_iter());
        cache.set_bonus(&rep("seed"), 42);
        let all = cache.all_weights();
        assert_eq!(all.get(&account("human")), Some(&WEIGHT_PER_HUMAN));
        assert_eq!(all.get(&rep("seed")), Some(&42u128));
        assert_eq!(cache.rep_count(), 2);
    }

    #[test]
    fn rebuild_clears_previous_derived_state() {
        let mut cache = RepWeightCache::new();
        cache.rebuild_from_accounts(vec![(true, account("stale"), 0)].into_iter());
        assert_eq!(cache.weight(&account("stale")), WEIGHT_PER_HUMAN);
        cache.rebuild_from_accounts(std::iter::empty());
        assert_eq!(cache.weight(&account("stale")), 0);
        assert_eq!(cache.total_weight(), 0);
    }
}
