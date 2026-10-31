use super::RateSegment;

/// Exact cumulative rate × time. Valid histories cover at most u64::MAX
/// seconds, so their total fits in 192 bits even when a query exceeds u128.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Accrued {
    high: u64,
    low_high: u64,
    low_low: u64,
}

impl Accrued {
    fn from_parts(high: u64, low: u128) -> Self {
        Self {
            high,
            low_high: (low >> 64) as u64,
            low_low: low as u64,
        }
    }

    fn low(self) -> u128 {
        (u128::from(self.low_high) << 64) | u128::from(self.low_low)
    }

    pub(super) fn product(rate: u128, seconds: u64) -> Self {
        let lower = (rate as u64 as u128) * u128::from(seconds);
        let upper = (rate >> 64) * u128::from(seconds);
        let (low, carry) = lower.overflowing_add(upper << 64);
        // A 128-bit rate times a 64-bit duration has at most 192 bits.
        Self::from_parts(((upper >> 64) + u128::from(carry)) as u64, low)
    }

    pub(super) fn add(self, other: Self) -> Self {
        let (low, carry) = self.low().overflowing_add(other.low());
        // Callers sum disjoint intervals on one u64 timestamp axis, so their
        // combined duration cannot exceed u64::MAX and the high limb fits.
        Self::from_parts(self.high + other.high + u64::from(carry), low)
    }

    pub(super) fn difference(self, earlier: Self) -> Option<u128> {
        let (low, borrow) = self.low().overflowing_sub(earlier.low());
        let high = self
            .high
            .checked_sub(earlier.high)?
            .checked_sub(u64::from(borrow))?;
        (high == 0).then_some(low)
    }
}

/// Index accrual at each segment start. Adjacent boundaries must match and the
/// final segment must be open. Equal starts retain the last segment at that time;
/// an empty history produces an empty index. This does not validate rate policy.
pub(super) fn build(
    segments: &[RateSegment],
) -> Option<imbl::OrdMap<burst_types::Timestamp, (usize, Accrued)>> {
    let mut prefix = imbl::OrdMap::new();
    let mut accrued = Accrued::default();
    for (index, segment) in segments.iter().enumerate() {
        prefix.insert(segment.start, (index, accrued));
        match segments.get(index + 1) {
            Some(next) => {
                if segment.end != Some(next.start) || next.start < segment.start {
                    return None;
                }
                accrued = accrued.add(Accrued::product(
                    segment.rate,
                    next.start.as_secs() - segment.start.as_secs(),
                ));
            }
            None if segment.end.is_some() => return None,
            None => {}
        }
    }
    Some(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RateHistory;
    use burst_types::Timestamp;
    use proptest::prelude::*;

    fn scan(history: &RateHistory, from: u64, until: u64) -> Option<u128> {
        history.segments().iter().try_fold(0u128, |sum, segment| {
            let start = segment.start.as_secs().max(from);
            let end = segment.end.map_or(until, |end| end.as_secs().min(until));
            sum.checked_add(
                segment
                    .rate
                    .checked_mul(u128::from(end.saturating_sub(start)))?,
            )
        })
    }

    #[test]
    fn prefix_layout_uses_three_limbs_without_alignment_padding() {
        assert_eq!(std::mem::size_of::<Accrued>(), 24);
        assert_eq!(std::mem::size_of::<(usize, Accrued)>(), 32);
        assert_eq!(
            Accrued::product(u128::MAX, u64::MAX)
                .difference(Accrued::product(u128::MAX, u64::MAX - 1)),
            Some(u128::MAX)
        );
    }

    proptest! {
        #[test]
        fn packed_prefix_sums_preserve_full_timestamp_range(rate in any::<u128>(), split in any::<u64>()) {
            let left = Accrued::product(rate, split);
            let right = Accrued::product(rate, u64::MAX - split);
            prop_assert_eq!(left.add(right), Accrued::product(rate, u64::MAX));
            prop_assert_eq!(left.difference(Accrued::default()), rate.checked_mul(u128::from(split)));
        }
        #[test]
        fn indexed_accrual_matches_checked_scan(
            entries in prop::collection::vec((0u64..10000, prop_oneof![0u128..10000, any::<u128>()]), 1..128),
            from in 0u64..12000, until in 0u64..12000
        ) {
            let mut entries = entries;
            entries.sort_by_key(|entry| entry.0);
            let mut history = RateHistory::new(entries[0].1, Timestamp::new(entries[0].0));
            for &(at, rate) in &entries[1..] {
                history.apply_rate_change(rate, Timestamp::new(at)).unwrap();
            }
            prop_assert_eq!(history.total_accrued_checked(Timestamp::new(from), Timestamp::new(until)), scan(&history, from, until));
            let bytes = bincode::serialize(&history).unwrap();
            let decoded: RateHistory = bincode::deserialize(&bytes).unwrap();
            prop_assert_eq!(&decoded, &history);
            // Existing persisted representation is exactly the segment vector.
            prop_assert_eq!(bytes, bincode::serialize(history.segments()).unwrap());
        }
    }

    #[test]
    fn malformed_history_keeps_scan_and_empty_history_can_start() {
        let malformed = RateHistory::from_segments(vec![
            RateSegment {
                rate: 7,
                start: Timestamp::new(5),
                end: None,
            },
            RateSegment {
                rate: 2,
                start: Timestamp::new(1),
                end: Some(Timestamp::new(9)),
            },
        ]);
        assert!(malformed.accrued.is_none());
        for from in 0..12 {
            for until in 0..12 {
                assert_eq!(
                    malformed.total_accrued_checked(Timestamp::new(from), Timestamp::new(until)),
                    scan(&malformed, from, until)
                );
            }
        }
        let mut empty = RateHistory::from_segments(vec![]);
        assert_eq!(
            empty.total_accrued_checked(Timestamp::EPOCH, Timestamp::new(20)),
            Some(0)
        );
        empty.apply_rate_change(3, Timestamp::new(10)).unwrap();
        assert_eq!(
            empty.total_accrued_checked(Timestamp::EPOCH, Timestamp::new(20)),
            Some(30)
        );
    }

    #[test]
    fn history_snapshots_share_storage_until_rate_changes() {
        let mut history = RateHistory::new(3, Timestamp::EPOCH);
        history.apply_rate_change(4, Timestamp::new(10)).unwrap();
        let mut candidate = history.clone();
        assert!(std::sync::Arc::ptr_eq(
            &history.segments,
            &candidate.segments
        ));
        assert!(std::sync::Arc::ptr_eq(&history.accrued, &candidate.accrued));
        candidate.apply_rate_change(5, Timestamp::new(20)).unwrap();
        assert_eq!(history.current_rate(), 4);
        assert_eq!(candidate.current_rate(), 5);
        assert!(!std::sync::Arc::ptr_eq(
            &history.segments,
            &candidate.segments
        ));
        assert_eq!(
            history.total_accrued_checked(Timestamp::EPOCH, Timestamp::new(30)),
            Some(110)
        );
        assert_eq!(
            candidate.total_accrued_checked(Timestamp::EPOCH, Timestamp::new(30)),
            Some(120)
        );
    }

    #[test]
    fn large_shared_append_preserves_untouched_chunks_and_encoded_history() {
        let mut history = RateHistory::new(3, Timestamp::EPOCH);
        for at in 1..4096 {
            history
                .apply_rate_change(3 + u128::from(at % 7), Timestamp::new(at))
                .unwrap();
        }
        let original = bincode::serialize(&history).unwrap();
        let mut candidate = history.clone();
        candidate
            .apply_rate_change(17, Timestamp::new(4096))
            .unwrap();
        assert!(std::ptr::eq(&history.segments[0], &candidate.segments[0]));
        assert!(std::ptr::eq(
            &history.accrued.as_ref().as_ref().unwrap()[&Timestamp::EPOCH],
            &candidate.accrued.as_ref().as_ref().unwrap()[&Timestamp::EPOCH]
        ));
        assert_eq!(bincode::serialize(&history).unwrap(), original);
        let legacy: Vec<RateSegment> = bincode::deserialize(&original).unwrap();
        assert_eq!(
            legacy,
            history.segments().iter().cloned().collect::<Vec<_>>()
        );
        assert_eq!(history.segments.last().unwrap().end, None);
        assert_eq!(candidate.segments[4095].end, Some(Timestamp::new(4096)));
        for (from, until) in [(0, 4100), (4094, 4100), (4096, 4097)] {
            assert_eq!(
                candidate.total_accrued_checked(Timestamp::new(from), Timestamp::new(until)),
                scan(&candidate, from, until)
            );
        }
        let before_invalid = bincode::serialize(&candidate).unwrap();
        assert!(candidate
            .apply_rate_change(1, Timestamp::new(4095))
            .is_err());
        assert_eq!(bincode::serialize(&candidate).unwrap(), before_invalid);
    }

    #[test]
    fn history_equality_ignores_reconstructed_index() {
        let mut history = RateHistory::from_segments(vec![RateSegment {
            rate: 4,
            start: Timestamp::EPOCH,
            end: Some(Timestamp::new(10)),
        }]);
        assert!(history.accrued.is_none());
        history.apply_rate_change(5, Timestamp::new(10)).unwrap();
        let decoded: RateHistory =
            bincode::deserialize(&bincode::serialize(&history).unwrap()).unwrap();
        assert!(decoded.accrued.is_some());
        assert_eq!(history, decoded);
        assert_eq!(
            history.total_accrued_checked(Timestamp::EPOCH, Timestamp::new(20)),
            decoded.total_accrued_checked(Timestamp::EPOCH, Timestamp::new(20))
        );
    }

    #[test]
    fn historical_overflow_does_not_poison_later_small_intervals() {
        let mut history = RateHistory::new(u128::MAX, Timestamp::EPOCH);
        history.apply_rate_change(7, Timestamp::new(10)).unwrap();
        assert_eq!(
            history.total_accrued_checked(Timestamp::EPOCH, Timestamp::new(10)),
            None
        );
        assert_eq!(
            history.total_accrued_checked(Timestamp::new(10), Timestamp::new(15)),
            Some(35)
        );
        assert_eq!(
            history.total_accrued_checked(Timestamp::new(1), Timestamp::new(2)),
            Some(u128::MAX)
        );
        assert_eq!(
            Accrued::product(u128::MAX, u64::MAX)
                .difference(Accrued::product(u128::MAX, u64::MAX - 1)),
            Some(u128::MAX)
        );
    }

    #[test]
    fn contained_interval_preserves_boundaries_and_checked_overflow() {
        let mut history = RateHistory::new(u128::MAX, Timestamp::new(10));
        history.apply_rate_change(9, Timestamp::new(20)).unwrap();
        history.apply_rate_change(7, Timestamp::new(20)).unwrap();
        history.apply_rate_change(0, Timestamp::new(30)).unwrap();
        for from in [0, 9, 10, 11, 19, 20, 21, 29, 30, 31] {
            for until in [0, 9, 10, 11, 12, 19, 20, 21, 29, 30, 31, 100] {
                assert_eq!(
                    history.total_accrued_checked(Timestamp::new(from), Timestamp::new(until)),
                    scan(&history, from, until),
                    "{from}..{until}"
                );
            }
        }
        assert_eq!(
            history.total_accrued_checked(Timestamp::new(10), Timestamp::new(11)),
            Some(u128::MAX)
        );
        assert_eq!(
            history.total_accrued_checked(Timestamp::new(10), Timestamp::new(12)),
            None
        );
        assert_eq!(
            history.total_accrued_checked(Timestamp::new(20), Timestamp::new(21)),
            Some(7)
        );
    }

    fn rate_pause_case(rate_count: usize, pause_count: usize) {
        use crate::BrnWalletState;
        use std::{hint::black_box, sync::Arc, time::Instant};
        fn timed<T>(operation: impl FnOnce() -> T) -> (T, f64) {
            let start = Instant::now();
            let result = operation();
            (result, start.elapsed().as_secs_f64() * 1000.0)
        }
        fn memory() -> [u64; 2] {
            let status = std::fs::read_to_string("/proc/self/status").unwrap();
            ["VmRSS:", "VmHWM:"].map(|prefix| {
                status
                    .lines()
                    .find(|line| line.starts_with(prefix))
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .parse()
                    .unwrap()
            })
        }
        let horizon = ((rate_count * 10).max((pause_count + 1) * 20)) as u64;
        let (history, construction_ms) = timed(|| {
            let mut history = RateHistory::new(100, Timestamp::EPOCH);
            for index in 1..rate_count {
                history
                    .apply_rate_change(
                        100 + (index % 97) as u128,
                        Timestamp::new(index as u64 * horizon / rate_count as u64),
                    )
                    .unwrap();
            }
            history
        });
        let mut wallet = BrnWalletState::new(Timestamp::EPOCH);
        let step = horizon / (pause_count + 1) as u64;
        let pause_memory_before = memory();
        let pause_started = Instant::now();
        for index in 1..=pause_count {
            let at = index as u64 * step;
            wallet.stop_accrual(Timestamp::new(at));
            wallet.resume_accrual(Timestamp::new(at + step / 2));
        }
        let pause_construction_ms = pause_started.elapsed().as_secs_f64() * 1000.0;
        let pause_memory_built = memory();
        let (pause_bytes, pause_encode_ms) =
            timed(|| bincode::serialize(&wallet.paused_intervals).unwrap());
        let (legacy_pauses, legacy_decode_ms) =
            timed(|| bincode::deserialize::<Vec<(Timestamp, Timestamp)>>(&pause_bytes).unwrap());
        let (decoded_pauses, pause_decode_ms) =
            timed(|| bincode::deserialize::<crate::pauses::PauseHistory>(&pause_bytes).unwrap());
        assert_eq!(decoded_pauses, wallet.paused_intervals);
        assert_eq!(bincode::serialize(&legacy_pauses).unwrap(), pause_bytes);
        let mut clone_ms = Vec::new();
        let mut legacy_clone_ms = Vec::new();
        let mut append_ms = Vec::new();
        let mut legacy_append_ms = Vec::new();
        let mut scalar_delta_ms = Vec::new();
        let mut append_delta_ms = Vec::new();
        for _ in 0..9 {
            let (mut shared, elapsed) = timed(|| black_box(wallet.clone()));
            clone_ms.push(elapsed);
            let (mut legacy, elapsed) = timed(|| black_box(legacy_pauses.clone()));
            legacy_clone_ms.push(elapsed);
            shared.total_burned = 1;
            let mut count = 0;
            let (_, elapsed) = timed(|| {
                crate::wallet_storage::visit_delta(Some(&wallet), Some(&shared), |_, _, _| {
                    count += 1;
                    Ok(())
                })
                .unwrap()
            });
            scalar_delta_ms.push(elapsed);
            assert_eq!(count, 1);
            let interval = (Timestamp::new(horizon + 10), Timestamp::new(horizon + 20));
            let (_, elapsed) = timed(|| shared.paused_intervals.push(black_box(interval)));
            append_ms.push(elapsed);
            let (_, elapsed) = timed(|| legacy.push(black_box(interval)));
            legacy_append_ms.push(elapsed);
            count = 0;
            let (_, elapsed) = timed(|| {
                crate::wallet_storage::visit_delta(Some(&wallet), Some(&shared), |_, _, _| {
                    count += 1;
                    Ok(())
                })
                .unwrap()
            });
            append_delta_ms.push(elapsed);
            assert_eq!(count, 2);
            assert_eq!(shared.paused_intervals, legacy);
        }
        println!("pause_history_cost pauses={pause_count} construction_ms={pause_construction_ms} encode_ms={pause_encode_ms} decode_ms={pause_decode_ms} legacy_decode_ms={legacy_decode_ms} memory_before_kib={pause_memory_before:?} memory_built_kib={pause_memory_built:?} clone_ms={clone_ms:?} legacy_clone_ms={legacy_clone_ms:?} append_ms={append_ms:?} legacy_append_ms={legacy_append_ms:?} scalar_delta_ms={scalar_delta_ms:?} append_delta_ms={append_delta_ms:?}");
        wallet.total_burned = 7;
        wallet.total_staked = 11;
        assert_eq!(wallet.paused_intervals.len(), pause_count);
        let bytes = bincode::serialize(&history).unwrap();
        let (restored, restoration_ms) =
            timed(|| bincode::deserialize::<RateHistory>(&bytes).unwrap());
        assert_eq!(restored, history);
        let mut reference = history.clone();
        // Both paths share wallet pause arithmetic; only rate lookup differs.
        // The independent scan above checks interval arithmetic separately.
        reference.accrued = Arc::new(None);
        let mut previous_ms = Vec::new();
        let mut indexed_ms = Vec::new();
        for stopped in [false, true] {
            let mut state = wallet.clone();
            if stopped {
                state.stop_accrual(Timestamp::new(horizon - step / 4));
            }
            let now = Timestamp::new(horizon + step);
            let expected = state.available_balance_checked(&reference, now);
            assert!(expected.is_some());
            assert_eq!(state.available_balance_checked(&history, now), expected);
            assert_eq!(state.available_balance_checked(&restored, now), expected);
            let (previous, elapsed) = timed(|| {
                (0..3)
                    .map(|_| {
                        black_box(
                            state.available_balance_checked(black_box(&reference), black_box(now)),
                        )
                    })
                    .collect::<Vec<_>>()
            });
            previous_ms.push(elapsed);
            let (current, elapsed) = timed(|| {
                (0..3)
                    .map(|_| {
                        black_box(
                            state.available_balance_checked(black_box(&history), black_box(now)),
                        )
                    })
                    .collect::<Vec<_>>()
            });
            indexed_ms.push(elapsed);
            assert_eq!(previous, vec![expected; 3]);
            assert_eq!(current, previous);
        }
        let before_append_memory = memory();
        let mut appended = history.clone();
        let (_, shared_append_ms) = timed(|| {
            appended
                .apply_rate_change(17, Timestamp::new(horizon + 1))
                .unwrap()
        });
        let after_append_memory = memory();
        assert_eq!(history.segments().len(), rate_count);
        assert_eq!(appended.segments().len(), rate_count + 1);
        assert_eq!(
            appended.total_accrued_checked(Timestamp::EPOCH, Timestamp::new(horizon + 11)),
            scan(&history, 0, horizon + 1).and_then(|value| value.checked_add(170))
        );
        println!("rate_pause_cost {{\"rates\":{rate_count},\"pauses\":{pause_count},\"construction_ms\":{construction_ms},\"restoration_ms\":{restoration_ms},\"previous_ms\":{previous_ms:?},\"indexed_ms\":{indexed_ms:?},\"shared_append_ms\":{shared_append_ms},\"history_bytes\":{},\"before_append_memory_kib\":{before_append_memory:?},\"after_append_memory_kib\":{after_append_memory:?}}}", bytes.len());
    }

    #[test]
    fn rate_pause_diagnostic_preserves_active_stopped_and_restored_balances() {
        for rates in [1, 12] {
            for pauses in [0, 5] {
                rate_pause_case(rates, pauses);
            }
        }
        let history = RateHistory::new(u128::MAX, Timestamp::EPOCH);
        let mut reference = history.clone();
        reference.accrued = std::sync::Arc::new(None);
        let mut wallet = crate::BrnWalletState::new(Timestamp::EPOCH);
        wallet.stop_accrual(Timestamp::new(1));
        wallet.resume_accrual(Timestamp::new(2));
        for now in [0, 1, 2, 3, 4] {
            assert_eq!(
                wallet.available_balance_checked(&history, Timestamp::new(now)),
                wallet.available_balance_checked(&reference, Timestamp::new(now))
            );
        }
        assert_eq!(
            wallet.available_balance_checked(&history, Timestamp::new(3)),
            None
        );
    }

    #[test]
    #[ignore = "explicit capped-host rate and pause history diagnostic"]
    fn rate_pause_scaling() {
        let rates = std::env::var("BURST_RATE_PROBE_SEGMENTS")
            .unwrap()
            .parse()
            .unwrap();
        let pauses = std::env::var("BURST_RATE_PROBE_PAUSES")
            .unwrap()
            .parse()
            .unwrap();
        assert!([10, 1000, 100000].contains(&rates));
        assert!([0, 1000, 100000].contains(&pauses));
        rate_pause_case(rates, pauses);
    }
}
