use super::*;

#[test]
fn trst_merge_preserves_provenance() {
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(5000);
    let _expiry = 86400u64 * 365;

    let alice = make_address(70);
    let bob = make_address(71);

    let token1 = trst
        .mint(TxHash::new([1u8; 32]), bob.clone(), 100, alice.clone(), now)
        .unwrap();
    trst.track_token(token1.clone());

    let token2 = trst
        .mint(
            TxHash::new([2u8; 32]),
            bob.clone(),
            200,
            alice.clone(),
            Timestamp::new(6000),
        )
        .unwrap();
    trst.track_token(token2.clone());

    assert_eq!(trst.transferable_balance(&bob, now), Some(300));

    let merge_tx = TxHash::new([3u8; 32]);
    let merged = trst
        .merge(&[token1, token2], bob.clone(), merge_tx, now)
        .unwrap();

    assert_eq!(merged.amount, 300);
    assert_eq!(merged.holder, bob);
    assert_eq!(
        merged.origin, merge_tx,
        "merged token's origin is the merge transaction hash"
    );
    assert!(
        trst.merger_graph.get_merge(&merge_tx).is_some(),
        "merger graph records the merge's immediate inputs"
    );
    assert_eq!(
        merged.effective_origin_timestamp,
        Timestamp::new(5000),
        "merged token uses earliest origin timestamp"
    );
}

#[test]
fn trst_revocation_zeroes_originators_holdings() {
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(5000);
    let _expiry = 86400u64 * 365;

    let sybil = make_address(90);
    let bob = make_address(91);
    let carol = make_address(92);

    let t1 = trst
        .mint(
            TxHash::new([20u8; 32]),
            bob.clone(),
            500,
            sybil.clone(),
            now,
        )
        .unwrap();
    trst.track_token(t1);

    let t2 = trst
        .mint(
            TxHash::new([21u8; 32]),
            carol.clone(),
            300,
            sybil.clone(),
            now,
        )
        .unwrap();
    trst.track_token(t2);

    assert_eq!(trst.transferable_balance(&bob, now), Some(500));
    assert_eq!(trst.transferable_balance(&carol, now), Some(300));

    let _revocations = trst.revoke_by_originator(&sybil);

    assert_eq!(
        trst.transferable_balance(&bob, now),
        Some(0),
        "bob's balance should be 0 after sybil revocation"
    );
    assert_eq!(
        trst.transferable_balance(&carol, now),
        Some(0),
        "carol's balance should be 0 after sybil revocation"
    );
}

#[test]
fn trst_expiry_zeroes_old_tokens() {
    let expiry_secs = 1000u64;
    let mut trst = TrstEngine::with_expiry(expiry_secs);
    let mint_time = Timestamp::new(5000);

    let alice = make_address(100);
    let bob = make_address(101);

    let token = trst
        .mint(
            TxHash::new([30u8; 32]),
            bob.clone(),
            1000,
            alice.clone(),
            mint_time,
        )
        .unwrap();
    trst.track_token(token);

    let before_expiry = Timestamp::new(5500);
    assert!(
        trst.transferable_balance(&bob, before_expiry).unwrap() > 0,
        "token should be active before expiry"
    );

    let after_expiry = Timestamp::new(7000);
    assert_eq!(
        trst.transferable_balance(&bob, after_expiry),
        Some(0),
        "token should be expired"
    );
}

#[test]
fn trst_debit_saturates_at_zero() {
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(5000);
    let _expiry = 86400u64 * 365;

    let alice = make_address(160);
    let bob = make_address(161);

    let token = trst
        .mint(
            TxHash::new([40u8; 32]),
            bob.clone(),
            100,
            alice.clone(),
            now,
        )
        .unwrap();
    trst.track_token(token);

    assert_eq!(trst.transferable_balance(&bob, now), Some(100));

    let token_origin = TxHash::new([40u8; 32]);
    trst.debit_wallet(&bob, &token_origin, 100);
    assert_eq!(trst.transferable_balance(&bob, now), Some(0));

    trst.debit_wallet(&bob, &token_origin, 50);
    assert_eq!(
        trst.transferable_balance(&bob, now),
        Some(0),
        "overdraft should saturate to 0"
    );
}

#[test]
fn brn_balance_monotonically_increases_with_time() {
    let brn = BrnEngine::with_rate(100, Timestamp::new(0));
    let state = burst_brn::BrnWalletState::new(Timestamp::new(1000));

    let t1 = Timestamp::new(2000);
    let t2 = Timestamp::new(3000);
    let t3 = Timestamp::new(4000);

    let b1 = brn.compute_balance(&state, t1);
    let b2 = brn.compute_balance(&state, t2);
    let b3 = brn.compute_balance(&state, t3);

    assert!(b2 >= b1, "BRN should not decrease over time");
    assert!(b3 >= b2, "BRN should not decrease over time");
    assert!(b3 > b1, "BRN should increase over time with nonzero rate");
}

#[test]
fn trst_revoke_then_restore() {
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(5000);
    let _expiry = 86400u64 * 365;

    let origin_wallet = make_address(224);
    let holder = make_address(225);

    let token = trst
        .mint(
            TxHash::new([0x50; 32]),
            holder.clone(),
            1000,
            origin_wallet.clone(),
            now,
        )
        .unwrap();
    trst.track_token(token);

    assert_eq!(trst.transferable_balance(&holder, now), Some(1000));

    // Revoke all tokens from origin_wallet
    let _revoked = trst.revoke_by_originator(&origin_wallet);
    assert_eq!(
        trst.transferable_balance(&holder, now),
        Some(0),
        "balance should be 0 after revocation"
    );

    // Restore holdings derived from this originator, including other holders.
    let _unrevoked = trst.restore_by_originator(&origin_wallet, now).unwrap();
    assert_eq!(
        trst.transferable_balance(&holder, now),
        Some(1000),
        "balance should be restored after restoration"
    );
}

#[test]
fn brn_accrual_piecewise_rate_history() {
    use burst_brn::state::RateHistory;

    let mut history = RateHistory::new(100, Timestamp::new(0));
    history
        .apply_rate_change(200, Timestamp::new(5000))
        .unwrap();

    let verified_at = Timestamp::new(1000);
    let now = Timestamp::new(8000);

    // Expected: 100 * (5000-1000) + 200 * (8000-5000) = 400_000 + 600_000 = 1_000_000
    let accrued = history.total_accrued(verified_at, now);
    assert_eq!(accrued, 1_000_000);
}
