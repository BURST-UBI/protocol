use super::*;

#[test]
fn endorsement_burns_brn_correctly() {
    let mut brn = BrnEngine::with_rate(100, Timestamp::new(0));
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(10000);

    let endorser = make_address(196);
    let target = make_address(197);

    brn.track_wallet(
        endorser.clone(),
        burst_brn::BrnWalletState::new(Timestamp::new(0)),
    );

    let endorse_block = make_block(
        BlockType::Endorse,
        &endorser,
        BlockHash::new([1u8; 32]),
        &endorser,
        1300, // ascending odometer: prev 1000 spent + 300 burned
        0,
        BlockHash::new(pubkey_bytes(&target)),
        TxHash::ZERO,
        now.as_secs(),
    );

    let result = burst_node::process_block_economics(
        &endorse_block,
        &mut brn,
        &mut trst,
        now,
        86400 * 365,
        1000,
    );

    match result {
        burst_node::EconomicResult::Endorse {
            burn_amount,
            target: t,
        } => {
            assert_eq!(burn_amount, 300);
            assert_eq!(t.unwrap(), target);
        }
        other => panic!("expected Endorse, got {:?}", other),
    }
}

#[test]
fn challenge_stakes_brn_correctly() {
    let mut brn = BrnEngine::with_rate(100, Timestamp::new(0));
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(10000);

    let challenger = make_address(198);
    let target = make_address(199);

    brn.track_wallet(
        challenger.clone(),
        burst_brn::BrnWalletState::new(Timestamp::new(0)),
    );

    let challenge_block = make_block(
        BlockType::Challenge,
        &challenger,
        BlockHash::new([1u8; 32]),
        &challenger,
        1800, // ascending odometer: prev 1000 spent + 800 staked
        0,
        BlockHash::new(pubkey_bytes(&target)),
        TxHash::ZERO,
        now.as_secs(),
    );

    let result = burst_node::process_block_economics(
        &challenge_block,
        &mut brn,
        &mut trst,
        now,
        86400 * 365,
        1000,
    );

    match result {
        burst_node::EconomicResult::Challenge {
            stake_amount,
            stake,
            target: t,
        } => {
            assert_eq!(stake_amount, 800);
            assert_eq!(stake.amount, 800);
            assert!(!stake.resolved);
            assert_eq!(t.unwrap(), target);
        }
        other => panic!("expected Challenge, got {:?}", other),
    }
}

#[test]
fn verification_vote_records_vote_value_and_stake() {
    let mut brn = BrnEngine::with_rate(100, Timestamp::new(0));
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(10000);

    let voter = make_address(200);
    let target = make_address(201);

    // The stake is recorded in the BRN engine, so the voter must be tracked
    // and have accrued enough.
    brn.track_wallet(
        voter.clone(),
        burst_brn::BrnWalletState::new(Timestamp::new(0)),
    );

    // Create a VerificationVote block
    let mut vote_block = make_block(
        BlockType::VerificationVote,
        &voter,
        BlockHash::new([1u8; 32]),
        &voter,
        1200, // ascending odometer: prev 1000 spent + 200 staked
        0,
        BlockHash::new(pubkey_bytes(&target)),
        TxHash::ZERO,
        now.as_secs(),
    );
    vote_block.transaction = TxHash::new({
        let mut bytes = [0u8; 32];
        bytes[0] = 1; // vote value = 1 (Yea)
        bytes
    });
    vote_block.hash = vote_block.compute_hash();

    let result = burst_node::process_block_economics(
        &vote_block,
        &mut brn,
        &mut trst,
        now,
        86400 * 365,
        1000,
    );

    match result {
        burst_node::EconomicResult::VerificationVote {
            voter: v,
            target: t,
            vote,
            stake,
        } => {
            assert_eq!(v, voter);
            assert_eq!(t.unwrap(), target);
            assert_eq!(vote, 1);
            assert_eq!(stake, 200); // 1200 - 1000
        }
        other => panic!("expected VerificationVote, got {:?}", other),
    }
}

#[test]
fn verifier_rewards_are_burn_backed_pending_entries() {
    let (_dir, env) = temp_env();
    let pending_store = env.pending_store();

    let mut brn = BrnEngine::with_rate(100, Timestamp::new(0));
    let correct = make_address(230);
    let dissenter = make_address(231);
    let verified_wallet = make_address(232);

    brn.track_wallet(
        correct.clone(),
        burst_brn::BrnWalletState::new(Timestamp::new(0)),
    );
    brn.track_wallet(
        dissenter.clone(),
        burst_brn::BrnWalletState::new(Timestamp::new(0)),
    );
    // Both staked 500.
    brn.get_wallet_mut(&correct).unwrap().total_staked = 500;
    brn.get_wallet_mut(&dissenter).unwrap().total_staked = 500;

    let outcome = burst_verification::compute_verification_outcomes(
        &verified_wallet,
        burst_verification::VerificationResult::Verified,
        &[],
        &[
            (correct.clone(), 500, true),
            (dissenter.clone(), 500, false),
        ],
    )
    .unwrap();

    let ts = Timestamp::new(9000);
    lifecycle_simulation::resolve_verifier_outcomes(
        &mut brn,
        &pending_store,
        &outcome.verifiers,
        &verified_wallet,
        ts,
    );

    // Correct voter: stake unlocked; dissenter: stake burned.
    assert_eq!(brn.get_wallet(&correct).unwrap().total_staked, 0);
    assert_eq!(brn.get_wallet(&correct).unwrap().total_burned, 0);
    assert_eq!(brn.get_wallet(&dissenter).unwrap().total_staked, 0);
    assert_eq!(brn.get_wallet(&dissenter).unwrap().total_burned, 500);

    // The correct voter's TRST reward equals the forfeited stake and sits
    // in pending, claimable via a normal Receive block.
    use burst_store::pending::PendingStore;
    let all = pending_store.get_all_pending().unwrap();
    assert_eq!(all.len(), 1);
    let (recipient, _hash, info) = &all[0];
    assert_eq!(recipient, &correct);
    assert_eq!(info.amount, 500);
    assert_eq!(info.provenance.len(), 1);
    // Conservation: minted reward == burned dissenter stake.
    assert_eq!(
        info.amount,
        brn.get_wallet(&dissenter).unwrap().total_burned
    );
}
