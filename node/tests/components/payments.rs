use super::*;

#[test]
fn economics_burn_mints_trst_token() {
    let mut brn = BrnEngine::with_rate(100, Timestamp::new(0));
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(10000);
    let expiry = 86400 * 365;

    let burner = make_address(40);
    let receiver = make_address(41);

    brn.track_wallet(
        burner.clone(),
        burst_brn::BrnWalletState::new(Timestamp::new(100)),
    );

    let burn_block = make_block(
        BlockType::Burn,
        &burner,
        BlockHash::ZERO,
        &burner,
        700, // ascending odometer: prev 500 spent + 200 burned
        0,
        BlockHash::new(pubkey_bytes(&receiver)),
        TxHash::ZERO,
        now.as_secs(),
    );

    let previous_brn_spent = 500;
    let result = burst_node::process_block_economics(
        &burn_block,
        &mut brn,
        &mut trst,
        now,
        expiry,
        previous_brn_spent,
    );

    match result {
        burst_node::EconomicResult::BurnAndMint {
            burn_amount,
            mint_token,
        } => {
            assert_eq!(burn_amount, 200);
            let token = mint_token.expect("token should be minted");
            assert_eq!(token.amount, 200);
            assert_eq!(token.holder, receiver);
            assert_eq!(token.origin_wallet, burner);
            assert_eq!(token.state, TrstState::Active);
        }
        other => panic!("expected BurnAndMint, got {:?}", other),
    }
}

#[test]
fn economics_send_records_sender_and_balance() {
    let mut brn = BrnEngine::new();
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(5000);
    let expiry = 86400 * 365;

    let sender = make_address(50);
    let receiver = make_address(51);

    let send_block = make_block(
        BlockType::Send,
        &sender,
        BlockHash::new([1u8; 32]),
        &sender,
        0,
        700,
        BlockHash::new(pubkey_bytes(&receiver)),
        TxHash::ZERO,
        now.as_secs(),
    );

    let result =
        burst_node::process_block_economics(&send_block, &mut brn, &mut trst, now, expiry, 0);

    match result {
        burst_node::EconomicResult::Send {
            sender: s,
            receiver: r,
            trst_balance_after,
        } => {
            assert_eq!(s, sender);
            assert!(r.is_some());
            assert_eq!(trst_balance_after, 700);
        }
        other => panic!("expected Send, got {:?}", other),
    }
}

#[test]
fn economics_full_burn_send_receive_chain() {
    let mut brn = BrnEngine::with_rate(100, Timestamp::new(0));
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(10000);
    let expiry_secs = 86400u64 * 365;

    let alice = make_address(60);
    let bob = make_address(61);
    let carol = make_address(62);

    brn.track_wallet(
        alice.clone(),
        burst_brn::BrnWalletState::new(Timestamp::new(100)),
    );

    // Stage Alice's burn and Bob's matching TRST.
    let burn = make_block(
        BlockType::Burn,
        &alice,
        BlockHash::ZERO,
        &alice,
        700, // ascending odometer: prev 500 spent + 200 burned
        0,
        BlockHash::new(pubkey_bytes(&bob)),
        TxHash::ZERO,
        now.as_secs(),
    );

    let result =
        burst_node::process_block_economics(&burn, &mut brn, &mut trst, now, expiry_secs, 500);

    let token = match &result {
        burst_node::EconomicResult::BurnAndMint {
            mint_token: Some(t),
            ..
        } => {
            trst.track_token(t.clone());
            t.clone()
        }
        other => panic!("expected BurnAndMint, got {:?}", other),
    };

    assert_eq!(token.amount, 200);
    assert_eq!(token.holder, bob);
    assert_eq!(trst.transferable_balance(&bob, now), Some(200));

    // Stage Bob's 150 TRST payment to Carol.
    let send = make_block(
        BlockType::Send,
        &bob,
        BlockHash::new([1u8; 32]),
        &bob,
        0,
        50,
        BlockHash::new(pubkey_bytes(&carol)),
        TxHash::ZERO,
        now.as_secs() + 10,
    );

    let send_result = burst_node::process_block_economics(
        &send,
        &mut brn,
        &mut trst,
        Timestamp::new(now.as_secs() + 10),
        expiry_secs,
        0,
    );

    match &send_result {
        burst_node::EconomicResult::Send {
            sender,
            trst_balance_after,
            ..
        } => {
            assert_eq!(sender, &bob);
            assert_eq!(*trst_balance_after, 50);
        }
        other => panic!("expected Send, got {:?}", other),
    }

    let provenance = trst.debit_wallet_with_provenance(&bob, &token.origin, 150);
    assert!(
        !provenance.is_empty(),
        "provenance should track consumed tokens"
    );
    assert_eq!(provenance[0].amount, 150);
    assert_eq!(provenance[0].origin_wallet, alice);

    assert_eq!(trst.transferable_balance(&bob, now), Some(50));

    // Apply Carol's receipt.
    let receive = make_block(
        BlockType::Receive,
        &carol,
        BlockHash::ZERO,
        &carol,
        0,
        150,
        send.hash,
        TxHash::ZERO,
        now.as_secs() + 20,
    );

    let recv_result = burst_node::process_block_economics(
        &receive,
        &mut brn,
        &mut trst,
        Timestamp::new(now.as_secs() + 20),
        expiry_secs,
        0,
    );

    match recv_result {
        burst_node::EconomicResult::Receive {
            receiver,
            trst_balance_after,
            ..
        } => {
            assert_eq!(receiver, carol);
            assert_eq!(trst_balance_after, 150);
        }
        other => panic!("expected Receive, got {:?}", other),
    }
}

#[test]
fn economics_burn_reports_amount_on_success() {
    let mut brn = BrnEngine::with_rate(100, Timestamp::new(0));
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(10000);

    let alice = make_address(150);
    let bob = make_address(151);

    brn.track_wallet(
        alice.clone(),
        burst_brn::BrnWalletState::new(Timestamp::new(100)),
    );

    let burn = make_block(
        BlockType::Burn,
        &alice,
        BlockHash::ZERO,
        &alice,
        100, // ascending odometer: prev 50 spent + 50 burned
        0,
        BlockHash::new(pubkey_bytes(&bob)),
        TxHash::ZERO,
        now.as_secs(),
    );

    let previous_brn_spent: u128 = 50;
    let result =
        burst_node::process_block_economics(&burn, &mut brn, &mut trst, now, 86400 * 365, previous_brn_spent);

    match &result {
        burst_node::EconomicResult::BurnAndMint {
            burn_amount,
            mint_token,
        } => {
            assert_eq!(*burn_amount, 50);
            assert!(mint_token.is_some(), "small burn should still mint");
        }
        burst_node::EconomicResult::Rejected { reason: _ } => {
            // This test permits rejection; it does not establish small-burn acceptance.
        }
        other => panic!("unexpected: {:?}", other),
    }
}

#[test]
fn signed_burn_and_send_pass_component_checks() {
    use burst_node::BlockProcessor;

    let alice_kp = keypair_from_seed(&[0xA1; 32]);
    let alice = derive_address(&alice_kp.public);

    let bob_kp = keypair_from_seed(&[0xB2; 32]);
    let bob = derive_address(&bob_kp.public);

    let carol_kp = keypair_from_seed(&[0xC3; 32]);
    let carol = derive_address(&carol_kp.public);

    let mut processor = BlockProcessor::new(0, burst_types::BlockHash::new([1; 32]));
    processor.set_validate_timestamps(false);
    let mut frontier = DagFrontier::new();

    // Alice opens her account
    let mut open = make_block(
        BlockType::Open,
        &alice,
        BlockHash::ZERO,
        &alice,
        1000,
        0,
        BlockHash::ZERO,
        TxHash::ZERO,
        1000,
    );
    open.signature = sign_message(open.hash.as_bytes(), &alice_kp.private);
    assert_eq!(
        processor.process(&open, &mut frontier),
        burst_node::ProcessResult::Accepted
    );

    // Alice burns BRN → Bob gets TRST (link = bob's pubkey)
    let bob_pubkey = pubkey_bytes(&bob);
    let mut burn = make_block(
        BlockType::Burn,
        &alice,
        open.hash,
        &alice,
        500,
        0,
        BlockHash::new(bob_pubkey),
        TxHash::ZERO,
        2000,
    );
    burn.signature = sign_message(burn.hash.as_bytes(), &alice_kp.private);
    assert_eq!(
        processor.process(&burn, &mut frontier),
        burst_node::ProcessResult::Accepted
    );

    // Process through economics
    let mut brn = BrnEngine::with_rate(100, Timestamp::new(0));
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    brn.track_wallet(
        alice.clone(),
        burst_brn::BrnWalletState::new(Timestamp::new(0)),
    );

    let result = burst_node::process_block_economics(
        &burn,
        &mut brn,
        &mut trst,
        Timestamp::new(2000),
        86400 * 365,
        0, // This component fixture supplies the previous spending separately.
    );
    match &result {
        burst_node::EconomicResult::BurnAndMint {
            burn_amount,
            mint_token,
            ..
        } => {
            assert_eq!(*burn_amount, 500);
            let token = mint_token.as_ref().unwrap();
            assert_eq!(token.amount, 500);
            assert_eq!(token.holder, bob);
            assert_eq!(token.origin_wallet, alice);
            trst.track_token(token.clone());
        }
        other => panic!("expected BurnAndMint, got {:?}", other),
    }

    // Bob opens his account with a receive
    let mut bob_open = make_block(
        BlockType::Open,
        &bob,
        BlockHash::ZERO,
        &bob,
        0,
        500,
        burn.hash,
        TxHash::ZERO,
        3000,
    );
    bob_open.signature = sign_message(bob_open.hash.as_bytes(), &bob_kp.private);

    assert_eq!(
        processor.process(&bob_open, &mut frontier),
        burst_node::ProcessResult::Accepted
    );
    assert_eq!(frontier.get_head(&bob), Some(&bob_open.hash));

    // Bob sends 300 TRST to Carol
    let carol_pubkey = pubkey_bytes(&carol);
    let mut bob_send = make_block(
        BlockType::Send,
        &bob,
        bob_open.hash,
        &bob,
        0,
        200,
        BlockHash::new(carol_pubkey),
        TxHash::ZERO,
        4000,
    );
    bob_send.signature = sign_message(bob_send.hash.as_bytes(), &bob_kp.private);
    assert_eq!(
        processor.process(&bob_send, &mut frontier),
        burst_node::ProcessResult::Accepted
    );

    let send_result = burst_node::process_block_economics(
        &bob_send,
        &mut brn,
        &mut trst,
        Timestamp::new(4000),
        86400 * 365,
        0,
    );
    match &send_result {
        burst_node::EconomicResult::Send {
            sender,
            trst_balance_after,
            ..
        } => {
            assert_eq!(sender, &bob);
            assert_eq!(*trst_balance_after, 200);
        }
        other => panic!("expected Send, got {:?}", other),
    }
}

#[test]
fn create_received_token_single_provenance() {
    let sender = make_address(204);
    let receiver = make_address(205);
    let origin_wallet = make_address(206);

    let receive_block = make_block(
        BlockType::Receive,
        &receiver,
        BlockHash::ZERO,
        &receiver,
        0,
        500,
        BlockHash::new([0xDD; 32]),
        TxHash::ZERO,
        5000,
    );

    let pending = burst_store::pending::PendingInfo {
        return_credit: false,
        source: sender.clone(),
        amount: 500,
        timestamp: Timestamp::new(4000),
        provenance: vec![burst_store::pending::PendingProvenance {
            basis: Default::default(),
            revoked_amounts: Default::default(),
            amount: 500,
            origin: TxHash::new([0x01; 32]),
            origin_wallet: origin_wallet.clone(),
            origin_timestamp: Timestamp::new(1000),
            effective_origin_timestamp: Timestamp::new(1000),
        }],
    };

    let token =
        burst_node::block_economics::create_received_token(&receive_block, &pending, 86400 * 365);
    assert_eq!(token.amount, 500);
    assert_eq!(token.holder, receiver);
    assert_eq!(token.origin_wallet, origin_wallet);
    assert_eq!(token.origin, TxHash::new([0x01; 32]));
    assert_eq!(token.origin_timestamp, Timestamp::new(1000));
    assert_eq!(token.state, TrstState::Active);
}

#[test]
fn create_received_token_single_provenance_preserves_origin() {
    let sender = make_address(207);
    let receiver = make_address(208);
    let origin_wallet = make_address(209);

    let receive_block = make_block(
        BlockType::Receive,
        &receiver,
        BlockHash::ZERO,
        &receiver,
        0,
        400,
        BlockHash::new([0xEE; 32]),
        TxHash::ZERO,
        8000,
    );

    let origin_hash = TxHash::new([0x02; 32]);
    let pending = burst_store::pending::PendingInfo {
        return_credit: false,
        source: sender.clone(),
        amount: 400,
        timestamp: Timestamp::new(7000),
        provenance: vec![burst_store::pending::PendingProvenance {
            basis: Default::default(),
            revoked_amounts: Default::default(),
            amount: 400,
            origin: origin_hash,
            origin_wallet: origin_wallet.clone(),
            origin_timestamp: Timestamp::new(3000),
            effective_origin_timestamp: Timestamp::new(3000),
        }],
    };

    let token =
        burst_node::block_economics::create_received_token(&receive_block, &pending, 86400 * 365);
    assert_eq!(token.amount, 400);
    assert_eq!(token.holder, receiver);
    assert_eq!(
        token.origin, origin_hash,
        "origin should pass through from sender"
    );
    assert_eq!(
        token.origin_wallet, origin_wallet,
        "origin_wallet should pass through"
    );
    assert_eq!(token.origin_timestamp, Timestamp::new(3000));
    assert_eq!(token.effective_origin_timestamp, Timestamp::new(3000));
    assert!(token.revoked_origin.is_none());
}

#[test]
fn create_received_token_no_provenance_uses_pending_timestamp() {
    let sender = make_address(211);
    let receiver = make_address(212);

    let receive_block = make_block(
        BlockType::Receive,
        &receiver,
        BlockHash::ZERO,
        &receiver,
        0,
        100,
        BlockHash::new([0xFF; 32]),
        TxHash::ZERO,
        9000,
    );

    let pending = burst_store::pending::PendingInfo {
        return_credit: false,
        source: sender.clone(),
        amount: 100,
        timestamp: Timestamp::new(8000),
        provenance: Vec::new(),
    };

    let token =
        burst_node::block_economics::create_received_token(&receive_block, &pending, 86400 * 365);
    assert_eq!(token.amount, 100);
    assert_eq!(token.holder, receiver);
    assert_eq!(token.origin_wallet, sender);
    assert_eq!(token.origin_timestamp, Timestamp::new(8000));
    assert!(token.revoked_origin.is_none());
}

#[test]
fn reject_receive_returns_no_balance_change() {
    let mut brn = BrnEngine::new();
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(5000);

    let rejecter = make_address(229);

    let reject_block = make_block(
        BlockType::RejectReceive,
        &rejecter,
        BlockHash::new([1u8; 32]),
        &rejecter,
        0,
        100,
        BlockHash::new([0xDD; 32]),
        TxHash::ZERO,
        now.as_secs(),
    );

    let result = burst_node::process_block_economics(
        &reject_block,
        &mut brn,
        &mut trst,
        now,
        86400 * 365,
        0,
    );

    match result {
        burst_node::EconomicResult::RejectReceive {
            rejecter: r,
            send_block_hash,
        } => {
            assert_eq!(r, rejecter);
            assert_eq!(send_block_hash, BlockHash::new([0xDD; 32]));
        }
        other => panic!("expected RejectReceive, got {:?}", other),
    }
}
