use super::*;
use super::super::tests::{fixture, id, refresh};
use std::collections::BTreeMap;

const EXPIRY: u64 = 100_000;

fn check_origin_indexes(root: &IndexedPortfolio, portfolio: &WalletPortfolio) {
    let mut origins: BTreeMap<TxHash, Vec<TxHash>> = BTreeMap::new();
    let mut revoked: BTreeMap<TxHash, Vec<TxHash>> = BTreeMap::new();
    let mut balances: BTreeMap<TxHash, u128> = BTreeMap::new();
    for token in &portfolio.tokens {
        if let Some(tag) = token.revoked_origin {
            revoked.entry(tag).or_default().push(token.id);
        } else {
            origins.entry(token.origin).or_default().push(token.id);
        }
        if token.state == TrstState::Active {
            *balances.entry(token.origin).or_default() += token.amount;
        }
    }
    assert_eq!(root.origins.keys().copied().collect::<Vec<_>>(), origins.keys().copied().collect::<Vec<_>>());
    assert_eq!(root.revoked_origins.keys().copied().collect::<Vec<_>>(), revoked.keys().copied().collect::<Vec<_>>());
    for (origin, ids) in origins {
        assert_eq!(root.origin_tokens(&origin).map(|t| t.id).collect::<Vec<_>>(), ids);
    }
    for (origin, ids) in revoked {
        assert_eq!(root.revoked_tokens(&origin).map(|t| t.id).collect::<Vec<_>>(), ids);
    }
    for origin in balances.keys().chain(root.origin_balances.keys()) {
        assert_eq!(root.origin_active_balance(origin), balances.get(origin).copied().unwrap_or(0));
    }
    assert_eq!(root.origin_active_balance(&id(999_999)), 0);
    let mut split: BTreeMap<(TxHash, Option<TxHash>), Vec<TxHash>> = BTreeMap::new();
    for token in &portfolio.tokens {
        if token.revoked_origin.is_some() { continue; }
        let basis = match token.basis {
            TrstBasis::Single(single) if single != TxHash::ZERO => Some(single),
            _ => None,
        };
        split.entry((token.origin, basis)).or_default().push(token.id);
    }
    let actual: BTreeMap<_, _> = root.split_members.iter().map(|(key, orders)| {
        (*key, orders.iter().map(|order| root.order[order]).collect::<Vec<_>>())
    }).collect();
    assert_eq!(actual, split);
}

// Apply row deltas to an in-memory map and compare canonical output. Database
// transactions and concurrent writes are outside this helper's scope.
fn compare(before: &IndexedPortfolio, after: &IndexedPortfolio, expected: &WalletPortfolio) {
    assert_eq!(&after.to_portfolio(), expected);
    check_origin_indexes(after, expected);
    assert_eq!(bincode::serialize(&after.to_portfolio()).unwrap(),
        bincode::serialize(expected).unwrap());
    let before_portfolio = before.to_portfolio();
    let mut rows: BTreeMap<_, _> = super::super::delta(&before.holder, None, Some(&before_portfolio))
        .unwrap().into_iter().map(|(_, new)| new.unwrap().storage_entry().unwrap()).collect();
    after.visit_delta(before, |e| e, |old, new| {
        let old = old.as_ref().map(Row::storage_entry).transpose()?;
        let new = new.as_ref().map(Row::storage_entry).transpose()?;
        let key = &new.as_ref().or(old.as_ref()).unwrap().0;
        assert_eq!(rows.get(key), old.as_ref().map(|(_, value)| value));
        if let Some((key, value)) = new { rows.insert(key, value); }
        else { rows.remove(key); }
        Ok::<_, String>(())
    }).unwrap();
    let expected_rows: BTreeMap<_, _> = super::super::delta(&after.holder, None, Some(expected))
        .unwrap().into_iter().map(|(_, new)| new.unwrap().storage_entry().unwrap()).collect();
    assert_eq!(rows, expected_rows);
}

#[test]
fn checked_updates_match_canonical_rows_and_preserve_restored_ties() {
    let (holder, mut original) = fixture(4);
    for token in &mut original.tokens { token.origin_timestamp = Timestamp::new(100); }
    original.tokens.reverse();
    refresh(&mut original);
    let root = IndexedPortfolio::from_portfolio(holder, &original, EXPIRY).unwrap();
    for mask in 0..16 {
        let mut expected = original.clone();
        let mut current = root.clone();
        for (n, token) in original.tokens.iter().enumerate() {
            if mask & (1 << n) == 0 {
                current = current.with_removed(&token.id).unwrap();
                expected.tokens.retain(|t| t.id != token.id);
            }
        }
        for (n, timestamp) in [1, 100, 1000].into_iter().enumerate() {
            let mut token = original.tokens[0].clone();
            token.id = id(900 + n as u64);
            token.origin_timestamp = Timestamp::new(timestamp);
            current = current.with_inserted(token.clone()).unwrap();
            expected.insert_sorted(token);
        }
        let mut replacement = current.get(&id(900)).unwrap().clone();
        replacement.amount = 23;
        replacement.state = TrstState::Expired;
        current = current.with_replaced(replacement.clone()).unwrap();
        let replacement_id = replacement.id;
        *expected.tokens.iter_mut().find(|t| t.id == replacement_id).unwrap() = replacement;
        refresh(&mut expected);
        compare(&root, &current, &expected);
        compare(&current, &root, &original);
        assert_eq!(root.to_portfolio(), original);
    }
}

#[test]
fn point_update_shares_untouched_tokens_and_visitor_stops_immediately() {
    let (holder, original) = fixture(1024);
    let root = IndexedPortfolio::from_portfolio(holder, &original, EXPIRY).unwrap();
    assert!(root.rows.shares_storage_with(&root.clone().rows));
    let mut token = root.get(&id(512)).unwrap().clone();
    token.amount += 1;
    let after = root.with_replaced(token).unwrap();
    assert_eq!(after.rows.changed_keys(&root.rows).count(), 1);
    assert!(after.origins.ptr_eq(&root.origins));
    assert!(after.revoked_origins.ptr_eq(&root.revoked_origins));
    assert!(after.split_members.ptr_eq(&root.split_members));
    for token in &original.tokens {
        if token.id != id(512) {
            assert!(std::ptr::eq(root.get(&token.id).unwrap(), after.get(&token.id).unwrap()));
        }
    }
    #[derive(Debug, PartialEq)]
    enum Failure { Invalid(String), Budget }
    let mut calls = 0;
    let result = after.visit_delta(&root, Failure::Invalid, |_, _| {
        calls += 1;
        Err(Failure::Budget)
    });
    assert_eq!(result, Err(Failure::Budget));
    assert_eq!(calls, 1);
    let mut relabeled = root.clone();
    relabeled.next_order = u128::MAX;
    let mut token = original.tokens[0].clone();
    token.id = id(5000);
    let after = relabeled.with_inserted(token.clone()).unwrap();
    let mut expected = original.clone();
    expected.insert_sorted(token);
    refresh(&mut expected);
    compare(&root, &after, &expected);
}

#[test]
fn owned_insertion_errors_preserve_all_indexes_and_batch_atomicity() {
    let (holder, mut original) = fixture(1);
    original.tokens[0].amount = u128::MAX;
    refresh(&mut original);
    let root = IndexedPortfolio::from_portfolio(holder, &original, EXPIRY).unwrap();
    let mut fresh = original.tokens[0].clone();
    fresh.id = id(900);
    fresh.amount = 1;
    let mut expired = fresh.clone();
    expired.id = id(901);
    expired.state = TrstState::Expired;
    let mut wrong_holder = fresh.clone();
    wrong_holder.holder = WalletAddress::new("brst_wrong");
    let mut wrong_state = fresh.clone();
    wrong_state.state = TrstState::Revoked;
    for tokens in [
        vec![original.tokens[0].clone()], vec![fresh.clone()],
        vec![wrong_holder], vec![wrong_state],
        vec![expired.clone(), fresh], vec![expired.clone(), expired.clone()],
    ] {
        let mut candidate = root.clone();
        assert!(candidate.insert_batch(&tokens).is_err());
        assert!(candidate.rows.shares_storage_with(&root.rows));
        assert!(candidate.order.ptr_eq(&root.order));
        assert!(candidate.active_times.ptr_eq(&root.active_times));
        assert!(candidate.origins.ptr_eq(&root.origins));
        assert!(candidate.revoked_origins.ptr_eq(&root.revoked_origins));
        assert!(candidate.split_members.ptr_eq(&root.split_members));
        assert!(candidate.origin_balances.ptr_eq(&root.origin_balances));
        assert!(candidate.counters.shares_storage_with(&root.counters));
        assert_eq!(candidate.next_order, root.next_order);
        compare(&root, &candidate, &original);
    }
    let mut candidate = root.clone();
    candidate.insert_batch(&[expired.clone()]).unwrap();
    let mut expected = original.clone();
    expected.insert_sorted(expired);
    compare(&root, &candidate, &expected);
    assert_eq!(root.to_portfolio(), original);
}

#[test]
fn invalid_inputs_and_updates_never_publish_a_partial_root() {
    let (holder, original) = fixture(3);
    let root = IndexedPortfolio::from_portfolio(holder.clone(), &original, EXPIRY).unwrap();
    for mutation in 0..5 {
        let mut invalid = original.clone();
        match mutation {
            0 => invalid.tokens[1].id = invalid.tokens[0].id,
            1 => invalid.tokens.reverse(),
            2 => invalid.cached_transferable += 1,
            3 => invalid.earliest_expiry = None,
            _ => invalid.earliest_expiry = Some(Timestamp::new(1)),
        }
        assert!(IndexedPortfolio::from_portfolio(holder.clone(), &invalid, EXPIRY).is_err());
    }
    assert!(root.with_inserted(original.tokens[0].clone()).is_err());
    assert!(root.with_removed(&id(999)).is_err());
    for mutation in 0..5 {
        let mut token = original.tokens[0].clone();
        match mutation {
            0 => token.holder = WalletAddress::new("brst_wrong"),
            1 => token.state = TrstState::Revoked,
            2 => token.amount = u128::MAX,
            3 => token.origin_timestamp = Timestamp::new(999),
            _ => token.id = id(999),
        }
        assert!(root.with_replaced(token).is_err());
        assert_eq!(root.to_portfolio(), original);
    }
    let mut other = root.clone();
    other.expiry_secs += 1;
    let mut calls = 0;
    assert!(other.visit_delta(&root, |e| e, |_, _| { calls += 1; Ok(()) }).is_err());
    assert_eq!(calls, 0);
}

#[test]
fn indexed_expiry_matches_scan_and_keeps_idle_roots_shared() {
    let (holder, mut portfolio) = fixture(64);
    portfolio.tokens[3].state = TrstState::Revoked;
    portfolio.tokens[3].revoked_origin = Some(id(3));
    portfolio.tokens[5].state = TrstState::Expired;
    refresh(&mut portfolio);
    let root = IndexedPortfolio::from_portfolio(holder, &portfolio, EXPIRY).unwrap();
    let (idle, amount) = root.with_expired_at(Timestamp::EPOCH).unwrap();
    assert_eq!(amount, 0);
    assert!(idle.rows.shares_storage_with(&root.rows));
    assert!(idle.order.ptr_eq(&root.order));
    assert!(idle.active_times.ptr_eq(&root.active_times));
    let mut current = root.clone();
    for now in [100_100, 100_101, 100_102, 100_106, 100_200, u64::MAX] {
        let before = current.clone();
        let expected_amount = portfolio.flush_expired(Timestamp::new(now), EXPIRY);
        let (next, actual_amount) = current.with_expired_at(Timestamp::new(now)).unwrap();
        assert_eq!(actual_amount, expected_amount);
        compare(&before, &next, &portfolio);
        current = next;
    }
    assert_eq!(current.to_portfolio().tokens[3].state, TrstState::Revoked);
    let (again, amount) = current.with_expired_at(Timestamp::EPOCH).unwrap();
    assert_eq!(amount, 0); // Ordinary expiry never resurrects on a backward clock.
    assert!(again.rows.shares_storage_with(&current.rows));
}

#[test]
fn governed_expiry_matches_engine_resurrection_and_rejects_overflow_atomically() {
    let (holder, mut portfolio) = fixture(8);
    portfolio.tokens[2].state = TrstState::Revoked;
    portfolio.tokens[2].revoked_origin = Some(id(2));
    refresh(&mut portfolio);
    let mut root = IndexedPortfolio::from_portfolio(holder.clone(), &portfolio, EXPIRY).unwrap();
    let mut engine = crate::TrstEngine::with_expiry(EXPIRY);
    engine.wallets.insert(holder.clone(), portfolio.into());
    for (period, now) in [(0, 200), (1000, 200), (10, 120), (u64::MAX, 200),
        (u64::MAX, u64::MAX), (0, 0), (1000, 200)] {
        let saved = root.to_portfolio();
        engine.set_expiry_period(period, Timestamp::new(now)).unwrap();
        let next = root.with_expiry_period(period, Timestamp::new(now)).unwrap();
        assert!(next.counters.shares_storage_with(&root.counters));
        let expected = engine.get_portfolio(&holder).unwrap();
        assert_eq!(next.to_portfolio(), *expected);
        check_origin_indexes(&next, expected);
        assert_eq!(root.to_portfolio(), saved);
        assert!(IndexedPortfolio::from_portfolio(holder.clone(), expected, period).is_ok());
        root = next;
    }
    let (_, mut overflow) = fixture(2);
    overflow.tokens[0].amount = u128::MAX;
    overflow.tokens[1].state = TrstState::Expired;
    refresh(&mut overflow);
    let root = IndexedPortfolio::from_portfolio(holder, &overflow, EXPIRY).unwrap();
    assert!(root.with_expiry_period(EXPIRY, Timestamp::EPOCH).is_err());
    assert_eq!(root.to_portfolio(), overflow);
}

#[test]
fn origin_indexes_follow_payload_reassignment_revocation_and_last_removal() {
    let (holder, mut portfolio) = fixture(12);
    for (n, token) in portfolio.tokens.iter_mut().enumerate() {
        token.origin = id(100 + (n % 3) as u64);
        token.origin_timestamp = Timestamp::new(100);
        if n % 4 == 0 {
            token.state = TrstState::Revoked;
            token.revoked_origin = Some(id(200));
        }
    }
    portfolio.tokens.reverse();
    refresh(&mut portfolio);
    let mut root = IndexedPortfolio::from_portfolio(holder, &portfolio, EXPIRY).unwrap();
    check_origin_indexes(&root, &portfolio);
    for step in 0..4 {
        let before = root.clone();
        let token = &mut portfolio.tokens[1];
        match step {
            0 => { token.origin = id(300); token.amount = 27; }
            1 => { token.state = TrstState::Revoked; token.revoked_origin = Some(id(201)); }
            2 => token.revoked_origin = Some(id(202)),
            _ => { token.state = TrstState::Expired; token.revoked_origin = None; }
        }
        root = root.with_replaced(token.clone()).unwrap();
        refresh(&mut portfolio);
        compare(&before, &root, &portfolio);
    }
    for token in portfolio.tokens.clone() {
        let before = root.clone();
        root = root.with_removed(&token.id).unwrap();
        portfolio.tokens.retain(|t| t.id != token.id);
        refresh(&mut portfolio);
        compare(&before, &root, &portfolio);
    }
    assert!(root.origins.is_empty());
    assert!(root.revoked_origins.is_empty());
    assert!(root.origin_balances.is_empty());
}

#[test]
fn indexed_debit_matches_engine_order_balances_and_separate_provenance() {
    let (holder, mut portfolio) = fixture(24);
    for (n, token) in portfolio.tokens.iter_mut().enumerate() {
        token.origin = id(100 + n as u64 % 3);
        token.origin_timestamp = Timestamp::new(100);
        token.amount = [0, 1, 7, 1u128 << 70][n % 4];
        token.basis = match n % 3 {
            0 => TrstBasis::Entire,
            1 => TrstBasis::Single(id(300)),
            _ => TrstBasis::Remainder(id(301)),
        };
        if n % 7 == 0 {
            token.state = TrstState::Revoked;
            token.revoked_origin = Some(id(200));
        } else if n % 5 == 0 {
            token.state = TrstState::Expired;
        }
    }
    // Multiple bases within each origin, including repeated portions.
    for token in portfolio.tokens.iter_mut().step_by(4) { token.basis = TrstBasis::Entire; }
    for reverse in [false, true] {
        if reverse { portfolio.tokens.reverse(); }
        refresh(&mut portfolio);
        let root = IndexedPortfolio::from_portfolio(holder.clone(), &portfolio, EXPIRY).unwrap();
        for origin in [id(100), id(101), id(102), id(999)] {
            for amount in [0, 1, 6, 7, 8, 1u128 << 70, (1u128 << 70) + 8, u128::MAX] {
                let mut engine = crate::TrstEngine::with_expiry(EXPIRY);
                engine.wallets.insert(holder.clone(), portfolio.clone().into());
                engine.rebuild_indexes();
                let expected_parts = engine.debit_unindexed_portfolio(&holder, &origin, amount);
                let (after, parts) = root.with_debited_origin(&origin, amount).unwrap();
                compare(&root, &after, engine.get_portfolio(&holder).unwrap());
                assert_eq!(bincode::serialize(&parts).unwrap(), bincode::serialize(&expected_parts).unwrap());
                assert_eq!(root.to_portfolio(), portfolio);
                for token in portfolio.tokens.iter().filter(|t| t.origin != origin) {
                    assert!(std::ptr::eq(root.get(&token.id).unwrap(), after.get(&token.id).unwrap()));
                }
                if amount == 0 || origin == id(999) {
                    assert!(root.rows.shares_storage_with(&after.rows));
                }
            }
        }
    }
}

#[test]
fn indexed_whole_revocation_and_restore_match_engine_including_zero_amounts() {
    for zero in [false, true] {
        let (holder, mut portfolio) = fixture(16);
        for (n, token) in portfolio.tokens.iter_mut().enumerate() {
            token.origin = id(100 + n as u64 % 2);
            token.amount = if zero { 0 } else { n as u128 + 1 };
            if n % 5 == 4 {
                token.state = TrstState::Revoked;
                token.revoked_origin = Some(id(100));
                token.basis = TrstBasis::Single(id(100));
            } else if n % 7 == 6 { token.state = TrstState::Expired; }
        }
        refresh(&mut portfolio);
        let root = IndexedPortfolio::from_portfolio(holder.clone(), &portfolio, EXPIRY).unwrap();
        let mut engine = crate::TrstEngine::with_expiry(EXPIRY);
        engine.wallets.insert(holder.clone(), portfolio.clone().into());
        engine.rebuild_indexes();
        engine.merger_graph.apply_revocation(id(100));
        let mut expected_events = Vec::new();
        engine.revoke_whole_tokens(&holder, &id(100), &mut expected_events);
        let (revoked, events) = root.with_revoked_origin(&id(100)).unwrap();
        compare(&root, &revoked, engine.get_portfolio(&holder).unwrap());
        // Removing the earliest active zero-value token must still refresh its deadline.
        IndexedPortfolio::from_portfolio(holder.clone(), engine.get_portfolio(&holder).unwrap(), EXPIRY).unwrap();
        let fields = |events: &[crate::engine::RevocationEvent]| events.iter().map(|e|
            (e.holder.clone(), e.token_id, e.revoked_origin, e.revoked_amount, e.total_amount)).collect::<Vec<_>>();
        assert_eq!(fields(&events), fields(&expected_events));
        let (again, events) = revoked.with_revoked_origin(&id(100)).unwrap();
        assert!(events.is_empty());
        assert!(again.rows.shares_storage_with(&revoked.rows));
        for now in [0, 100_105, u64::MAX] {
            let mut expected = engine.clone();
            let expected_events = expected.restore_origins(vec![id(100)], Timestamp::new(now)).unwrap();
            let (restored, events) = revoked.with_restored_origin(&id(100), Timestamp::new(now)).unwrap();
            compare(&revoked, &restored, expected.get_portfolio(&holder).unwrap());
            let fields = |events: &[crate::engine::RestorationEvent]| events.iter().map(|e|
                (e.holder.clone(), e.token_id, e.amount)).collect::<Vec<_>>();
            assert_eq!(fields(&events), fields(&expected_events));
        }
        assert_eq!(root.to_portfolio(), portfolio);
    }
}

// Synthetic token amounts exercise rounding and overflow boundaries; their sum
// deliberately exceeds the merge supply. This is not an admitted ledger state.
fn split_fixture() -> (WalletAddress, WalletPortfolio, MergerGraph) {
    let (holder, mut portfolio) = fixture(12);
    let mut graph = MergerGraph::new();
    graph.record_merge(crate::MergeNode {
        merge_tx: id(900), total_amount: 3,
        source_origins: vec![
            crate::MergeSource { origin: id(100), amount: 2 },
            crate::MergeSource { origin: id(101), amount: 1 },
        ],
        revoked_contribs: HashMap::new(),
    });
    let remainder = graph.reduced_basis(&id(900), TrstBasis::Entire, id(100), 2);
    for (n, token) in portfolio.tokens.iter_mut().enumerate() {
        token.origin = id(900);
        token.origin_timestamp = Timestamp::new(100);
        token.amount = [7, 1, 0, 1u128 << 70][n % 4];
        token.basis = match n % 4 {
            0 | 1 => TrstBasis::Entire,
            2 => TrstBasis::Single(id(101)),
            _ => remainder,
        };
        if n % 5 == 4 { token.state = TrstState::Expired; }
    }
    refresh(&mut portfolio);
    (holder, portfolio, graph)
}

#[test]
fn indexed_proportional_splits_match_graph_rounding_remainders_and_tie_order() {
    for reverse in [false, true] {
        let (holder, mut portfolio, graph) = split_fixture();
        if reverse { portfolio.tokens.reverse(); }
        let mut root = IndexedPortfolio::from_portfolio(holder.clone(), &portfolio, EXPIRY).unwrap();
        let mut engine = crate::TrstEngine::with_expiry(EXPIRY);
        engine.wallets.insert(holder.clone(), portfolio.into());
        engine.merger_graph = graph;
        engine.rebuild_indexes();
        for (origin, tainted) in [(id(100), 2), (id(100), 2), (id(101), 1)] {
            engine.merger_graph.apply_revocation(origin);
            let graph_before = engine.merger_graph.clone();
            let saved = graph_before.to_bytes();
            let mut expected_events = Vec::new();
            engine.split_revoke_tokens(&holder, &id(900), origin, tainted, &mut expected_events);
            let (next, graph, events) = root.with_split_revocation(
                &graph_before, &id(900), origin, tainted).unwrap();
            compare(&root, &next, engine.get_portfolio(&holder).unwrap());
            assert_eq!(graph.to_bytes(), engine.merger_graph.to_bytes());
            assert_eq!(graph_before.to_bytes(), saved);
            let fields = |events: &[crate::engine::RevocationEvent]| events.iter().map(|e|
                (e.holder.clone(), e.token_id, e.revoked_origin, e.revoked_amount, e.total_amount)).collect::<Vec<_>>();
            assert_eq!(fields(&events), fields(&expected_events));
            root = next;
        }
    }
}


fn shared_history_split_case(history: usize, tokens: usize) {
    use std::time::Instant;
    let merge = id(900_000);
    let first = id(900_001);
    let target = id(900_002);
    let (holder, mut portfolio) = fixture(tokens as u64);
    let mut graph = MergerGraph::new();
    graph.record_merge(crate::MergeNode {
        merge_tx: merge, total_amount: tokens as u128 * 10,
        source_origins: vec![
            crate::MergeSource { origin: first, amount: tokens as u128 * 4 },
            crate::MergeSource { origin: target, amount: tokens as u128 * 6 },
        ],
        revoked_contribs: HashMap::new(),
    });
    for _ in 0..history / 2 {
        graph.apply_revocation(first);
        graph.apply_restoration(&first);
    }
    let basis = graph.current_basis(&merge);
    assert!(matches!(basis, TrstBasis::Remainder(_)));
    for (n, token) in portfolio.tokens.iter_mut().enumerate() {
        token.origin = merge;
        token.amount = 10;
        token.basis = basis;
        token.origin_timestamp = Timestamp::new(100);
        token.state = if n % 5 == 4 { TrstState::Expired } else { TrstState::Active };
        token.revoked_origin = None;
    }
    refresh(&mut portfolio);
    let root = IndexedPortfolio::from_portfolio(holder.clone(), &portfolio, EXPIRY).unwrap();
    graph.apply_revocation(target);
    graph.validate_persisted().unwrap();
    let saved_graph = graph.to_bytes();
    let mut baseline = crate::TrstEngine::with_expiry(EXPIRY);
    baseline.wallets.insert(holder.clone(), portfolio.clone().into());
    baseline.merger_graph = graph.clone();
    baseline.rebuild_indexes();
    for sample in 0..3 {
        let mut raw = baseline.clone();
        let mut expected_events = Vec::new();
        let start = Instant::now();
        raw.split_revoke_tokens(&holder, &merge, target, tokens as u128 * 6, &mut expected_events);
        let raw_ns = start.elapsed().as_nanos();
        let start = Instant::now();
        let (next, after_graph, events) = root.with_split_revocation(
            &graph, &merge, target, tokens as u128 * 6).unwrap();
        let indexed_ns = start.elapsed().as_nanos();
        assert_eq!(events.len(), tokens);
        assert!(events.iter().all(|e| e.revoked_amount == 6 && e.total_amount == 10));
        assert_eq!(events.iter().map(|e| e.token_id).collect::<Vec<_>>(),
            portfolio.tokens.iter().map(|t| t.id).collect::<Vec<_>>());
        let fields = |events: &[crate::engine::RevocationEvent]| events.iter().map(|e|
            (e.holder.clone(), e.token_id, e.revoked_origin, e.revoked_amount, e.total_amount)).collect::<Vec<_>>();
        assert_eq!(fields(&events), fields(&expected_events));
        compare(&root, &next, raw.get_portfolio(&holder).unwrap());
        assert_eq!(after_graph.to_bytes(), raw.merger_graph.to_bytes());
        after_graph.validate_persisted().unwrap();
        assert_eq!(root.to_portfolio(), portfolio);
        assert_eq!(graph.to_bytes(), saved_graph);
        let output = next.to_portfolio();
        assert_eq!(output.tokens.iter().map(|t| t.amount).sum::<u128>(), tokens as u128 * 10);
        assert_eq!(output.tokens.iter().filter(|t| t.revoked_origin == Some(target)).map(|t| t.amount).sum::<u128>(), tokens as u128 * 6);
        println!("shared_history_split history={history} tokens={tokens} sample={sample} raw_ns={raw_ns} indexed_ns={indexed_ns}");
    }
}

#[test]
fn shared_history_splits_preserve_order_amounts_and_prior_roots() {
    shared_history_split_case(64, 32);
}

#[test]
#[ignore = "explicit capped-host full split component history scaling"]
fn shared_history_split_scaling() {
    for (history, tokens) in [(100, 100), (1_000, 100), (10_000, 100), (10_000, 1_000)] {
        shared_history_split_case(history, tokens);
    }
}

#[test]
fn split_failure_preserves_both_roots_after_partial_work() {
    let (holder, portfolio, graph) = split_fixture();
    for collision in [false, true] {
        let mut portfolio = portfolio.clone();
        if collision {
            let mut token = portfolio.tokens[0].clone();
            token.id = crate::engine::split_token_id(&token.id, &id(101));
            token.origin = id(999);
            portfolio.tokens.push(token);
        } else {
            portfolio.tokens.last_mut().unwrap().basis = TrstBasis::Remainder(id(999));
        }
        refresh(&mut portfolio);
        let root = IndexedPortfolio::from_portfolio(holder.clone(), &portfolio, EXPIRY).unwrap();
        let saved = graph.to_bytes();
        assert!(root.with_split_revocation(&graph, &id(900), id(101), 1).is_err());
        assert_eq!(root.to_portfolio(), portfolio);
        assert_eq!(graph.to_bytes(), saved);
        check_origin_indexes(&root, &portfolio);
    }
}

#[test]
#[ignore = "explicit capped-host attribution of distinct-origin point update cost"]
fn distinct_origin_replacement_stages() {
    use std::time::Instant;
    for population in [1_000, 10_000, 100_000] {
        let (holder, mut portfolio) = fixture(population);
        for token in &mut portfolio.tokens { token.amount = 100; }
        refresh(&mut portfolio);
        let root = IndexedPortfolio::from_portfolio(holder.clone(), &portfolio, EXPIRY).unwrap();
        let mut engine = crate::TrstEngine::with_expiry(EXPIRY);
        for token in &portfolio.tokens { engine.track_token(token.clone()); }
        engine.wallets.insert(holder.clone(), WalletPortfolio::from_indexed(root.clone()).into());
        for sample in 0..3 {
            let old = root.rows.get(&id(1)).unwrap();
            let mut token = (*old.token).clone();
            token.amount += 1;
            let start = Instant::now();
            let actual = root.with_replaced(token.clone()).unwrap();
            let total_ns = start.elapsed().as_nanos();
            // Measure an explicit rebuild of each index. Production skips membership
            // updates for this amount-only change, so these timings do not
            // decompose total_ns. Check equivalent contents outside the timers.
            let start = Instant::now();
            let mut next = root.clone();
            let clone_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            next.remove_token_totals(&old.token);
            let remove_active_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            next.add_token_totals(&token).unwrap();
            let add_active_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            next.remove_membership(old.order, &old.token);
            let remove_membership_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            next.add_membership(old.order, &token);
            let add_membership_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            let token_id = token.id;
            next.rows.get_mut(&token_id).unwrap().token = token.into();
            let row_ns = start.elapsed().as_nanos();
            assert!(next.same_canonical_state(&actual));
            assert_eq!(next.to_portfolio(), actual.to_portfolio());
            assert_eq!(root.get(&id(1)).unwrap().amount, 100);
            let mut candidate = engine.clone();
            let mut engine_call = || {
                let start = Instant::now();
                let result = candidate.debit_wallet_with_provenance(&holder, &id(1), 1);
                (result, start.elapsed().as_nanos())
            };
            let direct_call = || {
                let start = Instant::now();
                let result = root.with_debited_origin(&id(1), 1).unwrap();
                (result, start.elapsed().as_nanos())
            };
            let engine_first = sample % 2 == 0;
            let ((portions, engine_ns), ((direct, expected_portions), direct_ns)) = if engine_first {
                let first = engine_call();
                (first, direct_call())
            } else {
                let first = direct_call();
                (engine_call(), first)
            };
            let mut staged = engine.clone();
            let start = Instant::now();
            let checked = staged.wallets[&holder].validated_index(&holder, EXPIRY).unwrap();
            let validate_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            let (changed, staged_portions) = checked.with_debited_origin(&id(1), 1).unwrap();
            let debit_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            assert!(changed.origin_tokens(&id(1)).next().is_some());
            let membership_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            staged.wallets.insert(holder.clone(), WalletPortfolio::from_indexed(changed).into());
            let publish_ns = start.elapsed().as_nanos();
            assert_eq!(bincode::serialize(&portions).unwrap(), bincode::serialize(&expected_portions).unwrap());
            assert_eq!(bincode::serialize(&portions).unwrap(), bincode::serialize(&staged_portions).unwrap());
            assert_eq!(*candidate.wallets[&holder], direct.to_portfolio());
            assert_eq!(candidate.wallets, staged.wallets);
            assert_eq!(engine.wallets[&holder].indexed().unwrap().get(&id(1)).unwrap().amount, 100);
            println!("indexed_engine_parts {{\"population\":{population},\"sample\":{sample},\"engine_first\":{engine_first},\"engine_ns\":{engine_ns},\"direct_ns\":{direct_ns},\"validate_ns\":{validate_ns},\"debit_ns\":{debit_ns},\"membership_ns\":{membership_ns},\"publish_ns\":{publish_ns}}}");
            println!("indexed_replacement_parts {{\"population\":{population},\"sample\":{sample},\"total_ns\":{total_ns},\"clone_ns\":{clone_ns},\"remove_active_ns\":{remove_active_ns},\"add_active_ns\":{add_active_ns},\"remove_membership_ns\":{remove_membership_ns},\"add_membership_ns\":{add_membership_ns},\"row_ns\":{row_ns}}}");
        }
    }
}


fn fragmented_owned_debit(count: u64) {
    use std::time::Instant;
    let (holder, mut portfolio) = fixture(count);
    for token in &mut portfolio.tokens { token.origin = id(1); }
    refresh(&mut portfolio);
    let root = IndexedPortfolio::from_portfolio(holder.clone(), &portfolio, EXPIRY).unwrap();
    for sample in 0..3 {
        // Compare repeated immutable removals with one batch debit. Both must
        // produce the same holdings and row deltas.
        let start = Instant::now();
        let mut repeated = root.clone();
        for token in root.origin_tokens(&id(1)) {
            repeated = repeated.with_removed(&token.id).unwrap();
        }
        let repeated_ns = start.elapsed().as_nanos();
        let start = Instant::now();
        let (actual, parts) = root.with_debited_origin(&id(1), count as u128).unwrap();
        let debit_ns = start.elapsed().as_nanos();
        assert_eq!(parts.iter().map(|p| p.amount).sum::<u128>(), count as u128);
        assert_eq!(actual.to_portfolio(), repeated.to_portfolio());
        assert!(actual.same_canonical_state(&repeated));
        compare(&root, &actual, &repeated.to_portfolio());
        assert_eq!(root.to_portfolio(), portfolio);
        println!("fragmented_owned_debit count={count} sample={sample} repeated_remove_ns={repeated_ns} debit_with_provenance_ns={debit_ns}");
    }
    let mut candidate = root.clone();
    assert!(candidate.remove_owned(&id(count + 1)).is_err());
    assert_eq!(candidate.to_portfolio(), portfolio);
}

#[test]
fn fragmented_owned_debit_preserves_retained_cut_and_row_deltas() {
    fragmented_owned_debit(128);
}

#[test]
#[ignore = "explicit capped-host fragmented debit ownership diagnostic; not signed payment or release throughput"]
fn fragmented_owned_debit_scaling() {
    for count in [1, 10, 100, 1000, 10000] { fragmented_owned_debit(count); }
}

fn governed_period_cost_case(count: u64, transition: &str) {
    use std::time::Instant;
    fn timed<T>(operation: impl FnOnce() -> T) -> (T, f64) {
        let start = Instant::now();
        let result = operation();
        (result, start.elapsed().as_secs_f64() * 1000.0)
    }
    // Reuse a valid holding template; this component fixture does not construct
    // an authenticated ledger or measure origin/graph admission.
    let (holder, mut portfolio) = fixture(1);
    let template = portfolio.tokens[0].clone();
    let baseline_now = Timestamp::new(EXPIRY + 100 + count / 2);
    portfolio.tokens = (1..=count).map(|n| {
        let mut token = template.clone();
        token.id = id(n);
        token.origin = id(n);
        token.origin_timestamp = Timestamp::new(100 + n);
        token.effective_origin_timestamp = token.origin_timestamp;
        if n % 5 == 0 {
            token.state = TrstState::Revoked;
            token.revoked_origin = Some(token.origin);
        } else if token.is_expired(baseline_now, EXPIRY) {
            token.state = TrstState::Expired;
        }
        token
    }).collect();
    refresh(&mut portfolio);
    let (root, construction_ms) = timed(||
        IndexedPortfolio::from_portfolio(holder.clone(), &portfolio, EXPIRY).unwrap());
    let (period, now) = match transition {
        "same" => (EXPIRY, baseline_now),
        "backward" => (EXPIRY, Timestamp::EPOCH),
        "shorten" => (0, baseline_now),
        "extend" => (u64::MAX, baseline_now),
        _ => panic!("unknown governed period diagnostic transition"),
    };
    let mut raw = crate::TrstEngine::with_expiry(EXPIRY);
    raw.wallets.insert(holder.clone(), portfolio.clone().into());
    assert!(raw.wallets[&holder].indexed().is_none());
    let (_, raw_ms) = timed(|| raw.set_expiry_period(period, now).unwrap());
    let (next, indexed_ms) = timed(|| root.with_expiry_period(period, now).unwrap());
    if std::env::var_os("BURST_PERIOD_STAGE_TIMING").is_some() {
        for _ in 0..2 {
            let repeated = root.with_expiry_period(period, now).unwrap();
            assert_eq!(repeated.to_portfolio(), next.to_portfolio());
        }
    }
    let expected = raw.get_portfolio(&holder).unwrap();
    assert_eq!(next.to_portfolio(), *expected);
    check_origin_indexes(&next, expected);
    assert_eq!(root.to_portfolio(), portfolio);
    let (bytes, serialization_ms) = timed(|| bincode::serialize(&next.to_portfolio()).unwrap());
    assert_eq!(bytes, bincode::serialize(expected).unwrap());
    let (restored, restoration_ms) = timed(|| {
        let decoded: WalletPortfolio = bincode::deserialize(&bytes).unwrap();
        IndexedPortfolio::from_portfolio(holder.clone(), &decoded, period).unwrap()
    });
    assert_eq!(restored.to_portfolio(), *expected);
    for at in [Timestamp::EPOCH, now, Timestamp::new(u64::MAX)] {
        assert_eq!(restored.counter_window(at), next.counter_window(at));
    }
    println!("governed_period_cost {{\"count\":{count},\"transition\":\"{transition}\",\"construction_ms\":{construction_ms},\"raw_ms\":{raw_ms},\"indexed_ms\":{indexed_ms},\"serialization_ms\":{serialization_ms},\"restoration_ms\":{restoration_ms},\"bytes\":{}}}", bytes.len());
}

#[test]
fn governed_period_cost_fixture_matches_raw_normalization_and_restoration() {
    for transition in ["same", "backward", "shorten", "extend"] {
        governed_period_cost_case(16, transition);
    }
}

#[test]
#[ignore = "explicit capped-host governed period stage diagnostic; not full node activation"]
fn governed_period_cost_scaling() {
    let count = std::env::var("BURST_PERIOD_PROBE_COUNT").unwrap().parse().unwrap();
    assert!([1000, 10000, 100000].contains(&count));
    let transition = std::env::var("BURST_PERIOD_PROBE_TRANSITION").unwrap();
    governed_period_cost_case(count, &transition);
}


#[test]
fn same_period_normalization_shares_unchanged_indexes_and_handles_mixed_transitions() {
    let (holder, portfolio) = fixture(32);
    let root = IndexedPortfolio::from_portfolio(holder.clone(), &portfolio, EXPIRY).unwrap();
    let unchanged = root.with_expiry_period(EXPIRY, Timestamp::EPOCH).unwrap();
    assert!(unchanged.rows.shares_storage_with(&root.rows));
    assert!(unchanged.active_times.ptr_eq(&root.active_times));
    assert!(unchanged.origin_balances.ptr_eq(&root.origin_balances));
    assert!(unchanged.counters.shares_storage_with(&root.counters));
    let expired = root.with_expiry_period(EXPIRY, Timestamp::new(u64::MAX)).unwrap();
    assert!(expired.counters.shares_storage_with(&root.counters));
    let restored = expired.with_expiry_period(EXPIRY, Timestamp::EPOCH).unwrap();
    assert!(restored.counters.shares_storage_with(&root.counters));
    assert_eq!(restored.to_portfolio(), portfolio);

    let (_, mut mixed) = fixture(2);
    mixed.tokens[0].amount = u128::MAX;
    mixed.tokens[0].state = TrstState::Expired;
    mixed.tokens[0].effective_origin_timestamp = Timestamp::new(200);
    mixed.tokens[0].origin_timestamp = Timestamp::new(200);
    mixed.tokens[1].amount = u128::MAX;
    mixed.tokens[1].effective_origin_timestamp = Timestamp::EPOCH;
    mixed.tokens[1].origin_timestamp = Timestamp::EPOCH;
    mixed.tokens.sort_by_key(|token| token.origin_timestamp);
    mixed.recompute_transferable(Timestamp::EPOCH, 100);
    mixed.recompute_earliest_expiry(100);
    let root = IndexedPortfolio::from_portfolio(holder.clone(), &mixed, 100).unwrap();
    let next = root.with_expiry_period(100, Timestamp::new(150)).unwrap();
    let mut raw = crate::TrstEngine::with_expiry(100);
    raw.wallets.insert(holder.clone(), mixed.clone().into());
    raw.set_expiry_period(100, Timestamp::new(150)).unwrap();
    assert_eq!(next.to_portfolio(), *raw.get_portfolio(&holder).unwrap());
    assert_eq!(next.cached_balance(), u128::MAX);
    assert_eq!(root.to_portfolio(), mixed);
    assert!(next.counters.shares_storage_with(&root.counters));
}


#[test]
fn governed_summary_rebuild_preserves_zero_active_entries_and_sparse_transitions() {
    let (holder, mut portfolio) = fixture(12);
    portfolio.tokens.last_mut().unwrap().amount = 0;
    refresh(&mut portfolio);
    let root = IndexedPortfolio::from_portfolio(holder.clone(), &portfolio, EXPIRY).unwrap();
    // One expiry favors incremental updates; eleven favor rebuilding the one
    // surviving active entry, which must remain indexed even with zero value.
    for now in [EXPIRY + 101, EXPIRY + 111, EXPIRY + 112] {
        let mut raw = crate::TrstEngine::with_expiry(EXPIRY);
        raw.wallets.insert(holder.clone(), portfolio.clone().into());
        raw.set_expiry_period(EXPIRY, Timestamp::new(now)).unwrap();
        let actual = root.with_expiry_period(EXPIRY, Timestamp::new(now)).unwrap();
        compare(&root, &actual, raw.get_portfolio(&holder).unwrap());
        assert!(actual.counters.shares_storage_with(&root.counters));
        if now == EXPIRY + 111 {
            assert_eq!(actual.cached_balance(), 0);
            assert_eq!(actual.cached_expiry(), Some(Timestamp::new(EXPIRY + 112)));
            assert_eq!(actual.active_times.len(), 1);
        }
    }
    assert_eq!(root.to_portfolio(), portfolio);
}

#[test]
fn revocation_shares_revision_history_across_raw_and_retained_holders() {
    use crate::merger_graph::REVISION_VISITS;
    let merge = id(900_000);
    let first = id(900_001);
    let target = id(900_002);
    let mut graph = MergerGraph::new();
    graph.record_merge(crate::MergeNode {
        merge_tx: merge, total_amount: 80,
        source_origins: vec![
            crate::MergeSource { origin: first, amount: 32 },
            crate::MergeSource { origin: target, amount: 48 },
        ],
        revoked_contribs: HashMap::new(),
    });
    for _ in 0..32 {
        graph.apply_revocation(first);
        graph.apply_restoration(&first);
    }
    let basis = graph.current_basis(&merge);
    for retained in [false, true] {
        let mut actual = crate::TrstEngine::with_expiry(EXPIRY);
        actual.merger_graph = graph.clone();
        let mut holders = Vec::new();
        for n in 0..8 {
            let holder = WalletAddress::new(format!("brst_history_holder_{n}"));
            let (_, mut portfolio) = fixture(1);
            let token = &mut portfolio.tokens[0];
            token.id = id(10_000 + n);
            token.holder = holder.clone();
            token.origin = merge;
            token.amount = 10;
            token.basis = basis;
            token.state = TrstState::Active;
            token.revoked_origin = None;
            refresh(&mut portfolio);
            let portfolio = if retained {
                WalletPortfolio::from_indexed(IndexedPortfolio::from_portfolio(
                    holder.clone(), &portfolio, EXPIRY).unwrap())
            } else { portfolio };
            actual.wallets.insert(holder.clone(), portfolio.into());
            holders.push(holder);
        }
        actual.rebuild_indexes();
        let mut reference = actual.clone();
        reference.merger_graph.apply_revocation(target);
        let mut expected_events = Vec::new();
        REVISION_VISITS.with(|p| p.set(Some(0)));
        for holder in &holders {
            reference.split_revoke_tokens(holder, &merge, target, 48, &mut expected_events);
        }
        let reference_visits = REVISION_VISITS.with(|p| p.replace(None).unwrap());
        REVISION_VISITS.with(|p| p.set(Some(0)));
        let (events, pending) = actual.revoke_origins(vec![target]);
        let actual_visits = REVISION_VISITS.with(|p| p.replace(None).unwrap());
        assert_eq!(reference_visits, 8 * 64);
        assert_eq!(actual_visits, 64);
        let fields = |events: Vec<crate::engine::RevocationEvent>| events.into_iter().map(|e|
            (e.holder, e.token_id, e.revoked_origin, e.revoked_amount, e.total_amount)).collect::<Vec<_>>();
        assert_eq!(fields(events), fields(expected_events));
        assert_eq!(actual.save_wallets(), reference.save_wallets());
        assert_eq!(actual.merger_graph.to_bytes(), reference.merger_graph.to_bytes());
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].origin, target);
        assert_eq!(pending[0].taints[&merge], (48, 80));
    }
}

#[test]
fn split_selector_skips_unrelated_single_bases_and_preserves_ties() {
    let (holder, mut portfolio, graph) = split_fixture();
    for (n, token) in portfolio.tokens.iter_mut().enumerate() {
        token.basis = if n % 3 == 0 { TrstBasis::Entire }
            else { TrstBasis::Single(id(100 + (n % 2) as u64)) };
    }
    portfolio.tokens.reverse();
    refresh(&mut portfolio);
    let root = IndexedPortfolio::from_portfolio(holder.clone(), &portfolio, EXPIRY).unwrap();
    for origin in [id(100), id(101), id(999)] {
        let expected: Vec<_> = portfolio.tokens.iter().filter(|token| {
            !matches!(token.basis, TrstBasis::Single(single) if single != origin)
        }).map(|token| token.id).collect();
        assert_eq!(root.split_tokens(id(900), origin).map(|token| token.id).collect::<Vec<_>>(), expected);
    }
    let before = root.to_portfolio();
    let (revoked, _, _) = root.with_split_revocation(&graph, &id(900), id(100), 2).unwrap();
    let (restored, _) = revoked.with_restored_origin(&id(100), Timestamp::new(100)).unwrap();
    for current in [&root, &revoked, &restored] {
        check_origin_indexes(current, &current.to_portfolio());
        let expected: Vec<_> = current.to_portfolio().tokens.iter().filter(|token| {
            token.revoked_origin.is_none() && token.origin == id(900)
                && !matches!(token.basis, TrstBasis::Single(single) if single != id(101))
        }).map(|token| token.id).collect();
        assert_eq!(current.split_tokens(id(900), id(101)).map(|token| token.id).collect::<Vec<_>>(), expected);
    }
    assert_eq!(root.to_portfolio(), before);
    let entire = portfolio.tokens.iter().find(|token| token.basis == TrstBasis::Entire).unwrap();
    let mut replacement = entire.clone();
    let mut revised_graph = graph.clone();
    replacement.basis = revised_graph.reduced_basis(&id(900), entire.basis, id(100), 1);
    let revised = root.with_replaced(replacement).unwrap();
    assert!(revised.split_members.ptr_eq(&root.split_members));
    assert!(revised.origins.ptr_eq(&root.origins));
    check_origin_indexes(&revised, &revised.to_portfolio());
    // Zero Single bases are invalid, rather than provably unrelated holdings.
    portfolio.tokens[0].basis = TrstBasis::Single(TxHash::ZERO);
    let invalid = IndexedPortfolio::from_portfolio(holder, &portfolio, EXPIRY).unwrap();
    assert!(invalid.with_split_revocation(&graph, &id(900), id(100), 2).is_err());
    assert_eq!(invalid.to_portfolio(), portfolio);
}
