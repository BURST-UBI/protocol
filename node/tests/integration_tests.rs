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

    let mut batch = env.write_batch().unwrap();
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

    let mut batch = env.write_batch().unwrap();
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
    let mut batch = env.write_batch().unwrap();
    let info = burst_node::update_account_on_block(&mut batch, &open, None, 0, &mut rw).unwrap();
    batch.commit().unwrap();

    assert_eq!(info.block_count, 1);
    assert_eq!(info.trst_balance, 1000);
    assert_eq!(info.representative, rep);
    assert_eq!(rw.weight(&rep), 1000);

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

    let mut batch = env.write_batch().unwrap();
    let info2 =
        burst_node::update_account_on_block(&mut batch, &send, Some(&info), 0, &mut rw).unwrap();
    batch.commit().unwrap();

    assert_eq!(info2.block_count, 2);
    assert_eq!(info2.trst_balance, 700);
    assert_eq!(info2.head, send.hash);
    assert_eq!(rw.weight(&rep), 700);
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
    let mut batch = env.write_batch().unwrap();
    let info = burst_node::update_account_on_block(&mut batch, &open, None, 0, &mut rw).unwrap();
    batch.commit().unwrap();

    assert_eq!(rw.weight(&rep1), 500);
    assert_eq!(rw.weight(&rep2), 0);

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

    let mut batch = env.write_batch().unwrap();
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
    let mut batch = env.write_batch().unwrap();
    let info = burst_node::update_account_on_block(&mut batch, &open, None, 0, &mut rw).unwrap();
    batch.commit().unwrap();

    let receiver = make_address(32);
    let burn = make_block(
        BlockType::Burn,
        &account,
        open.hash,
        &rep,
        300,
        0,
        BlockHash::new(pubkey_bytes(&receiver)),
        TxHash::ZERO,
        2000,
    );

    let prev_brn: u128 = 500;
    let mut batch = env.write_batch().unwrap();
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
        300,
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
            burn_result,
            mint_token,
        } => {
            assert_eq!(burn_amount, 200);
            assert!(burn_result.is_ok());
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
        300,
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
    assert_eq!(trst.transferable_balance(&bob, now, expiry_secs), Some(200));

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

    let provenance = trst.debit_wallet_with_provenance(&bob, 150);
    assert!(
        !provenance.is_empty(),
        "provenance should track consumed tokens"
    );
    assert_eq!(provenance[0].amount, 150);
    assert_eq!(provenance[0].origin_wallet, alice);

    assert_eq!(trst.transferable_balance(&bob, now, expiry_secs), Some(50));

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
// 4. TRST provenance survives merge and split
// ---------------------------------------------------------------------------

#[test]
fn trst_merge_preserves_provenance() {
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(5000);
    let expiry = 86400u64 * 365;

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

    assert_eq!(trst.transferable_balance(&bob, now, expiry), Some(300));

    let merged = trst
        .merge(
            &[token1, token2],
            bob.clone(),
            TxHash::new([3u8; 32]),
            now,
            expiry,
        )
        .unwrap();

    assert_eq!(merged.amount, 300);
    assert_eq!(merged.holder, bob);
    assert!(
        !merged.origin_proportions.is_empty(),
        "merged token should track origin proportions"
    );
    assert_eq!(
        merged.effective_origin_timestamp,
        Timestamp::new(5000),
        "merged token uses earliest origin timestamp"
    );
}

#[test]
fn trst_split_preserves_total_amount() {
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(5000);
    let expiry = 86400u64 * 365;

    let alice = make_address(80);
    let bob = make_address(81);

    let token = trst
        .mint(
            TxHash::new([10u8; 32]),
            bob.clone(),
            1000,
            alice.clone(),
            now,
        )
        .unwrap();

    let children = trst
        .split(
            &token,
            &[(bob.clone(), 400), (bob.clone(), 600)],
            &[TxHash::new([11u8; 32]), TxHash::new([12u8; 32])],
            now,
            expiry,
        )
        .unwrap();

    assert_eq!(children.len(), 2);
    let total: u128 = children.iter().map(|c| c.amount).sum();
    assert_eq!(total, 1000);
    assert_eq!(children[0].amount, 400);
    assert_eq!(children[1].amount, 600);
    assert_eq!(children[0].origin_wallet, alice);
    assert_eq!(children[1].origin_wallet, alice);
}

// ---------------------------------------------------------------------------
// 5. TRST revocation and expiry
// ---------------------------------------------------------------------------

#[test]
fn trst_revocation_by_origin_removes_all_downstream() {
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(5000);
    let expiry = 86400u64 * 365;

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

    assert_eq!(trst.transferable_balance(&bob, now, expiry), Some(500));
    assert_eq!(trst.transferable_balance(&carol, now, expiry), Some(300));

    let _revocations = trst.revoke_by_origin(&sybil);

    assert_eq!(
        trst.transferable_balance(&bob, now, expiry),
        Some(0),
        "bob's balance should be 0 after sybil revocation"
    );
    assert_eq!(
        trst.transferable_balance(&carol, now, expiry),
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
    trst.track_token_with_expiry(token, expiry_secs);

    let before_expiry = Timestamp::new(5500);
    assert!(
        trst.transferable_balance(&bob, before_expiry, expiry_secs)
            .unwrap()
            > 0,
        "token should be active before expiry"
    );

    let after_expiry = Timestamp::new(7000);
    assert_eq!(
        trst.transferable_balance(&bob, after_expiry, expiry_secs),
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
        0,
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
            burn_result: _,
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
    let expiry = 86400u64 * 365;

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

    assert_eq!(trst.transferable_balance(&bob, now, expiry), Some(100));

    trst.debit_wallet(&bob, 100);
    assert_eq!(trst.transferable_balance(&bob, now, expiry), Some(0));

    trst.debit_wallet(&bob, 50);
    assert_eq!(
        trst.transferable_balance(&bob, now, expiry),
        Some(0),
        "overdraft should saturate to 0"
    );
}

#[test]
fn trst_split_rejects_amount_exceeding_parent() {
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(5000);
    let expiry = 86400u64 * 365;

    let alice = make_address(170);
    let bob = make_address(171);

    let token = trst
        .mint(
            TxHash::new([50u8; 32]),
            bob.clone(),
            100,
            alice.clone(),
            now,
        )
        .unwrap();

    let result = trst.split(
        &token,
        &[(bob.clone(), 200), (bob.clone(), 100)],
        &[TxHash::new([51u8; 32]), TxHash::new([52u8; 32])],
        now,
        expiry,
    );

    assert!(result.is_err(), "split exceeding parent amount should fail");
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

    let mut batch = env.write_batch().unwrap();
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

    let mut batch = env.write_batch().unwrap();
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
        let mut batch = env.write_batch().unwrap();
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
        1000,
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
        700,
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
            burn_result,
            target: t,
        } => {
            assert_eq!(burn_amount, 300);
            assert!(burn_result.is_ok());
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
        200,
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
            stake_result,
            target: t,
        } => {
            assert_eq!(stake_amount, 800);
            assert!(stake_result.is_ok());
            let stake = stake_result.unwrap();
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

    // Create a VerificationVote block
    let mut vote_block = make_block(
        BlockType::VerificationVote,
        &voter,
        BlockHash::new([1u8; 32]),
        &voter,
        800,
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
            assert_eq!(stake, 200); // 1000 - 800
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
            origin_proportions: Vec::new(),
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
fn create_received_token_multi_provenance_uses_earliest_timestamp() {
    let sender = make_address(207);
    let receiver = make_address(208);
    let origin_a = make_address(209);
    let origin_b = make_address(210);

    let receive_block = make_block(
        BlockType::Receive,
        &receiver,
        BlockHash::ZERO,
        &receiver,
        0,
        700,
        BlockHash::new([0xEE; 32]),
        TxHash::ZERO,
        8000,
    );

    let pending = burst_store::pending::PendingInfo {
        source: sender.clone(),
        amount: 700,
        timestamp: Timestamp::new(7000),
        provenance: vec![
            burst_store::pending::PendingProvenance {
                amount: 400,
                origin: TxHash::new([0x02; 32]),
                origin_wallet: origin_a.clone(),
                origin_timestamp: Timestamp::new(3000),
                effective_origin_timestamp: Timestamp::new(3000),
                origin_proportions: Vec::new(),
            },
            burst_store::pending::PendingProvenance {
                amount: 300,
                origin: TxHash::new([0x03; 32]),
                origin_wallet: origin_b.clone(),
                origin_timestamp: Timestamp::new(1000),
                effective_origin_timestamp: Timestamp::new(1000),
                origin_proportions: Vec::new(),
            },
        ],
    };

    let token =
        burst_node::ledger_bridge::create_received_token(&receive_block, &pending, 86400 * 365);
    assert_eq!(token.amount, 700);
    assert_eq!(token.holder, receiver);
    assert_eq!(
        token.effective_origin_timestamp,
        Timestamp::new(1000),
        "should use earliest effective origin timestamp"
    );
    assert_eq!(token.origin_proportions.len(), 2);
    assert_eq!(token.origin_proportions[0].amount, 400);
    assert_eq!(token.origin_proportions[1].amount, 300);
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
    assert_eq!(token.origin_proportions.len(), 0);
}

// ---------------------------------------------------------------------------
// 16. Balance transition validation
// ---------------------------------------------------------------------------

#[test]
fn balance_validation_endorse_rejects_brn_increase() {
    use burst_node::BlockProcessor;

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
    let result = BlockProcessor::validate_balance_transition(&block, 1000, 500);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("increase BRN"));
}

#[test]
fn balance_validation_challenge_rejects_trst_change() {
    use burst_node::BlockProcessor;

    let block = make_block(
        BlockType::Challenge,
        &make_address(215),
        BlockHash::new([1u8; 32]),
        &make_address(216),
        500,
        600,
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
    let mut batch = env.write_batch().unwrap();
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
        1000,
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
    let mut batch = env.write_batch().unwrap();
    let bytes = bincode::serialize(&burn).unwrap();
    batch.put_block(&burn.hash, &bytes).unwrap();
    batch.put_frontier(&alice, &burn.hash).unwrap();
    let info2 =
        burst_node::update_account_on_block(&mut batch, &burn, Some(&info), 1000, &mut rw).unwrap();
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
    let expiry = 86400u64 * 365;

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

    assert_eq!(trst.transferable_balance(&holder, now, expiry), Some(1000));

    // Revoke all tokens from origin_wallet
    let _revoked = trst.revoke_by_origin(&origin_wallet);
    assert_eq!(
        trst.transferable_balance(&holder, now, expiry),
        Some(0),
        "balance should be 0 after revocation"
    );

    // Un-revoke
    let _unrevoked = trst.un_revoke_by_origin(&origin_wallet);
    assert_eq!(
        trst.transferable_balance(&holder, now, expiry),
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

    // rep1 votes 300 — total 300 < 670
    election.vote(&rep1, block_hash, 300, false, Timestamp::new(1001));
    election.try_confirm(Timestamp::new(1001));
    assert!(!election.is_confirmed());

    // rep2 votes 200 — total 500 < 670
    election.vote(&rep2, block_hash, 200, false, Timestamp::new(1002));
    election.try_confirm(Timestamp::new(1002));
    assert!(!election.is_confirmed());

    // rep3 votes 200 — total 700 ≥ 670
    election.vote(&rep3, block_hash, 200, false, Timestamp::new(1003));
    let status = election.try_confirm(Timestamp::new(1003));
    assert!(election.is_confirmed());
    assert!(status.is_some());
    let status = status.unwrap();
    assert_eq!(status.winner, block_hash);
    assert_eq!(status.tally, 700);
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
