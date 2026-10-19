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
            engine.revoke_by_origin(&address(1));
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
            engine.revoke_by_origin(&address(1));
        }
        assert_eq!(engine.transferable_balance(&address(5), time(120)), Some(0));
        let mut engine = restart(&engine);
        let restored = engine.un_revoke_by_origin(&address(1), time(130));
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
            engine.revoke_by_origin(&address(1));
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
        engine.un_revoke_by_origin(&address(1), time(150));
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
    engine.un_revoke_by_origin(&address(1), time(120));
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
    engine.set_expiry_period(10, time(140));
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
    engine.set_expiry_period(u64::MAX, time(160));
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
