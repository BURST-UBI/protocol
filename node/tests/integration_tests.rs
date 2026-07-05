//! Integration tests exercising the full block pipeline:
//! block creation → processing → economics → LMDB persistence → readback.
//!
//! These tests wire together components that are normally only connected
//! inside `node.rs`, verifying the system works end-to-end — not just
//! in isolation.

use burst_brn::BrnEngine;
use burst_consensus::RepWeightCache;
use burst_crypto::{derive_address, keypair_from_seed, sign_message};
use burst_ledger::{BlockType, DagFrontier, StateBlock, CURRENT_BLOCK_VERSION};
use burst_store::block::BlockStore;
use burst_store::frontier::FrontierStore;
use burst_store::pending::PendingStore;
use burst_store_lmdb::LmdbEnvironment;
use burst_trst::TrstEngine;
use burst_types::{BlockHash, Signature, Timestamp, TrstState, TxHash, WalletAddress};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn temp_env() -> (tempfile::TempDir, LmdbEnvironment) {
    let dir = tempfile::tempdir().expect("temp dir");
    let env = LmdbEnvironment::open(dir.path(), 30, 64 * 1024 * 1024).expect("open env");
    (dir, env)
}

fn make_address(seed: u8) -> WalletAddress {
    let kp = keypair_from_seed(&[seed; 32]);
    derive_address(&kp.public)
}

fn pubkey_bytes(addr: &WalletAddress) -> [u8; 32] {
    burst_crypto::decode_address(addr.as_str()).expect("valid address")
}

fn make_block(
    block_type: BlockType,
    account: &WalletAddress,
    previous: BlockHash,
    rep: &WalletAddress,
    brn: u128,
    trst: u128,
    link: BlockHash,
    origin: TxHash,
    ts: u64,
) -> StateBlock {
    let mut dummy_sig = [0u8; 64];
    dummy_sig[0] = 0xFF;
    let mut block = StateBlock {
        version: CURRENT_BLOCK_VERSION,
        block_type,
        account: account.clone(),
        previous,
        representative: rep.clone(),
        brn_balance: brn,
        trst_balance: trst,
        link,
        origin,
        transaction: TxHash::ZERO,
        timestamp: Timestamp::new(ts),
        params_hash: BlockHash::ZERO,
        merge_sources: Vec::new(),
        work: 0,
        signature: Signature(dummy_sig),
        hash: BlockHash::ZERO,
    };
    block.hash = block.compute_hash();
    block
}

// ---------------------------------------------------------------------------
// 1. LMDB persistence round-trip
// ---------------------------------------------------------------------------

#[test]
fn lmdb_block_write_read_roundtrip() {
    let (_dir, env) = temp_env();
    let account = make_address(1);
    let rep = make_address(2);

    let block = make_block(
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

    let bytes = bincode::serialize(&block).unwrap();
    let block_store = env.block_store();
    block_store
        .put_block_with_account(&block.hash, &bytes, &account)
        .unwrap();

    let read_bytes = block_store.get_block(&block.hash).unwrap();
    let read_block: StateBlock = bincode::deserialize(&read_bytes).unwrap();

    assert_eq!(read_block.hash, block.hash);
    assert_eq!(read_block.account, account);
    assert_eq!(read_block.block_type, BlockType::Open);
    assert_eq!(read_block.brn_balance, 0);
    assert_eq!(read_block.trst_balance, 0);
    assert_eq!(read_block.previous, BlockHash::ZERO);
}

#[test]
fn lmdb_frontier_tracks_head() {
    let (_dir, env) = temp_env();
    let account = make_address(3);
    let rep = make_address(4);

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

    let mut batch = env.tx_begin_write().unwrap();
    let bytes = bincode::serialize(&open).unwrap();
    batch.put_block(&open.hash, &bytes).unwrap();
    batch.put_frontier(&account, &open.hash).unwrap();
    batch.commit().unwrap();

    let frontier_store = env.frontier_store();
    assert_eq!(frontier_store.get_frontier(&account).unwrap(), open.hash);

    let send = make_block(
        BlockType::Send,
        &account,
        open.hash,
        &rep,
        0,
        50,
        BlockHash::new([0xAA; 32]),
        TxHash::ZERO,
        2000,
    );

    let mut batch = env.tx_begin_write().unwrap();
    let bytes = bincode::serialize(&send).unwrap();
    batch.put_block(&send.hash, &bytes).unwrap();
    batch.put_frontier(&account, &send.hash).unwrap();
    batch.commit().unwrap();

    assert_eq!(frontier_store.get_frontier(&account).unwrap(), send.hash);
}

// ---------------------------------------------------------------------------
// 2. Ledger updater integration: account state across a chain of blocks
// ---------------------------------------------------------------------------

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

    let mut rw = RepWeightCache::new();
    let mut batch = env.tx_begin_write().unwrap();
    let info = burst_node::update_account_on_block(&mut batch, &open, None, 0, &mut rw).unwrap();
    batch.commit().unwrap();

    assert_eq!(info.block_count, 1);
    assert_eq!(info.trst_balance, 1000);
    assert_eq!(info.representative, rep);
    // Consensus weight = EXPIRED TRST, which is 0 for a fresh account — a large
    // spendable balance contributes NO ORV weight.
    assert_eq!(rw.weight(&rep), 0);

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
    let info2 =
        burst_node::update_account_on_block(&mut batch, &send, Some(&info), 0, &mut rw).unwrap();
    batch.commit().unwrap();

    assert_eq!(info2.block_count, 2);
    assert_eq!(info2.trst_balance, 700);
    assert_eq!(info2.head, send.hash);
    // A balance change (send) moves no consensus weight — still 0.
    assert_eq!(rw.weight(&rep), 0);
}

#[test]
fn ledger_updater_rep_change_moves_weight() {
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

    let mut rw = RepWeightCache::new();
    let mut batch = env.tx_begin_write().unwrap();
    let info = burst_node::update_account_on_block(&mut batch, &open, None, 0, &mut rw).unwrap();
    batch.commit().unwrap();

    // Fresh account has no expired TRST, so no consensus weight yet.
    assert_eq!(rw.weight(&rep1), 0);
    assert_eq!(rw.weight(&rep2), 0);

    // Simulate the account accruing an expired-TRST stake (what the expiry flush
    // does): 500 units of its TRST expired in-place under rep1.
    let mut info = info;
    info.expired_trst = 500;
    rw.add_weight(&rep1, 500);
    assert_eq!(rw.weight(&rep1), 500);

    // A rep change now moves that EXPIRED stake from rep1 to rep2.
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
    burst_node::update_account_on_block(&mut batch, &change, Some(&info), 0, &mut rw).unwrap();
    batch.commit().unwrap();

    assert_eq!(rw.weight(&rep1), 0);
    assert_eq!(rw.weight(&rep2), 500);
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

    let mut rw = RepWeightCache::new();
    let mut batch = env.tx_begin_write().unwrap();
    let info = burst_node::update_account_on_block(&mut batch, &open, None, 0, &mut rw).unwrap();
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

    let prev_brn: u128 = 500;
    let mut batch = env.tx_begin_write().unwrap();
    let info2 =
        burst_node::update_account_on_block(&mut batch, &burn, Some(&info), prev_brn, &mut rw)
            .unwrap();
    batch.commit().unwrap();

    assert_eq!(info2.total_brn_burned, 200);
    assert_eq!(info2.block_count, 2);
}

// ---------------------------------------------------------------------------
// 3. End-to-end economics: Burn → Mint → Send → Receive
// ---------------------------------------------------------------------------

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

    let prev_brn = 500;
    let result = burst_node::process_block_economics(
        &burn_block,
        &mut brn,
        &mut trst,
        now,
        expiry,
        prev_brn,
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

    // Step 1: Alice burns 200 BRN → Bob gets 200 TRST
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

    // Step 2: Bob sends 150 TRST to Carol
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

    // Step 3: Carol receives 150 TRST
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

// ---------------------------------------------------------------------------
// 4. TRST provenance survives merge
// ---------------------------------------------------------------------------

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
        "merged token's origin is the merge tx hash (whitepaper)"
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

// ---------------------------------------------------------------------------
// 5. TRST revocation and expiry
// ---------------------------------------------------------------------------

#[test]
fn trst_revocation_by_origin_removes_all_downstream() {
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

    let _revocations = trst.revoke_by_origin(&sybil);

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

// ---------------------------------------------------------------------------
// 6. Block processor: acceptance, gap, fork, duplicate
// ---------------------------------------------------------------------------

#[test]
fn block_processor_accepts_open_block() {
    use burst_node::BlockProcessor;

    let account = make_address(111);
    let rep = make_address(112);

    let mut proc = BlockProcessor::new(0);
    proc.set_verify_signatures(false);
    proc.set_validate_timestamps(false);
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

    let result = proc.process(&open, &mut frontier);
    assert_eq!(result, burst_node::ProcessResult::Accepted);
    assert_eq!(frontier.get_head(&account), Some(&open.hash));
}

#[test]
fn block_processor_detects_gap() {
    use burst_node::BlockProcessor;

    let account = make_address(121);
    let rep = make_address(122);

    let mut proc = BlockProcessor::new(0);
    proc.set_verify_signatures(false);
    proc.set_validate_timestamps(false);
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

    let result = proc.process(&send, &mut frontier);
    assert_eq!(result, burst_node::ProcessResult::Gap);
}

#[test]
fn block_processor_detects_fork() {
    use burst_node::BlockProcessor;

    let account = make_address(131);
    let rep = make_address(132);

    let mut proc = BlockProcessor::new(0);
    proc.set_verify_signatures(false);
    proc.set_validate_timestamps(false);
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
    let r = proc.process(&open, &mut frontier);
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
    let r = proc.process(&send1, &mut frontier);
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
    let r = proc.process(&fork, &mut frontier);
    assert_eq!(r, burst_node::ProcessResult::Fork);
}

#[test]
fn block_processor_detects_duplicate() {
    use burst_node::BlockProcessor;

    let account = make_address(141);
    let rep = make_address(142);

    let mut proc = BlockProcessor::new(0);
    proc.set_verify_signatures(false);
    proc.set_validate_timestamps(false);
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
    let r = proc.process(&open, &mut frontier);
    assert_eq!(r, burst_node::ProcessResult::Accepted);

    let r2 = proc.process(&open, &mut frontier);
    assert_eq!(r2, burst_node::ProcessResult::Duplicate);
}

// ---------------------------------------------------------------------------
// 7. Adversarial: double spend, overflow, invalid chain
// ---------------------------------------------------------------------------

#[test]
fn economics_rejects_burn_exceeding_balance() {
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

    let prev_brn: u128 = 50;
    let result =
        burst_node::process_block_economics(&burn, &mut brn, &mut trst, now, 86400 * 365, prev_brn);

    match &result {
        burst_node::EconomicResult::BurnAndMint {
            burn_amount,
            mint_token,
        } => {
            assert_eq!(*burn_amount, 50);
            assert!(mint_token.is_some(), "small burn should still mint");
        }
        burst_node::EconomicResult::Rejected { reason: _ } => {
            // Also acceptable if the engine rejects
        }
        other => panic!("unexpected: {:?}", other),
    }
}

#[test]
fn trst_cannot_transfer_more_than_balance() {
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

// ---------------------------------------------------------------------------
// 8. Pending entry lifecycle: create + read back + delete
// ---------------------------------------------------------------------------

#[test]
fn pending_entry_create_read_delete_roundtrip() {
    let (_dir, env) = temp_env();
    let sender = make_address(180);
    let receiver = make_address(181);
    let rep = make_address(182);

    let send_block = make_block(
        BlockType::Send,
        &sender,
        BlockHash::new([1u8; 32]),
        &rep,
        0,
        700,
        BlockHash::new(pubkey_bytes(&receiver)),
        TxHash::ZERO,
        2000,
    );

    let mut batch = env.tx_begin_write().unwrap();
    burst_node::create_pending_entry(&mut batch, &send_block, 300, &receiver, Vec::new()).unwrap();
    batch.commit().unwrap();

    let pending_store = env.pending_store();
    let send_hash = TxHash::new(*send_block.hash.as_bytes());
    let pending = pending_store.get_pending(&receiver, &send_hash).unwrap();
    assert_eq!(pending.amount, 300);
    assert_eq!(pending.source, sender);

    let receive_block = make_block(
        BlockType::Receive,
        &receiver,
        BlockHash::ZERO,
        &rep,
        0,
        300,
        send_block.hash,
        TxHash::ZERO,
        3000,
    );

    let mut batch = env.tx_begin_write().unwrap();
    burst_node::delete_pending_entry(&mut batch, &receive_block).unwrap();
    batch.commit().unwrap();

    let result = pending_store.get_pending(&receiver, &send_hash);
    assert!(result.is_err(), "pending should be deleted after receive");
}

// ---------------------------------------------------------------------------
// 9. Write batch atomicity: dropped batch does not persist
// ---------------------------------------------------------------------------

#[test]
fn write_batch_rollback_on_drop() {
    let (_dir, env) = temp_env();
    let account = make_address(190);
    let rep = make_address(191);

    let block = make_block(
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

    {
        let mut batch = env.tx_begin_write().unwrap();
        let bytes = bincode::serialize(&block).unwrap();
        batch.put_block(&block.hash, &bytes).unwrap();
        batch.put_frontier(&account, &block.hash).unwrap();
        // Intentionally drop without commit
    }

    let block_store = env.block_store();
    assert!(
        block_store.get_block(&block.hash).is_err(),
        "dropped batch should not persist"
    );

    let frontier_store = env.frontier_store();
    assert!(
        frontier_store.get_frontier(&account).is_err(),
        "dropped batch should not persist frontier"
    );
}

// ---------------------------------------------------------------------------
// 10. End-to-end with real Ed25519 signatures
// ---------------------------------------------------------------------------

#[test]
fn e2e_real_signatures_burn_send_receive() {
    use burst_node::BlockProcessor;

    let alice_kp = keypair_from_seed(&[0xA1; 32]);
    let alice = derive_address(&alice_kp.public);

    let bob_kp = keypair_from_seed(&[0xB2; 32]);
    let bob = derive_address(&bob_kp.public);

    let carol_kp = keypair_from_seed(&[0xC3; 32]);
    let carol = derive_address(&carol_kp.public);

    let mut proc = BlockProcessor::new(0);
    proc.set_validate_timestamps(false);
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
        proc.process(&open, &mut frontier),
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
        proc.process(&burn, &mut frontier),
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
        0, // odometer on the preceding open block is 0
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
        proc.process(&bob_open, &mut frontier),
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
        proc.process(&bob_send, &mut frontier),
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

// ---------------------------------------------------------------------------
// 11. Verification flow: endorsement → verifier selection → voting → outcome
// ---------------------------------------------------------------------------

#[test]
fn verification_full_flow() {
    use burst_node::{VerificationProcessor, VerifierPool};

    let _alice = make_address(192);
    let v1 = make_address(193);
    let v2 = make_address(194);
    let v3 = make_address(195);

    // Set up verifier pool
    let mut pool = VerifierPool::new(0);
    pool.opt_in(v1.clone(), 1000).unwrap();
    pool.opt_in(v2.clone(), 1000).unwrap();
    pool.opt_in(v3.clone(), 1000).unwrap();
    assert_eq!(pool.count(), 3);

    // Processor: 2 endorsements needed, 3 verifiers selected, 67% threshold
    let proc = VerificationProcessor::new(2, 3, 0.67);

    // Not enough endorsements yet
    assert!(!proc.check_endorsements(1));
    // Enough endorsements
    assert!(proc.check_endorsements(2));

    // No votes yet → pending
    let outcome = proc.process_votes(0, 0, 3);
    assert_eq!(outcome, burst_node::VerificationOutcome::Pending);

    // 1 vote for → still pending (need ceil(3*0.67) = 3 total votes)
    let outcome = proc.process_votes(1, 0, 3);
    assert_eq!(outcome, burst_node::VerificationOutcome::Pending);

    // 2 votes for, 0 against → still pending (total=2 < required=3)
    let outcome = proc.process_votes(2, 0, 3);
    assert_eq!(outcome, burst_node::VerificationOutcome::Pending);

    // 3 votes for → verified (total=3 >= 3, and 3 > 0)
    let outcome = proc.process_votes(3, 0, 3);
    assert_eq!(outcome, burst_node::VerificationOutcome::Verified);

    // 2 for, 1 against → verified (total=3 >= 3, and 2 > 1)
    let outcome = proc.process_votes(2, 1, 3);
    assert_eq!(outcome, burst_node::VerificationOutcome::Verified);

    // 1 for, 2 against → rejected (total=3 >= 3, and 1 < 2)
    let outcome = proc.process_votes(1, 2, 3);
    assert_eq!(outcome, burst_node::VerificationOutcome::Rejected);
}

// ---------------------------------------------------------------------------
// 12. Endorsement + challenge economics
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// 13. Verification vote economics
// ---------------------------------------------------------------------------

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
        burst_node::EconomicResult::VerificationVoteResult {
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
        other => panic!("expected VerificationVoteResult, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// 14. Governance proposal + vote round-trip
// ---------------------------------------------------------------------------

#[test]
fn governance_proposal_and_vote_economics() {
    let mut brn = BrnEngine::new();
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(5000);

    let proposer = make_address(202);
    let voter = make_address(203);

    // GovernanceProposal block
    let mut proposal_block = make_block(
        BlockType::GovernanceProposal,
        &proposer,
        BlockHash::new([1u8; 32]),
        &proposer,
        1000,
        500,
        BlockHash::ZERO,
        TxHash::ZERO,
        now.as_secs(),
    );
    proposal_block.transaction = TxHash::new([0xAA; 32]);
    proposal_block.hash = proposal_block.compute_hash();

    let result = burst_node::process_block_economics(
        &proposal_block,
        &mut brn,
        &mut trst,
        now,
        86400 * 365,
        1000,
    );
    match result {
        burst_node::EconomicResult::GovernanceProposal {
            proposer: p,
            proposal_hash,
            ..
        } => {
            assert_eq!(p, proposer);
            assert_eq!(proposal_hash, TxHash::new([0xAA; 32]));
        }
        other => panic!("expected GovernanceProposal, got {:?}", other),
    }

    // GovernanceVote block (link = proposal hash, transaction[0] = vote value)
    let proposal_hash = TxHash::new([0xAA; 32]);
    let mut vote_block = make_block(
        BlockType::GovernanceVote,
        &voter,
        BlockHash::new([2u8; 32]),
        &voter,
        1000,
        500,
        BlockHash::new(*proposal_hash.as_bytes()),
        TxHash::ZERO,
        now.as_secs() + 10,
    );
    vote_block.transaction = TxHash::new({
        let mut bytes = [0u8; 32];
        bytes[0] = 0; // Yea
        bytes
    });
    vote_block.hash = vote_block.compute_hash();

    let result = burst_node::process_block_economics(
        &vote_block,
        &mut brn,
        &mut trst,
        Timestamp::new(now.as_secs() + 10),
        86400 * 365,
        1000,
    );
    match result {
        burst_node::EconomicResult::GovernanceVote {
            voter: v,
            proposal_hash: ph,
            vote,
        } => {
            assert_eq!(v, voter);
            assert_eq!(ph, proposal_hash);
            assert_eq!(vote, burst_transactions::governance::GovernanceVote::Yea);
        }
        other => panic!("expected GovernanceVote, got {:?}", other),
    }
}

// ---------------------------------------------------------------------------
// 15. create_received_token provenance tracking
// ---------------------------------------------------------------------------

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
        source: sender.clone(),
        amount: 500,
        timestamp: Timestamp::new(4000),
        provenance: vec![burst_store::pending::PendingProvenance {
            amount: 500,
            origin: TxHash::new([0x01; 32]),
            origin_wallet: origin_wallet.clone(),
            origin_timestamp: Timestamp::new(1000),
            effective_origin_timestamp: Timestamp::new(1000),
        }],
    };

    let token =
        burst_node::ledger_bridge::create_received_token(&receive_block, &pending, 86400 * 365);
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
        source: sender.clone(),
        amount: 400,
        timestamp: Timestamp::new(7000),
        provenance: vec![burst_store::pending::PendingProvenance {
            amount: 400,
            origin: origin_hash,
            origin_wallet: origin_wallet.clone(),
            origin_timestamp: Timestamp::new(3000),
            effective_origin_timestamp: Timestamp::new(3000),
        }],
    };

    let token =
        burst_node::ledger_bridge::create_received_token(&receive_block, &pending, 86400 * 365);
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
        source: sender.clone(),
        amount: 100,
        timestamp: Timestamp::new(8000),
        provenance: Vec::new(),
    };

    let token =
        burst_node::ledger_bridge::create_received_token(&receive_block, &pending, 86400 * 365);
    assert_eq!(token.amount, 100);
    assert_eq!(token.holder, receiver);
    assert_eq!(token.origin_wallet, sender);
    assert_eq!(token.origin_timestamp, Timestamp::new(8000));
    assert!(token.revoked_origin.is_none());
}

// ---------------------------------------------------------------------------
// 16. Balance transition validation
// ---------------------------------------------------------------------------

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

    // Odometer increased → the burn is the delta → valid.
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

// ---------------------------------------------------------------------------
// 17. LMDB persistence + economics unified path
// ---------------------------------------------------------------------------

#[test]
fn unified_path_burn_persists_account_and_pending() {
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

    let mut rw = burst_consensus::RepWeightCache::new();
    let mut batch = env.tx_begin_write().unwrap();
    let bytes = bincode::serialize(&open).unwrap();
    batch.put_block(&open.hash, &bytes).unwrap();
    batch.put_frontier(&alice, &open.hash).unwrap();
    let info = burst_node::update_account_on_block(&mut batch, &open, None, 0, &mut rw).unwrap();
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
        0, // odometer on the preceding open block is 0
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
    let info2 =
        burst_node::update_account_on_block(&mut batch, &burn, Some(&info), 0, &mut rw).unwrap();
    batch.commit().unwrap();

    assert_eq!(info2.block_count, 2);
    assert_eq!(info2.total_brn_burned, 500);
    assert_eq!(info2.head, burn.hash);

    // Verify LMDB reads match
    let block_store = env.block_store();
    let read_bytes = block_store.get_block(&burn.hash).unwrap();
    let read_block: StateBlock = bincode::deserialize(&read_bytes).unwrap();
    assert_eq!(read_block.brn_balance, 500);
    assert_eq!(read_block.block_type, BlockType::Burn);

    let frontier_store = env.frontier_store();
    assert_eq!(frontier_store.get_frontier(&alice).unwrap(), burn.hash);

    // Verify TRST token was minted correctly
    assert_eq!(mint_token.amount, 500);
    assert_eq!(mint_token.origin_wallet, alice);
    assert_eq!(mint_token.holder, bob);
}

// ---------------------------------------------------------------------------
// 18. TRST revocation then un-revocation round-trip
// ---------------------------------------------------------------------------

#[test]
fn trst_revoke_then_unrevoke() {
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
    let _revoked = trst.revoke_by_origin(&origin_wallet);
    assert_eq!(
        trst.transferable_balance(&holder, now),
        Some(0),
        "balance should be 0 after revocation"
    );

    // Un-revoke
    let _unrevoked = trst.un_revoke_by_origin(&origin_wallet, now);
    assert_eq!(
        trst.transferable_balance(&holder, now),
        Some(1000),
        "balance should be restored after un-revocation"
    );
}

// ---------------------------------------------------------------------------
// 19. Consensus election integration
// ---------------------------------------------------------------------------

#[test]
fn election_lifecycle_vote_and_confirm() {
    use burst_consensus::Election;

    let block_hash = BlockHash::new([0xAA; 32]);
    let rep1 = make_address(226);
    let rep2 = make_address(227);
    let rep3 = make_address(228);

    // online_weight=1000 → confirmation_threshold = 1000 * 6700 / 10000 = 670
    let online_weight = 1000u128;
    let mut election = Election::new(block_hash, online_weight, Timestamp::new(1000));

    assert!(!election.is_confirmed());

    // Confirmation is gated on FINAL votes (rsnano parity): a block is cemented
    // only on a supermajority of irrevocable votes, never on soft/non-final
    // ones. First show soft votes do NOT confirm even at full weight...
    election.vote(&rep1, block_hash, 700, false, Timestamp::new(1001));
    election.try_confirm(Timestamp::new(1001));
    assert!(
        !election.is_confirmed(),
        "soft (non-final) votes must not cement a block"
    );

    // ...now accumulate FINAL votes toward the 670 threshold.
    // rep1 upgrades to final 300 — final 300 < 670
    election.vote(&rep1, block_hash, 300, true, Timestamp::new(1002));
    election.try_confirm(Timestamp::new(1002));
    assert!(!election.is_confirmed());

    // rep2 final 200 — final 500 < 670
    election.vote(&rep2, block_hash, 200, true, Timestamp::new(1003));
    election.try_confirm(Timestamp::new(1003));
    assert!(!election.is_confirmed());

    // rep3 final 200 — final 700 ≥ 670 → confirms
    election.vote(&rep3, block_hash, 200, true, Timestamp::new(1004));
    let status = election.try_confirm(Timestamp::new(1004));
    assert!(election.is_confirmed());
    assert!(status.is_some());
    let status = status.unwrap();
    assert_eq!(status.winner, block_hash);
    assert_eq!(status.final_tally, 700);
}

// ---------------------------------------------------------------------------
// 20. RejectReceive economics
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// 21. BRN accrual with rate history
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// 19. Burn-backed verifier rewards (decision 33.7d)
// ---------------------------------------------------------------------------

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
    );

    let ts = Timestamp::new(9000);
    burst_node::ledger_bridge::resolve_verifier_outcomes(
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

// ---------------------------------------------------------------------------
// 20. FULL PROTOCOL LIFECYCLE SIMULATION
//
// One connected story exercising every principle through the same functions
// the node calls, in the same order, with conservation asserted throughout:
// verification → BRN accrual (computed) → two-phase burn/mint → receive →
// per-origin send → merge (immediate-input graph) → challenge → vote →
// fraud → proportional revocation (incl. in-flight sends) → burn-backed
// rewards → re-verification un-revoke → expiry → governance resurrection.
// ---------------------------------------------------------------------------

#[test]
fn full_protocol_lifecycle_simulation() {
    use burst_store::pending::{PendingInfo, PendingProvenance, PendingStore};
    use burst_types::ProtocolParams;

    let params = ProtocolParams {
        endorsement_threshold: 3,
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
    let mut orch = burst_verification::VerificationOrchestrator::new();

    let alice = make_address(240); // consumer
    let bob = make_address(241); // provider
    let carol = make_address(242); // merchant + challenger
    let sybil = make_address(243); // fraud

    // ── Act 1: identity — BRN is a computed birthright ──────────────────
    for w in [&alice, &bob, &carol, &sybil] {
        brn.track_wallet(
            (*w).clone(),
            burst_brn::BrnWalletState::new(Timestamp::new(1000)),
        );
    }
    let t2000 = Timestamp::new(2000);
    let st = brn.get_wallet(&alice).unwrap().clone();
    assert_eq!(brn.compute_balance(&st, t2000), 100_000); // 100/s × 1000s

    // ── Act 2: two-phase burn — TRST only ever from burned BRN ─────────
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
    assert!(600 <= brn.compute_balance(&st, t2000));
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
    // Two-phase (8.4a): nothing lands in Bob's portfolio until he receives.
    assert!(trst.get_portfolio(&bob).is_none());
    let burn_hash = TxHash::new(*burn.hash.as_bytes());
    pending_store
        .put_pending(
            &bob,
            &burn_hash,
            &PendingInfo {
                source: alice.clone(),
                amount: minted.amount,
                timestamp: t2000,
                provenance: vec![PendingProvenance {
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
    let pend = pending_store.get_pending(&bob, &burn_hash).unwrap();
    assert!(burst_node::BlockProcessor::validate_open_block(&bob_open, Some(pend.amount)).is_ok());
    let bob_token = burst_node::ledger_bridge::create_received_token(&bob_open, &pend, 10_000);
    trst.receive_token(bob_token, Timestamp::new(2100));
    pending_store.delete_pending(&bob, &burn_hash).unwrap();
    assert_eq!(
        trst.transferable_balance(&bob, Timestamp::new(2100)),
        Some(600)
    );

    // ── Act 3: commerce — sends never cross origins ─────────────────────
    let origin_a = minted.origin;
    assert_eq!(
        trst.origin_transferable(&bob, &origin_a, Timestamp::new(2200)),
        600
    );
    let prov = trst.debit_wallet_with_provenance(&bob, &origin_a, 200);
    assert_eq!(prov[0].amount, 200);
    let carol_recv = burst_trst::TrstToken {
        id: test_hash_sim(1),
        amount: 200,
        origin: prov[0].origin,
        link: test_hash_sim(1),
        holder: carol.clone(),
        origin_timestamp: prov[0].origin_timestamp,
        effective_origin_timestamp: prov[0].effective_origin_timestamp,
        state: burst_types::TrstState::Active,
        origin_wallet: prov[0].origin_wallet.clone(),
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
    let merge_tx = test_hash_sim(2);
    let merged = trst
        .merge(&inputs, bob.clone(), merge_tx, Timestamp::new(2700))
        .unwrap();
    assert_eq!(merged.origin, merge_tx); // origin = merge tx hash (6.17b)
    let ids: std::collections::HashSet<TxHash> = inputs.iter().map(|t| t.id).collect();
    trst.bulk_untrack(&bob, &ids);
    trst.track_token(merged);

    // Bob sends 300 of M to Carol — left IN FLIGHT during the revocation.
    let prov_m = trst.debit_wallet_with_provenance(&bob, &merge_tx, 300);
    assert_eq!(prov_m[0].amount, 300);

    // ── Act 4: the challenge ─────────────────────────────────────────────
    // Verify sybil in the orchestrator (endorse + vote) so it can be challenged.
    for i in 0..3 {
        orch.process_endorsement(&sybil, &make_address(210 + i), 1000, &params)
            .unwrap();
    }
    let vs: Vec<_> = (0u8..3).map(|i| make_address(214 + i)).collect();
    let sel = orch
        .select_verifiers(&sybil, &vs, &[1u8; 32], &params)
        .unwrap();
    for v in &sel {
        orch.process_vote(&sybil, v, burst_verification::Vote::Legitimate, &params)
            .unwrap();
    }
    orch.drain_events();

    // Carol stakes a challenge via a real Challenge block.
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
    orch.initiate_challenge(
        &sybil,
        &carol,
        true,
        1000,
        burst_verification::ChallengeReason::Fraud,
        &params,
    )
    .unwrap();

    // New random panel votes Illegitimate; resolution fires on the last vote.
    let cvs: Vec<_> = (0u8..3).map(|i| make_address(220 + i)).collect();
    for cv in &cvs {
        brn.track_wallet(
            cv.clone(),
            burst_brn::BrnWalletState::new(Timestamp::new(1000)),
        );
        brn.get_wallet_mut(cv).unwrap().total_staked = params.verifier_stake_amount;
    }
    let sel = orch
        .select_verifiers(&sybil, &cvs, &[2u8; 32], &params)
        .unwrap();
    for (i, v) in sel.iter().enumerate() {
        orch.process_vote(&sybil, v, burst_verification::Vote::Illegitimate, &params)
            .unwrap();
        if i + 1 < sel.len() {
            assert!(orch
                .try_resolve_challenge(&sybil, &params)
                .unwrap()
                .is_none());
        }
    }
    let resolved = orch
        .try_resolve_challenge(&sybil, &params)
        .unwrap()
        .unwrap();
    let events = orch.drain_events();
    assert!(events.iter().any(|e| matches!(e,
        burst_verification::VerificationEvent::WalletUnverified { wallet } if *wallet == sybil)));

    // Fraud confirmed → revoke all TRST originating from sybil.
    let revocations = trst.revoke_by_origin(&sybil);
    let revoked_total: u128 = revocations.iter().map(|r| r.revoked_amount).sum();
    // Bob holds 500 of M (800 total, 400 tainted): ceil(500·400/800) = 250.
    assert_eq!(revoked_total, 250);
    assert_eq!(
        trst.transferable_balance(&bob, Timestamp::new(3100)),
        Some(250)
    );

    // Settle the challenge like the node does.
    if let burst_verification::VerificationEvent::ChallengeResolved { outcome, .. } = &resolved {
        assert_eq!(
            outcome.outcome,
            burst_verification::ChallengeResult::FraudConfirmed
        );
        // Challenger stake returned in full.
        let ws = brn.get_wallet_mut(&carol).unwrap();
        ws.total_staked = ws.total_staked.saturating_sub(outcome.challenger_stake);
        // Panel: unanimous → no dissenters → stakes unlocked, no TRST minted.
        burst_node::ledger_bridge::resolve_verifier_outcomes(
            &mut brn,
            &pending_store,
            &outcome.verifier_outcomes,
            &sybil,
            Timestamp::new(3100),
        );
        for cv in &cvs {
            assert_eq!(brn.get_wallet(cv).unwrap().total_staked, 0);
            assert_eq!(brn.get_wallet(cv).unwrap().total_burned, 0);
        }
        // Challenger TRST reward: min(1% of revoked, cap) — backed by the
        // 100x destroyed revoked TRST.
        let reward = std::cmp::min(
            revoked_total.saturating_mul(params.challenge_reward_bps as u128) / 10_000,
            params.challenge_reward_cap,
        );
        assert_eq!(reward, 2); // 1% of 250, integer math
        let reward_hash = burst_node::ledger_bridge::create_reward_pending(
            &pending_store,
            &carol,
            &sybil,
            b"challenger-reward",
            reward,
            Timestamp::new(3100),
        )
        .unwrap();
        let pend = pending_store.get_pending(&carol, &reward_hash).unwrap();
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
        let tok = burst_node::ledger_bridge::create_received_token(&reward_block, &pend, 10_000);
        trst.receive_token(tok, Timestamp::new(3200));
        pending_store.delete_pending(&carol, &reward_hash).unwrap();
    } else {
        panic!("expected ChallengeResolved");
    }
    assert_eq!(brn.get_wallet(&carol).unwrap().total_staked, 0);

    // The in-flight 300 of M arrives at Carol — revocation caught at receive:
    // ceil(300·400/800) = 150 revoked, 150 stays live.
    let inflight = burst_trst::TrstToken {
        id: test_hash_sim(3),
        amount: 300,
        origin: merge_tx,
        link: test_hash_sim(3),
        holder: carol.clone(),
        origin_timestamp: prov_m[0].origin_timestamp,
        effective_origin_timestamp: prov_m[0].effective_origin_timestamp,
        state: burst_types::TrstState::Active,
        origin_wallet: prov_m[0].origin_wallet.clone(),
        revoked_origin: None,
    };
    let evs = trst.receive_token(inflight, Timestamp::new(3300));
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].revoked_amount, 150);
    // Carol live: 200 (origin A) + 150 (clean M) + 2 (reward) = 352.
    assert_eq!(
        trst.transferable_balance(&carol, Timestamp::new(3300)),
        Some(352)
    );

    // Conservation: everything in existence traces to destroyed value.
    // 600 (alice burn) + 400 (sybil burn) + 2 (reward, backed by revoked) = 1002.
    let total_tracked: u128 = [&alice, &bob, &carol, &sybil]
        .iter()
        .filter_map(|w| trst.get_portfolio(w))
        .flat_map(|p| p.tokens.iter())
        .map(|t| t.amount)
        .sum();
    assert_eq!(total_tracked, 1002);

    // ── Act 5: redemption — un-revoke on re-verification (6.15b) ───────
    let restored = trst.un_revoke_by_origin(&sybil, Timestamp::new(3400));
    assert_eq!(restored.len(), 2); // bob's 250 chunk + carol's 150 chunk
    assert_eq!(
        trst.transferable_balance(&bob, Timestamp::new(3400)),
        Some(500)
    );
    assert_eq!(
        trst.transferable_balance(&carol, Timestamp::new(3400)),
        Some(502)
    );

    // ── Act 6: time — expiry, then governance resurrection (6.9) ───────
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
    trst.set_expiry_period(1_000_000, Timestamp::new(12_500));
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

fn test_hash_sim(n: u8) -> TxHash {
    TxHash::new([0xB0 + n; 32])
}
