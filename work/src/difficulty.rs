//! Adaptive PoW difficulty adjustment based on recent transaction throughput.
//!
//! When the network is under high load (many transactions per window), difficulty
//! increases to make spam more expensive. During low activity, difficulty decreases
//! to minimize friction for legitimate users.

use std::collections::VecDeque;

/// Adaptive PoW difficulty adjuster.
///
/// Tracks recent block timestamps in a sliding window and scales difficulty
/// linearly when observed TPS exceeds the target.
pub struct DifficultyAdjuster {
    window: VecDeque<u64>,
    window_size: usize,
    base_difficulty: u64,
    target_tps: u64,
    max_multiplier: u64,
}

impl DifficultyAdjuster {
    pub fn new(base_difficulty: u64, target_tps: u64, window_size: usize) -> Self {
        Self {
            window: VecDeque::with_capacity(window_size),
            window_size,
            base_difficulty,
            target_tps,
            max_multiplier: 16,
        }
    }

    /// Record a block timestamp for throughput tracking.
    pub fn record_block(&mut self, timestamp_secs: u64) {
        self.window.push_back(timestamp_secs);
        while self.window.len() > self.window_size {
            self.window.pop_front();
        }
    }

    /// Compute the current effective difficulty based on recent throughput.
    pub fn current_difficulty(&self) -> u64 {
        if self.window.len() < 2 {
            return self.base_difficulty;
        }

        let first = self.window.front().unwrap();
        let last = self.window.back().unwrap();
        let elapsed = last.saturating_sub(*first).max(1);
        let count = self.window.len() as u64;
        let tps = count / elapsed;

        if tps <= self.target_tps {
            return self.base_difficulty;
        }

        let multiplier = (tps / self.target_tps.max(1)).min(self.max_multiplier);
        self.base_difficulty.saturating_mul(multiplier)
    }

    /// Update the base difficulty (e.g., via governance).
    pub fn set_base_difficulty(&mut self, new_base: u64) {
        self.base_difficulty = new_base;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_difficulty_with_no_blocks() {
        let adj = DifficultyAdjuster::new(1000, 10, 100);
        assert_eq!(adj.current_difficulty(), 1000);
    }

    #[test]
    fn difficulty_unchanged_below_target() {
        let mut adj = DifficultyAdjuster::new(1000, 10, 100);
        for i in 0..5 {
            adj.record_block(i * 10);
        }
        assert_eq!(adj.current_difficulty(), 1000);
    }

    #[test]
    fn difficulty_increases_above_target() {
        let mut adj = DifficultyAdjuster::new(1000, 10, 1000);
        // 100 blocks in 1 second = 100 TPS (10x target)
        for i in 0..100 {
            adj.record_block(i / 100);
        }
        assert!(adj.current_difficulty() > 1000);
    }

    #[test]
    fn difficulty_capped_at_max_multiplier() {
        let mut adj = DifficultyAdjuster::new(1000, 1, 10000);
        for _ in 0..10000 {
            adj.record_block(0);
        }
        adj.record_block(1);
        assert!(adj.current_difficulty() <= 1000 * 16);
    }

    #[test]
    fn set_base_difficulty() {
        let mut adj = DifficultyAdjuster::new(1000, 10, 100);
        adj.set_base_difficulty(2000);
        assert_eq!(adj.current_difficulty(), 2000);
    }
}
