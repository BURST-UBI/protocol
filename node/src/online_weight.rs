//! Tracks the online voting weight of the network for consensus quorum calculation.
//!
//! Uses a sampling approach: periodically records the online weight and computes
//! a trended weight using the median of recent samples. This complements the
//! consensus crate's [`OnlineWeightSampler`] (EMA-based, per-representative) with
//! an aggregate historical view used for quorum stability.

use burst_types::Timestamp;
use std::collections::VecDeque;

/// Samples the online voting weight over time.
pub struct OnlineWeightTracker {
    /// Weight samples: (timestamp, weight).
    samples: VecDeque<(Timestamp, u128)>,
    /// Maximum number of samples to retain.
    max_samples: usize,
    /// Current live online weight (sum of online representatives' weight).
    current_weight: u128,
    /// Minimum weight floor (prevents quorum from dropping to zero).
    minimum_weight: u128,
}

impl OnlineWeightTracker {
    /// Two weeks of samples at 30-minute intervals.
    const DEFAULT_MAX_SAMPLES: usize = 672;

    pub fn new(initial_weight: u128, minimum_weight: u128) -> Self {
        Self {
            samples: VecDeque::new(),
            max_samples: Self::DEFAULT_MAX_SAMPLES,
            current_weight: initial_weight,
            minimum_weight,
        }
    }

    /// Record the current online weight as a sample.
    pub fn record_sample(&mut self, weight: u128, now: Timestamp) {
        if self.samples.len() >= self.max_samples {
            self.samples.pop_front();
        }
        self.samples.push_back((now, weight));
        self.current_weight = weight;
    }

    /// Set the current live weight (called when reps go online/offline).
    pub fn set_current_weight(&mut self, weight: u128) {
        self.current_weight = weight;
    }

    /// Get the trended weight (median of samples).
    pub fn trended_weight(&self) -> u128 {
        if self.samples.is_empty() {
            return self.current_weight.max(self.minimum_weight);
        }

        let mut weights: Vec<u128> = self.samples.iter().map(|(_, w)| *w).collect();
        weights.sort_unstable();
        let median = weights[weights.len() / 2];
        median.max(self.minimum_weight)
    }

    /// Get the quorum delta (67% of max(current, trended, minimum)).
    pub fn quorum_delta(&self) -> u128 {
        let base = self
            .current_weight
            .max(self.trended_weight())
            .max(self.minimum_weight);
        base * 67 / 100
    }

    /// Current live weight.
    pub fn current_weight(&self) -> u128 {
        self.current_weight
    }

    /// Number of samples.
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }
}

impl Default for OnlineWeightTracker {
    fn default() -> Self {
        Self::new(0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(secs: u64) -> Timestamp {
        Timestamp::new(secs)
    }

    #[test]
    fn new_tracker_has_initial_weight() {
        let tracker = OnlineWeightTracker::new(5000, 100);
        assert_eq!(tracker.current_weight(), 5000);
        assert_eq!(tracker.sample_count(), 0);
    }

    #[test]
    fn default_tracker_is_zero() {
        let tracker = OnlineWeightTracker::default();
        assert_eq!(tracker.current_weight(), 0);
        assert_eq!(tracker.sample_count(), 0);
    }

    #[test]
    fn record_sample_updates_current_weight() {
        let mut tracker = OnlineWeightTracker::new(0, 0);
        tracker.record_sample(1000, ts(100));
        assert_eq!(tracker.current_weight(), 1000);
        assert_eq!(tracker.sample_count(), 1);
    }

    #[test]
    fn trended_weight_is_median_of_samples() {
        let mut tracker = OnlineWeightTracker::new(0, 0);
        tracker.record_sample(100, ts(1));
        tracker.record_sample(300, ts(2));
        tracker.record_sample(200, ts(3));

        // Sorted: [100, 200, 300]. Median (index 1) = 200.
        assert_eq!(tracker.trended_weight(), 200);
    }

    #[test]
    fn trended_weight_even_sample_count() {
        let mut tracker = OnlineWeightTracker::new(0, 0);
        tracker.record_sample(100, ts(1));
        tracker.record_sample(200, ts(2));
        tracker.record_sample(300, ts(3));
        tracker.record_sample(400, ts(4));

        // Sorted: [100, 200, 300, 400]. len/2 = 2 → median = 300.
        assert_eq!(tracker.trended_weight(), 300);
    }

    #[test]
    fn trended_weight_empty_returns_current_or_minimum() {
        let tracker = OnlineWeightTracker::new(500, 100);
        assert_eq!(tracker.trended_weight(), 500);

        let tracker2 = OnlineWeightTracker::new(0, 100);
        assert_eq!(tracker2.trended_weight(), 100);
    }

    #[test]
    fn minimum_weight_floor_applied_to_trended() {
        let mut tracker = OnlineWeightTracker::new(0, 1000);
        tracker.record_sample(50, ts(1));
        tracker.record_sample(60, ts(2));
        tracker.record_sample(70, ts(3));

        // Median = 60, but minimum_weight = 1000 → trended = 1000.
        assert_eq!(tracker.trended_weight(), 1000);
    }

    #[test]
    fn quorum_delta_is_67_percent() {
        let mut tracker = OnlineWeightTracker::new(1000, 0);
        tracker.record_sample(1000, ts(1));
        assert_eq!(tracker.quorum_delta(), 670);
    }

    #[test]
    fn quorum_delta_uses_max_of_current_trended_minimum() {
        let mut tracker = OnlineWeightTracker::new(0, 500);
        tracker.record_sample(100, ts(1));
        // current = 100, trended = max(100, 500) = 500, minimum = 500.
        // base = max(100, 500, 500) = 500. quorum = 500 * 67/100 = 335.
        assert_eq!(tracker.quorum_delta(), 335);
    }

    #[test]
    fn quorum_delta_current_higher_than_trended() {
        let mut tracker = OnlineWeightTracker::new(0, 0);
        tracker.record_sample(100, ts(1));
        tracker.set_current_weight(2000);
        // current = 2000, trended = max(100, 0) = 100.
        // base = max(2000, 100, 0) = 2000. quorum = 2000 * 67/100 = 1340.
        assert_eq!(tracker.quorum_delta(), 1340);
    }

    #[test]
    fn set_current_weight() {
        let mut tracker = OnlineWeightTracker::new(0, 0);
        tracker.set_current_weight(42);
        assert_eq!(tracker.current_weight(), 42);
    }

    #[test]
    fn sample_eviction_at_capacity() {
        let mut tracker = OnlineWeightTracker {
            samples: VecDeque::new(),
            max_samples: 3,
            current_weight: 0,
            minimum_weight: 0,
        };

        tracker.record_sample(10, ts(1));
        tracker.record_sample(20, ts(2));
        tracker.record_sample(30, ts(3));
        assert_eq!(tracker.sample_count(), 3);

        tracker.record_sample(40, ts(4));
        assert_eq!(tracker.sample_count(), 3);

        // Oldest sample (10) should have been evicted.
        // Remaining: [20, 30, 40]. Sorted median (index 1) = 30.
        assert_eq!(tracker.trended_weight(), 30);
    }

    #[test]
    fn single_sample_median_is_itself() {
        let mut tracker = OnlineWeightTracker::new(0, 0);
        tracker.record_sample(999, ts(1));
        assert_eq!(tracker.trended_weight(), 999);
    }

    #[test]
    fn quorum_delta_with_zero_weight_and_zero_floor() {
        let tracker = OnlineWeightTracker::new(0, 0);
        assert_eq!(tracker.quorum_delta(), 0);
    }
}
