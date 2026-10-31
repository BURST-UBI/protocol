use super::*;
use proptest::prelude::*;
use std::collections::BTreeMap;

fn hash(seed: u8) -> BlockHash {
    BlockHash::new([seed + 1; 32])
}

fn legacy_acknowledge(
    wallet: &mut BrnWalletState,
    sources: &[BlockHash],
    at: Timestamp,
) -> Result<(), String> {
    if sources.is_empty() {
        return Ok(());
    }
    let supplied: BTreeSet<_> = sources.iter().copied().collect();
    let mut covered = BTreeSet::new();
    let mut outcomes = Vec::new();
    for (outcome, release) in wallet.pending_releases.iter() {
        if !release.sources.is_empty() && release.sources.iter().all(|hash| supplied.contains(hash))
        {
            if at < release.completed_at {
                return Err("stake release predates its outcome".into());
            }
            covered.extend(release.sources.iter().copied());
            outcomes.push(*outcome);
        }
    }
    if covered != supplied {
        return Err("missing, partial or already consumed stake release".into());
    }
    for outcome in outcomes {
        wallet.pending_releases.remove(&outcome);
    }
    Ok(())
}

proptest! {
    #[test]
    fn lookup_matches_legacy_scan_and_totals(
        entries in prop::collection::vec((0u8..32, any::<u128>(), prop::collection::vec(0u8..32, 0..8), 0u64..20), 0..64),
        selected in prop::collection::vec(0u8..32, 0..32), at in 0u64..25
    ) {
        let rows: BTreeMap<_, _> = entries.into_iter().map(|(outcome, amount, sources, completed)| (
            hash(outcome), StakeRelease { amount, sources: sources.into_iter().map(hash).collect(), completed_at: Timestamp::new(completed) }
        )).collect();
        let mut indexed = BrnWalletState::new(Timestamp::EPOCH);
        indexed.pending_releases = rows.clone().into();
        prop_assert_eq!(bincode::serialize(&indexed.pending_releases).unwrap(), bincode::serialize(&rows).unwrap());
        let bytes = bincode::serialize(&indexed).unwrap();
        indexed = BrnWalletState::from_persisted_bytes(&bytes).unwrap();
        let mut legacy = indexed.clone();
        let mut sources: Vec<_> = selected.into_iter().map(hash).collect();
        sources.sort_unstable(); sources.dedup();
        let before = bincode::serialize(&indexed).unwrap();
        let expected = legacy_acknowledge(&mut legacy, &sources, Timestamp::new(at));
        let actual = indexed.acknowledge_releases(&sources, Timestamp::new(at));
        prop_assert_eq!(actual.is_ok(), expected.is_ok());
        prop_assert_eq!(&indexed, &legacy);
        if actual.is_err() { prop_assert_eq!(bincode::serialize(&indexed).unwrap(), before); }
        let total = indexed.pending_releases.values().try_fold(0u128, |sum, release| sum.checked_add(release.amount));
        prop_assert_eq!(indexed.pending_releases.total_checked(), total);
    }
}

#[test]
fn overflowing_total_recovers_after_acknowledgement_and_survives_update() {
    let mut wallet = BrnWalletState::new(Timestamp::EPOCH);
    wallet
        .record_release(hash(1), u128::MAX, &[hash(1)], Timestamp::EPOCH)
        .unwrap();
    wallet
        .record_release(hash(2), 1, &[hash(2)], Timestamp::EPOCH)
        .unwrap();
    assert_eq!(wallet.pending_releases.total_checked(), None);
    wallet
        .record_release(hash(2), 5, &[hash(2)], Timestamp::EPOCH)
        .unwrap();
    assert_eq!(wallet.pending_releases.total_checked(), None);
    let before = wallet.clone();
    assert!(wallet
        .record_release(hash(1), 1, &[hash(1)], Timestamp::EPOCH)
        .is_err());
    assert_eq!(wallet, before);
    wallet
        .acknowledge_releases(&[hash(1)], Timestamp::EPOCH)
        .unwrap();
    assert_eq!(wallet.pending_releases.total_checked(), Some(6));
    assert_eq!(
        wallet
            .spendable_balance_checked(&RateHistory::new(10, Timestamp::EPOCH), Timestamp::new(1)),
        Some(4)
    );
}

#[test]
fn small_proof_leaves_large_unrelated_backlog_unchanged() {
    let mut wallet = BrnWalletState::new(Timestamp::EPOCH);
    for n in 1u64..=4096 {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&n.to_be_bytes());
        let outcome = BlockHash::new(bytes);
        wallet
            .record_release(outcome, 1, &[outcome], Timestamp::EPOCH)
            .unwrap();
    }
    let first = *wallet.pending_releases.keys().next().unwrap();
    wallet
        .acknowledge_releases(&[first], Timestamp::EPOCH)
        .unwrap();
    assert_eq!(wallet.pending_releases.len(), 4095);
    assert!(!wallet.pending_releases.contains_key(&first));
    assert_eq!(wallet.pending_releases.total_checked(), Some(4095));
    assert!(wallet
        .pending_releases
        .unindexed_outcomes()
        .next()
        .is_none());
}

fn legacy_release_sources(wallet: &BrnWalletState, limit: usize) -> Vec<BlockHash> {
    let mut selected = BTreeSet::new();
    for release in wallet.pending_releases.values() {
        let extra = release
            .sources
            .iter()
            .filter(|hash| !selected.contains(*hash))
            .count();
        if selected.len() + extra <= limit {
            selected.extend(release.sources.iter().copied());
        }
    }
    selected.into_iter().collect()
}

proptest! {
    #[test]
    fn release_selection_matches_full_scan_with_overlapping_and_raw_proofs(
        entries in prop::collection::vec((0u8..32, prop::collection::vec(0u8..32, 0..16)), 0..64),
        limit in 0usize..40,
    ) {
        let rows: BTreeMap<_, _> = entries.into_iter().map(|(outcome, sources)| (
            hash(outcome), StakeRelease { amount: 1, sources: sources.into_iter().map(hash).collect(), completed_at: Timestamp::new(9) }
        )).collect();
        let mut wallet = BrnWalletState::new(Timestamp::EPOCH);
        // Include raw duplicate/unordered/incomplete historical rows as well as
        // overlapping and empty proofs; preserve their existing selection behavior.
        wallet.pending_releases = rows.into();
        let before = bincode::serialize(&wallet).unwrap();
        let expected = legacy_release_sources(&wallet, limit);
        prop_assert_eq!(wallet.release_sources(limit), expected);
        prop_assert_eq!(bincode::serialize(&wallet).unwrap(), before);
        prop_assert_eq!(wallet.release_sources(usize::MAX), legacy_release_sources(&wallet, usize::MAX));
    }
}

#[test]
fn release_selection_does_not_visit_tail_after_limit_is_filled() {
    let release = StakeRelease {
        amount: 1,
        sources: vec![hash(0)],
        completed_at: Timestamp::new(9),
    };
    let untouched = std::iter::from_fn(|| -> Option<&StakeRelease> {
        panic!("selection must not visit a proof at limit zero")
    });
    assert!(select_release_sources(untouched, 0).is_empty());
    let tail = std::iter::from_fn(|| -> Option<&StakeRelease> {
        panic!("selection must not visit the retained tail after filling the limit")
    });
    assert_eq!(
        select_release_sources(std::iter::once(&release).chain(tail), 1),
        release.sources
    );
}

#[test]
fn release_selection_skips_oversized_proofs_and_keeps_complete_overlapping_proofs() {
    let mut wallet = BrnWalletState::new(Timestamp::EPOCH);
    wallet
        .record_release(
            hash(0),
            1,
            &[hash(0), hash(1), hash(2), hash(3)],
            Timestamp::new(9),
        )
        .unwrap();
    wallet
        .record_release(hash(4), 1, &[hash(4), hash(5)], Timestamp::new(9))
        .unwrap();
    wallet
        .record_release(hash(6), 1, &[hash(5), hash(6)], Timestamp::new(9))
        .unwrap();
    assert_eq!(wallet.release_sources(3), vec![hash(4), hash(5), hash(6)]);
    assert!(wallet.has_more_releases(&wallet.release_sources(3)));
    assert_eq!(
        wallet.release_sources(3),
        legacy_release_sources(&wallet, 3)
    );
}

#[test]
fn full_release_page_visits_only_its_prefix_of_a_large_backlog() {
    use std::cell::Cell;
    let releases: Vec<_> = (1u64..=4096)
        .map(|n| {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&n.to_be_bytes());
            StakeRelease {
                amount: 1,
                sources: vec![BlockHash::new(bytes)],
                completed_at: Timestamp::EPOCH,
            }
        })
        .collect();
    let visits = Cell::new(0);
    let selected = select_release_sources(
        releases.iter().inspect(|_| visits.set(visits.get() + 1)),
        1024,
    );
    assert_eq!(visits.get(), 1024);
    assert_eq!(
        selected,
        releases[..1024]
            .iter()
            .map(|r| r.sources[0])
            .collect::<Vec<_>>()
    );
}

fn legacy_has_more(wallet: &BrnWalletState, selected: &[BlockHash]) -> bool {
    wallet.pending_releases.values().any(|release| {
        release
            .sources
            .iter()
            .any(|source| selected.binary_search(source).is_err())
    })
}

proptest! {
    #[test]
    fn source_index_matches_scan_across_replace_remove_clone_and_restart(
        actions in prop::collection::vec((0u8..16, prop::collection::vec(0u8..16, 0..10), any::<bool>()), 0..64),
        selected in prop::collection::vec(0u8..16, 0..20),
    ) {
        let selected: Vec<_> = selected.into_iter().map(hash).collect();
        let mut canonical = selected.clone(); canonical.sort_unstable(); canonical.dedup();
        let mut wallet = BrnWalletState::new(Timestamp::EPOCH);
        for (outcome, sources, remove) in actions {
            let retained = wallet.clone();
            if remove { wallet.pending_releases.remove(&hash(outcome)); }
            else {
                wallet.pending_releases.insert(hash(outcome), StakeRelease {
                    amount: 1, sources: sources.into_iter().map(hash).collect(), completed_at: Timestamp::EPOCH,
                });
            }
            for slice in [&selected, &canonical] {
                prop_assert_eq!(wallet.has_more_releases(slice), legacy_has_more(&wallet, slice));
                prop_assert_eq!(retained.has_more_releases(slice), legacy_has_more(&retained, slice));
            }
            let expected = wallet.pending_releases.values().filter(|r| !r.sources.is_empty()).count();
            prop_assert_eq!(wallet.pending_releases.nonempty_count(), expected);
            let rows: BTreeMap<_, _> = wallet.pending_releases.iter().map(|(k,v)| (*k,v.clone())).collect();
            prop_assert_eq!(bincode::serialize(&wallet.pending_releases).unwrap(), bincode::serialize(&rows).unwrap());
            let bytes = bincode::serialize(&wallet).unwrap();
            wallet = BrnWalletState::from_persisted_bytes(&bytes).unwrap();
            prop_assert_eq!(wallet.has_more_releases(&selected), legacy_has_more(&wallet, &selected));
            prop_assert_eq!(wallet.pending_releases.nonempty_count(), expected);
        }
    }
}

#[test]
fn source_index_ignores_large_unproven_backlogs_and_retains_shared_sources() {
    let mut wallet = BrnWalletState::new(Timestamp::EPOCH);
    for n in 1u64..=4096 {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&n.to_be_bytes());
        wallet
            .record_unproven_release(BlockHash::new(bytes), 1, Timestamp::EPOCH)
            .unwrap();
    }
    assert_eq!(wallet.pending_releases.nonempty_count(), 0);
    assert!(!wallet.has_more_releases(&[]));
    // Raw rows deliberately share a source, including duplicate references.
    for n in [0, 1] {
        wallet.pending_releases.insert(
            hash(n),
            StakeRelease {
                amount: 1,
                sources: vec![hash(4), hash(4)],
                completed_at: Timestamp::EPOCH,
            },
        );
    }
    assert_eq!(wallet.pending_releases.nonempty_count(), 2);
    assert!(wallet.has_more_releases(&[]));
    assert!(!wallet.has_more_releases(&[hash(4)]));
    wallet.pending_releases.remove(&hash(0));
    assert_eq!(wallet.pending_releases.nonempty_count(), 1);
    wallet.pending_releases.remove(&hash(1));
    assert_eq!(wallet.pending_releases.nonempty_count(), 0);
    assert!(!wallet.has_more_releases(&[]));
}

fn release_source_index_case(population: usize, shape: &str) {
    use std::{hint::black_box, time::Instant};
    fn source(n: u64) -> BlockHash {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&n.to_be_bytes());
        BlockHash::new(bytes)
    }
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
    let initial_memory = memory();
    let (rows, rows_ms) = timed(|| {
        (1..=population as u64)
            .map(|n| {
                let outcome = source(n * 64);
                let sources = match shape {
                    "disjoint" => (0..8).map(|offset| source(n * 64 + offset)).collect(),
                    "overlap" => std::iter::once(outcome)
                        .chain((1..=32).map(source))
                        .collect(),
                    "unproven" => Vec::new(),
                    _ => panic!("unknown release index shape"),
                };
                (
                    outcome,
                    StakeRelease {
                        amount: 1,
                        sources,
                        completed_at: Timestamp::EPOCH,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>()
    });
    let rows_memory = memory();
    // Baseline construction includes totals and exceptional rows, but excludes
    // the nonempty-proof index. It is not the production deserializer.
    let ((old_total, old_unindexed), old_derived_ms) = timed(|| {
        let total = rows.values().map(|release| release.amount).sum::<u128>();
        let unindexed: BTreeSet<_> = rows
            .iter()
            .filter(|(outcome, release)| {
                !release.sources.is_empty() && !release.sources.contains(outcome)
            })
            .map(|(outcome, _)| *outcome)
            .collect();
        (total, unindexed)
    });
    assert!(old_unindexed.is_empty());
    let (pending, indexed_derived_ms) = timed(|| PendingReleases::from(rows));
    let index_memory = memory();
    assert_eq!(pending.total_checked(), Some(old_total));
    let expected_distinct = match shape {
        "disjoint" => population * 8,
        "overlap" => population + 32,
        "unproven" => 0,
        _ => unreachable!(),
    };
    assert_eq!(
        pending.nonempty_count(),
        if shape == "unproven" { 0 } else { population }
    );
    let mut wallet = BrnWalletState::new(Timestamp::EPOCH);
    wallet.pending_releases = pending;
    let all: Vec<_> = wallet
        .pending_releases
        .values()
        .flat_map(|r| r.sources.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(all.len(), expected_distinct);
    let page = wallet.release_sources(1024);
    assert_eq!(page, legacy_release_sources(&wallet, 1024));
    let mut previous_query_ms = Vec::new();
    let mut indexed_query_ms = Vec::new();
    for selected in [&[][..], page.as_slice(), all.as_slice()] {
        let expected = legacy_has_more(&wallet, selected);
        assert_eq!(wallet.has_more_releases(selected), expected);
        let (previous, elapsed) = timed(|| {
            (0..3)
                .map(|_| black_box(legacy_has_more(black_box(&wallet), black_box(selected))))
                .collect::<Vec<_>>()
        });
        previous_query_ms.push(elapsed);
        let (current, elapsed) = timed(|| {
            (0..3)
                .map(|_| black_box(wallet.has_more_releases(black_box(selected))))
                .collect::<Vec<_>>()
        });
        indexed_query_ms.push(elapsed);
        assert_eq!(previous, vec![expected; 3]);
        assert_eq!(current, previous);
    }
    // Separate update measurements from the construction memory checkpoints.
    let mut baseline: BTreeMap<_, _> = wallet
        .pending_releases
        .iter()
        .map(|(k, v)| (*k, v.clone()))
        .collect();
    let changes: Vec<_> = (1..=population.min(1024) as u64)
        .map(|n| {
            let outcome = source(n * 64);
            (
                outcome,
                StakeRelease {
                    amount: 2,
                    sources: vec![source(1), outcome],
                    completed_at: Timestamp::EPOCH,
                },
            )
        })
        .collect();
    let mut previous_clone_ms = Vec::new();
    let mut shared_clone_ms = Vec::new();
    let mut previous_retained_update_ms = Vec::new();
    let mut shared_retained_update_ms = Vec::new();
    let (outcome, release) = &changes[0];
    for _ in 0..3 {
        let (mut previous, elapsed) = timed(|| black_box(&baseline).clone());
        previous_clone_ms.push(elapsed);
        let (mut shared, elapsed) = timed(|| black_box(&wallet.pending_releases).clone());
        shared_clone_ms.push(elapsed);
        let (_, elapsed) = timed(|| {
            previous.insert(*outcome, release.clone());
        });
        previous_retained_update_ms.push(elapsed);
        let (_, elapsed) = timed(|| shared.insert(*outcome, release.clone()));
        shared_retained_update_ms.push(elapsed);
        assert_eq!(
            shared.iter().collect::<Vec<_>>(),
            previous.iter().collect::<Vec<_>>()
        );
        assert_eq!(baseline[outcome].amount, 1);
        assert_eq!(wallet.pending_releases[outcome].amount, 1);
        assert_eq!(shared.total_checked(), Some(population as u128 + 1));
        assert_eq!(
            wallet.pending_releases.total_checked(),
            Some(population as u128)
        );
        black_box((&previous, &shared));
    }
    println!("release_snapshot_sharing {{\"population\":{population},\"shape\":\"{shape}\",\"previous_clone_ms\":{previous_clone_ms:?},\"shared_clone_ms\":{shared_clone_ms:?},\"previous_retained_update_ms\":{previous_retained_update_ms:?},\"shared_retained_update_ms\":{shared_retained_update_ms:?}}}");
    let (_, previous_update_ms) = timed(|| {
        for (outcome, release) in &changes {
            baseline.insert(*outcome, release.clone());
        }
        for (outcome, _) in &changes {
            baseline.remove(outcome);
        }
    });
    let (_, indexed_update_ms) = timed(|| {
        for (outcome, release) in &changes {
            wallet.pending_releases.insert(*outcome, release.clone());
        }
        for (outcome, _) in &changes {
            wallet.pending_releases.remove(outcome);
        }
    });
    assert_eq!(
        wallet.pending_releases.iter().collect::<Vec<_>>(),
        baseline.iter().collect::<Vec<_>>()
    );
    let expected = baseline.values().filter(|r| !r.sources.is_empty()).count();
    assert_eq!(wallet.pending_releases.nonempty_count(), expected);
    assert_eq!(
        wallet.has_more_releases(&page),
        legacy_has_more(&wallet, &page)
    );
    // Raw-map updates exclude existing totals/exception maintenance, so this is
    // a lower-bound baseline for total derived-index overhead, not its isolation.
    println!("release_source_index {{\"population\":{population},\"shape\":\"{shape}\",\"distinct\":{expected_distinct},\"rows_ms\":{rows_ms},\"old_derived_ms\":{old_derived_ms},\"indexed_derived_ms\":{indexed_derived_ms},\"previous_query_ms\":{previous_query_ms:?},\"indexed_query_ms\":{indexed_query_ms:?},\"updates\":{},\"previous_update_ms\":{previous_update_ms},\"indexed_update_ms\":{indexed_update_ms},\"initial_memory_kib\":{initial_memory:?},\"rows_memory_kib\":{rows_memory:?},\"index_memory_kib\":{index_memory:?}}}", changes.len());
}

#[test]
fn release_source_index_diagnostic_matches_reference_workloads() {
    for shape in ["disjoint", "overlap", "unproven"] {
        release_source_index_case(12, shape);
    }
}

#[test]
#[ignore = "explicit capped-host release source index cost diagnostic"]
fn release_source_index_scaling() {
    let population = std::env::var("BURST_RELEASE_PROBE_POPULATION")
        .unwrap()
        .parse()
        .unwrap();
    let shape = std::env::var("BURST_RELEASE_PROBE_SHAPE").unwrap();
    assert!([1000, 100000].contains(&population));
    release_source_index_case(population, &shape);
}

#[test]
fn outcome_count_requires_distinct_complete_proofs_and_preserves_raw_fallback() {
    let mut wallet = BrnWalletState::new(Timestamp::EPOCH);
    wallet
        .record_release(hash(0), 1, &[hash(0)], Timestamp::EPOCH)
        .unwrap();
    wallet
        .record_release(hash(1), 1, &[hash(1), hash(2)], Timestamp::EPOCH)
        .unwrap();
    for selected in [
        vec![],
        vec![hash(0)],
        vec![hash(0), hash(0)],
        vec![hash(0), hash(1)],
        vec![hash(0), hash(1), hash(2)],
        vec![hash(2), hash(0), hash(1)],
    ] {
        assert_eq!(
            wallet.has_more_releases(&selected),
            legacy_has_more(&wallet, &selected)
        );
    }
    let retained = wallet.clone();
    wallet.pending_releases.insert(
        hash(3),
        StakeRelease {
            amount: 1,
            sources: vec![hash(4)],
            completed_at: Timestamp::EPOCH,
        },
    );
    assert!(wallet.has_more_releases(&[hash(0), hash(1), hash(2)]));
    assert!(!wallet.has_more_releases(&[hash(0), hash(1), hash(2), hash(4)]));
    wallet.pending_releases.remove(&hash(3));
    assert_eq!(wallet, retained);
    assert!(!wallet.has_more_releases(&[hash(0), hash(1), hash(2)]));
}

#[test]
fn release_selection_skips_unproven_history_and_preserves_snapshot_indexes() {
    let key = |id: u64| {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&id.to_be_bytes());
        BlockHash::new(bytes)
    };
    let mut wallet = BrnWalletState::new(Timestamp::EPOCH);
    for id in 1..=10_000 {
        wallet
            .record_unproven_release(key(id), 1, Timestamp::EPOCH)
            .unwrap();
    }
    assert_eq!(wallet.pending_releases.selectable_values().len(), 0);
    assert!(wallet.release_sources(1024).is_empty());
    let before = wallet.clone();
    let proof = StakeRelease {
        amount: 1,
        sources: vec![key(5000)],
        completed_at: Timestamp::EPOCH,
    };
    wallet.pending_releases.insert(key(5000), proof);
    assert_eq!(wallet.pending_releases.selectable_values().len(), 1);
    assert_eq!(
        wallet.release_sources(1024),
        legacy_release_sources(&wallet, 1024)
    );
    assert_eq!(wallet.release_sources(1024), vec![key(5000)]);
    assert!(before.release_sources(1024).is_empty());
    let bytes = bincode::serialize(&wallet).unwrap();
    let restored = BrnWalletState::from_persisted_bytes(&bytes).unwrap();
    assert_eq!(restored.pending_releases.selectable_values().len(), 1);
    assert_eq!(restored.release_sources(1024), wallet.release_sources(1024));
    wallet.pending_releases.insert(
        key(5000),
        StakeRelease {
            amount: 1,
            sources: vec![],
            completed_at: Timestamp::EPOCH,
        },
    );
    assert!(wallet.release_sources(1024).is_empty());
    assert_eq!(restored.release_sources(1024), vec![key(5000)]);
    wallet.pending_releases.remove(&key(5000));
    assert_eq!(wallet.pending_releases.selectable_values().len(), 0);
    assert_eq!(bincode::serialize(&restored).unwrap(), bytes);
}
