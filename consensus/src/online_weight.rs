//! Recent voting activity and smoothed online-weight telemetry.
//!
//! The node samples attendance for diagnostics. Elections use confirmed
//! representative authority, frozen when they start; this sampler cannot lower
//! their quorum. `effective_weight` retains an attendance estimate with a 50%
//! total-weight floor, but the node does not use it to authorize confirmation.

use std::collections::HashMap;

use burst_types::WalletAddress;

/// Floor for the effective attendance estimate, in basis points of supplied
/// total weight. This is a telemetry constant, not the election quorum policy.
const MIN_ONLINE_WEIGHT_FRACTION_BPS: u128 = 5000;

/// Decay percentage for EMA trending (95 = 0.95, slow decay).
const TREND_DECAY_PCT: u128 = 95;

/// Tracks recently-active representatives for online weight calculation.
pub struct OnlineWeightSampler {
    /// Representative → timestamp of their most recent vote.
    recent_voters: HashMap<WalletAddress, u64>,
    /// Sampling window in seconds (representatives that voted within this
    /// window are considered online).
    window_secs: u64,
    /// Trended (smoothed) online weight — EMA of recent samples.
    trended_weight: u128,
}

impl OnlineWeightSampler {
    /// Create a new sampler with the given window (in seconds).
    pub fn new(window_secs: u64) -> Self {
        Self {
            recent_voters: HashMap::new(),
            window_secs,
            trended_weight: 0,
        }
    }

    /// Record that a representative cast a vote at the given timestamp.
    pub fn record_vote(&mut self, rep: &WalletAddress, timestamp: u64) {
        let entry = self.recent_voters.entry(rep.clone()).or_insert(0);
        // Only update if this is a more recent vote.
        if timestamp > *entry {
            *entry = timestamp;
        }
    }

    /// Return the list of representatives that voted within the window.
    pub fn online_representatives(&self, now: u64) -> Vec<WalletAddress> {
        let cutoff = now.saturating_sub(self.window_secs);
        self.recent_voters
            .iter()
            .filter(|(_, &last_vote)| last_vote >= cutoff)
            .map(|(addr, _)| addr.clone())
            .collect()
    }

    /// Calculate the total delegated weight of online representatives.
    ///
    /// `weights` maps each representative to the total weight delegated to it
    /// (equal units from verified people who selected that representative).
    pub fn online_weight(&self, now: u64, weights: &HashMap<WalletAddress, u128>) -> u128 {
        let cutoff = now.saturating_sub(self.window_secs);
        self.recent_voters
            .iter()
            .filter(|(_, &last_vote)| last_vote >= cutoff)
            .map(|(addr, _)| weights.get(addr).copied().unwrap_or(0))
            .sum()
    }

    /// Compute online weight from a weight map (convenience for internal use).
    fn compute_online_weight(&self, now: u64, weight_map: &HashMap<WalletAddress, u128>) -> u128 {
        self.online_weight(now, weight_map)
    }

    /// Update the trended weight using exponential moving average.
    pub fn update_trend(&mut self, current_online_weight: u128) {
        if self.trended_weight == 0 {
            self.trended_weight = current_online_weight;
        } else {
            // EMA: trended = decay * trended + (1-decay) * current
            self.trended_weight = (self.trended_weight * TREND_DECAY_PCT / 100)
                + (current_online_weight * (100 - TREND_DECAY_PCT) / 100);
        }
    }

    /// Maximum of current attendance, its EMA, and half the supplied total weight.
    /// The node reports this estimate without using it as an election denominator.
    pub fn effective_weight(&self, now: u64, weight_map: &HashMap<WalletAddress, u128>) -> u128 {
        let current = self.compute_online_weight(now, weight_map);
        let total_weight: u128 = weight_map.values().sum();
        let adaptive_floor = total_weight.saturating_mul(MIN_ONLINE_WEIGHT_FRACTION_BPS) / 10_000;
        current.max(self.trended_weight).max(adaptive_floor)
    }

    /// Whether a representative is a "principal" rep (≥0.1% of online weight).
    ///
    /// This classification does not determine voting eligibility or discard
    /// smaller representatives' votes from an election.
    pub fn is_principal(&self, rep_weight: u128, total_online: u128) -> bool {
        if total_online == 0 {
            return false;
        }
        // 0.1% = 10 basis points out of 10_000
        rep_weight * 10_000 / total_online >= 10
    }

    /// Get the trended weight.
    pub fn trended_weight(&self) -> u128 {
        self.trended_weight
    }

    /// Remove representatives that haven't voted within the window.
    pub fn prune(&mut self, now: u64) {
        let cutoff = now.saturating_sub(self.window_secs);
        self.recent_voters
            .retain(|_, &mut last_vote| last_vote >= cutoff);
    }

    /// Number of tracked representatives (including stale ones not yet pruned).
    pub fn tracked_count(&self) -> usize {
        self.recent_voters.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::WalletAddress;

    fn rep(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{name}"))
    }

    #[test]
    fn test_record_and_query() {
        let mut sampler = OnlineWeightSampler::new(300);
        sampler.record_vote(&rep("alice"), 1000);
        sampler.record_vote(&rep("bob"), 1100);

        let online = sampler.online_representatives(1200);
        assert_eq!(online.len(), 2);
    }

    #[test]
    fn test_stale_representatives_excluded() {
        let mut sampler = OnlineWeightSampler::new(300);
        sampler.record_vote(&rep("alice"), 500);
        sampler.record_vote(&rep("bob"), 900);

        // At time 1000, window covers 700..1000.
        // Alice voted at 500 — stale. Bob at 900 — online.
        let online = sampler.online_representatives(1000);
        assert_eq!(online.len(), 1);
        assert_eq!(online[0], rep("bob"));
    }

    #[test]
    fn test_online_weight() {
        let mut sampler = OnlineWeightSampler::new(300);
        sampler.record_vote(&rep("alice"), 1000);
        sampler.record_vote(&rep("bob"), 1000);
        sampler.record_vote(&rep("carol"), 500); // stale

        let mut weights = HashMap::new();
        weights.insert(rep("alice"), 100);
        weights.insert(rep("bob"), 200);
        weights.insert(rep("carol"), 999); // should not be counted

        let total = sampler.online_weight(1100, &weights);
        assert_eq!(total, 300); // alice + bob only
    }

    #[test]
    fn test_prune() {
        let mut sampler = OnlineWeightSampler::new(300);
        sampler.record_vote(&rep("alice"), 500);
        sampler.record_vote(&rep("bob"), 900);

        sampler.prune(1000);
        assert_eq!(sampler.tracked_count(), 1);
    }

    #[test]
    fn test_vote_updates_timestamp() {
        let mut sampler = OnlineWeightSampler::new(300);
        sampler.record_vote(&rep("alice"), 500);
        sampler.record_vote(&rep("alice"), 900);

        // Alice's latest vote is 900 — she should be online at 1100.
        let online = sampler.online_representatives(1100);
        assert_eq!(online.len(), 1);
    }

    #[test]
    fn test_old_vote_does_not_overwrite_newer() {
        let mut sampler = OnlineWeightSampler::new(300);
        sampler.record_vote(&rep("alice"), 900);
        sampler.record_vote(&rep("alice"), 500); // older — should be ignored

        let online = sampler.online_representatives(1100);
        assert_eq!(online.len(), 1); // still online from the 900 vote
    }

    #[test]
    fn test_empty_sampler() {
        let sampler = OnlineWeightSampler::new(300);
        assert!(sampler.online_representatives(1000).is_empty());
        assert_eq!(sampler.online_weight(1000, &HashMap::new()), 0);
    }

    #[test]
    fn test_trend_initializes_from_first_sample() {
        let mut sampler = OnlineWeightSampler::new(300);
        assert_eq!(sampler.trended_weight(), 0);

        sampler.update_trend(1_000_000);
        assert_eq!(sampler.trended_weight(), 1_000_000);
    }

    #[test]
    fn test_trend_ema_smoothing() {
        let mut sampler = OnlineWeightSampler::new(300);

        // Initialize with 1_000_000
        sampler.update_trend(1_000_000);
        assert_eq!(sampler.trended_weight(), 1_000_000);

        // Drop to 500_000 — trended should decay slowly
        // EMA: 1_000_000 * 95/100 + 500_000 * 5/100 = 950_000 + 25_000 = 975_000
        sampler.update_trend(500_000);
        assert_eq!(sampler.trended_weight(), 975_000);

        // Another sample at 500_000
        // EMA: 975_000 * 95/100 + 500_000 * 5/100 = 926_250 + 25_000 = 951_250
        sampler.update_trend(500_000);
        assert_eq!(sampler.trended_weight(), 951_250);
    }

    #[test]
    fn test_trend_rises_with_increased_weight() {
        let mut sampler = OnlineWeightSampler::new(300);
        sampler.update_trend(1_000_000);

        // Jump to 2_000_000
        // EMA: 1_000_000 * 95/100 + 2_000_000 * 5/100 = 950_000 + 100_000 = 1_050_000
        sampler.update_trend(2_000_000);
        assert_eq!(sampler.trended_weight(), 1_050_000);
    }

    #[test]
    fn test_effective_weight_uses_adaptive_floor() {
        let sampler = OnlineWeightSampler::new(300);
        // Weight exists (total 1000) but nobody is online / no trend → the
        // effective weight is the adaptive floor = 50% of total = 500.
        let mut weights = HashMap::new();
        weights.insert(rep("alice"), 600);
        weights.insert(rep("bob"), 400);
        let effective = sampler.effective_weight(1000, &weights);
        assert_eq!(effective, 500); // 50% of 1000
    }

    #[test]
    fn test_effective_weight_zero_when_no_weight_exists() {
        // No weight in existence at all → floor is 0 (nothing to confirm against).
        let sampler = OnlineWeightSampler::new(300);
        let weights: HashMap<WalletAddress, u128> = HashMap::new();
        assert_eq!(sampler.effective_weight(1000, &weights), 0);
    }

    #[test]
    fn restart_does_not_invent_online_weight_from_offline_people() {
        use crate::election::Election;
        use crate::rep_weights::WEIGHT_PER_HUMAN;
        use burst_types::{BlockHash, Timestamp};

        // Exercise the sampler with no attendance history. The standalone
        // election below deliberately uses its estimate as a denominator;
        // node elections instead use confirmed authority.
        let weights = HashMap::from([
            (rep("online"), 4 * WEIGHT_PER_HUMAN),
            (rep("offline_a"), WEIGHT_PER_HUMAN),
            (rep("offline_b"), WEIGHT_PER_HUMAN),
            (rep("offline_c"), WEIGHT_PER_HUMAN),
        ]);
        let mut sampler = OnlineWeightSampler::new(300);
        assert_eq!(sampler.trended_weight(), 0);
        let initial = sampler.effective_weight(1000, &weights);
        assert_eq!(initial, 7 * WEIGHT_PER_HUMAN / 2);
        let mut election = Election::new(BlockHash::new([1; 32]), initial, Timestamp::new(1000));
        election.vote(
            &rep("online"),
            BlockHash::new([2; 32]),
            4 * WEIGHT_PER_HUMAN,
            true,
            Timestamp::new(1001),
        );
        assert!(election.try_confirm(Timestamp::new(1001)).is_some());
        sampler.record_vote(&rep("online"), 1001);
        assert_eq!(
            sampler.effective_weight(1001, &weights),
            4 * WEIGHT_PER_HUMAN
        );

        // The existing floor still excludes an isolated one-person minority.
        let mut minority = Election::new(BlockHash::new([3; 32]), initial, Timestamp::new(1000));
        minority.vote(
            &rep("offline_a"),
            BlockHash::new([4; 32]),
            WEIGHT_PER_HUMAN,
            true,
            Timestamp::new(1001),
        );
        assert!(minority.try_confirm(Timestamp::new(1001)).is_none());
    }

    #[test]
    fn test_small_network_can_reach_quorum() {
        // With all supplied weight online, the estimate equals that weight
        // regardless of its absolute size. This is not a node cementation test.
        let mut sampler = OnlineWeightSampler::new(300);
        sampler.record_vote(&rep("genesis"), 1000);
        sampler.update_trend(1_000_000);
        let mut weights = HashMap::new();
        weights.insert(rep("genesis"), 1_000_000);
        let effective = sampler.effective_weight(1100, &weights);
        // effective = max(online 1M, trended 1M, floor 500K) = 1M.
        assert_eq!(effective, 1_000_000);
        // 67% quorum = 670K; genesis's 1M vote clears it.
        let quorum = effective * 6700 / 10_000;
        assert!(1_000_000 >= quorum);
    }

    #[test]
    fn test_effective_weight_uses_trended_over_current() {
        let mut sampler = OnlineWeightSampler::new(300);

        // Set a high trend
        sampler.update_trend(100_000_000);

        // But current online weight is low (only alice online with 1_000)
        sampler.record_vote(&rep("alice"), 1000);
        let mut weights = HashMap::new();
        weights.insert(rep("alice"), 1_000);

        // Effective should use trended (100M) since it's higher than current (1K)
        let effective = sampler.effective_weight(1100, &weights);
        assert_eq!(effective, 100_000_000);
    }

    #[test]
    fn test_effective_weight_uses_current_when_highest() {
        let mut sampler = OnlineWeightSampler::new(300);

        // Low trend
        sampler.update_trend(500);

        // But current online weight is high (alice online with all 10K weight,
        // so current 10K > adaptive floor 5K > trended 500).
        sampler.record_vote(&rep("alice"), 1000);
        let mut weights = HashMap::new();
        weights.insert(rep("alice"), 10_000);

        let effective = sampler.effective_weight(1100, &weights);
        assert_eq!(effective, 10_000);
    }

    #[test]
    fn test_is_principal_basic() {
        let sampler = OnlineWeightSampler::new(300);

        // 0.1% of 1_000_000 = 1_000
        assert!(sampler.is_principal(1_000, 1_000_000));
        assert!(sampler.is_principal(10_000, 1_000_000));
        assert!(!sampler.is_principal(999, 1_000_000));
    }

    #[test]
    fn test_is_principal_zero_total() {
        let sampler = OnlineWeightSampler::new(300);
        assert!(!sampler.is_principal(1_000, 0));
    }

    #[test]
    fn test_is_principal_exact_threshold() {
        let sampler = OnlineWeightSampler::new(300);
        // Exactly 0.1%: 1 out of 1000
        assert!(sampler.is_principal(1, 1_000));
        // Below: 0 out of 1000
        assert!(!sampler.is_principal(0, 1_000));
    }

    #[test]
    fn test_is_principal_large_values() {
        let sampler = OnlineWeightSampler::new(300);
        let total: u128 = 100_000_000_000_000_000;
        let threshold = total / 1000; // 0.1%
        assert!(sampler.is_principal(threshold, total));
        assert!(!sampler.is_principal(threshold - 1, total));
    }
}
