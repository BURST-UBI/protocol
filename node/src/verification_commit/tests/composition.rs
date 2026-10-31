use super::*;
use burst_store::pending::{PendingInfo, PendingProvenance};
use burst_types::{TrstBasis, TrstState};

fn send(f: &mut Fixture, sender: u8, receiver: u8, origin: TxHash, amount: u128, hash: TxHash) {
    let portions = f
        .trst
        .debit_wallet_with_provenance(&address(sender), &origin, amount);
    assert_eq!(portions.iter().map(|p| p.amount).sum::<u128>(), amount);
    let pending = PendingInfo {
        return_credit: false,
        source: address(sender),
        amount,
        timestamp: test_time(200),
        provenance: portions
            .into_iter()
            .map(|p| PendingProvenance {
                basis: p.basis,
                amount: p.amount,
                origin: p.origin,
                origin_wallet: p.origin_wallet,
                origin_timestamp: p.origin_timestamp,
                effective_origin_timestamp: p.effective_origin_timestamp,
                revoked_amounts: Default::default(),
            })
            .collect(),
    };
    f.store
        .pending_store()
        .put_pending(&address(receiver), &hash, &pending)
        .unwrap();
}

fn receive(f: &mut Fixture, receiver: u8, hash: TxHash) {
    let pending = f
        .store
        .pending_store()
        .get_pending(&address(receiver), &hash)
        .unwrap();
    let mut template =
        crate::block_economics::create_returned_token(&address(receiver), &hash, &pending);
    template.holder = address(receiver);
    crate::block_economics::deliver_pending(&mut f.trst, &pending, template, test_time(300))
        .unwrap();
    f.store
        .pending_store()
        .delete_pending(&address(receiver), &hash)
        .unwrap();
}

fn revoke(f: &mut Fixture, seed: u8) -> u128 {
    let (held, effects) = f.trst.revoke_with_pending_effects(&address(seed));
    let mut batch = f.store.tx_begin_write().unwrap();
    let pending = revoke_pending(&mut batch, &effects, &mut f.trst).unwrap();
    let mut account = batch.get_account(&address(seed)).unwrap();
    account.state = WalletState::Revoked;
    batch.put_account_info(&account, false).unwrap();
    crate::ledger_updater::persist_economics(&mut batch, &f.brn, &f.trst).unwrap();
    batch.commit().unwrap();
    held.iter().map(|event| event.revoked_amount).sum::<u128>() + pending
}

fn restore(f: &mut Fixture, seed: u8, time: u64) {
    let mut batch = f.store.tx_begin_write().unwrap();
    mark_wallet_verified(
        &mut batch,
        &address(seed),
        test_time(time),
        &mut f.brn,
        &mut f.trst,
    )
    .unwrap();
    crate::ledger_updater::persist_economics(&mut batch, &f.brn, &f.trst).unwrap();
    batch.commit().unwrap();
}

fn merged_fixture() -> (Fixture, TxHash) {
    let mut f = Fixture::new();
    let mut tokens = Vec::new();
    for (seed, amount) in [(0, 40), (1, 30), (2, 30)] {
        tokens.push(
            f.trst
                .mint(
                    TxHash::new([seed + 1; 32]),
                    address(5),
                    amount,
                    address(seed),
                    test_time(100),
                )
                .unwrap(),
        );
    }
    let origin = TxHash::new([20; 32]);
    let merged = f
        .trst
        .merge(&tokens, address(5), origin, test_time(150))
        .unwrap();
    f.trst.track_token(merged);
    let mut batch = f.store.tx_begin_write().unwrap();
    batch
        .put_meta(
            crate::endorsement_commit::SNAPSHOT_KEY,
            &f.orchestrator.snapshot().encode().unwrap(),
        )
        .unwrap();
    batch.commit().unwrap();
    (f, origin)
}

#[test]
fn stable_retained_revocation_restoration_survives_storage_and_restart() {
    let (mut f, _) = merged_fixture();
    let period = f.trst.expiry_secs;
    let mut batch = f.store.tx_begin_write().unwrap();
    crate::graph_storage::stage_replayed(&mut batch, &f.trst.merger_graph).unwrap();
    crate::trst_storage::stage_replayed_stable(&mut batch, &f.trst).unwrap();
    crate::ledger_updater::persist_brn(&mut batch, &f.brn).unwrap();
    f.trst = batch.read_trst_engine(period).unwrap();
    batch.commit().unwrap();
    assert_eq!(revoke(&mut f, 0), 40);
    f = f.reopen();
    let batch = f.store.tx_begin_write().unwrap();
    f.trst = batch.read_trst_engine(period).unwrap();
    batch.commit().unwrap();
    assert_eq!(f.trst.transferable_balance_snapshot(&address(5)), Some(60));
    restore(&mut f, 0, 250);
    f = f.reopen();
    let batch = f.store.tx_begin_write().unwrap();
    f.trst = batch.read_trst_engine(period).unwrap();
    batch.commit().unwrap();
    assert_eq!(f.trst.transferable_balance_snapshot(&address(5)), Some(100));
    assert_eq!(revoke(&mut f, 1), 30);
    f = f.reopen();
    assert_eq!(f.trst.transferable_balance_snapshot(&address(5)), Some(70));
}

#[test]
fn old_pending_has_no_receive_timeout_and_remains_claimable_after_restart() {
    let year = 365 * 24 * 60 * 60;
    let tick_seconds = test_time(1).as_secs() - test_time(0).as_secs();
    for expiry in [u64::MAX, 2 * year] {
        let mut f = Fixture::new();
        f.params.trst_expiry_secs = expiry;
        f.trst.set_expiry_period(expiry, test_time(0)).unwrap();
        let mut batch = f.store.tx_begin_write().unwrap();
        batch
            .put_meta(
                crate::endorsement_commit::SNAPSHOT_KEY,
                &f.orchestrator.snapshot().encode().unwrap(),
            )
            .unwrap();
        batch.commit().unwrap();
        let destination =
            BlockHash::new(burst_crypto::decode_address(address(5).as_str()).unwrap());
        let burn = f
            .payment(0, BlockType::Burn, 100, (destination, TxHash::ZERO), 200)
            .unwrap();
        let source = burn.into_tx_hash();
        let original = f
            .store
            .pending_store()
            .get_pending(&address(5), &source)
            .unwrap();
        let recipient_head = f
            .store
            .account_store()
            .get_account(&address(5))
            .unwrap()
            .head;
        for elapsed in [31 * 24 * 60 * 60, year] {
            assert_eq!(elapsed % tick_seconds, 0);
            let now = test_time(200 + elapsed / tick_seconds);
            assert_eq!(now.as_secs() - test_time(200).as_secs(), elapsed);
            crate::expiry_commit::apply(&f.store, &mut f.trst, now).unwrap();
            f = f.reopen();
            let pending = f
                .store
                .pending_store()
                .get_pending(&address(5), &source)
                .unwrap();
            assert_eq!(
                bincode::serialize(&pending).unwrap(),
                bincode::serialize(&original).unwrap()
            );
            assert!(!pending.return_credit);
            assert_eq!(f.store.pending_store().pending_count().unwrap(), 1);
            assert_eq!(
                f.store
                    .account_store()
                    .get_account(&address(5))
                    .unwrap()
                    .head,
                recipient_head
            );
        }
        f.payment(
            5,
            BlockType::Receive,
            100,
            (burn, source),
            201 + year / tick_seconds,
        )
        .unwrap();
        f = f.reopen();
        assert_eq!(f.store.pending_store().pending_count().unwrap(), 0);
        let portfolio = f.trst.get_portfolio(&address(5)).unwrap();
        assert_eq!(portfolio.tokens.len(), 1);
        assert_eq!(portfolio.tokens[0].amount, 100);
        assert_eq!(portfolio.tokens[0].origin, source);
        assert_eq!(portfolio.tokens[0].origin_timestamp, test_time(200));
        assert_eq!(portfolio.tokens[0].state, TrstState::Active);
        assert_eq!(
            f.trst
                .transferable_balance(&address(5), test_time(202 + year / tick_seconds)),
            Some(100)
        );
    }
}

#[test]
fn revoked_holder_keeps_clean_trst_and_spends_it_after_reverification_and_restart() {
    let mut f = Fixture::new();
    let mut batch = f.store.tx_begin_write().unwrap();
    batch
        .put_meta(
            crate::endorsement_commit::SNAPSHOT_KEY,
            &f.orchestrator.snapshot().encode().unwrap(),
        )
        .unwrap();
    batch.commit().unwrap();
    let destination =
        |seed| BlockHash::new(burst_crypto::decode_address(address(seed).as_str()).unwrap());
    let burn = f
        .payment(0, BlockType::Burn, 100, (destination(5), TxHash::ZERO), 200)
        .unwrap();
    let origin = burn.into_tx_hash();
    f.payment(5, BlockType::Receive, 100, (burn, origin), 201)
        .unwrap();
    let before = f.trst.get_portfolio(&address(5)).unwrap().tokens.clone();
    let head = f
        .store
        .account_store()
        .get_account(&address(5))
        .unwrap()
        .head;
    assert_eq!(
        revoke(&mut f, 5),
        0,
        "holder revocation must not revoke another originator's TRST"
    );
    f = f.reopen();
    assert_eq!(f.trst.get_portfolio(&address(5)).unwrap().tokens, before);
    assert_eq!(
        f.store
            .account_store()
            .get_account(&address(5))
            .unwrap()
            .state,
        WalletState::Revoked
    );
    assert!(f
        .payment(5, BlockType::Send, 30, (destination(6), origin), 202)
        .unwrap_err()
        .contains("spending requires a verified wallet"));
    assert_eq!(
        f.store
            .account_store()
            .get_account(&address(5))
            .unwrap()
            .head,
        head
    );
    assert_eq!(f.trst.get_portfolio(&address(5)).unwrap().tokens, before);
    restore(&mut f, 5, 203);
    f = f.reopen();
    assert_eq!(
        f.store
            .account_store()
            .get_account(&address(5))
            .unwrap()
            .state,
        WalletState::Verified
    );
    assert_eq!(f.trst.get_portfolio(&address(5)).unwrap().tokens, before);
    let send = f
        .payment(5, BlockType::Send, 30, (destination(6), origin), 204)
        .unwrap();
    f.payment(6, BlockType::Receive, 30, (send, origin), 205)
        .unwrap();
    f = f.reopen();
    for (holder, amount) in [(5, 70), (6, 30)] {
        let portfolio = f.trst.get_portfolio(&address(holder)).unwrap();
        assert_eq!(
            portfolio
                .tokens
                .iter()
                .map(|token| token.amount)
                .sum::<u128>(),
            amount
        );
        assert!(portfolio
            .tokens
            .iter()
            .all(|token| token.origin == before[0].origin
                && token.origin_timestamp == before[0].origin_timestamp
                && token.effective_origin_timestamp == before[0].effective_origin_timestamp
                && token.origin_wallet == before[0].origin_wallet
                && token.state == TrstState::Active));
    }
}

#[test]
fn expiry_return_receipt_does_not_double_count_revocation_or_restore_tainted_value() {
    let (mut f, origin) = merged_fixture();
    let source = TxHash::new([91; 32]);
    send(&mut f, 5, 6, origin, 100, source);
    f.params.trst_expiry_secs = 100;
    f.trst.set_expiry_period(100, test_time(300)).unwrap();
    crate::expiry_commit::apply(&f.store, &mut f.trst, test_time(300)).unwrap();
    assert_eq!(revoke(&mut f, 0), 40);
    restore(&mut f, 0, 310);
    assert_eq!(revoke(&mut f, 0), 40);
    assert_eq!(revoke(&mut f, 1), 30);
    let receipt = f
        .store
        .pending_store()
        .get_pending(&address(5), &source)
        .unwrap();
    assert!(receipt.return_credit);
    assert_eq!(receipt.amount, 100);
    let extended_expiry = test_time(400).as_secs() - test_time(100).as_secs();
    f.params.trst_expiry_secs = extended_expiry;
    f.trst
        .set_expiry_period(extended_expiry, test_time(320))
        .unwrap();
    let mut batch = f.store.tx_begin_write().unwrap();
    crate::ledger_updater::persist_economics(&mut batch, &f.brn, &f.trst).unwrap();
    batch.commit().unwrap();
    f.payment(
        5,
        BlockType::Receive,
        100,
        (BlockHash::new(*source.as_bytes()), origin),
        320,
    )
    .unwrap();
    assert_eq!(
        f.trst.transferable_balance(&address(5), test_time(320)),
        Some(30)
    );
    let holder = f.store.account_store().get_account(&address(5)).unwrap();
    assert_eq!((holder.trst_balance, holder.revoked_trst), (100, 70));
    let destination = BlockHash::new(burst_crypto::decode_address(address(6).as_str()).unwrap());
    assert!(f
        .payment(5, BlockType::Send, 31, (destination, origin), 321)
        .is_err());
    f.payment(5, BlockType::Send, 30, (destination, origin), 321)
        .unwrap();
    assert!(f
        .store
        .pending_store()
        .get_pending(&address(5), &source)
        .is_err());
}

#[test]
fn restored_pending_splits_keep_composition_through_delivery_resend_and_restart() {
    let (mut f, origin) = merged_fixture();
    let first = TxHash::new([21; 32]);
    send(&mut f, 5, 6, origin, 100, first);
    assert_eq!(revoke(&mut f, 0), 40);
    f = f.reopen();
    restore(&mut f, 0, 250);
    f = f.reopen();
    assert_eq!(revoke(&mut f, 1), 30);
    f = f.reopen();
    receive(&mut f, 6, first);
    assert_eq!(
        f.trst.transferable_balance(&address(6), test_time(300)),
        Some(70)
    );
    let portfolio = f.trst.get_portfolio(&address(6)).unwrap();
    assert_eq!(portfolio.tokens.len(), 3);
    assert!(portfolio.tokens.iter().any(|token| token.basis
        == TrstBasis::Single(TxHash::new([1; 32]))
        && token.amount == 40
        && token.state == TrstState::Active));

    let second = TxHash::new([22; 32]);
    send(&mut f, 6, 4, origin, 70, second);
    let pending = f
        .store
        .pending_store()
        .get_pending(&address(4), &second)
        .unwrap();
    assert_eq!(pending.provenance.len(), 2);
    assert_eq!(revoke(&mut f, 0), 40);
    f = f.reopen();
    receive(&mut f, 4, second);
    assert_eq!(
        f.trst.transferable_balance(&address(4), test_time(300)),
        Some(30)
    );
    assert_eq!(revoke(&mut f, 2), 30);
    f = f.reopen();
    assert_eq!(
        f.trst.transferable_balance(&address(4), test_time(300)),
        Some(0)
    );
}

#[test]
fn restored_pending_splits_survive_expiry_return_without_becoming_blended() {
    let (mut f, origin) = merged_fixture();
    let hash = TxHash::new([23; 32]);
    send(&mut f, 5, 6, origin, 100, hash);
    assert_eq!(revoke(&mut f, 0), 40);
    restore(&mut f, 0, 250);
    assert_eq!(revoke(&mut f, 1), 30);
    f = f.reopen();
    let expiry = Timestamp::new(test_time(100).as_secs() + f.trst.expiry_secs + 1);
    crate::expiry_commit::apply(&f.store, &mut f.trst, expiry).unwrap();
    f = f.reopen();
    assert!(f
        .store
        .pending_store()
        .get_pending(&address(6), &hash)
        .is_err());
    let holder = f.store.account_store().get_account(&address(5)).unwrap();
    assert_eq!(holder.expired_trst, 70);
    assert_eq!(holder.revoked_trst, 30);
    assert_eq!(revoke(&mut f, 0), 40);
    assert_eq!(revoke(&mut f, 2), 30);
}

#[test]
fn signed_payments_preserve_multiple_compositions_and_can_merge_the_received_remainder() {
    let mut f = Fixture::new();
    let mut batch = f.store.tx_begin_write().unwrap();
    batch
        .put_meta(
            crate::endorsement_commit::SNAPSHOT_KEY,
            &f.orchestrator.snapshot().encode().unwrap(),
        )
        .unwrap();
    batch.commit().unwrap();
    let destination =
        |seed| BlockHash::new(burst_crypto::decode_address(address(seed).as_str()).unwrap());
    for (seed, amount) in [(0, 40), (1, 30), (2, 30)] {
        let burn = f
            .payment(
                seed,
                BlockType::Burn,
                amount,
                (destination(5), TxHash::ZERO),
                2000,
            )
            .unwrap();
        f.payment(
            5,
            BlockType::Receive,
            amount,
            (burn, burn.into_tx_hash()),
            2001 + seed as u64,
        )
        .unwrap();
    }
    let merge = f
        .payment(
            5,
            BlockType::Merge,
            0,
            (BlockHash::ZERO, TxHash::ZERO),
            2010,
        )
        .unwrap();
    let sent = f
        .payment(
            5,
            BlockType::Send,
            100,
            (destination(6), merge.into_tx_hash()),
            2020,
        )
        .unwrap();
    assert_eq!(revoke(&mut f, 0), 40);
    restore(&mut f, 0, 2021);
    assert_eq!(revoke(&mut f, 1), 30);
    f = f.reopen();
    f.payment(
        6,
        BlockType::Receive,
        100,
        (sent, merge.into_tx_hash()),
        2030,
    )
    .unwrap();
    assert_eq!(
        f.trst.transferable_balance(&address(6), test_time(2030)),
        Some(70)
    );
    let sent = f
        .payment(
            6,
            BlockType::Send,
            70,
            (destination(4), merge.into_tx_hash()),
            2040,
        )
        .unwrap();
    assert_eq!(
        f.store
            .pending_store()
            .get_pending(&address(4), &sent.into_tx_hash())
            .unwrap()
            .provenance
            .len(),
        2
    );
    assert_eq!(revoke(&mut f, 0), 40);
    f = f.reopen();
    f.payment(
        4,
        BlockType::Receive,
        70,
        (sent, merge.into_tx_hash()),
        2050,
    )
    .unwrap();
    let fresh = f
        .payment(3, BlockType::Burn, 20, (destination(4), TxHash::ZERO), 2060)
        .unwrap();
    f.payment(
        4,
        BlockType::Receive,
        20,
        (fresh, fresh.into_tx_hash()),
        2061,
    )
    .unwrap();
    f.payment(
        4,
        BlockType::Merge,
        0,
        (BlockHash::ZERO, TxHash::ZERO),
        2070,
    )
    .unwrap();
    f = f.reopen();
    assert_eq!(revoke(&mut f, 2), 30);
    assert_eq!(
        f.trst.transferable_balance(&address(4), test_time(2080)),
        Some(20)
    );
    assert!(f
        .payment(
            4,
            BlockType::Receive,
            70,
            (sent, merge.into_tx_hash()),
            2080
        )
        .is_err());
}
