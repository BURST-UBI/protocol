use super::*;

#[test]
fn ledger_updater_open_then_send_updates_account_correctly() {
    let (_dir, env) = temp_env();
    let account = make_address(10);
    let rep = make_address(11);

    let open = make_block(
        BlockType::Open,
        &account,
        BlockHash::ZERO,
        &rep,
        0,
        1000,
        BlockHash::ZERO,
        TxHash::ZERO,
        1000,
    );

    let mut batch = env.tx_begin_write().unwrap();
    let info = burst_node::update_account_on_block(&mut batch, &open, None, 0).unwrap();
    batch.commit().unwrap();

    assert_eq!(info.block_count, 1);
    assert_eq!(info.trst_balance, 1000);
    assert_eq!(info.representative, rep);

    let send_link = BlockHash::new(pubkey_bytes(&make_address(99)));
    let send = make_block(
        BlockType::Send,
        &account,
        open.hash,
        &rep,
        0,
        700,
        send_link,
        TxHash::ZERO,
        2000,
    );

    let mut batch = env.tx_begin_write().unwrap();
    let info2 = burst_node::update_account_on_block(&mut batch, &send, Some(&info), 0).unwrap();
    batch.commit().unwrap();

    assert_eq!(info2.block_count, 2);
    assert_eq!(info2.trst_balance, 700);
    assert_eq!(info2.head, send.hash);
}

#[test]
fn ledger_updater_rep_change_updates_representative() {
    // update_account_on_block records the new representative in AccountInfo;
    // consensus weight itself is derived from account state by the rep-weight
    // cache (rebuilt separately), so here we verify the state transition and
    // that a rebuild attributes the account's vote to the NEW rep.
    let (_dir, env) = temp_env();
    let account = make_address(20);
    let rep1 = make_address(21);
    let rep2 = make_address(22);

    let open = make_block(
        BlockType::Open,
        &account,
        BlockHash::ZERO,
        &rep1,
        0,
        500,
        BlockHash::ZERO,
        TxHash::ZERO,
        1000,
    );

    let mut batch = env.tx_begin_write().unwrap();
    let mut info = burst_node::update_account_on_block(&mut batch, &open, None, 0).unwrap();
    batch.commit().unwrap();
    assert_eq!(info.representative, rep1);

    // Mark verified (a human) so it counts as one delegated vote.
    info.state = burst_types::WalletState::Verified;

    let change = make_block(
        BlockType::ChangeRepresentative,
        &account,
        open.hash,
        &rep2,
        0,
        500,
        BlockHash::ZERO,
        TxHash::ZERO,
        2000,
    );

    let mut batch = env.tx_begin_write().unwrap();
    let info2 = burst_node::update_account_on_block(&mut batch, &change, Some(&info), 0).unwrap();
    batch.commit().unwrap();
    assert_eq!(info2.representative, rep2);

    // A rebuilt cache attributes this verified human's one vote to rep2, not rep1.
    let mut rep_weights = RepWeightCache::new();
    rep_weights.rebuild_from_accounts(std::iter::once((true, info2.representative.clone())));
    assert_eq!(rep_weights.weight(&rep1), 0);
    assert_eq!(
        rep_weights.weight(&rep2),
        burst_consensus::rep_weights::WEIGHT_PER_HUMAN
    );
}

#[test]
fn ledger_updater_burn_tracks_brn_correctly() {
    let (_dir, env) = temp_env();
    let account = make_address(30);
    let rep = make_address(31);

    let open = make_block(
        BlockType::Open,
        &account,
        BlockHash::ZERO,
        &rep,
        0,
        0,
        BlockHash::ZERO,
        TxHash::ZERO,
        1000,
    );

    let mut batch = env.tx_begin_write().unwrap();
    let info = burst_node::update_account_on_block(&mut batch, &open, None, 0).unwrap();
    batch.commit().unwrap();

    let receiver = make_address(32);
    let burn = make_block(
        BlockType::Burn,
        &account,
        open.hash,
        &rep,
        700, // ascending odometer: prev 500 spent + 200 burned
        0,
        BlockHash::new(pubkey_bytes(&receiver)),
        TxHash::ZERO,
        2000,
    );

    let previous_brn_spent: u128 = 500;
    let mut batch = env.tx_begin_write().unwrap();
    let info2 =
        burst_node::update_account_on_block(&mut batch, &burn, Some(&info), previous_brn_spent).unwrap();
    batch.commit().unwrap();

    assert_eq!(info2.total_brn_burned, 200);
    assert_eq!(info2.block_count, 2);
}

#[test]
fn block_processor_accepts_open_block() {
    use burst_node::BlockProcessor;

    let account = make_address(111);
    let rep = make_address(112);

    let mut processor = BlockProcessor::new(0, burst_types::BlockHash::new([1; 32]));
    processor.set_verify_signatures(false);
    processor.set_validate_timestamps(false);
    let mut frontier = DagFrontier::new();

    let open = make_block(
        BlockType::Open,
        &account,
        BlockHash::ZERO,
        &rep,
        0,
        100,
        BlockHash::ZERO,
        TxHash::ZERO,
        1000,
    );

    let result = processor.process(&open, &mut frontier);
    assert_eq!(result, burst_node::ProcessResult::Accepted);
    assert_eq!(frontier.get_head(&account), Some(&open.hash));
}

#[test]
fn block_processor_detects_gap() {
    use burst_node::BlockProcessor;

    let account = make_address(121);
    let rep = make_address(122);

    let mut processor = BlockProcessor::new(0, burst_types::BlockHash::new([1; 32]));
    processor.set_verify_signatures(false);
    processor.set_validate_timestamps(false);
    let mut frontier = DagFrontier::new();

    let send = make_block(
        BlockType::Send,
        &account,
        BlockHash::new([0xFF; 32]),
        &rep,
        0,
        50,
        BlockHash::new([0xAA; 32]),
        TxHash::ZERO,
        1000,
    );

    let result = processor.process(&send, &mut frontier);
    assert_eq!(result, burst_node::ProcessResult::Gap);
}

#[test]
fn block_processor_detects_fork() {
    use burst_node::BlockProcessor;

    let account = make_address(131);
    let rep = make_address(132);

    let mut processor = BlockProcessor::new(0, burst_types::BlockHash::new([1; 32]));
    processor.set_verify_signatures(false);
    processor.set_validate_timestamps(false);
    let mut frontier = DagFrontier::new();

    let open = make_block(
        BlockType::Open,
        &account,
        BlockHash::ZERO,
        &rep,
        0,
        100,
        BlockHash::ZERO,
        TxHash::ZERO,
        1000,
    );
    let r = processor.process(&open, &mut frontier);
    assert_eq!(r, burst_node::ProcessResult::Accepted);

    let send1 = make_block(
        BlockType::Send,
        &account,
        open.hash,
        &rep,
        0,
        80,
        BlockHash::new([0xAA; 32]),
        TxHash::ZERO,
        2000,
    );
    let r = processor.process(&send1, &mut frontier);
    assert_eq!(r, burst_node::ProcessResult::Accepted);

    let fork = make_block(
        BlockType::Send,
        &account,
        open.hash,
        &rep,
        0,
        60,
        BlockHash::new([0xBB; 32]),
        TxHash::ZERO,
        2001,
    );
    let r = processor.process(&fork, &mut frontier);
    assert_eq!(r, burst_node::ProcessResult::Fork);
}

#[test]
fn block_processor_detects_duplicate() {
    use burst_node::BlockProcessor;

    let account = make_address(141);
    let rep = make_address(142);

    let mut processor = BlockProcessor::new(0, burst_types::BlockHash::new([1; 32]));
    processor.set_verify_signatures(false);
    processor.set_validate_timestamps(false);
    let mut frontier = DagFrontier::new();

    let open = make_block(
        BlockType::Open,
        &account,
        BlockHash::ZERO,
        &rep,
        0,
        100,
        BlockHash::ZERO,
        TxHash::ZERO,
        1000,
    );
    let r = processor.process(&open, &mut frontier);
    assert_eq!(r, burst_node::ProcessResult::Accepted);

    let r2 = processor.process(&open, &mut frontier);
    assert_eq!(r2, burst_node::ProcessResult::Duplicate);
}

#[test]
fn balance_validation_endorse_requires_brn_burn() {
    use burst_node::BlockProcessor;

    // Odometer unchanged → no burn happened → invalid endorse.
    let block = make_block(
        BlockType::Endorse,
        &make_address(213),
        BlockHash::new([1u8; 32]),
        &make_address(214),
        1000,
        500,
        BlockHash::ZERO,
        TxHash::ZERO,
        5000,
    );
    let result = BlockProcessor::validate_balance_transition(&block, 1000, 500);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("must burn BRN"));

    // The delta satisfies balance-shape validation; admission checks the required burn.
    let block = make_block(
        BlockType::Endorse,
        &make_address(213),
        BlockHash::new([1u8; 32]),
        &make_address(214),
        1100,
        500,
        BlockHash::ZERO,
        TxHash::ZERO,
        5000,
    );
    assert!(BlockProcessor::validate_balance_transition(&block, 1000, 500).is_ok());
}

#[test]
fn balance_validation_challenge_rejects_trst_change() {
    use burst_node::BlockProcessor;

    let block = make_block(
        BlockType::Challenge,
        &make_address(215),
        BlockHash::new([1u8; 32]),
        &make_address(216),
        1500, // ascending odometer: valid 500 stake…
        600,  // …but TRST must not change on a challenge
        BlockHash::ZERO,
        TxHash::ZERO,
        5000,
    );
    let result = BlockProcessor::validate_balance_transition(&block, 1000, 500);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("TRST"));
}

#[test]
fn balance_validation_governance_preserves_both_balances() {
    use burst_node::BlockProcessor;

    let block = make_block(
        BlockType::GovernanceVote,
        &make_address(217),
        BlockHash::new([1u8; 32]),
        &make_address(218),
        1000,
        500,
        BlockHash::ZERO,
        TxHash::ZERO,
        5000,
    );
    assert!(BlockProcessor::validate_balance_transition(&block, 1000, 500).is_ok());

    let bad_block = make_block(
        BlockType::GovernanceVote,
        &make_address(217),
        BlockHash::new([1u8; 32]),
        &make_address(218),
        999,
        500,
        BlockHash::ZERO,
        TxHash::ZERO,
        5000,
    );
    let result = BlockProcessor::validate_balance_transition(&bad_block, 1000, 500);
    assert!(result.is_err());
}

#[test]
fn balance_validation_verification_vote_preserves_both() {
    use burst_node::BlockProcessor;

    let ok_block = make_block(
        BlockType::VerificationVote,
        &make_address(219),
        BlockHash::new([1u8; 32]),
        &make_address(220),
        1000,
        500,
        BlockHash::ZERO,
        TxHash::ZERO,
        5000,
    );
    assert!(BlockProcessor::validate_balance_transition(&ok_block, 1000, 500).is_ok());

    let bad_block = make_block(
        BlockType::VerificationVote,
        &make_address(219),
        BlockHash::new([1u8; 32]),
        &make_address(220),
        1000,
        400,
        BlockHash::ZERO,
        TxHash::ZERO,
        5000,
    );
    assert!(BlockProcessor::validate_balance_transition(&bad_block, 1000, 500).is_err());
}

#[test]
fn burn_persists_block_frontier_and_account() {
    let (_dir, env) = temp_env();

    let alice = make_address(221);
    let bob = make_address(222);
    let rep = make_address(223);

    // Step 1: Open block for Alice
    let open = make_block(
        BlockType::Open,
        &alice,
        BlockHash::ZERO,
        &rep,
        1000,
        0,
        BlockHash::ZERO,
        TxHash::ZERO,
        1000,
    );

    let mut batch = env.tx_begin_write().unwrap();
    let bytes = bincode::serialize(&open).unwrap();
    batch.put_block(&open.hash, &bytes).unwrap();
    batch.put_frontier(&alice, &open.hash).unwrap();
    let info = burst_node::update_account_on_block(&mut batch, &open, None, 0).unwrap();
    batch.commit().unwrap();

    assert_eq!(info.block_count, 1);
    assert_eq!(info.trst_balance, 0);

    // Step 2: Burn block — Alice burns 500 BRN, Bob is receiver
    let burn = make_block(
        BlockType::Burn,
        &alice,
        open.hash,
        &rep,
        500,
        0,
        BlockHash::new(pubkey_bytes(&bob)),
        TxHash::ZERO,
        2000,
    );

    // Economics
    let mut brn = BrnEngine::with_rate(100, Timestamp::new(0));
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    brn.track_wallet(
        alice.clone(),
        burst_brn::BrnWalletState::new(Timestamp::new(0)),
    );

    let econ = burst_node::process_block_economics(
        &burn,
        &mut brn,
        &mut trst,
        Timestamp::new(2000),
        86400 * 365,
        0, // This component fixture supplies the previous spending separately.
    );

    let mint_token = match &econ {
        burst_node::EconomicResult::BurnAndMint {
            burn_amount,
            mint_token,
            ..
        } => {
            assert_eq!(*burn_amount, 500);
            mint_token.clone().unwrap()
        }
        other => panic!("expected BurnAndMint, got {:?}", other),
    };

    // Persist burn block + account update atomically
    let mut batch = env.tx_begin_write().unwrap();
    let bytes = bincode::serialize(&burn).unwrap();
    batch.put_block(&burn.hash, &bytes).unwrap();
    batch.put_frontier(&alice, &burn.hash).unwrap();
    let info2 = burst_node::update_account_on_block(&mut batch, &burn, Some(&info), 0).unwrap();
    batch.commit().unwrap();

    assert_eq!(info2.block_count, 2);
    assert_eq!(info2.total_brn_burned, 500);
    assert_eq!(info2.head, burn.hash);

    // Verify LMDB reads match
    let block_store = env.block_store();
    let read_bytes = block_store.get_block(&burn.hash).unwrap();
    let read_block: StateBlock = bincode::deserialize(&read_bytes).unwrap();
    assert_eq!(read_block.brn_spent, 500);
    assert_eq!(read_block.block_type, BlockType::Burn);

    let frontier_store = env.frontier_store();
    assert_eq!(frontier_store.get_frontier(&alice).unwrap(), burn.hash);

    // Verify TRST token was minted correctly
    assert_eq!(mint_token.amount, 500);
    assert_eq!(mint_token.origin_wallet, alice);
    assert_eq!(mint_token.holder, bob);
}
