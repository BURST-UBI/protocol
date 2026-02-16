//! Per-wallet BRN state.

use burst_types::Timestamp;
use serde::{Deserialize, Serialize};

/// A segment of BRN accrual at a specific rate.
///
/// When the community votes to change the BRN rate, accrual is split at the
/// change point. Previous accrual keeps the old rate; new accrual uses the new rate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RateSegment {
    /// The rate (raw units per second) during this segment.
    pub rate: u128,
    /// When this rate became effective for this wallet.
    pub start: Timestamp,
    /// When this rate stopped being effective (None if still active).
    pub end: Option<Timestamp>,
}

/// BRN state for a single wallet.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrnWalletState {
    /// When this wallet was verified (BRN accrual begins here).
    pub verified_at: Timestamp,

    /// Total BRN ever burned by this wallet (cumulative, never decreases).
    pub total_burned: u128,

    /// Total BRN currently locked in active stakes.
    pub total_staked: u128,

    /// History of rate changes affecting this wallet's accrual.
    /// The last segment has `end = None` (currently active rate).
    pub rate_segments: Vec<RateSegment>,
}

impl BrnWalletState {
    /// Create a new BRN state for a freshly verified wallet.
    pub fn new(verified_at: Timestamp, initial_rate: u128) -> Self {
        Self {
            verified_at,
            total_burned: 0,
            total_staked: 0,
            rate_segments: vec![RateSegment {
                rate: initial_rate,
                start: verified_at,
                end: None,
            }],
        }
    }

    /// Total BRN accrued across all rate segments up to `now`.
    pub fn total_accrued(&self, now: Timestamp) -> u128 {
        self.rate_segments
            .iter()
            .map(|seg| {
                let end = seg.end.unwrap_or(now);
                let duration = end.elapsed_since(seg.start);
                seg.rate * duration as u128
            })
            .sum()
    }

    /// Available BRN balance = accrued − burned − staked.
    pub fn available_balance(&self, now: Timestamp) -> u128 {
        self.total_accrued(now)
            .saturating_sub(self.total_burned)
            .saturating_sub(self.total_staked)
    }
}
