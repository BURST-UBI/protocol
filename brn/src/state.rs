//! Per-wallet BRN state and global rate history.

use crate::error::BrnError;
use burst_types::Timestamp;
use serde::{Deserialize, Serialize};

/// A segment of BRN accrual at a specific rate.
///
/// Rate segments are stored ONCE in the global `RateHistory`, not per-wallet.
/// Every wallet uses the same rate at the same time.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RateSegment {
    /// The rate (raw units per second) during this segment.
    pub rate: u128,
    /// When this rate became effective (global protocol time).
    pub start: Timestamp,
    /// When this rate stopped being effective (None if still active).
    pub end: Option<Timestamp>,
}

/// Global rate history shared by all wallets.
///
/// A rate change via governance appends one segment here — O(1).
/// Balance computation for any wallet intersects this history with the
/// wallet's `verified_at` — O(k) where k = number of rate changes
/// (typically 1-3 over the protocol's lifetime).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RateHistory {
    pub segments: Vec<RateSegment>,
}

impl RateHistory {
    pub fn new(initial_rate: u128, genesis: Timestamp) -> Self {
        Self {
            segments: vec![RateSegment {
                rate: initial_rate,
                start: genesis,
                end: None,
            }],
        }
    }

    /// Apply a global rate change — O(1). Just closes the current segment
    /// and appends a new one. No per-wallet iteration.
    pub fn apply_rate_change(&mut self, new_rate: u128, change_at: Timestamp) -> Result<(), BrnError> {
        if let Some(current) = self.segments.last() {
            if change_at.as_secs() < current.start.as_secs() {
                return Err(BrnError::InvalidTimestamp);
            }
        }
        if let Some(current) = self.segments.last_mut() {
            current.end = Some(change_at);
        }
        self.segments.push(RateSegment {
            rate: new_rate,
            start: change_at,
            end: None,
        });
        Ok(())
    }

    /// Get the current active rate.
    pub fn current_rate(&self) -> u128 {
        self.segments.last().map(|s| s.rate).unwrap_or(0)
    }

    /// Compute total BRN accrued for a wallet verified at `verified_at`, up to `now`.
    ///
    /// Intersects the global rate history with the wallet's lifetime.
    /// O(k) where k = number of rate changes (not number of wallets).
    pub fn total_accrued_checked(&self, verified_at: Timestamp, now: Timestamp) -> Option<u128> {
        let mut total: u128 = 0;
        for seg in &self.segments {
            let seg_end = seg.end.unwrap_or(now);
            // Clamp segment to wallet's lifetime
            let effective_start = if seg.start.as_secs() > verified_at.as_secs() {
                seg.start
            } else {
                verified_at
            };
            let effective_end = seg_end;
            if effective_start.as_secs() >= effective_end.as_secs() {
                continue;
            }
            let duration = effective_end.as_secs().saturating_sub(effective_start.as_secs());
            let segment_accrual = seg.rate.checked_mul(duration as u128)?;
            total = total.checked_add(segment_accrual)?;
        }
        Some(total)
    }

    /// Compute total BRN accrued, returning 0 on overflow.
    pub fn total_accrued(&self, verified_at: Timestamp, now: Timestamp) -> u128 {
        self.total_accrued_checked(verified_at, now).unwrap_or(0)
    }
}

impl Default for RateHistory {
    fn default() -> Self {
        Self::new(0, Timestamp::new(0))
    }
}

/// BRN state for a single wallet.
///
/// Lightweight: only stores per-wallet data (verified_at, burned, staked).
/// Rate segments are global and shared — see `RateHistory`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrnWalletState {
    /// When this wallet was verified (BRN accrual begins here).
    pub verified_at: Timestamp,

    /// Total BRN ever burned by this wallet (cumulative, never decreases).
    pub total_burned: u128,

    /// Total BRN currently locked in active stakes.
    pub total_staked: u128,

    /// Whether accrual is currently active for this wallet.
    /// Set to false on de-verification; re-set to true on re-verification.
    /// When false, the `accrual_stopped_at` field records when accrual stopped.
    #[serde(default = "default_true")]
    pub accrual_active: bool,

    /// If accrual has been stopped, the timestamp when it was stopped.
    /// Used to cap accrual computation.
    #[serde(default)]
    pub accrual_stopped_at: Option<Timestamp>,
}

fn default_true() -> bool {
    true
}

impl BrnWalletState {
    /// Create a new BRN state for a freshly verified wallet.
    /// No longer takes `initial_rate` — rate is global.
    pub fn new(verified_at: Timestamp) -> Self {
        Self {
            verified_at,
            total_burned: 0,
            total_staked: 0,
            accrual_active: true,
            accrual_stopped_at: None,
        }
    }

    /// Backward-compatible constructor that ignores the rate parameter.
    /// Rate is now stored globally in `RateHistory`.
    pub fn new_compat(verified_at: Timestamp, _initial_rate: u128) -> Self {
        Self::new(verified_at)
    }

    /// Compute available balance using the global rate history.
    pub fn available_balance_checked(&self, rates: &RateHistory, now: Timestamp) -> Option<u128> {
        let effective_now = if self.accrual_active {
            now
        } else {
            self.accrual_stopped_at.unwrap_or(now)
        };
        let accrued = rates.total_accrued_checked(self.verified_at, effective_now)?;
        let after_burned = accrued.checked_sub(self.total_burned)?;
        after_burned.checked_sub(self.total_staked)
    }

    /// Compute available balance, returning 0 on overflow.
    pub fn available_balance(&self, rates: &RateHistory, now: Timestamp) -> u128 {
        self.available_balance_checked(rates, now).unwrap_or(0)
    }

    /// Stop BRN accrual (e.g. on de-verification).
    pub fn stop_accrual(&mut self, at: Timestamp) {
        self.accrual_active = false;
        self.accrual_stopped_at = Some(at);
    }

    /// Resume BRN accrual (e.g. on re-verification).
    pub fn resume_accrual(&mut self, at: Timestamp) {
        self.accrual_active = true;
        self.accrual_stopped_at = None;
        self.verified_at = at;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- RateHistory ---

    #[test]
    fn rate_history_single_segment_accrual() {
        let h = RateHistory::new(100, Timestamp::new(0));
        assert_eq!(h.current_rate(), 100);
        assert_eq!(h.total_accrued(Timestamp::new(0), Timestamp::new(10)), 1000);
    }

    #[test]
    fn rate_history_wallet_verified_after_genesis() {
        let h = RateHistory::new(50, Timestamp::new(0));
        // Wallet verified at t=500, check at t=1000 → only 500s of accrual
        assert_eq!(h.total_accrued(Timestamp::new(500), Timestamp::new(1000)), 25_000);
    }

    #[test]
    fn rate_history_two_segments() {
        let mut h = RateHistory::new(100, Timestamp::new(0));
        h.apply_rate_change(200, Timestamp::new(1000)).unwrap();
        assert_eq!(h.current_rate(), 200);
        assert_eq!(h.segments.len(), 2);

        // Wallet from t=0 to t=2000: 100*1000 + 200*1000 = 300_000
        assert_eq!(h.total_accrued(Timestamp::new(0), Timestamp::new(2000)), 300_000);
    }

    #[test]
    fn rate_history_wallet_spans_partial_segments() {
        let mut h = RateHistory::new(10, Timestamp::new(0));
        h.apply_rate_change(20, Timestamp::new(100)).unwrap();
        h.apply_rate_change(30, Timestamp::new(200)).unwrap();

        // Wallet verified at t=50, now at t=250
        // seg1: rate=10, effective 50..100 = 50s → 500
        // seg2: rate=20, effective 100..200 = 100s → 2000
        // seg3: rate=30, effective 200..250 = 50s → 1500
        // total = 4000
        assert_eq!(h.total_accrued(Timestamp::new(50), Timestamp::new(250)), 4000);
    }

    #[test]
    fn rate_history_zero_elapsed_returns_zero() {
        let h = RateHistory::new(100, Timestamp::new(0));
        assert_eq!(h.total_accrued(Timestamp::new(500), Timestamp::new(500)), 0);
    }

    #[test]
    fn rate_history_reject_backwards_rate_change() {
        let mut h = RateHistory::new(100, Timestamp::new(1000));
        let result = h.apply_rate_change(200, Timestamp::new(500));
        assert!(result.is_err());
    }

    #[test]
    fn rate_history_checked_returns_none_on_overflow() {
        let h = RateHistory::new(u128::MAX, Timestamp::new(0));
        let result = h.total_accrued_checked(Timestamp::new(0), Timestamp::new(2));
        assert!(result.is_none(), "u128::MAX * 2 should overflow");
    }

    #[test]
    fn rate_history_default_is_zero_rate() {
        let h = RateHistory::default();
        assert_eq!(h.current_rate(), 0);
        assert_eq!(h.total_accrued(Timestamp::new(0), Timestamp::new(1000)), 0);
    }

    // --- BrnWalletState ---

    #[test]
    fn wallet_state_basic_balance() {
        let rates = RateHistory::new(100, Timestamp::new(0));
        let state = BrnWalletState::new(Timestamp::new(1000));
        let balance = state.available_balance(&rates, Timestamp::new(2000));
        assert_eq!(balance, 100_000);
    }

    #[test]
    fn wallet_state_balance_after_burn_and_stake() {
        let rates = RateHistory::new(100, Timestamp::new(0));
        let mut state = BrnWalletState::new(Timestamp::new(0));
        state.total_burned = 5000;
        state.total_staked = 3000;
        let balance = state.available_balance(&rates, Timestamp::new(100));
        assert_eq!(balance, 2000); // 10000 - 5000 - 3000
    }

    #[test]
    fn wallet_state_balance_returns_zero_when_overdrawn() {
        let rates = RateHistory::new(1, Timestamp::new(0));
        let mut state = BrnWalletState::new(Timestamp::new(0));
        state.total_burned = 999999;
        let balance = state.available_balance(&rates, Timestamp::new(10));
        assert_eq!(balance, 0);
    }

    #[test]
    fn wallet_state_stop_accrual_caps_balance() {
        let rates = RateHistory::new(100, Timestamp::new(0));
        let mut state = BrnWalletState::new(Timestamp::new(0));
        state.stop_accrual(Timestamp::new(50));

        // Even though "now" is 1000, accrual stopped at 50
        let balance = state.available_balance(&rates, Timestamp::new(1000));
        assert_eq!(balance, 5000); // 100 * 50
    }

    #[test]
    fn wallet_state_resume_accrual_resets_verified_at() {
        let rates = RateHistory::new(100, Timestamp::new(0));
        let mut state = BrnWalletState::new(Timestamp::new(0));
        state.total_burned = 5000;
        state.stop_accrual(Timestamp::new(50));
        state.resume_accrual(Timestamp::new(100));

        assert!(state.accrual_active);
        assert_eq!(state.verified_at, Timestamp::new(100));
        // Previous burns still count, but accrual restarts from t=100
        let balance = state.available_balance(&rates, Timestamp::new(200));
        // 100 * (200-100) = 10000, minus 5000 burned = 5000
        assert_eq!(balance, 5000);
    }

    #[test]
    fn wallet_state_compat_constructor_ignores_rate() {
        let s1 = BrnWalletState::new(Timestamp::new(42));
        let s2 = BrnWalletState::new_compat(Timestamp::new(42), 999);
        assert_eq!(s1.verified_at, s2.verified_at);
        assert_eq!(s1.total_burned, s2.total_burned);
    }
}
