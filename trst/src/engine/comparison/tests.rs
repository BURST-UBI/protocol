use super::*;

fn fixture() -> TrstEngine {
    let mut engine = TrstEngine::with_expiry(100);
    let holder = WalletAddress::new("brst_holder");
    for id in 1..=3 {
        let owner = WalletAddress::new(format!("brst_origin_{id}"));
        let token = engine.mint(TxHash::new([id; 32]), holder.clone(), id as u128 * 10,
            owner, Timestamp::new(100 + id as u64)).unwrap();
        engine.track_token(token);
    }
    engine.wallets.insert(WalletAddress::new("brst_empty"), WalletPortfolio::default().into());
    engine.revoke_by_originator(&WalletAddress::new("brst_origin_3"));
    for cause in [77, 78] {
        engine.invalidate_outcome_origins(BlockHash::new([cause; 32]),
            &[(WalletAddress::new("brst_origin_3"), TxHash::new([3; 32]))]).unwrap();
    }
    engine.set_expiry_period(100, Timestamp::new(202)).unwrap();
    engine
}

fn indexed(engine: TrstEngine) -> TrstEngine {
    let mut state = RestoredTrstState::from_engine(engine).unwrap();
    state.retain_portfolio_indexes().unwrap();
    state.into_engine()
}

fn oracle(a: &TrstEngine, b: &TrstEngine, period: u64, now: Timestamp) -> Result<bool, String> {
    let mut a = a.clone();
    let mut b = b.clone();
    a.set_expiry_period(period, now)?;
    b.set_expiry_period(period, now)?;
    Ok(a.wallet_snapshot_eq(&b))
}

#[test]
fn borrowed_comparison_matches_normalization_without_mutation() {
    let raw = fixture();
    for left in [raw.clone(), indexed(raw.clone())] {
        for right in [raw.clone(), indexed(raw.clone())] {
            let before = (left.save_wallets(), right.save_wallets());
            for period in [0, 50, 100, 200, u64::MAX] {
                for clock in [0, 100, 201, 202, 203, 303, u64::MAX] {
                    let now = Timestamp::new(clock);
                    assert_eq!(left.wallet_snapshot_eq_at_with_poll(&right, period, now, &|_| Ok(())),
                        oracle(&left, &right, period, now));
                }
            }
            assert_eq!((left.save_wallets(), right.save_wallets()), before);
        }
    }
}

#[test]
fn borrowed_comparison_checks_every_token_field_order_and_registry() {
    let before = fixture();
    let holder = WalletAddress::new("brst_holder");
    for field in 0..19 {
        let mut after = before.clone();
        match field {
            0 => after.wallets.get_mut(&holder).unwrap().tokens[0].basis = TrstBasis::Single(TxHash::new([99; 32])),
            1 => after.wallets.get_mut(&holder).unwrap().tokens[0].id = TxHash::new([99; 32]),
            2 => after.wallets.get_mut(&holder).unwrap().tokens[0].amount += 1,
            3 => after.wallets.get_mut(&holder).unwrap().tokens[0].origin = TxHash::new([99; 32]),
            4 => after.wallets.get_mut(&holder).unwrap().tokens[0].link = TxHash::new([99; 32]),
            5 => after.wallets.get_mut(&holder).unwrap().tokens[0].holder = WalletAddress::new("brst_changed"),
            6 => after.wallets.get_mut(&holder).unwrap().tokens[0].origin_timestamp = Timestamp::new(99),
            7 => after.wallets.get_mut(&holder).unwrap().tokens[0].effective_origin_timestamp = Timestamp::new(99),
            8 => after.wallets.get_mut(&holder).unwrap().tokens[0].state = TrstState::Revoked,
            9 => after.wallets.get_mut(&holder).unwrap().tokens[0].origin_wallet = WalletAddress::new("brst_changed"),
            10 => after.wallets.get_mut(&holder).unwrap().tokens[0].revoked_origin = Some(TxHash::new([99; 32])),
            11 => after.wallets.get_mut(&holder).unwrap().tokens.reverse(),
            12 => { after.wallets.remove(&WalletAddress::new("brst_empty")); },
            13 => { after.wallet_origins.clear(); },
            14 => { after.outcome_invalidations.insert(TxHash::new([99; 32]), OutcomeInvalidation {
                wallet_revoked: true, causes: HashSet::from([BlockHash::new([98; 32])]) }); },
            15 => {
                // Normalized comparison ignores the stored balance and deadline caches.
                let portfolio = after.wallets.get_mut(&holder).unwrap();
                portfolio.cached_transferable = 999;
                portfolio.earliest_expiry = Some(Timestamp::new(1));
            }
            16 => after.outcome_invalidations.get_mut(&TxHash::new([3; 32])).unwrap().wallet_revoked = false,
            17 => { after.outcome_invalidations.get_mut(&TxHash::new([3; 32])).unwrap().causes.remove(&BlockHash::new([77; 32])); },
            _ => after.wallets.get_mut(&holder).unwrap().tokens[0].state = TrstState::Active,
        }
        for clock in [0, 202, u64::MAX] {
            let now = Timestamp::new(clock);
            assert_eq!(before.wallet_snapshot_eq_at_with_poll(&after, 100, now, &|_| Ok(())),
                oracle(&before, &after, 100, now), "field={field} clock={clock}");
        }
    }
}

#[test]
fn borrowed_comparison_rejects_resurrection_overflow_even_for_shared_roots() {
    let mut engine = TrstEngine::with_expiry(1);
    let holder = WalletAddress::new("brst_holder");
    for id in 1..=2 {
        let mut token = engine.mint(TxHash::new([id; 32]), holder.clone(), u128::MAX,
            holder.clone(), Timestamp::new(100)).unwrap();
        token.state = TrstState::Expired;
        engine.track_token(token);
    }
    for engine in [engine.clone(), indexed(engine)] {
        let empty = TrstEngine::new();
        for pair in [(&engine, &engine), (&empty, &engine)] {
            assert_eq!(pair.0.wallet_snapshot_eq_at_with_poll(pair.1, 200, Timestamp::new(100), &|_| Ok(())),
                Err("TRST holding balance overflow".into()));
        }
    }
}

#[test]
fn borrowed_comparison_cancels_balance_and_ordered_walks_without_changes() {
    let left = fixture();
    let right = indexed(fixture());
    let original = (left.save_wallets(), right.save_wallets());
    let stages = std::cell::RefCell::new(std::collections::BTreeMap::new());
    assert!(left.wallet_snapshot_eq_at_with_poll(&right, 100, Timestamp::new(200), &|stage| {
        *stages.borrow_mut().entry(stage).or_insert(0usize) += 1; Ok(())
    }).unwrap());
    for (target, count) in stages.into_inner() {
        for stop in [1, count.min(2)] {
            let visits = std::cell::Cell::new(0);
            let result = left.wallet_snapshot_eq_at_with_poll(&right, 100, Timestamp::new(200), &|stage| {
                if stage == target {
                    visits.set(visits.get() + 1);
                    if visits.get() == stop { return Err("comparison stop".into()); }
                }
                Ok(())
            });
            assert_eq!(result, Err("comparison stop".into()));
            assert_eq!(visits.get(), stop);
        }
    }
    assert_eq!((left.save_wallets(), right.save_wallets()), original);
}
