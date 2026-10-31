use super::*;
use crate::TrstEngine;
use std::collections::BTreeMap;

const EXPIRY: u64 = 100_000;

pub(super) fn id(n: u64) -> TxHash {
    let mut bytes = [0; 32];
    bytes[..8].copy_from_slice(&n.to_be_bytes());
    TxHash::new(bytes)
}

pub(super) fn fixture(count: u64) -> (WalletAddress, WalletPortfolio) {
    let holder = WalletAddress::new("brst_stable_rows");
    let mut engine = TrstEngine::with_expiry(EXPIRY);
    for n in 1..=count {
        let token = engine
            .mint(
                id(n),
                holder.clone(),
                1,
                holder.clone(),
                Timestamp::new(100 + n),
            )
            .unwrap();
        engine.track_token(token);
    }
    let portfolio = if count == 0 {
        WalletPortfolio::default()
    } else {
        engine.wallets[&holder].clone().into_inner()
    };
    (holder, portfolio)
}

pub(super) fn refresh(portfolio: &mut WalletPortfolio) {
    portfolio.recompute_transferable(Timestamp::EPOCH, EXPIRY);
    portfolio.recompute_earliest_expiry(EXPIRY);
}

fn restore_encoded(holder: &WalletAddress, entries: &BTreeMap<String, Vec<u8>>) -> WalletPortfolio {
    let header_key = portfolio_keys(holder).0;
    let header = Row::from_storage_entry(&header_key, &entries[&header_key]).unwrap();
    restore(
        holder,
        header,
        entries
            .iter()
            .filter(|(key, _)| **key != header_key)
            .map(|(key, bytes)| Row::from_storage_entry(key, bytes)),
        EXPIRY,
    )
    .unwrap()
}

fn apply(
    holder: &WalletAddress,
    before: Option<&WalletPortfolio>,
    after: Option<&WalletPortfolio>,
) -> usize {
    let mut entries: BTreeMap<_, _> = delta(holder, None, before)
        .unwrap()
        .into_iter()
        .map(|(_, row)| row.unwrap().storage_entry().unwrap())
        .collect();
    let changes = delta(holder, before, after).unwrap();
    for (old, new) in &changes {
        let old = old.as_ref().map(|row| row.storage_entry().unwrap());
        let new = new.as_ref().map(|row| row.storage_entry().unwrap());
        let key = new.as_ref().or(old.as_ref()).unwrap().0.clone();
        if let (Some(old), Some(new)) = (&old, &new) {
            assert_eq!(old.0, new.0);
        }
        assert_eq!(entries.get(&key), old.as_ref().map(|(_, bytes)| bytes));
        if let Some((key, bytes)) = new {
            entries.insert(key, bytes);
        } else {
            entries.remove(&key);
        }
    }
    let expected: BTreeMap<_, _> = delta(holder, None, after)
        .unwrap()
        .into_iter()
        .map(|(_, row)| row.unwrap().storage_entry().unwrap())
        .collect();
    assert_eq!(entries, expected);
    if let Some(after) = after {
        let restored = restore_encoded(holder, &entries);
        assert_eq!(&restored, after);
        assert_eq!(
            bincode::serialize(&restored).unwrap(),
            bincode::serialize(after).unwrap()
        );
    } else {
        assert!(entries.is_empty());
    }
    changes.len()
}

#[test]
fn stable_rows_bound_front_middle_and_tail_changes() {
    for n in [32, 256, 1024] {
        let (holder, before) = fixture(n);
        assert_eq!(apply(&holder, Some(&before), Some(&before)), 0);
        for (name, at, expected) in [
            ("front", 1, 2),
            ("oldest_tie", 101, 3),
            ("append", 101 + n, 3),
        ] {
            let mut after = before.clone();
            let token = TrstEngine::with_expiry(EXPIRY)
                .mint(
                    id(n + 1),
                    holder.clone(),
                    1,
                    holder.clone(),
                    Timestamp::new(at),
                )
                .unwrap();
            after.insert_sorted(token);
            refresh(&mut after);
            let rows = apply(&holder, Some(&before), Some(&after));
            assert_eq!(rows, expected, "{name} at {n}");
            println!("stable_holding_cost population={n} operation={name} all_changed_rows={rows}");
        }
        for (position, expected) in [(0, 2), (n as usize / 2, 3), (n as usize - 1, 3)] {
            let mut after = before.clone();
            after.tokens.remove(position);
            refresh(&mut after);
            assert_eq!(apply(&holder, Some(&before), Some(&after)), expected);
        }
        let mut after = before.clone();
        after.tokens[n as usize / 2].amount += 1;
        refresh(&mut after);
        assert_eq!(apply(&holder, Some(&before), Some(&after)), 2);
    }
}

#[test]
fn stable_rows_preserve_empty_portfolios_ties_and_undo() {
    let (holder, mut portfolio) = fixture(8);
    for token in &mut portfolio.tokens {
        token.origin_timestamp = Timestamp::new(100);
        token.effective_origin_timestamp = Timestamp::new(100);
    }
    portfolio.tokens.reverse(); // Equal timestamps must not become token-ID order.
    refresh(&mut portfolio);
    assert_eq!(apply(&holder, None, Some(&portfolio)), 9);
    assert_eq!(apply(&holder, Some(&portfolio), None), 9);
    let empty = WalletPortfolio::default();
    assert_eq!(apply(&holder, None, Some(&empty)), 1);
    assert_eq!(apply(&holder, Some(&empty), None), 1);
    let before = portfolio.clone();
    portfolio.tokens.remove(3);
    refresh(&mut portfolio);
    apply(&holder, Some(&before), Some(&portfolio));
    apply(&holder, Some(&portfolio), Some(&before));
}

#[test]
fn stable_reader_rejects_malformed_topology_owners_caches_and_duplicates() {
    let (holder, portfolio) = fixture(3);
    let rows: Vec<_> = delta(&holder, None, Some(&portfolio))
        .unwrap()
        .into_iter()
        .map(|(_, row)| row.unwrap())
        .collect();
    for fault in 0..11 {
        let mut corrupted = rows.clone();
        match fault {
            0 => {
                if let Row::Header { count, .. } = &mut corrupted[0] {
                    *count = u64::MAX;
                }
            }
            1 => {
                if let Row::Header { head, .. } = &mut corrupted[0] {
                    *head = None;
                }
            }
            2 => {
                if let Row::Holding { next, .. } = &mut corrupted[1] {
                    *next = Some(id(1));
                }
            }
            3 => {
                if let Row::Holding { next, .. } = &mut corrupted[1] {
                    *next = Some(id(99));
                }
            }
            4 => {
                if let Row::Holding { next, .. } = &mut corrupted[1] {
                    *next = None;
                }
            }
            5 => {
                corrupted.push(corrupted[1].clone());
            }
            6 => {
                if let Row::Holding { holder, .. } = &mut corrupted[1] {
                    *holder = WalletAddress::new("brst_wrong");
                }
            }
            7 => {
                if let Row::Header {
                    cached_transferable,
                    ..
                } = &mut corrupted[0]
                {
                    *cached_transferable += 1;
                }
            }
            8 => {
                if let Row::Header {
                    earliest_expiry, ..
                } = &mut corrupted[0]
                {
                    *earliest_expiry = Some(Timestamp::new(999));
                }
            }
            9 => {
                if let Row::Header { head, .. } = &mut corrupted[0] {
                    *head = Some(id(2));
                }
            }
            _ => {
                corrupted[2] = corrupted[0].clone();
            }
        }
        let header = corrupted.remove(0);
        assert!(
            restore(&holder, header, corrupted.into_iter().map(Ok), EXPIRY).is_err(),
            "fault {fault}"
        );
    }
    let mut duplicate = portfolio.clone();
    duplicate.tokens[1].id = duplicate.tokens[0].id;
    assert!(delta(&holder, None, Some(&duplicate)).is_err());
    let mut unordered = portfolio;
    unordered.tokens.reverse();
    assert!(delta(&holder, None, Some(&unordered)).is_err());
}

#[test]
fn stable_row_encoding_binds_version_key_owner_and_limits() {
    let (holder, portfolio) = fixture(1);
    let rows = delta(&holder, None, Some(&portfolio)).unwrap();
    for (_, row) in rows {
        let row = row.unwrap();
        let (key, bytes) = row.storage_entry().unwrap();
        assert_eq!(Row::from_storage_entry(&key, &bytes).unwrap(), row);
        assert!(Row::from_storage_entry(&(key.clone() + "0"), &bytes).is_err());
        assert!(Row::from_storage_entry(&key, &bincode::serialize(&(1u8, &row)).unwrap()).is_err());
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(Row::from_storage_entry(&key, &trailing).is_err());
    }
    let mut header = header(&holder, &portfolio).unwrap();
    if let Row::Header { holder, .. } = &mut header {
        *holder = WalletAddress::new(format!("brst_{}", "x".repeat(256)));
    }
    assert!(header.storage_entry().is_err());
}

fn positional_changes(
    holder: &WalletAddress,
    before: Option<&WalletPortfolio>,
    after: Option<&WalletPortfolio>,
) -> Vec<(String, Option<Vec<u8>>, Option<Vec<u8>>)> {
    let engine = |portfolio: Option<&WalletPortfolio>| {
        let mut engine = TrstEngine::with_expiry(EXPIRY);
        if let Some(portfolio) = portfolio {
            engine
                .wallets
                .insert(holder.clone(), portfolio.clone().into());
        }
        engine
    };
    let mut changes = Vec::new();
    engine(after)
        .visit_wallet_record_delta(&engine(before), |old, new| {
            let old = old.map(|row| row.storage_entry()).transpose()?;
            let new = new.map(|row| row.storage_entry()).transpose()?;
            let key = new.as_ref().or(old.as_ref()).unwrap().0.clone();
            changes.push((key, old.map(|row| row.1), new.map(|row| row.1)));
            Ok::<_, String>(())
        })
        .unwrap();
    changes
}

#[test]
fn positional_journal_context_recovers_exact_portfolios() {
    for count in [0, 3, 256] {
        let (holder, before) = fixture(count);
        let mut after = before.clone();
        let token = TrstEngine::with_expiry(EXPIRY)
            .mint(
                id(count + 1),
                holder.clone(),
                1,
                holder.clone(),
                Timestamp::new(1),
            )
            .unwrap();
        after.insert_sorted(token);
        refresh(&mut after);
        for (old, new) in [
            (None, Some(&before)),
            (Some(&before), None),
            (Some(&before), Some(&after)),
            (Some(&after), Some(&before)),
            (Some(&before), Some(&before)),
        ] {
            let changes = positional_changes(&holder, old, new);
            let restored = positional::rewind_portfolio(&holder, new, changes, 1 << 20).unwrap();
            assert_eq!(restored.as_ref(), old);
            // The converted stable delta must reproduce the same portfolio cut.
            apply(&holder, restored.as_ref(), new);
        }
    }
    let (holder, before) = fixture(3);
    let mut after = before.clone();
    // Preserve historical cache bytes across a governed-period change rather
    // than interpreting old images with the current period.
    after.earliest_expiry = before
        .earliest_expiry
        .map(|at| Timestamp::new(at.as_secs() + 1));
    let restored = positional::rewind_portfolio(
        &holder,
        Some(&after),
        positional_changes(&holder, Some(&before), Some(&after)),
        1 << 20,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        bincode::serialize(&restored).unwrap(),
        bincode::serialize(&before).unwrap()
    );
}

#[test]
fn positional_journal_context_rejects_stale_malformed_and_over_budget_images() {
    let (holder, before) = fixture(3);
    let mut after = before.clone();
    after.tokens[1].amount += 1;
    refresh(&mut after);
    let changes = positional_changes(&holder, Some(&before), Some(&after));
    assert!(
        positional::rewind_portfolio(&holder, Some(&before), changes.clone(), 1 << 20).is_err()
    );
    assert!(positional::rewind_portfolio(&holder, Some(&after), changes.clone(), 0).is_err());
    assert!(positional::rewind_portfolio(
        &WalletAddress::new("brst_wrong"),
        None,
        changes.clone(),
        1 << 20
    )
    .is_err());
    for case in 0..5 {
        let mut bad = changes.clone();
        match case {
            0 => bad.push(bad[0].clone()),
            1 => bad[0].1 = bad[0].2.clone(),
            2 => bad[0].1.as_mut().unwrap().push(0),
            3 => bad[0].0.push('x'),
            _ => {
                let row = crate::engine::WalletRecord::OriginOwner(holder.clone())
                    .storage_entry()
                    .unwrap();
                bad.push((row.0, Some(row.1), None));
            }
        }
        assert!(
            positional::rewind_portfolio(&holder, Some(&after), bad, 1 << 20).is_err(),
            "case {case}"
        );
    }
    // Removing the required before-header cannot produce an orphaned portfolio.
    let mut bad = positional_changes(&holder, Some(&before), None);
    bad.retain(|(key, _, _)| !key.starts_with("trst_state/v1/0/"));
    assert!(positional::rewind_portfolio(&holder, None, bad, 1 << 20).is_err());
}

#[test]
fn streaming_delta_stops_at_budget_without_losing_error_type() {
    #[derive(Debug, PartialEq)]
    enum Failure {
        Codec(String),
        Budget(usize),
    }
    let (holder, before) = fixture(1024);
    let mut after = before.clone();
    for token in &mut after.tokens {
        token.amount += 1;
    }
    refresh(&mut after);
    for limit in [0, 1, 2, 32, 1024, 1025] {
        let mut accepted = 0;
        let mut calls = 0;
        let result = visit_delta(
            &holder,
            Some(&before),
            Some(&after),
            Failure::Codec,
            |old, new| {
                calls += 1;
                if accepted == limit {
                    return Err(Failure::Budget(limit));
                }
                if accepted == 0 {
                    assert!(matches!(new, Some(Row::Header { .. })));
                } else {
                    assert!(matches!(old, Some(Row::Holding { .. })));
                }
                accepted += 1;
                Ok(())
            },
        );
        if limit < 1025 {
            assert_eq!(result, Err(Failure::Budget(limit)));
            assert_eq!(calls, limit + 1);
        } else {
            assert_eq!(result, Ok(()));
            assert_eq!(calls, 1025);
        }
        assert_eq!(accepted, limit);
    }
}

#[test]
fn streaming_delta_validates_both_portfolios_before_any_emission() {
    let (holder, valid) = fixture(3);
    for mutation in 0..10 {
        let mut invalid = valid.clone();
        match mutation {
            0 => invalid.tokens[1].id = invalid.tokens[0].id,
            1 => {
                invalid.tokens[0].id = id(999);
                invalid.tokens[1].id = id(999);
            }
            2 => invalid.tokens.reverse(),
            3 => invalid.tokens[0].state = burst_types::TrstState::Revoked,
            4 => invalid.tokens[0].revoked_origin = Some(id(999)),
            5 => invalid.tokens[0].amount = u128::MAX,
            6 => invalid.cached_transferable += 1,
            7 => invalid.earliest_expiry = None,
            8 => invalid.tokens[0].holder = WalletAddress::new("brst_wrong"),
            _ => invalid.tokens.clear(),
        }
        let expected = validate(&holder, &invalid).unwrap_err();
        for (old, new) in [
            (Some(&invalid), Some(&valid)),
            (Some(&valid), Some(&invalid)),
            (None, Some(&invalid)),
            (Some(&invalid), None),
        ] {
            let mut emitted = 0;
            let result = visit_delta(
                &holder,
                old,
                new,
                |error| error,
                |_, _| {
                    emitted += 1;
                    Ok(())
                },
            );
            assert_eq!(result, Err(expected.clone()), "mutation {mutation}");
            assert_eq!(emitted, 0);
        }
    }
}

#[test]
fn stable_delta_combines_new_retained_reordered_and_deleted_ids() {
    let (holder, mut before) = fixture(4);
    for token in &mut before.tokens {
        token.origin_timestamp = Timestamp::new(100);
        token.effective_origin_timestamp = Timestamp::new(100);
    }
    refresh(&mut before);
    for retained in 0..16 {
        let mut after = before.clone();
        after.tokens = before
            .tokens
            .iter()
            .enumerate()
            .filter(|(n, _)| retained & (1 << n) != 0)
            .map(|(_, token)| token.clone())
            .rev()
            .collect();
        let mut new = before.tokens[0].clone();
        new.id = id(999);
        let middle = after.tokens.len() / 2;
        after.tokens.insert(middle, new);
        refresh(&mut after);
        apply(&holder, Some(&before), Some(&after));
        apply(&holder, Some(&after), Some(&before));
    }
}

#[test]
#[ignore = "explicit capped-host stage attribution diagnostic, not release throughput"]
fn stable_holding_delta_phase_cost() {
    use std::time::Instant;
    let (holder, before) = fixture(100_000);
    let mut after = before.clone();
    after.tokens[50_000].amount += 1;
    refresh(&mut after);
    apply(&holder, Some(&before), Some(&after));
    for sample in 0..5 {
        let started = Instant::now();
        validate(&holder, &before).unwrap();
        let before_validation_ns = started.elapsed().as_nanos();
        let started = Instant::now();
        validate(&holder, &after).unwrap();
        let after_validation_ns = started.elapsed().as_nanos();
        let started = Instant::now();
        let mut old: HashMap<_, _> = before
            .tokens
            .iter()
            .enumerate()
            .map(|(n, token)| (token.id, (token, before.tokens.get(n + 1).map(|t| t.id))))
            .collect();
        let lookup_build_ns = started.elapsed().as_nanos();
        // This separate map-based comparison estimates lookup and scan costs;
        // it does not reproduce visit_delta's shared ID table. Time the actual
        // visitor separately, excluding encoding in both measurements.
        let started = Instant::now();
        let mut changed = 0;
        for (n, token) in after.tokens.iter().enumerate() {
            let next = after.tokens.get(n + 1).map(|t| t.id);
            if old.remove(&token.id) != Some((token, next)) {
                changed += 1;
            }
        }
        let compare_ns = started.elapsed().as_nanos();
        let started = Instant::now();
        for token in &before.tokens {
            if old.remove(&token.id).is_some() {
                changed += 1;
            }
        }
        let deletion_scan_ns = started.elapsed().as_nanos();
        assert_eq!(changed, 1);
        assert!(old.is_empty());
        let started = Instant::now();
        let mut emitted = 0;
        visit_delta(
            &holder,
            Some(&before),
            Some(&after),
            |e| e,
            |old, new| {
                std::hint::black_box((old, new));
                emitted += 1;
                Ok(())
            },
        )
        .unwrap();
        let production_no_encoding_ns = started.elapsed().as_nanos();
        assert_eq!(emitted, 2);
        println!("holding_phases {{\"sample\":{sample},\"population\":100000,\"operation\":\"payload\",\"before_validation_ns\":{before_validation_ns},\"after_validation_ns\":{after_validation_ns},\"lookup_build_ns\":{lookup_build_ns},\"compare_ns\":{compare_ns},\"deletion_scan_ns\":{deletion_scan_ns},\"production_no_encoding_ns\":{production_no_encoding_ns}}}");
    }
}

#[test]
#[ignore = "explicit capped-host holding delta diagnostic, not release throughput"]
fn stable_holding_delta_scaling() {
    use std::time::Instant;
    #[derive(Default)]
    struct Counts {
        rows: usize,
        bytes: usize,
        encoding_ns: u128,
    }
    fn observe(
        entries: impl Iterator<Item = Result<(String, Vec<u8>), String>>,
        counts: &mut Counts,
        instrumented: bool,
    ) -> Result<(), String> {
        let start = instrumented.then(Instant::now);
        counts.rows += 1;
        for entry in entries {
            let (key, value) = entry?;
            counts.bytes += key.len() + value.len();
            std::hint::black_box(value);
        }
        if let Some(start) = start {
            counts.encoding_ns += start.elapsed().as_nanos();
        }
        Ok(())
    }
    for population in [1_000, 10_000, 100_000] {
        let setup = Instant::now();
        let (holder, before) = fixture(population);
        println!(
            "holding_setup population={population} elapsed_us={}",
            setup.elapsed().as_micros()
        );
        let mut before_engine = TrstEngine::with_expiry(EXPIRY);
        before_engine
            .wallets
            .insert(holder.clone(), before.clone().into());
        for operation in ["append", "front_insert", "front_delete", "payload"] {
            let mutation = Instant::now();
            let mut after = before.clone();
            match operation {
                "append" | "front_insert" => {
                    let at = if operation == "append" {
                        population + 101
                    } else {
                        1
                    };
                    let token = TrstEngine::with_expiry(EXPIRY)
                        .mint(
                            id(population + 1),
                            holder.clone(),
                            1,
                            holder.clone(),
                            Timestamp::new(at),
                        )
                        .unwrap();
                    after.insert_sorted(token);
                }
                "front_delete" => {
                    after.tokens.remove(0);
                }
                _ => {
                    after.tokens[population as usize / 2].amount += 1;
                }
            }
            refresh(&mut after);
            let mutation_us = mutation.elapsed().as_micros();
            let mut after_engine = TrstEngine::with_expiry(EXPIRY);
            after_engine
                .wallets
                .insert(holder.clone(), after.clone().into());
            // Outside the timings, apply deltas to an in-memory map, compare old
            // row bytes, and verify reconstructed order and snapshot bytes. This
            // does not exercise a database transaction or concurrent writer.
            apply(&holder, Some(&before), Some(&after));
            println!("holding_mutation population={population} operation={operation} clone_modify_refresh_us={mutation_us}");
            for sample in 0..6 {
                let instrumented = sample == 5;
                let modes = if sample % 2 == 0 {
                    ["positional", "stable"]
                } else {
                    ["stable", "positional"]
                };
                for mode in modes {
                    let mut counts = Counts::default();
                    let started = Instant::now();
                    if mode == "positional" {
                        after_engine
                            .visit_wallet_record_delta(&before_engine, |old, new| {
                                observe(
                                    old.iter().chain(new.iter()).map(|row| row.storage_entry()),
                                    &mut counts,
                                    instrumented,
                                )
                            })
                            .unwrap();
                    } else {
                        visit_delta(
                            &holder,
                            Some(&before),
                            Some(&after),
                            |e| e,
                            |old, new| {
                                observe(
                                    old.iter().chain(new.iter()).map(|row| row.storage_entry()),
                                    &mut counts,
                                    instrumented,
                                )
                            },
                        )
                        .unwrap();
                    }
                    let total_ns = started.elapsed().as_nanos();
                    let expected = match (mode, operation) {
                        ("positional", "front_insert") => population as usize + 2,
                        ("positional", "front_delete") => population as usize + 1,
                        ("stable", "append") => 3,
                        _ => 2,
                    };
                    assert_eq!(counts.rows, expected);
                    println!("holding_scale {{\"population\":{population},\"operation\":\"{operation}\",\"mode\":\"{mode}\",\"sample\":{sample},\"instrumented\":{instrumented},\"total_ns\":{total_ns},\"encoding_ns\":{},\"changed_rows\":{},\"encoded_bytes\":{}}}",
                        counts.encoding_ns, counts.rows, counts.bytes);
                }
            }
        }
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status
                .lines()
                .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
            {
                println!("holding_process population={population} {line}");
            }
        }
    }
}

#[test]
fn direct_key_comparison_matches_canonical_encoder_at_boundaries() {
    let (_, portfolio) = fixture(1);
    for name in [
        String::new(),
        "å/零".into(),
        "x".repeat(214),
        "x".repeat(215),
        "x".repeat(247),
        "x".repeat(248),
        "x".repeat(300),
    ] {
        // Deserialization accepts raw strings that the address constructor rejects.
        // These cases test key encoding, not protocol address validity.
        let holder: WalletAddress =
            bincode::deserialize(&bincode::serialize(&name).unwrap()).unwrap();
        let mut token = portfolio.tokens[0].clone();
        token.holder = holder.clone();
        token.id = TxHash::new([0xab; 32]);
        let header = Row::Header {
            holder: holder.clone(),
            count: 1,
            head: Some(token.id),
            cached_transferable: 1,
            earliest_expiry: portfolio.earliest_expiry,
        };
        let holding = Row::Holding {
            holder: holder.clone(),
            next: None,
            token: Box::new(token),
        };
        for row in [header, holding] {
            let expected = row.key().ok();
            let unchecked_key = match &row {
                Row::Header { .. } => portfolio_keys(&holder).0,
                Row::Holding { token, .. } => {
                    format!("{}{}", portfolio_keys(&holder).1, hex(token.id.as_bytes()))
                }
            };
            let mut keys = vec![
                unchecked_key.clone(),
                unchecked_key.to_uppercase(),
                format!("{unchecked_key}/"),
                format!("{unchecked_key}0"),
                "é".into(),
            ];
            for n in 0..unchecked_key.len() {
                let mut bytes = unchecked_key.as_bytes().to_vec();
                bytes[n] = if bytes[n] == b'0' { b'1' } else { b'0' };
                keys.push(String::from_utf8(bytes).unwrap());
                keys.push(unchecked_key[..n].to_string());
            }
            let bytes = bincode::serialize(&(2u8, &row)).unwrap();
            for key in keys {
                let accepted = expected.as_deref() == Some(key.as_str());
                assert_eq!(row.matches_key(&key), accepted);
                assert_eq!(Row::from_storage_entry(&key, &bytes).is_ok(), accepted);
            }
            if let Row::Holding { mut token, .. } = row {
                token.holder = WalletAddress::new("brst_wrong");
                let bad = Row::Holding {
                    holder: holder.clone(),
                    next: None,
                    token,
                };
                assert!(bad.key().is_err());
                assert!(!bad.matches_key(&unchecked_key));
                assert!(Row::from_storage_entry(
                    &unchecked_key,
                    &bincode::serialize(&(2u8, &bad)).unwrap()
                )
                .is_err());
            }
        }
    }
}

#[test]
fn single_pass_encoding_matches_legacy_bytes_and_exact_limit() {
    let (holder, portfolio) = fixture(2);
    let mut rows = vec![header(&holder, &portfolio).unwrap()];
    for (n, token) in portfolio.tokens.iter().enumerate() {
        rows.push(Row::Holding {
            holder: holder.clone(),
            next: portfolio.tokens.get(n + 1).map(|t| t.id),
            token: Box::new(token.clone()),
        });
    }
    for row in rows {
        let (key, bytes) = row.storage_entry().unwrap();
        assert_eq!(bytes, bincode::serialize(&(2u8, &row)).unwrap());
        assert_eq!(Row::from_storage_entry(&key, &bytes).unwrap(), row);
    }
    // A long origin wallet grows the payload independently of the primary key.
    let mut row = Row::Holding {
        holder,
        next: None,
        token: Box::new(portfolio.tokens[0].clone()),
    };
    if let Row::Holding { token, .. } = &mut row {
        token.origin_wallet = WalletAddress::new("brst_");
    }
    let base = bincode::serialized_size(&(2u8, &row)).unwrap() as usize;
    for size in [
        burst_types::MAX_P2P_MESSAGE_BYTES - 1,
        burst_types::MAX_P2P_MESSAGE_BYTES,
        burst_types::MAX_P2P_MESSAGE_BYTES + 1,
    ] {
        if let Row::Holding { token, .. } = &mut row {
            token.origin_wallet = WalletAddress::new(format!("brst_{}", "x".repeat(size - base)));
        }
        let legacy = bincode::serialize(&(2u8, &row)).unwrap();
        assert_eq!(legacy.len(), size);
        if size <= burst_types::MAX_P2P_MESSAGE_BYTES {
            let (key, actual) = row.storage_entry().unwrap();
            assert_eq!(actual, legacy);
            assert_eq!(Row::from_storage_entry(&key, &actual).unwrap(), row);
        } else {
            assert!(row
                .storage_entry()
                .unwrap_err()
                .contains("stable holding row exceeds byte limit"));
        }
    }
}

#[test]
fn retention_row_links_are_visited_before_token_contents() {
    use burst_types::TrstReference::{Account, Token};
    use std::ops::ControlFlow;
    let (owner, portfolio) = fixture(2);
    let mut linked = false;
    for (_, row) in delta(&owner, None, Some(&portfolio)).unwrap() {
        let row = row.unwrap();
        let next = match &row {
            Row::Header { head, .. } => *head,
            Row::Holding { next, .. } => *next,
        };
        if let Some(next) = next {
            let mut seen = Vec::new();
            let result = row.visit_retention_references(|r| {
                seen.push(r);
                if seen.len() == 2 {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            });
            assert_eq!(result, ControlFlow::Break(()));
            assert_eq!(seen, vec![Account(&owner), Token(next)]);
            if matches!(row, Row::Holding { .. }) {
                linked = true;
            }
        }
    }
    assert!(linked);
}
