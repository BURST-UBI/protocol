//! Mutex-protected total and bulk token buckets. Callers share an instance for
//! node-wide limits or create one per socket. Both buckets start with one second
//! of credit and refill up to that capacity; these are burst-capable rates.
/// Per-socket outbound bandwidth budget: 5 MiB/s.
pub const DEFAULT_MAX_BYTES_PER_SEC: u64 = 5 * 1024 * 1024;

use serde::{Deserialize, Serialize};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BandwidthLimits {
    pub total_bytes_per_second: u64,
    pub bulk_bytes_per_second: u64,
}
impl Default for BandwidthLimits {
    fn default() -> Self {
        Self {
            total_bytes_per_second: 10 * 1024 * 1024,
            bulk_bytes_per_second: 8 * 1024 * 1024,
        }
    }
}
impl BandwidthLimits {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.bulk_bytes_per_second < 65_540
            || self.total_bytes_per_second <= self.bulk_bytes_per_second
        {
            return Err(
                "bandwidth requires 65540 <= bulk_bytes_per_second < total_bytes_per_second",
            );
        }
        Ok(())
    }
}

pub use burst_types::TrafficClass;

struct Bucket {
    rate: u64,
    credit: u128,
    updated: Instant,
}
impl Bucket {
    fn new(rate: u64, now: Instant) -> Self {
        Self {
            rate,
            credit: rate as u128 * 1_000_000_000,
            updated: now,
        }
    }
    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.updated).as_nanos();
        self.credit = self
            .credit
            .saturating_add(elapsed.saturating_mul(self.rate as u128))
            .min(self.rate as u128 * 1_000_000_000);
        self.updated = now;
    }
}

/// Bulk debits both buckets; control debits only the total bucket.
/// A failed reservation debits neither bucket. Configuration validation belongs
/// to the caller; requests larger than bucket capacity cannot become admissible.
pub struct SharedBandwidth(Mutex<(Bucket, Bucket)>);
impl SharedBandwidth {
    pub fn new(limits: &BandwidthLimits) -> Self {
        Self::at(limits, Instant::now())
    }
    fn at(limits: &BandwidthLimits, now: Instant) -> Self {
        Self(Mutex::new((
            Bucket::new(limits.total_bytes_per_second, now),
            Bucket::new(limits.bulk_bytes_per_second, now),
        )))
    }
    pub fn try_consume(&self, bytes: usize, class: TrafficClass) -> bool {
        self.consume_at(bytes, class, Instant::now())
    }

    /// Wait until the complete frame fits, then debit the applicable buckets.
    /// Failed attempts spend no credit. The caller owns cancellation/deadlines;
    /// a successful reservation is not refunded if later work fails.
    pub(crate) async fn acquire(&self, bytes: usize, class: TrafficClass) {
        while let Err(delay) = self.reserve_at(bytes, class, Instant::now()) {
            tokio::time::sleep(delay).await;
        }
    }

    fn consume_at(&self, bytes: usize, class: TrafficClass, now: Instant) -> bool {
        self.reserve_at(bytes, class, now).is_ok()
    }

    fn reserve_at(&self, bytes: usize, class: TrafficClass, now: Instant) -> Result<(), Duration> {
        let mut buckets = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let (total, bulk) = &mut *buckets;
        total.refill(now);
        bulk.refill(now);
        let cost = bytes as u128 * 1_000_000_000;
        let wait = |bucket: &Bucket| {
            cost.saturating_sub(bucket.credit)
                .div_ceil(u128::from(bucket.rate.max(1)))
        };
        let delay = if class == TrafficClass::Bulk {
            wait(total).max(wait(bulk))
        } else {
            wait(total)
        };
        if delay != 0 {
            return Err(Duration::from_nanos(delay.min(u128::from(u64::MAX)) as u64));
        }
        total.credit -= cost;
        if class == TrafficClass::Bulk {
            bulk.credit -= cost;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn pacing_waits_for_deficit_without_debiting_rejected_reservations() {
        let now = Instant::now();
        let b = SharedBandwidth::at(
            &BandwidthLimits {
                total_bytes_per_second: 100,
                bulk_bytes_per_second: 80,
            },
            now,
        );
        assert!(b.consume_at(100, TrafficClass::Control, now));
        assert_eq!(
            b.reserve_at(20, TrafficClass::Control, now),
            Err(Duration::from_millis(200))
        );
        assert_eq!(
            b.reserve_at(20, TrafficClass::Control, now + Duration::from_millis(100)),
            Err(Duration::from_millis(100))
        );
        assert!(b.consume_at(20, TrafficClass::Control, now + Duration::from_millis(200)));
        assert!(!b.consume_at(1, TrafficClass::Control, now + Duration::from_millis(200)));
    }

    #[test]
    fn many_callers_share_total_and_bulk_cannot_consume_control_reserve() {
        let now = Instant::now();
        let b = SharedBandwidth::at(
            &BandwidthLimits {
                total_bytes_per_second: 100,
                bulk_bytes_per_second: 80,
            },
            now,
        );
        for _ in 0..8 {
            assert!(b.consume_at(10, TrafficClass::Bulk, now));
        }
        assert!(!b.consume_at(1, TrafficClass::Bulk, now));
        assert!(b.consume_at(20, TrafficClass::Control, now));
        assert!(!b.consume_at(1, TrafficClass::Control, now));
        assert!(b.consume_at(50, TrafficClass::Control, now + Duration::from_millis(500)));
        assert!(!b.consume_at(1, TrafficClass::Bulk, now + Duration::from_millis(500)));
    }
    #[test]
    fn rejected_bulk_does_not_spend_total_and_submillisecond_refills_accumulate() {
        let now = Instant::now();
        let b = SharedBandwidth::at(
            &BandwidthLimits {
                total_bytes_per_second: 1000,
                bulk_bytes_per_second: 800,
            },
            now,
        );
        assert!(!b.consume_at(801, TrafficClass::Bulk, now));
        assert!(b.consume_at(1000, TrafficClass::Control, now));
        for n in 1..10 {
            assert!(!b.consume_at(
                1,
                TrafficClass::Control,
                now + Duration::from_micros(n * 100)
            ));
        }
        assert!(b.consume_at(1, TrafficClass::Control, now + Duration::from_millis(1)));
    }
}
