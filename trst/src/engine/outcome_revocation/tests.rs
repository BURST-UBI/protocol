use super::*;

fn address(n: u8) -> WalletAddress {
    WalletAddress::new(format!("brst_{n}"))
}
fn hash(n: u8) -> TxHash {
    TxHash::new([n; 32])
}
fn cause(n: u8) -> BlockHash {
    BlockHash::new([n; 32])
}
fn time(n: u64) -> Timestamp {
    Timestamp::new(n)
}

fn restart(engine: &TrstEngine) -> TrstEngine {
    let bytes = engine.save_wallets();
    let mut restored = TrstEngine::load_wallets(&bytes, engine.expiry_secs).unwrap();
    restored.merger_graph = MergerGraph::from_bytes(&engine.merger_graph.to_bytes()).unwrap();
    restored.validate_bases().unwrap();
    assert_eq!(restored.save_wallets(), bytes);
    restored
}

fn merged() -> (TrstEngine, TrstToken) {
    let mut engine = TrstEngine::new();
    let a = engine
        .mint(hash(1), address(5), 40, address(1), time(100))
        .unwrap();
    let b = engine
        .mint(hash(2), address(5), 60, address(1), time(100))
        .unwrap();
    engine.track_token(a.clone());
    engine.track_token(b.clone());
    let merged = engine
        .merge(&[a, b], address(5), hash(3), time(110))
        .unwrap();
    engine.bulk_untrack(&address(5), &HashSet::from([hash(1), hash(2)]));
    engine.track_token(merged.clone());
    (engine, merged)
}

#[test]
fn reward_invalidation_and_identity_revocation_are_independent_in_both_orders() {
    for identity_first in [false, true] {
        let (mut engine, _) = merged();
        let origins = [(address(1), hash(1))];
        assert!(engine.save_wallets().starts_with(&SNAPSHOT_MAGIC));
        if identity_first {
            engine.revoke_by_originator(&address(1));
        }
        let (events, _) = engine
            .invalidate_outcome_origins(cause(7), &origins)
            .unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.revoked_amount)
                .sum::<u128>(),
            if identity_first { 0 } else { 40 }
        );
        assert!(engine.save_wallets().starts_with(&OUTCOME_SNAPSHOT_MAGIC));
        let mut engine = restart(&engine);
        if !identity_first {
            engine.revoke_by_originator(&address(1));
        }
        assert_eq!(engine.transferable_balance(&address(5), time(120)), Some(0));
        let mut engine = restart(&engine);
        let restored = engine
            .restore_by_originator(&address(1), time(130))
            .unwrap();
        assert_eq!(restored.iter().map(|event| event.amount).sum::<u128>(), 60);
        assert_eq!(
            engine.transferable_balance(&address(5), time(130)),
            Some(60)
        );
        assert!(engine.merger_graph.is_origin_revoked(&hash(1)));
        let mut engine = restart(&engine);
        let restored = engine
            .restore_outcome_origins(cause(7), &origins, time(140))
            .unwrap();
        assert_eq!(restored.iter().map(|event| event.amount).sum::<u128>(), 40);
        assert_eq!(
            engine.transferable_balance(&address(5), time(140)),
            Some(100)
        );
        assert!(engine.save_wallets().starts_with(&SNAPSHOT_MAGIC));
    }
}

#[test]
fn removing_outcome_cause_does_not_clear_identity_or_another_outcome() {
    for keep_identity in [false, true] {
        let (mut engine, _) = merged();
        let origins = [(address(1), hash(1))];
        engine
            .invalidate_outcome_origins(cause(7), &origins)
            .unwrap();
        let (duplicate, pending) = engine
            .invalidate_outcome_origins(cause(7), &origins)
            .unwrap();
        assert!(duplicate.is_empty() && pending.is_empty());
        engine
            .invalidate_outcome_origins(cause(8), &origins)
            .unwrap();
        if keep_identity {
            engine.revoke_by_originator(&address(1));
        }
        let mut engine = restart(&engine);
        for id in [9, 7, 7] {
            assert!(engine
                .restore_outcome_origins(cause(id), &origins, time(130))
                .unwrap()
                .is_empty());
            assert!(engine.merger_graph.is_origin_revoked(&hash(1)));
        }
        let restored = engine
            .restore_outcome_origins(cause(8), &origins, time(140))
            .unwrap();
        assert_eq!(
            restored.iter().map(|event| event.amount).sum::<u128>(),
            if keep_identity { 0 } else { 40 }
        );
        assert_eq!(
            engine.merger_graph.is_origin_revoked(&hash(1)),
            keep_identity
        );
        let mut engine = restart(&engine);
        engine
            .restore_by_originator(&address(1), time(150))
            .unwrap();
        assert_eq!(
            engine.transferable_balance(&address(5), time(150)),
            Some(100)
        );
    }
}

#[test]
fn pending_merged_rewards_keep_their_cut_through_delivery_and_expiry() {
    let (mut engine, merged) = merged();
    let origin = merged.origin;
    engine.debit_wallet_with_provenance(&address(5), &origin, 100);
    let origins = [(address(1), hash(1))];
    let (held, pending) = engine
        .invalidate_outcome_origins(cause(7), &origins)
        .unwrap();
    assert!(held.is_empty());
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].cut(&origin, 100), 40);
    let mut engine = restart(&engine);
    engine
        .restore_by_originator(&address(1), time(120))
        .unwrap();
    let revoked = engine.receive_token(merged, time(130));
    assert_eq!(
        revoked
            .iter()
            .map(|event| event.revoked_amount)
            .sum::<u128>(),
        40
    );
    assert_eq!(
        engine.transferable_balance(&address(5), time(130)),
        Some(60)
    );
    engine.set_expiry_period(10, time(140)).unwrap();
    let mut engine = restart(&engine);
    engine
        .restore_outcome_origins(cause(7), &origins, time(150))
        .unwrap();
    assert_eq!(engine.transferable_balance(&address(5), time(150)), Some(0));
    assert!(engine
        .get_portfolio(&address(5))
        .unwrap()
        .tokens
        .iter()
        .all(|token| token.state == TrstState::Expired));
    engine.set_expiry_period(u64::MAX, time(160)).unwrap();
    assert_eq!(
        engine.transferable_balance(&address(5), time(160)),
        Some(100)
    );
}

#[test]
fn invalid_requests_are_atomic_and_cause_snapshots_are_checked() {
    let (mut engine, _) = merged();
    let original = engine.save_wallets();
    let original_graph = engine.merger_graph.to_bytes();
    for request in [
        vec![],
        vec![(address(2), hash(1))],
        vec![(address(1), hash(1)), (address(1), hash(9))],
        vec![(address(1), hash(1)), (address(1), hash(1))],
    ] {
        assert!(engine
            .invalidate_outcome_origins(cause(7), &request)
            .is_err());
        assert!(engine
            .restore_outcome_origins(cause(7), &request, time(130))
            .is_err());
        assert_eq!(engine.save_wallets(), original);
        assert_eq!(engine.merger_graph.to_bytes(), original_graph);
    }
    assert!(engine
        .invalidate_outcome_origins(BlockHash::ZERO, &[(address(1), hash(1))])
        .is_err());
    engine
        .invalidate_outcome_origins(cause(7), &[(address(1), hash(1))])
        .unwrap();
    let saved = engine.save_wallets();
    let decoded: OutcomeWalletSnapshot = bincode::deserialize(&saved).unwrap();
    for (origin, causes) in [
        (hash(1), vec![]),
        (hash(9), vec![cause(7)]),
        (hash(1), vec![BlockHash::ZERO]),
        (hash(1), vec![cause(7), cause(7)]),
    ] {
        let bytes = bincode::serialize(&(
            OUTCOME_SNAPSHOT_MAGIC,
            &decoded.wallets,
            &decoded.origins,
            vec![(origin, false, causes)],
        ))
        .unwrap();
        assert!(TrstEngine::load_wallets(&bytes, u64::MAX).is_err());
    }
    let mut inconsistent = TrstEngine::load_wallets(&saved, engine.expiry_secs).unwrap();
    inconsistent.merger_graph = MergerGraph::new();
    assert!(inconsistent.validate_bases().is_err());
}

#[test]
fn ordered_origin_snapshot_matches_sorted_reference_with_overlapping_holds() {
    for held in [false, true] {
        let (mut engine, _) = merged();
        // Reverse insertion and shared ownership exercise canonical origin order.
        for n in (10..140).rev() {
            let token = engine
                .mint(hash(n), address(5), 1, address(1), time(100))
                .unwrap();
            engine.track_token(token);
        }
        if held {
            engine
                .invalidate_outcome_origins(cause(9), &[(address(1), hash(1))])
                .unwrap();
        }
        let original = engine.clone();
        let mut reference = engine.clone();
        let mut ordered: Vec<_> = reference
            .wallet_origins
            .get(&address(1))
            .unwrap()
            .iter()
            .copied()
            .collect();
        ordered.sort_by_key(|origin| *origin.as_bytes());
        for origin in &ordered {
            if let Some(hold) = reference.outcome_invalidations.get_mut(origin) {
                hold.wallet_revoked = true;
            }
        }
        let (expected_events, expected_pending) = reference.revoke_origins(ordered.clone());
        let (events, pending) = engine.revoke_with_pending_effects(&address(1));
        let images = |events: &[RevocationEvent]| {
            events
                .iter()
                .map(|e| {
                    (
                        e.holder.clone(),
                        e.token_id,
                        e.revoked_origin,
                        e.revoked_amount,
                        e.total_amount,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(images(&events), images(&expected_events));
        assert_eq!(
            pending
                .iter()
                .map(|p| (p.origin, &p.taints))
                .collect::<Vec<_>>(),
            expected_pending
                .iter()
                .map(|p| (p.origin, &p.taints))
                .collect::<Vec<_>>()
        );
        assert_eq!(engine.save_wallets(), reference.save_wallets());
        assert_eq!(
            engine.merger_graph.to_bytes(),
            reference.merger_graph.to_bytes()
        );
        assert!(engine
            .wallet_origins
            .shares_storage_with(&original.wallet_origins));
        ordered.retain(|origin| {
            if let Some(hold) = reference.outcome_invalidations.get_mut(origin) {
                hold.wallet_revoked = false;
                false
            } else {
                true
            }
        });
        let expected = reference.restore_origins(ordered, time(130)).unwrap();
        let actual = engine
            .restore_by_originator(&address(1), time(130))
            .unwrap();
        let images = |events: &[RestorationEvent]| {
            events
                .iter()
                .map(|e| (e.holder.clone(), e.token_id, e.amount))
                .collect::<Vec<_>>()
        };
        assert_eq!(images(&actual), images(&expected));
        assert_eq!(engine.save_wallets(), reference.save_wallets());
        assert_eq!(
            engine.merger_graph.to_bytes(),
            reference.merger_graph.to_bytes()
        );
        assert!(engine
            .wallet_origins
            .shares_storage_with(&original.wallet_origins));
        if held {
            engine
                .restore_outcome_origins(cause(9), &[(address(1), hash(1))], time(140))
                .unwrap();
            reference
                .restore_outcome_origins(cause(9), &[(address(1), hash(1))], time(140))
                .unwrap();
            assert_eq!(engine.save_wallets(), reference.save_wallets());
            assert_eq!(
                engine.merger_graph.to_bytes(),
                reference.merger_graph.to_bytes()
            );
        }
    }
}

#[test]
#[ignore = "explicit capped-host overlapping-origin mutation phase diagnostic; not release throughput"]
fn overlapping_origin_revocation_phase_cost() {
    use std::time::Instant;
    let id = |n: u64| {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&n.to_be_bytes());
        TxHash::new(bytes)
    };
    let holder = address(5);
    let victim = address(1);
    let population = 1000u64;
    for origins in [8u64, 32] {
        // Each merged holding shares every tested burn origin. Phase probes
        // measure in-memory revocation; storage and network work are excluded.
        let mut engine = TrstEngine::with_expiry(100);
        let mut parents = Vec::new();
        for origin in 1..=origins + 1 {
            let (amount, owner) = if origin <= origins {
                (7, victim.clone())
            } else {
                (11, holder.clone())
            };
            parents.push(
                engine
                    .mint(
                        id(origin),
                        holder.clone(),
                        amount * population as u128,
                        owner,
                        time(100),
                    )
                    .unwrap(),
            );
        }
        let mut next_id = origins + 2;
        for n in 0..population {
            let mut inputs = Vec::new();
            let mut remaining = Vec::new();
            for (index, parent) in parents.into_iter().enumerate() {
                let amount = if index < origins as usize { 7 } else { 11 };
                let (input, change) = engine
                    .transfer(
                        &parent,
                        &holder,
                        holder.clone(),
                        amount,
                        id(next_id),
                        id(next_id + 1),
                        time(100),
                    )
                    .unwrap();
                next_id += 2;
                inputs.push(input);
                if n + 1 < population {
                    remaining.push(change.unwrap());
                } else {
                    assert!(change.is_none());
                }
            }
            assert_eq!(inputs.len(), origins as usize + 1);
            let merged = engine
                .merge(&inputs, holder.clone(), id(next_id), time(100))
                .unwrap();
            next_id += 1;
            engine.track_token(merged);
            parents = remaining;
        }
        assert!(parents.is_empty());
        let mut retained = RestoredTrstState::from_engine(engine).unwrap();
        retained.retain_portfolio_indexes().unwrap();
        let mut engine = retained.into_engine();
        for sample in 0..3 {
            let before = engine.clone();
            let before_bytes = before.save_wallets();
            REVOCATION_PHASES.with(|slot| {
                assert!(slot.borrow().is_none());
                *slot.borrow_mut() = Some(Vec::new());
            });
            SPLIT_PHASES.with(|slot| {
                assert!(slot.borrow().is_none());
                *slot.borrow_mut() = Some([0; 6]);
            });
            let started = Instant::now();
            let events = engine.revoke_by_originator(&victim);
            let total_ns = started.elapsed().as_nanos();
            let phases = REVOCATION_PHASES.with(|slot| slot.borrow_mut().take().unwrap());
            let split = SPLIT_PHASES.with(|slot| slot.borrow_mut().take().unwrap());
            assert!(split[..4].iter().sum::<u128>() <= split[4]);
            assert_eq!(phases.len(), origins as usize);
            let totals = phases.iter().fold([0u128; 4], |mut totals, phase| {
                for i in 0..4 {
                    totals[i] += phase[i];
                }
                totals
            });
            assert!(totals.iter().sum::<u128>() <= total_ns);
            assert!(split[4] + split[5] <= totals[2]);
            assert!(
                split[4] > 0 && split[5] > 0,
                "retained split path must be measured"
            );
            assert_eq!(events.len(), (origins * population) as usize);
            assert!(events.iter().all(|event| event.revoked_amount == 7));
            assert_eq!(
                engine.transferable_balance_snapshot(&holder),
                Some(11 * population as u128)
            );
            assert_eq!(before.save_wallets(), before_bytes);
            let raw = restart(&engine);
            assert_eq!(raw.merger_graph.to_bytes(), engine.merger_graph.to_bytes());
            assert_eq!(
                raw.transferable_balance_snapshot(&holder),
                Some(11 * population as u128)
            );
            let restored = engine.restore_by_originator(&victim, time(100)).unwrap();
            assert_eq!(restored.len(), events.len());
            assert!(restored.iter().all(|event| event.amount == 7));
            assert_eq!(
                engine.transferable_balance_snapshot(&holder),
                Some((7 * origins as u128 + 11) * population as u128)
            );
            let raw = restart(&engine);
            assert_eq!(raw.merger_graph.to_bytes(), engine.merger_graph.to_bytes());
            println!("overlap_mutation_phases origins={origins} population={population} sample={sample} total_ns={total_ns} graph_ns={} whole_ns={} split_ns={} pending_ns={}",
                totals[0], totals[1], totals[2], totals[3]);
            println!("overlap_split_phases origins={origins} population={population} sample={sample} basis_ns={} revision_ns={} replace_ns={} insert_ns={} helper_ns={} publish_ns={}",
                split[0], split[1], split[2], split[3], split[4], split[5]);
        }
    }
}
