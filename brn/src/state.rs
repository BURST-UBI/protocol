//! Per-wallet BRN state and global rate history.

use crate::error::BrnError;
use burst_types::Timestamp;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

mod accrual_index;
use accrual_index::Accrued;

/// A segment of BRN accrual at a specific rate.
///
/// Rate segments are stored in the shared `RateHistory`, not per-wallet.
/// Every wallet uses the same rate at the same time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
/// Persistent rate vectors and accrual maps copy only changed tree paths.
/// Candidate snapshots are O(1); appends use O(log k) work and copied storage.
/// Valid histories use timestamp-indexed cumulative accrual: O(log k) per
/// active interval, where k is the number of rate segments. Malformed histories
/// loaded with `from_segments` fall back to a linear scan.
#[derive(Debug)]
pub struct RateHistory {
    segments: Arc<imbl::Vector<RateSegment>>,
    accrued: Arc<Option<imbl::OrdMap<Timestamp, (usize, Accrued)>>>,
    append_base: Option<Arc<imbl::Vector<RateSegment>>>,
}

impl Clone for RateHistory {
    fn clone(&self) -> Self {
        Self {
            segments: self.segments.clone(),
            accrued: self.accrued.clone(),
            append_base: Some(self.segments.clone()),
        }
    }
}

impl Serialize for RateHistory {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("RateHistory", 1)?;
        state.serialize_field("segments", self.segments.as_ref())?;
        state.end()
    }
}

impl PartialEq for RateHistory {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.segments, &other.segments) || self.segments == other.segments
    }
}

impl Eq for RateHistory {}

impl<'de> Deserialize<'de> for RateHistory {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct StoredHistory {
            segments: Vec<RateSegment>,
        }
        let stored = StoredHistory::deserialize(deserializer)?;
        Ok(Self::from_segments(stored.segments))
    }
}

impl RateHistory {
    pub fn new(initial_rate: u128, genesis: Timestamp) -> Self {
        Self::from_segments(vec![RateSegment {
            rate: initial_rate,
            start: genesis,
            end: None,
        }])
    }

    /// Load segments and build an accrual index when their boundaries are valid.
    /// This does not reject malformed histories; those use a linear scan.
    /// Engine restoration must validate the history before using it.
    pub fn from_segments(segments: Vec<RateSegment>) -> Self {
        let accrued = accrual_index::build(&segments);
        Self {
            segments: Arc::new(segments.into_iter().collect()),
            accrued: Arc::new(accrued),
            append_base: None,
        }
    }

    pub fn segments(&self) -> &imbl::Vector<RateSegment> {
        &self.segments
    }

    /// First appended row relative to a retained snapshot, when that prefix is
    /// known by shared identity. Row encoding omits the derived segment end.
    /// Unrelated/restored histories require a complete comparison or replacement.
    pub fn appended_rows_start(&self, before: &Self) -> Option<usize> {
        if Arc::ptr_eq(&self.segments, &before.segments)
            || self.append_base.as_ref().is_some_and(|base| Arc::ptr_eq(base, &before.segments)) {
            Some(before.segments.len())
        } else {
            None
        }
    }

    /// Close the current segment and append a new one, without wallet iteration.
    /// O(log k) tree work, including when a snapshot shares the history.
    /// Equal timestamps are allowed: the last appended rate applies from that
    /// timestamp, and earlier segments at the same timestamp have zero duration.
    pub fn apply_rate_change(
        &mut self,
        new_rate: u128,
        change_at: Timestamp,
    ) -> Result<(), BrnError> {
        if let Some(current) = self.segments.last() {
            if change_at.as_secs() < current.start.as_secs() {
                return Err(BrnError::InvalidTimestamp);
            }
        }
        if self.append_base.is_none() {
            self.append_base = Some(self.segments.clone());
        }
        if let Some(prefix) = Arc::make_mut(&mut self.accrued) {
            let next = match self.segments.last() {
                Some(current) => prefix
                    .get_max()
                    .map(|(_, (_, total))| *total)
                    .unwrap_or_default()
                    .add(Accrued::product(
                        current.rate,
                        change_at.as_secs() - current.start.as_secs(),
                    )),
                None => Accrued::default(),
            };
            prefix.insert(change_at, (self.segments.len(), next));
        }
        let segments = Arc::make_mut(&mut self.segments);
        if let Some(current) = segments.back_mut() {
            current.end = Some(change_at);
        }
        segments.push_back(RateSegment {
            rate: new_rate,
            start: change_at,
            end: None,
        });
        Ok(())
    }

    /// Return the last stored rate, or zero for an empty history.
    /// This does not select a rate for a particular query timestamp.
    pub fn current_rate(&self) -> u128 {
        self.segments.last().map(|s| s.rate).unwrap_or(0)
    }

    /// Compute total BRN accrued for a wallet verified at `verified_at`, up to `now`.
    ///
    /// Intersects the global rate history with the wallet's lifetime.
    /// O(log k) for valid histories; raw malformed histories use a linear scan.
    /// Returns None if this interval exceeds u128, even though the cumulative
    /// index can represent larger totals. Earlier overflow does not invalidate
    /// a later interval whose accrual fits.
    pub fn total_accrued_checked(&self, verified_at: Timestamp, now: Timestamp) -> Option<u128> {
        if let Some(prefix) = self.accrued.as_ref() {
            if now <= verified_at {
                return Some(0);
            }
            let Some((_, &(end_index, end_total))) = prefix.range(..=now).next_back() else {
                return Some(0);
            };
            let end_segment = &self.segments[end_index];
            // An interval contained in one segment needs no prefix subtraction.
            if verified_at >= end_segment.start {
                return end_segment.rate.checked_mul(u128::from(
                    now.as_secs() - verified_at.as_secs(),
                ));
            }
            let end = end_total.add(Accrued::product(
                end_segment.rate,
                now.as_secs() - end_segment.start.as_secs(),
            ));
            let start = match prefix.range(..=verified_at).next_back() {
                None => Accrued::default(),
                Some((_, &(index, total))) => {
                    let segment = &self.segments[index];
                    total.add(Accrued::product(
                        segment.rate,
                        verified_at.as_secs() - segment.start.as_secs(),
                    ))
                }
            };
            return end.difference(start);
        }
        let mut total: u128 = 0;
        for seg in self.segments.iter() {
            let seg_end = seg.end.unwrap_or(now);
            let effective_start = if seg.start.as_secs() > verified_at.as_secs() {
                seg.start
            } else {
                verified_at
            };
            let effective_end = Timestamp::new(seg_end.as_secs().min(now.as_secs()));
            if effective_start.as_secs() >= effective_end.as_secs() {
                continue;
            }
            let duration = effective_end
                .as_secs()
                .saturating_sub(effective_start.as_secs());
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
/// Accrual eligibility, burns, stakes and pending releases for one wallet.
/// Rate changes are shared through `RateHistory`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrnWalletState {
    /// When this wallet was verified (BRN accrual begins here).
    pub verified_at: Timestamp,

    /// Cumulative burned BRN in this ledger state. Rollback can reduce it.
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
    /// Completed de-verification intervals; preserve historical rates rather
    /// than moving the original verification timestamp when accrual resumes.
    #[serde(default)]
    pub paused_intervals: crate::pauses::PauseHistory,
    /// Returned stakes not yet acknowledged by this account's signed chain.
    pub pending_releases: crate::releases::PendingReleases,
}

fn default_true() -> bool {
    true
}

impl BrnWalletState {
    /// Create a new BRN state for a freshly verified wallet.
    pub fn new(verified_at: Timestamp) -> Self {
        Self {
            verified_at,
            total_burned: 0,
            total_staked: 0,
            accrual_active: true,
            accrual_stopped_at: None,
            paused_intervals: Default::default(),
            pending_releases: Default::default(),
        }
    }

    /// Construct wallet state; the rate argument is ignored.
    /// The supplied rate has no effect; balances use `RateHistory`.
    pub fn new_compat(verified_at: Timestamp, _initial_rate: u128) -> Self {
        Self::new(verified_at)
    }

    /// Accrue over eligible intervals, then subtract burns and active stakes.
    /// Pending releases are deducted separately by `spendable_balance_checked`.
    /// Returns None on arithmetic overflow or an overdrawn state. For indexed
    /// rates, work is O((p + 1) log k), with p pauses visited and k rate segments.
    /// Pauses must be chronological and non-overlapping; this method does not
    /// validate them. An earlier `now` changes accrual only: burns and stakes
    /// still come from this state, so this is not a historical ledger lookup.
    pub fn available_balance_checked(&self, rates: &RateHistory, now: Timestamp) -> Option<u128> {
        let effective_now = if self.accrual_active {
            now
        } else {
            Timestamp::new(
                self.accrual_stopped_at
                    .unwrap_or(now)
                    .as_secs()
                    .min(now.as_secs()),
            )
        };
        let mut accrued = 0u128;
        let mut cursor = self.verified_at;
        for &(start, end) in self.paused_intervals.iter() {
            if start.as_secs() >= effective_now.as_secs() {
                break;
            }
            let until = Timestamp::new(start.as_secs().min(effective_now.as_secs()));
            accrued = accrued.checked_add(rates.total_accrued_checked(cursor, until)?)?;
            cursor = Timestamp::new(cursor.as_secs().max(end.as_secs()));
        }
        accrued = accrued.checked_add(rates.total_accrued_checked(cursor, effective_now)?)?;
        let after_burned = accrued.checked_sub(self.total_burned)?;
        after_burned.checked_sub(self.total_staked)
    }

    /// Compute available balance, returning zero if checked arithmetic fails.
    pub fn available_balance(&self, rates: &RateHistory, now: Timestamp) -> u128 {
        self.available_balance_checked(rates, now).unwrap_or(0)
    }

    /// Stop accrual at the supplied time. Repeated stops preserve the first time;
    /// the caller validates the transition and timestamp.
    pub fn stop_accrual(&mut self, at: Timestamp) {
        if !self.accrual_active {
            return;
        }
        self.accrual_active = false;
        self.accrual_stopped_at = Some(at);
    }

    /// Resume BRN accrual (e.g. on re-verification of a deactivated wallet).
    ///
    /// Keeps earned BRN at its original historical rates; grants nothing
    /// for the paused period, including when governance changed the rate.
    /// A timestamp before the recorded stop leaves the state unchanged. The
    /// caller must ensure this transition follows earlier pause intervals.
    pub fn resume_accrual(&mut self, at: Timestamp) {
        if !self.accrual_active {
            if let Some(stopped) = self.accrual_stopped_at {
                if at.as_secs() < stopped.as_secs() {
                    return;
                }
                if at != stopped {
                    self.paused_intervals.push((stopped, at));
                }
            }
        }
        self.accrual_active = true;
        self.accrual_stopped_at = None;
    }

    /// Decode the complete stored state and reject trailing bytes. This does not
    /// validate economic invariants. Records missing release fields are rejected.
    pub fn from_persisted_bytes(bytes: &[u8]) -> Result<Self, bincode::Error> {
        use bincode::Options;
        let decode = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes();
        decode.deserialize(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn historical_query_does_not_accrue_future_part_of_closed_segment() {
        let mut history = RateHistory::new(10, Timestamp::new(0));
        history.apply_rate_change(20, Timestamp::new(100)).unwrap();
        assert_eq!(
            history.total_accrued_checked(Timestamp::new(0), Timestamp::new(50)),
            Some(500)
        );
        assert_eq!(
            history.total_accrued_checked(Timestamp::new(60), Timestamp::new(50)),
            Some(0)
        );
    }

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
        assert_eq!(
            h.total_accrued(Timestamp::new(500), Timestamp::new(1000)),
            25_000
        );
    }

    #[test]
    fn rate_history_two_segments() {
        let mut h = RateHistory::new(100, Timestamp::new(0));
        h.apply_rate_change(200, Timestamp::new(1000)).unwrap();
        assert_eq!(h.current_rate(), 200);
        assert_eq!(h.segments.len(), 2);

        // Wallet from t=0 to t=2000: 100*1000 + 200*1000 = 300_000
        assert_eq!(
            h.total_accrued(Timestamp::new(0), Timestamp::new(2000)),
            300_000
        );
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
        assert_eq!(
            h.total_accrued(Timestamp::new(50), Timestamp::new(250)),
            4000
        );
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
    fn wallet_state_resume_accrual_skips_paused_gap() {
        let rates = RateHistory::new(100, Timestamp::new(0));
        let mut state = BrnWalletState::new(Timestamp::new(0));
        state.total_burned = 5000;
        state.stop_accrual(Timestamp::new(50));
        state.resume_accrual(Timestamp::new(100));

        assert!(state.accrual_active);
        assert_eq!(state.verified_at, Timestamp::new(0));
        assert_eq!(
            state.paused_intervals,
            vec![(Timestamp::new(50), Timestamp::new(100))]
        );
        let balance = state.available_balance(&rates, Timestamp::new(200));
        // accrued = 100 * (200 - 50) = 15000, minus 5000 burned = 10000
        assert_eq!(balance, 10000);
    }

    #[test]
    fn wallet_state_compat_constructor_ignores_rate() {
        let s1 = BrnWalletState::new(Timestamp::new(42));
        let s2 = BrnWalletState::new_compat(Timestamp::new(42), 999);
        assert_eq!(s1.verified_at, s2.verified_at);
        assert_eq!(s1.total_burned, s2.total_burned);
    }

    #[test]
    fn pause_does_not_reprice_previously_earned_brn() {
        let mut rates = RateHistory::new(100, Timestamp::new(0));
        rates.apply_rate_change(200, Timestamp::new(75)).unwrap();
        let mut state = BrnWalletState::new(Timestamp::new(0));
        state.stop_accrual(Timestamp::new(50));
        state.stop_accrual(Timestamp::new(70)); // repeated event is idempotent
        assert_eq!(state.available_balance(&rates, Timestamp::new(25)), 2500);
        state.resume_accrual(Timestamp::new(100));
        assert_eq!(state.available_balance(&rates, Timestamp::new(150)), 15000);
        assert_eq!(state.available_balance(&rates, Timestamp::new(60)), 5000);
        let bytes = bincode::serialize(&state).unwrap();
        assert_eq!(
            BrnWalletState::from_persisted_bytes(&bytes)
                .unwrap()
                .available_balance(&rates, Timestamp::new(150)),
            15000
        );
    }

    #[test]
    fn old_wallet_records_cannot_invent_empty_release_history() {
        let bytes = bincode::serialize(&(
            Timestamp::new(10),
            20u128,
            30u128,
            false,
            Some(Timestamp::new(50)),
        ))
        .unwrap();
        assert!(BrnWalletState::from_persisted_bytes(&bytes).is_err());
    }
}
