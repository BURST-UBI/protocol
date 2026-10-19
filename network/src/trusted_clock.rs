//! Conservative in-process signing time, separate from diagnostic clock offsets.
//! At least three distinct configured sources must supply fresh authenticated
//! observations. Bounds tolerate at most one incorrect source; no host clock is set.

use std::{
    sync::RwLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

mod nts;
pub use nts::{NtsClock, NtsSource};

const MAX_AGE: Duration = Duration::from_secs(90);
const MAX_WALL_DIVERGENCE: Duration = Duration::from_millis(250);
const MAX_WINDOW_NS: i128 = 10_000_000_000;
const SECOND_NS: i128 = 1_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SigningTime {
    /// Lower bound, rounded down. A cutoff at or before this value has passed.
    pub now: u64,
    /// Difference from the lower bound to the upper bound, rounded up.
    pub error_bound: u64,
}

struct Observation {
    lower_ns: i128,
    upper_ns: i128,
    received: Instant,
    wall: SystemTime,
}

pub struct TrustedClock {
    observations: RwLock<Vec<Option<Observation>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ClockError {
    #[error("trusted time requires three to eight distinct configured sources")]
    SourceCount,
    #[error("fewer than three authenticated time sources are fresh")]
    InsufficientSources,
    #[error("authenticated time bounds disagree or are too uncertain")]
    Uncertain,
    #[error("trusted-clock state is unavailable")]
    Unavailable,
}

impl TrustedClock {
    pub fn new(source_count: usize) -> Result<Self, ClockError> {
        if !(3..=8).contains(&source_count) {
            return Err(ClockError::SourceCount);
        }
        Ok(Self {
            observations: RwLock::new((0..source_count).map(|_| None).collect()),
        })
    }

    pub fn reading(&self) -> Result<SigningTime, ClockError> {
        self.reading_at(Instant::now(), SystemTime::now())
    }

    fn reading_at(&self, now: Instant, wall: SystemTime) -> Result<SigningTime, ClockError> {
        let observations = self
            .observations
            .read()
            .map_err(|_| ClockError::Unavailable)?;
        let mut intervals = Vec::new();
        for observation in observations.iter().flatten() {
            let Some(elapsed) = now.checked_duration_since(observation.received) else {
                continue;
            };
            let Ok(wall_elapsed) = wall.duration_since(observation.wall) else {
                continue;
            };
            if elapsed > MAX_AGE || elapsed.abs_diff(wall_elapsed) > MAX_WALL_DIVERGENCE {
                continue;
            }
            let elapsed_ns = elapsed.as_nanos() as i128;
            // Allow one percent monotonic-clock drift, far wider than normal
            // hardware error or operating-system clock discipline. The wall
            // comparison separately detects suspend gaps and time steps.
            let drift_ns = (elapsed_ns + 99) / 100;
            intervals.push((
                observation.lower_ns + elapsed_ns - drift_ns,
                observation.upper_ns + elapsed_ns + drift_ns,
            ));
        }
        if intervals.len() < 3 {
            return Err(ClockError::InsufficientSources);
        }
        // Each possible faulty source is excluded in turn. At least one of
        // these intersections contains UTC under the one-fault assumption.
        // Keep the hull of *all* consistent possibilities, never pick a
        // convenient pair or a median that no quorum actually supports.
        let mut lower = i128::MAX;
        let mut upper = i128::MIN;
        for excluded in 0..intervals.len() {
            let mut intersection = (i128::MIN, i128::MAX);
            for (index, &(start, end)) in intervals.iter().enumerate() {
                if index != excluded {
                    intersection.0 = intersection.0.max(start);
                    intersection.1 = intersection.1.min(end);
                }
            }
            if intersection.0 <= intersection.1 {
                lower = lower.min(intersection.0);
                upper = upper.max(intersection.1);
            }
        }
        if lower < 0 || upper < lower || upper - lower > MAX_WINDOW_NS {
            return Err(ClockError::Uncertain);
        }
        let lower_secs = u64::try_from(lower / SECOND_NS).map_err(|_| ClockError::Uncertain)?;
        let upper_secs = u64::try_from((upper + SECOND_NS - 1) / SECOND_NS)
            .map_err(|_| ClockError::Uncertain)?;
        Ok(SigningTime {
            now: lower_secs,
            error_bound: upper_secs - lower_secs,
        })
    }

    /// This boundary is private: only the authenticated NTS client may install
    /// observations. Peer telemetry and unauthenticated SNTP cannot reach it.
    fn record(
        &self,
        source: usize,
        utc_ns: i128,
        uncertainty_ns: u64,
        received: Instant,
        wall: SystemTime,
    ) -> Result<(), ClockError> {
        if uncertainty_ns > MAX_WINDOW_NS as u64 / 2
            || utc_ns < i128::from(uncertainty_ns)
            || utc_ns > i128::from(u64::MAX) * SECOND_NS - MAX_WINDOW_NS
            || wall.duration_since(UNIX_EPOCH).is_err()
        {
            return Err(ClockError::Uncertain);
        }
        let mut observations = self
            .observations
            .write()
            .map_err(|_| ClockError::Unavailable)?;
        let slot = observations
            .get_mut(source)
            .ok_or(ClockError::SourceCount)?;
        *slot = Some(Observation {
            lower_ns: utc_ns - i128::from(uncertainty_ns),
            upper_ns: utc_ns + i128::from(uncertainty_ns),
            received,
            wall,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1000)
    }

    #[test]
    fn one_source_cannot_move_time_or_supply_a_quorum_by_repeating() {
        let clock = TrustedClock::new(4).unwrap();
        let now = Instant::now();
        for _ in 0..10 {
            clock
                .record(0, 1000 * SECOND_NS, 100_000_000, now, wall())
                .unwrap();
        }
        assert!(matches!(
            clock.reading_at(now, wall()),
            Err(ClockError::InsufficientSources)
        ));
        clock
            .record(1, 1000 * SECOND_NS, 100_000_000, now, wall())
            .unwrap();
        assert!(matches!(
            clock.reading_at(now, wall()),
            Err(ClockError::InsufficientSources)
        ));
        for bad in [1, 1_000_000] {
            clock.record(2, bad * SECOND_NS, 1, now, wall()).unwrap();
            let time = clock.reading_at(now, wall()).unwrap();
            assert_eq!(
                time,
                SigningTime {
                    now: 999,
                    error_bound: 2
                }
            );
        }
    }

    #[test]
    fn stale_samples_suspend_and_wall_steps_remove_signing_authority() {
        let clock = TrustedClock::new(4).unwrap();
        let now = Instant::now();
        for source in 0..3 {
            clock
                .record(source, 1000 * SECOND_NS, 100_000_000, now, wall())
                .unwrap();
        }
        assert!(clock.reading_at(now, wall()).is_ok());
        let elapsed = Duration::from_secs(30);
        let time = clock.reading_at(now + elapsed, wall() + elapsed).unwrap();
        assert_eq!(
            time,
            SigningTime {
                now: 1029,
                error_bound: 2
            }
        );
        for (mono, wall_delta) in [
            (Duration::from_secs(91), Duration::from_secs(91)),
            (Duration::from_secs(1), Duration::from_secs(3600)),
            (Duration::from_secs(1), Duration::ZERO),
        ] {
            assert!(matches!(
                clock.reading_at(now + mono, wall() + wall_delta),
                Err(ClockError::InsufficientSources)
            ));
        }
        assert!(matches!(
            clock.reading_at(now, wall() - Duration::from_secs(1)),
            Err(ClockError::InsufficientSources)
        ));
    }

    #[test]
    fn disagreeing_or_wide_intervals_fail_closed_and_rounding_never_understates_error() {
        let clock = TrustedClock::new(3).unwrap();
        let now = Instant::now();
        for source in 0..3 {
            clock
                .record(
                    source,
                    (1000 + source as i128 * 10) * SECOND_NS,
                    0,
                    now,
                    wall(),
                )
                .unwrap();
        }
        assert!(matches!(
            clock.reading_at(now, wall()),
            Err(ClockError::Uncertain)
        ));
        for source in 0..3 {
            clock
                .record(source, 1000 * SECOND_NS + 1, 0, now, wall())
                .unwrap();
        }
        assert_eq!(
            clock.reading_at(now, wall()).unwrap(),
            SigningTime {
                now: 1000,
                error_bound: 1
            }
        );
        for source in 0..3 {
            clock
                .record(source, 1000 * SECOND_NS, 5_000_000_000, now, wall())
                .unwrap();
        }
        assert!(matches!(
            clock.reading_at(
                now + Duration::from_secs(1),
                wall() + Duration::from_secs(1)
            ),
            Err(ClockError::Uncertain)
        ));
        assert!(clock
            .record(0, 1000 * SECOND_NS, 5_000_000_001, now, wall())
            .is_err());
        assert!(clock.record(0, i128::MAX, 1, now, wall()).is_err());
    }

    #[test]
    fn ambiguous_honest_subsets_keep_the_whole_possible_time_window() {
        let clock = TrustedClock::new(3).unwrap();
        let now = Instant::now();
        clock
            .record(0, 1002 * SECOND_NS, 2_000_000_000, now, wall())
            .unwrap();
        clock
            .record(1, 1005 * SECOND_NS, 2_000_000_000, now, wall())
            .unwrap();
        clock
            .record(2, 1008 * SECOND_NS, 2_000_000_000, now, wall())
            .unwrap();
        // Possible honest pairs support [1003,1004] or [1006,1007].
        // Choosing either alone would let the other pair's faulty peer bias time.
        assert_eq!(
            clock.reading_at(now, wall()).unwrap(),
            SigningTime {
                now: 1003,
                error_bound: 4
            }
        );
    }

    #[test]
    fn every_single_fault_position_preserves_the_honest_time_bound() {
        let now = Instant::now();
        let truth = 1000 * SECOND_NS + 400_000_000;
        for count in 3..=8 {
            for faulty in 0..count {
                for bad in [1, 2000 * SECOND_NS, truth] {
                    let clock = TrustedClock::new(count).unwrap();
                    for source in 0..count {
                        let center = if source == faulty {
                            bad
                        } else {
                            truth + (source as i128 - 4) * 10_000_000
                        };
                        clock
                            .record(
                                source,
                                center,
                                if source == faulty { 0 } else { 100_000_000 },
                                now,
                                wall(),
                            )
                            .unwrap();
                    }
                    let time = clock.reading_at(now, wall()).unwrap();
                    assert!(i128::from(time.now) * SECOND_NS <= truth);
                    assert!(i128::from(time.now + time.error_bound) * SECOND_NS >= truth);
                }
            }
        }
    }
}
