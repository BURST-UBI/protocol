use super::*;

#[test]
fn component_lifecycle_accounting_simulation() {
    use burst_store::pending::{PendingInfo, PendingProvenance, PendingStore};
    use burst_types::ProtocolParams;

    let params = ProtocolParams {
        endorsement_threshold: 3,
        endorsement_burn_amount: 1000,
        num_verifiers: 3,
        verification_threshold_bps: 6000,
        verifier_stake_amount: 100,
        challenge_stake_amount: 1000,
        max_revotes: 3,
        ..ProtocolParams::burst_defaults()
    };

    let mut brn = BrnEngine::with_rate(100, Timestamp::new(0)); // 100 raw/sec
    let mut trst = TrstEngine::with_expiry(10_000);
    let (_dir, env) = temp_env();
    let pending_store = env.pending_store();
    let mut verification = burst_verification::VerificationOrchestrator::new();

    let alice = make_address(240); // consumer
    let bob = make_address(241); // provider
    let carol = make_address(242); // merchant + challenger
    let sybil = make_address(243); // fraud

    for w in [&alice, &bob, &carol, &sybil] {
        brn.track_wallet(
            (*w).clone(),
            burst_brn::BrnWalletState::new(Timestamp::new(1000)),
        );
    }
    let t2000 = Timestamp::new(2000);
    let wallet_state = brn.get_wallet(&alice).unwrap().clone();
    assert_eq!(brn.compute_balance(&wallet_state, t2000), 100_000); // 100/s × 1000s

    // Alice's open block: zero balances (birthright is computed, not claimed).
    let alice_open = make_block(
        BlockType::Open,
        &alice,
        BlockHash::ZERO,
        &alice,
        0,
        0,
        BlockHash::ZERO,
        TxHash::ZERO,
        1500,
    );
    assert!(burst_node::BlockProcessor::validate_open_block(&alice_open, None).is_ok());

    // Burn 600 to Bob: odometer 0 → 600, spend covered by computed BRN(w).
    let burn = make_block(
        BlockType::Burn,
        &alice,
        alice_open.hash,
        &alice,
        600,
        0,
        BlockHash::new(pubkey_bytes(&bob)),
        TxHash::ZERO,
        2000,
    );
    assert!(burst_node::BlockProcessor::validate_balance_transition(&burn, 0, 0).is_ok());
    assert!(600 <= brn.compute_balance(&wallet_state, t2000));
    let econ = burst_node::process_block_economics(&burn, &mut brn, &mut trst, t2000, 10_000, 0);
    let minted = match &econ {
        burst_node::EconomicResult::BurnAndMint {
            burn_amount,
            mint_token,
        } => {
            assert_eq!(*burn_amount, 600);
            mint_token.clone().unwrap()
        }
        other => panic!("expected BurnAndMint, got {other:?}"),
    };
    assert_eq!(brn.get_wallet(&alice).unwrap().total_burned, 600);
    // Minting creates a pending claim; Bob must receive it into his portfolio.
    assert!(trst.get_portfolio(&bob).is_none());
    let burn_hash = TxHash::new(*burn.hash.as_bytes());
    pending_store
        .put_pending(
            &bob,
            &burn_hash,
            &PendingInfo {
                return_credit: false,
                source: alice.clone(),
                amount: minted.amount,
                timestamp: t2000,
                provenance: vec![PendingProvenance {
                    basis: Default::default(),
                    revoked_amounts: Default::default(),
                    amount: minted.amount,
                    origin: minted.origin,
                    origin_wallet: minted.origin_wallet.clone(),
                    origin_timestamp: minted.origin_timestamp,
                    effective_origin_timestamp: minted.effective_origin_timestamp,
                }],
            },
        )
        .unwrap();

    // Bob receives: claimed amount must match the pending entry exactly.
    let bob_open = make_block(
        BlockType::Open,
        &bob,
        BlockHash::ZERO,
        &bob,
        0,
        600,
        BlockHash::new(*burn_hash.as_bytes()),
        TxHash::ZERO,
        2100,
    );
    let pending = pending_store.get_pending(&bob, &burn_hash).unwrap();
    assert!(
        burst_node::BlockProcessor::validate_open_block(&bob_open, Some(pending.amount)).is_ok()
    );
    let bob_token = burst_node::block_economics::create_received_token(&bob_open, &pending, 10_000);
    trst.receive_token(bob_token, Timestamp::new(2100));
    pending_store.delete_pending(&bob, &burn_hash).unwrap();
    assert_eq!(
        trst.transferable_balance(&bob, Timestamp::new(2100)),
        Some(600)
    );

    let origin_a = minted.origin;
    assert_eq!(
        trst.origin_transferable(&bob, &origin_a, Timestamp::new(2200)),
        600
    );
    let provenance = trst.debit_wallet_with_provenance(&bob, &origin_a, 200);
    assert_eq!(provenance[0].amount, 200);
    let carol_recv = burst_trst::TrstToken {
        basis: Default::default(),
        id: test_hash(1),
        amount: 200,
        origin: provenance[0].origin,
        link: test_hash(1),
        holder: carol.clone(),
        origin_timestamp: provenance[0].origin_timestamp,
        effective_origin_timestamp: provenance[0].effective_origin_timestamp,
        state: burst_types::TrstState::Active,
        origin_wallet: provenance[0].origin_wallet.clone(),
        revoked_origin: None,
    };
    trst.receive_token(carol_recv, Timestamp::new(2200));

    // Sybil (wrongly verified) burns 400 to Bob.
    let sybil_burn = make_block(
        BlockType::Burn,
        &sybil,
        BlockHash::new([8u8; 32]),
        &sybil,
        400,
        0,
        BlockHash::new(pubkey_bytes(&bob)),
        TxHash::ZERO,
        2500,
    );
    let econ = burst_node::process_block_economics(
        &sybil_burn,
        &mut brn,
        &mut trst,
        Timestamp::new(2500),
        10_000,
        0,
    );
    let sybil_token = match &econ {
        burst_node::EconomicResult::BurnAndMint { mint_token, .. } => mint_token.clone().unwrap(),
        other => panic!("expected BurnAndMint, got {other:?}"),
    };
    trst.receive_token(sybil_token.clone(), Timestamp::new(2600));

    // Bob merges clean 400 (origin A) + tainted 400 (origin S) → M(800).
    let inputs: Vec<burst_trst::TrstToken> = trst
        .get_portfolio(&bob)
        .unwrap()
        .tokens
        .iter()
        .filter(|t| t.state == burst_types::TrstState::Active)
        .cloned()
        .collect();
    assert_eq!(inputs.len(), 2);
    let merge_tx = test_hash(2);
    let merged = trst
        .merge(&inputs, bob.clone(), merge_tx, Timestamp::new(2700))
        .unwrap();
    assert_eq!(merged.origin, merge_tx);
    let ids: std::collections::HashSet<TxHash> = inputs.iter().map(|t| t.id).collect();
    trst.bulk_untrack(&bob, &ids);
    trst.track_token(merged);

    // Bob sends 300 of M to Carol — left IN FLIGHT during the revocation.
    let merged_provenance = trst.debit_wallet_with_provenance(&bob, &merge_tx, 300);
    assert_eq!(merged_provenance[0].amount, 300);

    // Verify sybil in the orchestrator (endorse + vote) so it can be challenged.
    for i in 0..3 {
        verification
            .process_endorsement(&sybil, &make_address(210 + i), 1000, &params)
            .unwrap();
    }
    let verifier_candidates: Vec<_> = (0u8..3).map(|i| make_address(214 + i)).collect();
    let selected_verifiers = verification
        .select_verifiers(&sybil, &verifier_candidates, &[1u8; 32], &params)
        .unwrap();
    for v in &selected_verifiers {
        verification
            .process_vote(&sybil, v, burst_verification::Vote::Legitimate, &params)
            .unwrap();
    }
    verification.drain_events();

    // Exercise challenge economics directly, without node admission.
    let challenge = make_block(
        BlockType::Challenge,
        &carol,
        BlockHash::new([7u8; 32]),
        &carol,
        1000,
        0,
        BlockHash::new(pubkey_bytes(&sybil)),
        TxHash::ZERO,
        3000,
    );
    let econ = burst_node::process_block_economics(
        &challenge,
        &mut brn,
        &mut trst,
        Timestamp::new(3000),
        10_000,
        0,
    );
    assert!(matches!(
        econ,
        burst_node::EconomicResult::Challenge {
            stake_amount: 1000,
            ..
        }
    ));
    assert_eq!(brn.get_wallet(&carol).unwrap().total_staked, 1000);
    verification
        .initiate_challenge(
            &sybil,
            &carol,
            true,
            1000,
            burst_verification::ChallengeReason::Fraud,
            &params,
        )
        .unwrap();

    // Assign the fixture panel directly; its last vote emits the component outcome.
    let challenge_verifiers: Vec<_> = (0u8..3).map(|i| make_address(220 + i)).collect();
    for cv in &challenge_verifiers {
        brn.track_wallet(
            cv.clone(),
            burst_brn::BrnWalletState::new(Timestamp::new(1000)),
        );
        brn.get_wallet_mut(cv).unwrap().total_staked = params.verifier_stake_amount;
    }
    let selected_verifiers = verification
        .select_verifiers(&sybil, &challenge_verifiers, &[2u8; 32], &params)
        .unwrap();
    for (i, v) in selected_verifiers.iter().enumerate() {
        verification
            .process_vote(&sybil, v, burst_verification::Vote::Illegitimate, &params)
            .unwrap();
        if i + 1 < selected_verifiers.len() {
            assert!(verification
                .try_resolve_challenge(&sybil, &params)
                .unwrap()
                .is_none());
        }
    }
    let resolved = verification
        .try_resolve_challenge(&sybil, &params)
        .unwrap()
        .unwrap();
    let events = verification.drain_events();
    assert!(events.iter().any(|e| matches!(e,
        burst_verification::VerificationEvent::WalletUnverified { wallet } if *wallet == sybil)));

    // Apply the component fraud outcome directly to the TRST engine.
    let revocations = trst.revoke_by_originator(&sybil);
    let revoked_total: u128 = revocations.iter().map(|r| r.revoked_amount).sum();
    // Bob holds 500 of M (800 total, 400 tainted): ceil(500·400/800) = 250.
    assert_eq!(revoked_total, 250);
    assert_eq!(
        trst.transferable_balance(&bob, Timestamp::new(3100)),
        Some(250)
    );

    // Apply the component simulation's accounting model.
    if let burst_verification::VerificationEvent::ChallengeResolved { outcome, .. } = &resolved {
        assert_eq!(
            outcome.outcome,
            burst_verification::ChallengeResult::FraudConfirmed
        );
        // Challenger stake returned in full.
        let challenger_wallet = brn.get_wallet_mut(&carol).unwrap();
        challenger_wallet.total_staked = challenger_wallet
            .total_staked
            .saturating_sub(outcome.challenger_stake);
        // Panel: unanimous → no dissenters → stakes unlocked, no TRST minted.
        lifecycle_simulation::resolve_verifier_outcomes(
            &mut brn,
            &pending_store,
            &outcome.verifier_outcomes,
            &sybil,
            Timestamp::new(3100),
        );
        for cv in &challenge_verifiers {
            assert_eq!(brn.get_wallet(cv).unwrap().total_staked, 0);
            assert_eq!(brn.get_wallet(cv).unwrap().total_burned, 0);
        }
        // Fixture bounty: min(configured share of revoked holdings, cap).
        let reward = std::cmp::min(
            revoked_total.saturating_mul(params.challenge_reward_bps as u128) / 10_000,
            params.challenge_reward_cap,
        );
        assert_eq!(reward, 2); // 1% of 250, integer math
        let reward_hash = lifecycle_simulation::create_reward_pending(
            &pending_store,
            &carol,
            &sybil,
            b"challenger-reward",
            reward,
            Timestamp::new(3100),
        )
        .unwrap();
        let pending = pending_store.get_pending(&carol, &reward_hash).unwrap();
        let reward_block = make_block(
            BlockType::Receive,
            &carol,
            BlockHash::new([6u8; 32]),
            &carol,
            0,
            0,
            BlockHash::new(*reward_hash.as_bytes()),
            TxHash::ZERO,
            3200,
        );
        let reward_token =
            burst_node::block_economics::create_received_token(&reward_block, &pending, 10_000);
        trst.receive_token(reward_token, Timestamp::new(3200));
        pending_store.delete_pending(&carol, &reward_hash).unwrap();
    } else {
        panic!("expected ChallengeResolved");
    }
    assert_eq!(brn.get_wallet(&carol).unwrap().total_staked, 0);

    // The in-flight 300 of M arrives at Carol — revocation caught at receive:
    // ceil(300·400/800) = 150 revoked, 150 stays live.
    let inflight = burst_trst::TrstToken {
        basis: Default::default(),
        id: test_hash(3),
        amount: 300,
        origin: merge_tx,
        link: test_hash(3),
        holder: carol.clone(),
        origin_timestamp: merged_provenance[0].origin_timestamp,
        effective_origin_timestamp: merged_provenance[0].effective_origin_timestamp,
        state: burst_types::TrstState::Active,
        origin_wallet: merged_provenance[0].origin_wallet.clone(),
        revoked_origin: None,
    };
    let receive_revocations = trst.receive_token(inflight, Timestamp::new(3300));
    assert_eq!(receive_revocations.len(), 1);
    assert_eq!(receive_revocations[0].revoked_amount, 150);
    // Carol live: 200 (origin A) + 150 (clean M) + 2 (reward) = 352.
    assert_eq!(
        trst.transferable_balance(&carol, Timestamp::new(3300)),
        Some(352)
    );

    // Check this fixture's tracked total, including revoked holdings.
    // 600 (alice burn) + 400 (sybil burn) + 2 (reward, backed by revoked) = 1002.
    let total_tracked: u128 = [&alice, &bob, &carol, &sybil]
        .iter()
        .filter_map(|w| trst.get_portfolio(w))
        .flat_map(|p| p.tokens.iter())
        .map(|t| t.amount)
        .sum();
    assert_eq!(total_tracked, 1002);

    let restored = trst
        .restore_by_originator(&sybil, Timestamp::new(3400))
        .unwrap();
    assert_eq!(restored.len(), 2); // bob's 250 chunk + carol's 150 chunk
    assert_eq!(
        trst.transferable_balance(&bob, Timestamp::new(3400)),
        Some(500)
    );
    assert_eq!(
        trst.transferable_balance(&carol, Timestamp::new(3400)),
        Some(502)
    );

    // M's effective timestamp is its earliest constituent (t=2000) → expires
    // at 12000. The reward token (t=3100) survives until 13100.
    trst.flush_all_expired(Timestamp::new(12_500));
    assert_eq!(
        trst.transferable_balance(&bob, Timestamp::new(12_500)),
        Some(0)
    );
    assert_eq!(
        trst.transferable_balance(&carol, Timestamp::new(12_500)),
        Some(2)
    );
    // Governance extends the expiry period — expired TRST becomes money again.
    trst.set_expiry_period(1_000_000, Timestamp::new(12_500))
        .unwrap();
    assert_eq!(
        trst.transferable_balance(&bob, Timestamp::new(12_500)),
        Some(500)
    );
    assert_eq!(
        trst.transferable_balance(&carol, Timestamp::new(12_500)),
        Some(502)
    );

    // Conservation still holds after the whole story.
    let total_tracked: u128 = [&alice, &bob, &carol, &sybil]
        .iter()
        .filter_map(|w| trst.get_portfolio(w))
        .flat_map(|p| p.tokens.iter())
        .map(|t| t.amount)
        .sum();
    assert_eq!(total_tracked, 1002);
}
