use super::*;
use burst_store::MetaStore;
use burst_trst::TrstEngine;

fn fixture() -> TrstEngine {
    let mut engine = TrstEngine::with_expiry(100);
    for id in 1..=4 {
        let owner = WalletAddress::new(format!("brst_holder_{id}"));
        let token = engine
            .mint(
                TxHash::new([id; 32]),
                owner.clone(),
                50,
                owner,
                Timestamp::new(100),
            )
            .unwrap();
        engine.track_token(token);
    }
    engine
}
fn seed(batch: &mut LmdbWriteTransaction<'_>, engine: &TrstEngine, uses_row_storage: bool) {
    if uses_row_storage {
        engine
            .visit_wallet_records(|record| {
                let (key, bytes) = record.storage_entry().unwrap();
                batch.put_meta(&key, &bytes)
            })
            .unwrap();
        engine
            .merger_graph
            .visit_records(|record| {
                let (key, bytes) = record.storage_entry().unwrap();
                batch.put_meta(&key, &bytes)
            })
            .unwrap();
        batch.put_meta(HOLDINGS_FORMAT, &[1]).unwrap();
        batch.put_meta(GRAPH_FORMAT, &[1]).unwrap();
    } else {
        batch
            .put_meta(TrstEngine::meta_key(), &engine.save_wallets())
            .unwrap();
        batch
            .put_meta(GRAPH_SNAPSHOT, &engine.merger_graph.to_bytes())
            .unwrap();
    }
}
fn delta(batch: &mut LmdbWriteTransaction<'_>, before: &TrstEngine, after: &TrstEngine) {
    after
        .visit_wallet_record_delta(before, |old, new| {
            if let Some(record) = new {
                let (key, bytes) = record.storage_entry().unwrap();
                batch.put_meta(&key, &bytes)
            } else {
                batch.delete_meta(&old.unwrap().storage_entry().unwrap().0)
            }
        })
        .unwrap();
}
fn reduced(engine: &TrstEngine, amount: u128) -> TrstEngine {
    let mut after = engine.clone();
    let portfolio = after
        .wallets
        .get_mut(&WalletAddress::new("brst_holder_1"))
        .unwrap();
    portfolio.tokens[0].amount = amount;
    portfolio.cached_transferable = amount;
    after
}
fn assert_matches_full_restore(batch: &LmdbWriteTransaction<'_>, cached: &TrstEngine) {
    let mut full = batch.read_trst_holdings(100).unwrap();
    full.merger_graph = batch.read_merger_graph().unwrap();
    let full = RestoredTrstState::from_engine(full).unwrap().into_engine();
    assert_eq!(cached.save_wallets(), full.save_wallets());
    let owner = WalletAddress::new("brst_holder_1");
    let mut actual = cached.clone();
    let mut expected = full;
    actual.revoke_by_originator(&owner);
    expected.revoke_by_originator(&owner);
    assert_eq!(actual.save_wallets(), expected.save_wallets());
}

#[test]
fn changed_portfolios_and_dirty_commits_reuse_unrelated_roots() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let initial = fixture();
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch, &initial, true);
    let baseline = batch.read_trst_engine(100).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(baseline
        .wallets
        .shares_storage_with(&batch.read_trst_engine(100).unwrap().wallets));
    let sender = WalletAddress::new("brst_holder_1");
    let receiver = WalletAddress::new("brst_receiver");
    let other = WalletAddress::new("brst_holder_4");
    let mut after = initial.clone();
    let mut portfolio = (*after.wallets.remove(&sender).unwrap()).clone();
    portfolio.tokens[0].holder = receiver.clone();
    after.wallets.insert(receiver.clone(), portfolio.into());
    delta(&mut batch, &initial, &after);
    let cached = batch.read_trst_engine(100).unwrap();
    assert_matches_full_restore(&batch, &cached);
    assert!(std::ptr::eq(
        &**baseline.wallets.get(&other).unwrap(),
        &**cached.wallets.get(&other).unwrap()
    ));
    assert!(baseline.wallets.contains_key(&sender));
    // Do not read after staging the reverse change: dirty owners must survive commit.
    delta(&mut batch, &after, &initial);
    batch.commit().unwrap();
    let batch = store.tx_begin_write().unwrap();
    let cached = batch.read_trst_engine(100).unwrap();
    assert_matches_full_restore(&batch, &cached);
    assert!(std::ptr::eq(
        &**baseline.wallets.get(&other).unwrap(),
        &**cached.wallets.get(&other).unwrap()
    ));
    assert_eq!(cached.save_wallets(), initial.save_wallets());
}

#[test]
fn corruption_period_and_provenance_changes_cannot_reuse_stale_validation() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let initial = fixture();
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch, &initial, true);
    batch.read_trst_engine(100).unwrap();
    batch.commit().unwrap();
    let owner = WalletAddress::new("brst_holder_1");
    let (portfolio_key, _) = WalletRecord::portfolio_storage_keys(&owner);
    for (key, bytes) in [
        (portfolio_key.clone(), vec![255]),
        (HOLDINGS_FORMAT.into(), vec![2]),
        (GRAPH_FORMAT.into(), vec![2]),
        WalletRecord::Origin {
            owner: WalletAddress::new("brst_holder_2"),
            origin: TxHash::new([1; 32]),
        }
        .storage_entry()
        .unwrap(),
        GraphRecord::RevokedOrigin(TxHash::new([99; 32]))
            .storage_entry()
            .unwrap(),
    ] {
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(&key, &bytes).unwrap();
        assert!(batch.read_trst_engine(100).is_err(), "key {key}");
        assert!(matches!(batch.read_view().read_trst_engine_for_comparison_with_poll(100, &|_| Ok(())),
            Err(StoreError::Corruption(_))), "checked reader accepted {key}");
    }
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch.read_trst_engine(200).is_err());
    assert!(batch.read_trst_engine(100).is_ok());
    let bad = WalletRecord::Portfolio {
        holder: owner.clone(),
        cached_transferable: 51,
        earliest_expiry: Some(Timestamp::new(200)),
    }
    .storage_entry()
    .unwrap();
    batch.put_meta(&bad.0, &bad.1).unwrap();
    assert!(batch.read_trst_engine(100).is_err());
    let good = WalletRecord::Portfolio {
        holder: owner,
        cached_transferable: 50,
        earliest_expiry: Some(Timestamp::new(200)),
    }
    .storage_entry()
    .unwrap();
    batch.put_meta(&good.0, &good.1).unwrap();
    assert_matches_full_restore(&batch, &batch.read_trst_engine(100).unwrap());
    drop(batch);
    store
        .meta_store()
        .put_meta(&portfolio_key, b"corrupt")
        .unwrap();
    assert!(store
        .tx_begin_write()
        .unwrap()
        .read_trst_engine(100)
        .is_err());
}

#[test]
fn legacy_writes_projection_clear_and_reopen_do_not_retain_old_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let initial = fixture();
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch, &initial, false);
    let original = batch.read_trst_engine(100).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(original
        .wallets
        .shares_storage_with(&batch.read_trst_engine(100).unwrap().wallets));
    let after = reduced(&initial, 30);
    batch
        .put_meta(TrstEngine::meta_key(), &after.save_wallets())
        .unwrap();
    assert_eq!(
        batch.read_trst_engine(100).unwrap().save_wallets(),
        after.save_wallets()
    );
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.clear_checkpoint_rows(UndoTable::Meta, None).unwrap();
    assert!(batch.read_trst_engine(100).is_err());
    drop(batch);
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    assert_eq!(
        store
            .tx_begin_write()
            .unwrap()
            .read_trst_engine(100)
            .unwrap()
            .save_wallets(),
        after.save_wallets()
    );
}

#[test]
fn rewind_and_failed_commit_preserve_the_correct_writer_cut() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let initial = fixture();
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch, &initial, true);
    batch.read_trst_engine(100).unwrap();
    batch.commit().unwrap();
    let account = WalletAddress::new("brst_holder_1");
    let block = BlockHash::new([9; 32]);
    let after = reduced(&initial, 30);
    let mut batch = store.tx_begin_write().unwrap();
    batch.begin_block_undo(block, account.clone(), 1).unwrap();
    let mut info = AccountInfo::unopened(account.clone());
    info.head = block;
    info.block_count = 1;
    batch
        .put_block_with_account(&block, b"block", &account, 1)
        .unwrap();
    batch.put_account_info(&info, false).unwrap();
    batch.put_frontier(&account, &block).unwrap();
    delta(&mut batch, &initial, &after);
    let losing = batch.read_trst_engine(100).unwrap();
    batch.commit().unwrap();
    for abort in [true, false] {
        let mut batch = store.tx_begin_write().unwrap();
        batch.rewind_block(&block).unwrap();
        assert_eq!(
            batch.read_trst_engine(100).unwrap().save_wallets(),
            initial.save_wallets()
        );
        assert_eq!(losing.save_wallets(), after.save_wallets());
        if abort {
            assert!(batch
                .put_block_meta(&block, &account, 1, "bad", b"bad")
                .is_err());
            assert!(batch.commit().is_err());
            assert_eq!(
                store
                    .tx_begin_write()
                    .unwrap()
                    .read_trst_engine(100)
                    .unwrap()
                    .save_wallets(),
                after.save_wallets()
            );
        } else {
            batch.commit().unwrap();
        }
    }
    assert_eq!(
        store
            .tx_begin_write()
            .unwrap()
            .read_trst_engine(100)
            .unwrap()
            .save_wallets(),
        initial.save_wallets()
    );
}

#[test]
fn burn_ownership_dirty_commit_and_rewind_preserve_unrelated_portfolios() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let initial = fixture();
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch, &initial, true);
    let baseline = batch.read_trst_engine(100).unwrap();
    batch.commit().unwrap();
    let owner = WalletAddress::new("brst_new_owner");
    let origin = TxHash::new([9; 32]);
    let mut after = initial.clone();
    let token = after
        .mint(
            origin,
            owner.clone(),
            50,
            owner.clone(),
            Timestamp::new(100),
        )
        .unwrap();
    after.track_token(token);
    let block = BlockHash::new([9; 32]);
    let mut batch = store.tx_begin_write().unwrap();
    batch.begin_block_undo(block, owner.clone(), 1).unwrap();
    let mut info = AccountInfo::unopened(owner.clone());
    info.head = block;
    info.block_count = 1;
    batch
        .put_block_with_account(&block, b"block", &owner, 1)
        .unwrap();
    batch.put_account_info(&info, false).unwrap();
    batch.put_frontier(&owner, &block).unwrap();
    delta(&mut batch, &initial, &after);
    batch.commit().unwrap(); // Read only in the next writer, after dirty sets were published.
    let batch = store.tx_begin_write().unwrap();
    let registered = batch.read_trst_engine(100).unwrap();
    assert_matches_full_restore(&batch, &registered);
    assert!(registered.wallet_snapshot_eq(&after));
    let unrelated = WalletAddress::new("brst_holder_4");
    assert!(std::ptr::eq(
        &**baseline.wallets.get(&unrelated).unwrap(),
        &**registered.wallets.get(&unrelated).unwrap()
    ));
    batch.commit().unwrap();
    for abort in [true, false] {
        let mut batch = store.tx_begin_write().unwrap();
        batch.rewind_block(&block).unwrap();
        let rewound = batch.read_trst_engine(100).unwrap();
        assert!(rewound.wallet_snapshot_eq(&initial));
        assert!(std::ptr::eq(
            &**registered.wallets.get(&unrelated).unwrap(),
            &**rewound.wallets.get(&unrelated).unwrap()
        ));
        if !abort {
            batch.commit().unwrap();
        }
    }
    assert!(store
        .tx_begin_write()
        .unwrap()
        .read_trst_engine(100)
        .unwrap()
        .wallet_snapshot_eq(&initial));
}

#[test]
fn ownership_without_holdings_survives_and_missing_headers_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let initial = fixture();
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch, &initial, true);
    batch.read_trst_engine(100).unwrap();
    batch.commit().unwrap();
    let owner = WalletAddress::new("brst_pending_owner");
    let origin = TxHash::new([9; 32]);
    let mut after = initial.clone();
    let _pending_token = after
        .mint(
            origin,
            owner.clone(),
            50,
            owner.clone(),
            Timestamp::new(100),
        )
        .unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    delta(&mut batch, &initial, &after);
    let restored = batch.read_trst_engine(100).unwrap();
    assert!(restored
        .wallet_origins
        .get(&owner)
        .unwrap()
        .contains(&origin));
    assert!(!restored.wallets.contains_key(&owner));
    assert_matches_full_restore(&batch, &restored);
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .delete_meta(&WalletRecord::origin_owner_storage_key(&owner))
        .unwrap();
    assert!(batch.read_trst_engine(100).is_err());
    drop(batch);
    assert!(store
        .tx_begin_write()
        .unwrap()
        .read_trst_engine(100)
        .unwrap()
        .wallet_snapshot_eq(&after));
}

fn seed_stable(batch: &mut LmdbWriteTransaction<'_>, engine: &TrstEngine) {
    seed(batch, engine, true);
    engine.visit_wallet_records(|record| {
        if matches!(record, WalletRecord::Portfolio { .. } | WalletRecord::Holding { .. }) {
            batch.delete_meta(&record.storage_entry().unwrap().0)?;
        }
        Ok::<_, StoreError>(())
    }).unwrap();
    for (owner, portfolio) in &engine.wallets {
        for (_, row) in holding_rows::delta(owner, None, Some(portfolio)).unwrap() {
            let (key, bytes) = row.unwrap().storage_entry().unwrap();
            batch.put_meta(&key, &bytes).unwrap();
        }
    }
    batch.put_meta(HOLDINGS_FORMAT, &[2]).unwrap();
}

fn stable_delta(batch: &mut LmdbWriteTransaction<'_>, before: &TrstEngine, after: &TrstEngine) {
    batch.persist_stable_trst_delta(before, after).unwrap();
}

#[test]
fn stable_holdings_reads_patch_dirty_portfolios_across_commit_and_abort() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let initial = fixture();
    let mut batch = store.tx_begin_write().unwrap();
    seed_stable(&mut batch, &initial);
    assert!(batch.uses_trst_records().is_err()); // Old positional writers stay closed.
    let baseline = batch.read_trst_engine(100).unwrap();
    assert_eq!(baseline.save_wallets(), initial.save_wallets());
    batch.commit().unwrap();
    let after = reduced(&initial, 30);
    let mut batch = store.tx_begin_write().unwrap();
    stable_delta(&mut batch, &initial, &after);
    let cached = batch.read_trst_engine(100).unwrap();
    assert_eq!(cached.save_wallets(), after.save_wallets());
    assert_matches_full_restore(&batch, &cached);
    let other = WalletAddress::new("brst_holder_4");
    assert!(std::ptr::eq(&**baseline.wallets.get(&other).unwrap(), &**cached.wallets.get(&other).unwrap()));
    // Dirty changes are carried across commit even without a read before commit.
    stable_delta(&mut batch, &after, &initial);
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.read_trst_engine(100).unwrap().save_wallets(), initial.save_wallets());
    stable_delta(&mut batch, &initial, &after);
    assert_eq!(batch.read_trst_engine(100).unwrap().save_wallets(), after.save_wallets());
    drop(batch);
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.read_trst_engine(100).unwrap().save_wallets(), initial.save_wallets());
    assert!(batch.read_trst_engine(200).is_err());
    drop(batch);
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    assert_eq!(store.tx_begin_write().unwrap().read_trst_engine(100).unwrap().save_wallets(), initial.save_wallets());
}

#[test]
fn stable_holdings_reject_mixed_orphaned_and_unknown_rows_even_with_warm_cache() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let initial = fixture();
    let owner = WalletAddress::new("brst_holder_1");
    let mut batch = store.tx_begin_write().unwrap();
    seed_stable(&mut batch, &initial);
    batch.read_trst_engine(100).unwrap();
    batch.commit().unwrap();
    let positional = WalletRecord::Portfolio { holder: owner.clone(), cached_transferable: 50,
        earliest_expiry: Some(Timestamp::new(200)) }.storage_entry().unwrap();
    let orphan_owner = WalletAddress::new("brst_orphan");
    let mut token = initial.wallets[&owner].tokens[0].clone();
    token.holder = orphan_owner.clone();
    let orphan = holding_rows::Row::Holding { holder: orphan_owner, next: None,
        token: Box::new(token) }.storage_entry().unwrap();
    for (key, value) in [positional, orphan, (format!("{}unknown", holding_rows::PREFIX), vec![0]),
        (TrstEngine::meta_key().into(), initial.save_wallets()),
        (HOLDINGS_FORMAT.into(), vec![1])] {
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(&key, &value).unwrap();
        assert!(batch.read_trst_holdings(100).is_err(), "full read accepted {key}");
        assert!(batch.read_trst_engine(100).is_err(), "cache accepted {key}");
        let checked = batch.read_view().read_trst_engine_for_comparison_with_poll(100, &|stage| {
            if stage == "storage:trst-restored" { return Err(StoreError::Backend("late stop".into())); }
            Ok(())
        });
        assert!(matches!(checked, Err(StoreError::Corruption(_))), "checked reader hid corrupt {key}");
    }
    let mut batch = store.tx_begin_write().unwrap();
    batch.delete_meta(&holding_rows::portfolio_keys(&owner).0).unwrap();
    assert!(batch.read_trst_holdings(100).is_err());
    assert!(batch.read_trst_engine(100).is_err());
}

#[test]
fn stable_rows_without_marker_invalidate_legacy_cache() {
    for positional in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let initial = fixture();
        let mut batch = store.tx_begin_write().unwrap();
        seed(&mut batch, &initial, positional);
        batch.read_trst_engine(100).unwrap();
        let owner = WalletAddress::new("brst_holder_1");
        let (_, row) = holding_rows::delta(&owner, None, Some(&initial.wallets[&owner])).unwrap().remove(0);
        let (key, bytes) = row.unwrap().storage_entry().unwrap();
        batch.put_meta(&key, &bytes).unwrap();
        assert!(batch.uses_trst_records().is_err());
        assert!(batch.read_trst_engine(100).is_err());
    }
}

#[test]
fn stable_writer_records_bounded_rows_and_rewinds_exact_linked_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let account = WalletAddress::new("brst_holder_1");
    let mut initial = fixture();
    // Arrival order within a timestamp must not become LMDB token-key order.
    for id in [90, 80, 70] {
        let token = initial.mint(TxHash::new([id; 32]), account.clone(), 10,
            account.clone(), Timestamp::new(100)).unwrap();
        initial.track_token(token);
    }
    let mut after = initial.clone();
    let removed = initial.wallets[&account].tokens[0].id;
    after.bulk_untrack(&account, &std::collections::HashSet::from([removed]));
    let mut batch = store.tx_begin_write().unwrap();
    seed_stable(&mut batch, &initial);
    assert_eq!(batch.read_trst_engine(100).unwrap().save_wallets(), initial.save_wallets());
    batch.commit().unwrap();
    let block = BlockHash::new([9; 32]);
    let mut batch = store.tx_begin_write().unwrap();
    batch.begin_block_undo(block, account.clone(), 1).unwrap();
    let mut info = AccountInfo::unopened(account.clone());
    info.head = block;
    info.block_count = 1;
    batch.put_block_with_account(&block, b"block", &account, 1).unwrap();
    batch.put_account_info(&info, false).unwrap();
    batch.put_frontier(&account, &block).unwrap();
    batch.persist_stable_trst_delta(&initial, &after).unwrap();
    assert_eq!(batch.read_trst_engine(100).unwrap().save_wallets(), after.save_wallets());
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let undo = batch.block_undo(&block).unwrap().unwrap();
    assert_eq!(undo.changes.iter().filter(|change|
        change.table == UndoTable::Meta && change.key.starts_with(holding_rows::PREFIX.as_bytes())).count(), 2);
    assert!(!undo.changes.iter().any(|change| change.key == HOLDINGS_FORMAT.as_bytes()));
    batch.rewind_block(&block).unwrap();
    assert_eq!(batch.read_trst_engine(100).unwrap().save_wallets(), initial.save_wallets());
    batch.commit().unwrap();
    // A scoped raw reload must retain indexes across commit. Otherwise a later
    // merge silently falls back to a whole-graph restore after its row writes.
    let mut batch = store.tx_begin_write().unwrap();
    let restored = batch.read_trst_engine(100).unwrap();
    let ids = [TxHash::new([90; 32]), TxHash::new([80; 32])];
    let mut merged = restored.clone();
    let inputs = merged.wallets[&account].select_merge_inputs(&ids).unwrap();
    let token = merged.merge(&inputs, account.clone(), TxHash::new([99; 32]), Timestamp::new(100)).unwrap();
    merged.bulk_untrack(&account, &ids.into_iter().collect());
    merged.track_token(token);
    batch.persist_stable_trst_transition(&restored, &merged).unwrap();
    assert!(batch.auxiliary_cache.borrow().trst.is_some());
    let cached = batch.read_trst_engine(100).unwrap();
    assert!(std::ptr::eq(&*cached.wallets[&account], &*merged.wallets[&account]));
    assert_matches_full_restore(&batch, &cached);
    batch.commit().unwrap();
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let cold = store.tx_begin_write().unwrap().read_trst_engine(100).unwrap();
    assert_eq!(cold.save_wallets(), merged.save_wallets());
    assert!(cold.merger_graph == merged.merger_graph);
}

#[test]
fn stable_writers_preserve_metadata_replacement_and_reject_stale_deltas() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let initial = fixture();
    let mut batch = store.tx_begin_write().unwrap();
    seed_stable(&mut batch, &initial);
    batch.commit().unwrap();
    let after = reduced(&initial, 30);
    let stale = reduced(&initial, 20);
    let mut batch = store.tx_begin_write().unwrap();
    batch.persist_stable_trst_delta(&initial, &after).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch.persist_stable_trst_delta(&initial, &stale).is_err());
    drop(batch); // As required by the persistence error contract.
    let mut batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.read_trst_engine(100).unwrap().save_wallets(), after.save_wallets());
    let empty = TrstEngine::with_expiry(100);
    batch.replace_stable_trst_holdings(&empty).unwrap();
    assert_eq!(batch.read_trst_holdings(100).unwrap().save_wallets(), empty.save_wallets());
    for prefix in [holding_rows::PREFIX, WalletRecord::STORAGE_PREFIX] {
        assert!(batch.checkpoint_metadata_entries(prefix.as_bytes()).unwrap().next().is_none());
    }
    batch.replace_stable_trst_holdings(&initial).unwrap();
    assert_eq!(batch.read_trst_engine(100).unwrap().save_wallets(), initial.save_wallets());
    batch.persist_stable_trst_delta(&initial, &empty).unwrap();
    assert_eq!(batch.read_trst_holdings(100).unwrap().save_wallets(), empty.save_wallets());
    batch.persist_stable_trst_delta(&empty, &initial).unwrap();
    assert_matches_full_restore(&batch, &batch.read_trst_engine(100).unwrap());
    assert_eq!(batch.get_meta(HOLDINGS_FORMAT).unwrap(), Some(vec![2]));
    drop(batch);
    for rows in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        seed(&mut batch, &initial, rows);
        assert!(batch.persist_stable_trst_delta(&initial, &after).is_err());
        assert!(batch.replace_stable_trst_holdings(&after).is_err());
        assert_eq!(batch.read_trst_engine(100).unwrap().save_wallets(), initial.save_wallets());
    }
}

#[test]
fn stable_rewind_keeps_a_later_expired_append_while_restoring_payload() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let initial = fixture();
    let account = WalletAddress::new("brst_holder_1");
    let after = reduced(&initial, 30);
    let mut batch = store.tx_begin_write().unwrap();
    seed_stable(&mut batch, &initial);
    batch.commit().unwrap();
    let block = BlockHash::new([99; 32]);
    let mut batch = store.tx_begin_write().unwrap();
    batch.begin_block_undo(block, account.clone(), 1).unwrap();
    let mut info = AccountInfo::unopened(account.clone());
    info.head = block;
    info.block_count = 1;
    batch.put_block_with_account(&block, b"fixture", &account, 1).unwrap();
    batch.put_account_info(&info, false).unwrap();
    batch.put_frontier(&account, &block).unwrap();
    batch.persist_stable_trst_delta(&initial, &after).unwrap();
    batch.commit().unwrap();
    // A later independent storage mutation changes the next-link and count,
    // but neither the earlier payload nor transferable/expiry cache fields.
    let mut live = after.clone();
    let mut added = live.mint(TxHash::new([90; 32]), account.clone(), 7,
        account.clone(), Timestamp::new(101)).unwrap();
    added.state = burst_types::TrstState::Expired;
    live.track_token(added.clone());
    let mut expected = initial.clone();
    expected.wallet_origins = live.wallet_origins.clone();
    expected.track_token(added);
    let mut batch = store.tx_begin_write().unwrap();
    batch.persist_stable_trst_delta(&after, &live).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.rewind_block(&block).unwrap();
    assert_eq!(batch.read_trst_holdings(100).unwrap().save_wallets(), expected.save_wallets());
    batch.commit().unwrap();
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.read_trst_holdings(100).unwrap().save_wallets(), expected.save_wallets());
}

#[test]
fn stable_retained_publication_survives_commit_and_abort_without_dirty_reload() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let holder = WalletAddress::new("brst_holder_1");
    let mut batch = store.tx_begin_write().unwrap();
    seed_stable(&mut batch, &fixture());
    let before = batch.read_trst_engine(100).unwrap();
    let mut after = before.clone();
    after.debit_wallet(&holder, &TxHash::new([1; 32]), 10);
    batch.persist_stable_trst_delta(&before, &after).unwrap();
    assert!(batch.auxiliary_cache.borrow().trst.as_ref().unwrap().dirty.is_empty());
    let cached = batch.read_trst_engine(100).unwrap();
    assert!(std::ptr::eq(&*cached.wallets[&holder], &*after.wallets[&holder]));
    assert_matches_full_restore(&batch, &cached);
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let current = batch.read_trst_engine(100).unwrap();
    assert!(std::ptr::eq(&*current.wallets[&holder], &*after.wallets[&holder]));
    let mut reverted = current.clone();
    reverted.debit_wallet(&holder, &TxHash::new([1; 32]), 5);
    batch.persist_stable_trst_delta(&current, &reverted).unwrap();
    assert!(std::ptr::eq(&*batch.read_trst_engine(100).unwrap().wallets[&holder], &*reverted.wallets[&holder]));
    drop(batch);
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.read_trst_engine(100).unwrap().save_wallets(), after.save_wallets());
    drop(batch);
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    assert_eq!(store.tx_begin_write().unwrap().read_trst_engine(100).unwrap().save_wallets(), after.save_wallets());
}

#[test]
fn stable_retained_publication_cannot_hide_an_unrelated_dirty_holding() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    seed_stable(&mut batch, &fixture());
    let before = batch.read_trst_engine(100).unwrap();
    let other = WalletAddress::new("brst_holder_4");
    let (key, _) = holding_rows::portfolio_keys(&other);
    // The payment does not touch this false balance. Keep the portfolio dirty
    // so the next read validates it instead of accepting the payment's cache.
    let mut header = holding_rows::Row::from_storage_entry(&key, &batch.get_meta(&key).unwrap().unwrap()).unwrap();
    if let holding_rows::Row::Header { cached_transferable, .. } = &mut header {
        *cached_transferable += 1;
    }
    batch.put_meta(&key, &header.storage_entry().unwrap().1).unwrap();
    let mut after = before.clone();
    after.debit_wallet(&WalletAddress::new("brst_holder_1"), &TxHash::new([1; 32]), 10);
    batch.persist_stable_trst_delta(&before, &after).unwrap();
    assert!(!batch.auxiliary_cache.borrow().trst.as_ref().unwrap().dirty.is_empty());
    assert!(batch.read_trst_engine(100).is_err());
}

#[test]
fn stable_retained_creation_and_removal_publish_a_reusable_writer_cut() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    seed_stable(&mut batch, &fixture());
    let before = batch.read_trst_engine(100).unwrap();
    let sender = WalletAddress::new("brst_holder_1");
    let recipient = WalletAddress::new("brst_new_holder");
    let mut token = before.wallets[&sender].tokens[0].clone();
    token.holder = recipient.clone();
    let mut after = before.clone();
    after.wallets.remove(&sender);
    after.track_token(token);
    batch.persist_stable_trst_delta(&before, &after).unwrap();
    assert!(batch.auxiliary_cache.borrow().trst.as_ref().unwrap().dirty.is_empty());
    let current = batch.read_trst_engine(100).unwrap();
    assert_eq!(current.save_wallets(), after.save_wallets());
    assert_matches_full_restore(&batch, &current);
    let mut next = current.clone();
    next.debit_wallet(&recipient, &TxHash::new([1; 32]), 1);
    batch.persist_stable_trst_delta(&current, &next).unwrap();
    assert!(batch.auxiliary_cache.borrow().trst.as_ref().unwrap().dirty.is_empty());
    assert!(std::ptr::eq(&*batch.read_trst_engine(100).unwrap().wallets[&recipient], &*next.wallets[&recipient]));
    batch.commit().unwrap();
}

#[test]
fn late_stable_cas_conflict_does_not_publish_the_prepared_cache() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    seed_stable(&mut batch, &fixture());
    let before = batch.read_trst_engine(100).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let holder = WalletAddress::new("brst_holder_1");
    let mut after = before.clone();
    after.debit_wallet(&holder, &TxHash::new([1; 32]), 10);
    let (header_key, _) = holding_rows::portfolio_keys(&holder);
    let old_header = batch.get_meta(&header_key).unwrap().unwrap();
    let mut holding = holding_rows::delta(&holder, None, Some(&before.wallets[&holder])).unwrap()
        .into_iter().filter_map(|(_, new)| new)
        .find(|row| matches!(row, holding_rows::Row::Holding { .. })).unwrap();
    if let holding_rows::Row::Holding { token, .. } = &mut holding { token.amount += 1; }
    let (key, value) = holding.storage_entry().unwrap();
    // Fault injection below the normal invalidation hook makes the later row
    // CAS fail even though the cache initially qualifies for publication.
    batch.env.meta_db.put(&mut batch.txn, key.as_bytes(), &value).unwrap();
    assert!(batch.auxiliary_cache.borrow().trst.as_ref().unwrap().dirty.is_empty());
    assert!(batch.persist_stable_trst_delta(&before, &after).is_err());
    assert_ne!(batch.get_meta(&header_key).unwrap().unwrap(), old_header,
        "header CAS must have succeeded before the holding conflict");
    assert!(batch.auxiliary_cache.borrow().trst.is_none());
    assert!(batch.read_trst_engine(100).is_err(),
        "partial CAS writes must undergo full validation, not reuse either cut");
    drop(batch);
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.read_trst_engine(100).unwrap().save_wallets(), before.save_wallets());
    assert_matches_full_restore(&batch, &before);
}

#[test]
fn combined_merge_cache_publication_commit_abort_and_late_conflict() {
    let holder = WalletAddress::new("brst_holder_1");
    for outcome in ["commit", "abort", "conflict", "graph_conflict", "unrelated_graph"] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut initial = fixture();
        let token = initial.mint(TxHash::new([5; 32]), holder.clone(), 30,
            holder.clone(), Timestamp::new(100)).unwrap();
        initial.track_token(token);
        let mut batch = store.tx_begin_write().unwrap();
        seed_stable(&mut batch, &initial);
        let before = batch.read_trst_engine(100).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let ids = [TxHash::new([1; 32]), TxHash::new([5; 32])];
        let mut after = before.clone();
        let inputs = after.wallets[&holder].select_merge_inputs(&ids).unwrap();
        let merged = after.merge(&inputs, holder.clone(), TxHash::new([90; 32]),
            Timestamp::new(100)).unwrap();
        after.bulk_untrack(&holder, &ids.into_iter().collect());
        after.track_token(merged);
        if outcome == "graph_conflict" || outcome == "unrelated_graph" {
            let hash = TxHash::new([if outcome == "graph_conflict" { 90 } else { 91 }; 32]);
            let (key, bytes) = GraphRecord::Node { hash, total: 1 }.storage_entry().unwrap();
            batch.put_meta(&key, &bytes).unwrap();
            assert!(batch.auxiliary_cache.borrow().trst.is_none());
            let result = batch.persist_stable_trst_transition(&before, &after);
            if outcome == "graph_conflict" {
                assert!(result.is_err());
            } else {
                result.unwrap();
            }
            assert!(batch.auxiliary_cache.borrow().trst.is_none());
            assert!(batch.read_trst_engine(100).is_err(),
                "an unproven graph cut must undergo full restoration");
        } else if outcome == "conflict" {
            let mut row = holding_rows::delta(&holder, None, Some(&before.wallets[&holder]))
                .unwrap().into_iter().filter_map(|(_, new)| new)
                .find(|row| matches!(row, holding_rows::Row::Holding { .. })).unwrap();
            if let holding_rows::Row::Holding { token, .. } = &mut row { token.amount += 1; }
            let (key, value) = row.storage_entry().unwrap();
            // Bypass invalidation to exercise a late CAS failure after proof preparation.
            batch.env.meta_db.put(&mut batch.txn, key.as_bytes(), &value).unwrap();
            assert!(batch.persist_stable_trst_transition(&before, &after).is_err());
            assert!(batch.read_merger_graph().unwrap().contains_merge(&TxHash::new([90; 32])));
            assert!(batch.auxiliary_cache.borrow().trst.is_none());
        } else {
            batch.persist_stable_trst_transition(&before, &after).unwrap();
            let cached = batch.read_trst_engine(100).unwrap();
            assert!(std::ptr::eq(&*cached.wallets[&holder], &*after.wallets[&holder]));
            assert!(cached.merger_graph.shares_storage_with(&after.merger_graph));
            assert_matches_full_restore(&batch, &cached);
        }
        let expected = if outcome == "commit" {
            batch.commit().unwrap();
            &after
        } else {
            drop(batch);
            &before
        };
        let batch = store.tx_begin_write().unwrap();
        let actual = batch.read_trst_engine(100).unwrap();
        assert_eq!(actual.save_wallets(), expected.save_wallets());
        assert_eq!(actual.merger_graph.to_bytes(), expected.merger_graph.to_bytes());
        assert_matches_full_restore(&batch, &actual);
        drop(batch);
        drop(store);
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let batch = store.tx_begin_write().unwrap();
        let actual = batch.read_trst_engine(100).unwrap();
        assert_eq!(actual.save_wallets(), expected.save_wallets());
        assert_eq!(actual.merger_graph.to_bytes(), expected.merger_graph.to_bytes());
        assert_matches_full_restore(&batch, &actual);
    }
}

#[test]
fn retained_revocation_transition_publishes_only_after_successful_writes() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let owner = WalletAddress::new("brst_holder_1");
    let mut batch = store.tx_begin_write().unwrap();
    seed_stable(&mut batch, &fixture());
    let original = batch.read_trst_engine(100).unwrap();
    batch.commit().unwrap();
    for commit in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        let before = batch.read_trst_engine(100).unwrap();
        let mut after = before.clone();
        after.revoke_by_originator(&owner);
        batch.persist_stable_trst_transition(&before, &after).unwrap();
        let cached = batch.read_trst_engine(100).unwrap();
        assert!(std::ptr::eq(&*cached.wallets[&owner], &*after.wallets[&owner]));
        assert_matches_full_restore(&batch, &cached);
        if commit { batch.commit().unwrap(); } else { drop(batch); }
        let mut batch = store.tx_begin_write().unwrap();
        let before = batch.read_trst_engine(100).unwrap();
        if commit {
            let mut after = before.clone();
            after.restore_by_originator(&owner, Timestamp::new(100)).unwrap();
            batch.persist_stable_trst_transition(&before, &after).unwrap();
            assert!(std::ptr::eq(&*batch.read_trst_engine(100).unwrap().wallets[&owner], &*after.wallets[&owner]));
            batch.commit().unwrap();
        } else {
            assert_eq!(before.save_wallets(), original.save_wallets());
        }
    }
}

#[test]
fn retained_revocation_merged_history_and_late_conflict_match_full_restore() {
    let owner = WalletAddress::new("brst_holder_1");
    for conflict in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut initial = fixture();
        let extra = initial.mint(TxHash::new([5; 32]), owner.clone(), 30,
            WalletAddress::new("brst_holder_2"), Timestamp::new(100)).unwrap();
        initial.track_token(extra);
        let ids = [TxHash::new([1; 32]), TxHash::new([5; 32])];
        let inputs = initial.wallets[&owner].select_merge_inputs(&ids).unwrap();
        let merged = initial.merge(&inputs, owner.clone(), TxHash::new([90; 32]), Timestamp::new(100)).unwrap();
        initial.bulk_untrack(&owner, &ids.into_iter().collect());
        initial.track_token(merged);
        let mut batch = store.tx_begin_write().unwrap();
        seed_stable(&mut batch, &initial);
        let original = batch.read_trst_engine(100).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let mut after = original.clone();
        after.revoke_by_originator(&owner);
        if conflict {
            let mut row = holding_rows::delta(&owner, None, Some(&original.wallets[&owner]))
                .unwrap().into_iter().filter_map(|(_, new)| new)
                .find(|row| matches!(row, holding_rows::Row::Holding { .. })).unwrap();
            if let holding_rows::Row::Holding { token, .. } = &mut row { token.amount += 1; }
            let (key, bytes) = row.storage_entry().unwrap();
            batch.env.meta_db.put(&mut batch.txn, key.as_bytes(), &bytes).unwrap();
            assert!(batch.persist_stable_trst_transition(&original, &after).is_err());
            assert!(batch.read_merger_graph().unwrap().is_origin_revoked(&TxHash::new([1; 32])));
            assert!(batch.auxiliary_cache.borrow().trst.is_none());
            drop(batch);
        } else {
            batch.persist_stable_trst_transition(&original, &after).unwrap();
            let cached = batch.read_trst_engine(100).unwrap();
            assert!(std::ptr::eq(&*cached.wallets[&owner], &*after.wallets[&owner]));
            assert_matches_full_restore(&batch, &cached);
            batch.commit().unwrap();
            // Keep old remainder revisions referenced while adding restoration history.
            let mut batch = store.tx_begin_write().unwrap();
            let before = batch.read_trst_engine(100).unwrap();
            let mut after = before.clone();
            after.restore_by_originator(&owner, Timestamp::new(100)).unwrap();
            batch.persist_stable_trst_transition(&before, &after).unwrap();
            let cached = batch.read_trst_engine(100).unwrap();
            assert!(std::ptr::eq(&*cached.wallets[&owner], &*after.wallets[&owner]));
            assert_matches_full_restore(&batch, &cached);
            batch.commit().unwrap();
        }
        drop(store);
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let batch = store.tx_begin_write().unwrap();
        let reopened = batch.read_trst_engine(100).unwrap();
        assert_matches_full_restore(&batch, &reopened);
        assert_eq!(reopened.transferable_balance_snapshot(&owner), Some(80));
        if conflict {
            assert_eq!(reopened.save_wallets(), original.save_wallets());
            assert_eq!(reopened.merger_graph.to_bytes(), original.merger_graph.to_bytes());
        }
    }
}

#[test]
fn retained_outcome_metadata_only_transitions_preserve_cache_and_abort() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let owner = WalletAddress::new("brst_holder_1");
    let origins = [(owner.clone(), TxHash::new([1; 32]))];
    let mut batch = store.tx_begin_write().unwrap();
    seed_stable(&mut batch, &fixture());
    batch.read_trst_engine(100).unwrap();
    batch.commit().unwrap();
    for (add, cause) in [(true, 1), (true, 2), (false, 1), (false, 2)] {
        for commit in [false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            let before = batch.read_trst_engine(100).unwrap();
            let mut after = before.clone();
            if add {
                after.invalidate_outcome_origins(BlockHash::new([cause; 32]), &origins).unwrap();
            } else {
                after.restore_outcome_origins(BlockHash::new([cause; 32]), &origins, Timestamp::new(100)).unwrap();
            }
            batch.persist_stable_trst_transition(&before, &after).unwrap();
            assert!(batch.auxiliary_cache.borrow().trst.is_some());
            let cached = batch.read_trst_engine(100).unwrap();
            assert!(std::ptr::eq(&*cached.wallets[&owner], &*after.wallets[&owner]));
            assert_matches_full_restore(&batch, &cached);
            if commit { batch.commit().unwrap(); } else { drop(batch); }
            let batch = store.tx_begin_write().unwrap();
            let expected = if commit { &after } else { &before };
            assert_eq!(batch.read_trst_engine(100).unwrap().save_wallets(), expected.save_wallets());
        }
    }
}

#[test]
fn retained_outcome_metadata_conflict_cannot_publish_prepared_state() {
    for graph_change in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let owner = WalletAddress::new("brst_holder_1");
        let other = WalletAddress::new("brst_holder_2");
        let origins = [(owner.clone(), TxHash::new([1; 32]))];
        let mut initial = fixture();
        if !graph_change {
            initial.invalidate_outcome_origins(BlockHash::new([1; 32]), &origins).unwrap();
        }
        let mut batch = store.tx_begin_write().unwrap();
        seed_stable(&mut batch, &initial);
        let before = batch.read_trst_engine(100).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let mut after = before.clone();
        after.debit_wallet(&other, &TxHash::new([2; 32]), 1);
        after.invalidate_outcome_origins(BlockHash::new([2; 32]), &origins).unwrap();
        let mut metadata = Vec::new();
        after.visit_wallet_metadata_delta(&before, |_, new| {
            if let Some(row @ WalletRecord::Cause { .. }) = new { metadata.push(row.storage_entry().unwrap()); }
            Ok::<_, String>(())
        }).unwrap();
        assert_eq!(metadata.len(), 1);
        let key = &metadata[0].0;
        // Fault injection below invalidation leaves a clean cache for preparation.
        batch.env.meta_db.put(&mut batch.txn, key.as_bytes(), b"conflicting metadata").unwrap();
        let (header, _) = holding_rows::portfolio_keys(&other);
        let old_header = batch.get_meta(&header).unwrap().unwrap();
        assert!(batch.persist_stable_trst_transition(&before, &after).is_err());
        assert_ne!(batch.get_meta(&header).unwrap().unwrap(), old_header);
        assert!(batch.auxiliary_cache.borrow().trst.is_none());
        assert!(batch.read_trst_engine(100).is_err(),
            "a failed publication must not hide corrupt metadata");
        drop(batch);
        let batch = store.tx_begin_write().unwrap();
        let actual = batch.read_trst_engine(100).unwrap();
        assert_eq!(actual.save_wallets(), before.save_wallets());
        assert_eq!(actual.merger_graph.to_bytes(), before.merger_graph.to_bytes());
        assert_matches_full_restore(&batch, &actual);
    }
}

fn retained_scale_fixture(population: usize) -> (tempfile::TempDir, crate::LmdbStore, WalletAddress, burst_trst::TrstToken) {
    retained_scale_fixture_with_deadlines(population, false)
}

fn retained_scale_fixture_with_deadlines(population: usize, distinct_deadlines: bool) -> (tempfile::TempDir, crate::LmdbStore, WalletAddress, burst_trst::TrstToken) {
    retained_scale_fixture_with_revocation(population, distinct_deadlines, false)
}

fn retained_scale_fixture_with_revocation(population: usize, distinct_deadlines: bool, revocation: bool) -> (tempfile::TempDir, crate::LmdbStore, WalletAddress, burst_trst::TrstToken) {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 512 << 20).unwrap();
    let holder = WalletAddress::new("brst_scale_holder");
    let origin = TxHash::new([1; 32]);
    let mut initial = TrstEngine::with_expiry(100);
    let token = initial.mint(origin, holder.clone(), 100, holder.clone(), Timestamp::new(100)).unwrap();
    for n in 0..population {
        let mut next = token.clone();
        let mut id = [0u8; 32];
        id[..8].copy_from_slice(&(n as u64 + 1).to_be_bytes());
        next.id = TxHash::new(id);
        if distinct_deadlines {
            next = initial.mint(next.id, holder.clone(), 100, holder.clone(),
                Timestamp::new(100 + n as u64)).unwrap();
        }
        initial.track_token(next);
    }
    if revocation {
        let target = initial.mint(TxHash::new([255; 32]), holder.clone(), 7,
            WalletAddress::new("brst_revoked_origin"), Timestamp::new(100)).unwrap();
        initial.track_token(target);
    }
    let mut batch = store.tx_begin_write().unwrap();
    seed_stable(&mut batch, &initial);
    batch.commit().unwrap();
    drop(initial);
    let prototype = if distinct_deadlines {
        let mut id = [0u8; 32];
        id[..8].copy_from_slice(&1u64.to_be_bytes());
        let mut first = token;
        first.id = TxHash::new(id);
        first.origin = first.id;
        first.link = first.id;
        first
    } else { token };
    (dir, store, holder, prototype)
}

#[test]
#[ignore = "explicit capped-host retained cache stage diagnostic, not release throughput"]
fn stable_retained_cache_stage_scaling() {
    retained_cache_stage_scaling(false);
}

#[test]
#[ignore = "explicit capped-host distinct-deadline counter diagnostic, not release throughput"]
fn stable_retained_distinct_deadline_counter_scaling() {
    retained_cache_stage_scaling(true);
}

#[test]
#[ignore = "explicit capped-host complete merge stage diagnostic, not release throughput"]
fn stable_retained_merge_stage_scaling() {
    use std::time::Instant;
    for population in [1_000usize, 10_000, 100_000] {
        let (_dir, store, holder, _) = retained_scale_fixture(population);
        let batch = store.tx_begin_write().unwrap();
        let start = Instant::now();
        let cold = batch.read_trst_engine(100).unwrap();
        let cold_ns = start.elapsed().as_nanos();
        batch.commit().unwrap();
        drop(cold);
        println!("merge_cold population={population} load_validate_index_ns={cold_ns}");
        for sample in 0..3u64 {
            let id = |n: u64| {
                let mut bytes = [0u8; 32];
                bytes[..8].copy_from_slice(&n.to_be_bytes());
                TxHash::new(bytes)
            };
            let ids = [id(sample * 2 + 1), id(sample * 2 + 2)];
            let mut batch = store.tx_begin_write().unwrap();
            let start = Instant::now();
            let before = batch.read_trst_engine(100).unwrap();
            let read_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            let inputs = before.wallets[&holder].select_merge_inputs(&ids).unwrap();
            let selection_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            let mut after = before.clone();
            let clone_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            let merged = after.merge(&inputs, holder.clone(), id(population as u64 + sample + 1),
                Timestamp::new(100)).unwrap();
            after.bulk_untrack(&holder, &ids.into_iter().collect());
            after.track_token(merged);
            let mutation_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            batch.persist_stable_trst_transition(&before, &after).unwrap();
            let persistence_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            let cached = batch.read_trst_engine(100).unwrap();
            let cached_read_ns = start.elapsed().as_nanos();
            assert!(std::ptr::eq(&*cached.wallets[&holder], &*after.wallets[&holder]));
            assert!(cached.merger_graph.shares_storage_with(&after.merger_graph));
            assert_eq!(cached.transferable_balance_snapshot(&holder), Some(population as u128 * 100));
            let commit = crate::write_batch::probe_commit(batch);
            let batch = store.tx_begin_write().unwrap();
            let start = Instant::now();
            let next = batch.read_trst_engine(100).unwrap();
            let next_read_ns = start.elapsed().as_nanos();
            assert!(std::ptr::eq(&*next.wallets[&holder], &*after.wallets[&holder]));
            println!("merge_stage population={population} sample={sample} read_ns={read_ns} selection_ns={selection_ns} clone_ns={clone_ns} mutation_ns={mutation_ns} persistence_ns={persistence_ns} cached_read_ns={cached_read_ns} database_commit_ns={} auxiliary_publish_ns={} other_publish_ns={} next_read_ns={next_read_ns}", commit[0], commit[1], commit[2]);
        }
        let batch = store.tx_begin_write().unwrap();
        let cached = batch.read_trst_engine(100).unwrap();
        let mut raw = batch.read_trst_holdings(100).unwrap();
        raw.merger_graph = batch.read_merger_graph().unwrap();
        let full = RestoredTrstState::from_engine(raw).unwrap().into_engine();
        assert_eq!(cached.save_wallets(), full.save_wallets());
        assert_eq!(cached.merger_graph.to_bytes(), full.merger_graph.to_bytes());
        let mut cached_revoked = cached.clone();
        let mut full_revoked = full;
        cached_revoked.revoke_by_originator(&holder);
        full_revoked.revoke_by_originator(&holder);
        assert_eq!(cached_revoked.save_wallets(), full_revoked.save_wallets());
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines().filter(|line| line.starts_with("VmHWM:") || line.starts_with("VmRSS:")) {
                println!("merge_memory population={population} {line}");
            }
        }
    }
}

#[test]
#[ignore = "explicit capped-host merge ancestry diagnostic, not release throughput"]
fn stable_retained_merge_depth_scaling() {
    use std::time::Instant;
    let id = |n: u64| {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&n.to_be_bytes());
        TxHash::new(bytes)
    };
    for depth in [100u64, 1_000, 10_000] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 512 << 20).unwrap();
        let holder = WalletAddress::new("brst_depth_holder");
        let mut initial = TrstEngine::with_expiry(100);
        let mut head = initial.mint(id(1), holder.clone(), 1, holder.clone(), Timestamp::new(100)).unwrap();
        for n in 0..depth {
            let next = initial.mint(id(2 + n * 2), holder.clone(), 1,
                holder.clone(), Timestamp::new(100)).unwrap();
            head = initial.merge(&[head, next], holder.clone(), id(3 + n * 2), Timestamp::new(100)).unwrap();
        }
        let mut head_id = head.id;
        initial.track_token(head);
        for sample in 0..3 {
            let token = initial.mint(id(2 * depth + 2 + sample), holder.clone(), 1,
                holder.clone(), Timestamp::new(100)).unwrap();
            initial.track_token(token);
        }
        let mut batch = store.tx_begin_write().unwrap();
        seed_stable(&mut batch, &initial);
        batch.commit().unwrap();
        drop(initial);
        let batch = store.tx_begin_write().unwrap();
        let start = Instant::now();
        let cold = batch.read_trst_engine(100).unwrap();
        let cold_ns = start.elapsed().as_nanos();
        assert_eq!(cold.transferable_balance_snapshot(&holder), Some(depth as u128 + 4));
        batch.commit().unwrap();
        drop(cold);
        println!("merge_depth_cold depth={depth} load_validate_index_ns={cold_ns}");
        for sample in 0..3 {
            let mut batch = store.tx_begin_write().unwrap();
            let before = batch.read_trst_engine(100).unwrap();
            let ids = [head_id, id(2 * depth + 2 + sample)];
            let start = Instant::now();
            let inputs = before.wallets[&holder].select_merge_inputs(&ids).unwrap();
            let selection_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            let mut after = before.clone();
            head_id = id(2 * depth + 5 + sample);
            let merged = after.merge(&inputs, holder.clone(), head_id, Timestamp::new(100)).unwrap();
            after.bulk_untrack(&holder, &ids.into_iter().collect());
            after.track_token(merged);
            let mutation_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            batch.persist_stable_trst_transition(&before, &after).unwrap();
            let persistence_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            let cached = batch.read_trst_engine(100).unwrap();
            let cached_read_ns = start.elapsed().as_nanos();
            assert!(std::ptr::eq(&*cached.wallets[&holder], &*after.wallets[&holder]));
            assert!(cached.merger_graph.shares_storage_with(&after.merger_graph));
            assert_eq!(cached.transferable_balance_snapshot(&holder), Some(depth as u128 + 4));
            let commit = crate::write_batch::probe_commit(batch);
            println!("merge_depth_stage depth={depth} sample={sample} selection_ns={selection_ns} mutation_ns={mutation_ns} persistence_ns={persistence_ns} cached_read_ns={cached_read_ns} database_commit_ns={} auxiliary_publish_ns={} other_publish_ns={}", commit[0], commit[1], commit[2]);
        }
        let batch = store.tx_begin_write().unwrap();
        let cached = batch.read_trst_engine(100).unwrap();
        let mut raw = batch.read_trst_holdings(100).unwrap();
        raw.merger_graph = batch.read_merger_graph().unwrap();
        let full = RestoredTrstState::from_engine(raw).unwrap().into_engine();
        assert_eq!(cached.save_wallets(), full.save_wallets());
        assert_eq!(cached.merger_graph.to_bytes(), full.merger_graph.to_bytes());
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines().filter(|line| line.starts_with("VmHWM:") || line.starts_with("VmRSS:")) {
                println!("merge_depth_memory depth={depth} {line}");
            }
        }
    }
}

#[test]
#[ignore = "explicit capped-host retained revocation stage diagnostic, not release throughput"]
fn stable_retained_revocation_stage_scaling() {
    use std::time::Instant;
    let owner = WalletAddress::new("brst_revoked_origin");
    for population in [1_000usize, 10_000, 100_000] {
        let (_dir, store, holder, _) = retained_scale_fixture_with_revocation(population, false, true);
        let batch = store.tx_begin_write().unwrap();
        batch.read_trst_engine(100).unwrap();
        batch.commit().unwrap();
        for sample in 0..3 {
            for revoke in [true, false] {
                let mut batch = store.tx_begin_write().unwrap();
                let before = batch.read_trst_engine(100).unwrap();
                let start = Instant::now();
                let mut after = before.clone();
                if revoke {
                    let events = after.revoke_by_originator(&owner);
                    assert_eq!(events.len(), 1);
                    assert_eq!(events[0].revoked_amount, 7);
                } else {
                    let events = after.restore_by_originator(&owner, Timestamp::new(100)).unwrap();
                    assert_eq!(events.len(), 1);
                    assert_eq!(events[0].amount, 7);
                }
                let mutation_ns = start.elapsed().as_nanos();
                assert_eq!(after.transferable_balance_snapshot(&holder),
                    Some(population as u128 * 100 + if revoke { 0 } else { 7 }));
                let start = Instant::now();
                batch.persist_stable_trst_transition(&before, &after).unwrap();
                let persistence_ns = start.elapsed().as_nanos();
                assert!(batch.auxiliary_cache.borrow().trst.as_ref().unwrap().dirty.is_empty());
                let start = Instant::now();
                let restored = batch.read_trst_engine(100).unwrap();
                let reload_ns = start.elapsed().as_nanos();
                assert!(std::ptr::eq(&*restored.wallets[&holder], &*after.wallets[&holder]));
                // Materialization and full semantic comparison are outside timings.
                assert_eq!(restored.save_wallets(), after.save_wallets());
                assert_eq!(restored.merger_graph.to_bytes(), after.merger_graph.to_bytes());
                let commit = crate::write_batch::probe_commit(batch);
                println!("revocation_stage population={population} sample={sample} revoke={revoke} mutation_ns={mutation_ns} persistence_ns={persistence_ns} reload_ns={reload_ns} database_commit_ns={} auxiliary_publish_ns={} other_publish_ns={}", commit[0], commit[1], commit[2]);
            }
        }
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines().filter(|line| line.starts_with("VmHWM:") || line.starts_with("VmRSS:")) {
                println!("revocation_memory population={population} {line}");
            }
        }
    }
}

#[test]
#[ignore = "explicit capped-host unrelated graph scaling, not release throughput"]
fn stable_retained_revocation_graph_scaling() {
    retained_revocation_graph_scaling(false, 1, &[100, 1_000, 10_000]);
}

#[test]
#[ignore = "explicit capped-host affected fanout scaling, not release throughput"]
fn stable_retained_revocation_fanout_scaling() {
    retained_revocation_graph_scaling(true, 1, &[100, 1_000, 10_000]);
}

#[test]
#[ignore = "explicit capped-host overlapping origins and affected merges, not release throughput"]
fn stable_retained_revocation_overlapping_origins_scaling() {
    for origins in [2, 8, 32] {
        retained_revocation_graph_scaling(true, origins, &[100, 1_000]);
    }
}

fn retained_revocation_graph_scaling(affected: bool, origins: u64, populations: &[u64]) {
    use std::time::Instant;
    let id = |n: u64| {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&n.to_be_bytes());
        TxHash::new(bytes)
    };
    for &population in populations {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 512 << 20).unwrap();
        let holder = WalletAddress::new("brst_graph_holder");
        let victim = WalletAddress::new("brst_graph_victim");
        let mut initial = TrstEngine::with_expiry(100);
        if affected && origins > 1 {
            let mut parents = Vec::new();
            for origin in 1..=origins + 1 {
                let (amount, owner) = if origin <= origins { (7, victim.clone()) }
                    else { (11, holder.clone()) };
                parents.push(initial.mint(id(origin), holder.clone(), amount * population as u128,
                    owner, Timestamp::new(100)).unwrap());
            }
            let mut next_id = origins + 2;
            for n in 0..population {
                let mut inputs = Vec::new();
                let mut remaining = Vec::new();
                for (index, parent) in parents.into_iter().enumerate() {
                    let amount = if index < origins as usize { 7 } else { 11 };
                    let (input, change) = initial.transfer(&parent, &holder, holder.clone(), amount,
                        id(next_id), id(next_id + 1), Timestamp::new(100)).unwrap();
                    next_id += 2;
                    inputs.push(input);
                    if n + 1 < population { remaining.push(change.unwrap()); }
                    else { assert!(change.is_none()); }
                }
                assert_eq!(inputs.len(), origins as usize + 1);
                let merged = initial.merge(&inputs, holder.clone(), id(next_id), Timestamp::new(100)).unwrap();
                next_id += 1;
                initial.track_token(merged);
                parents = remaining;
            }
            assert!(parents.is_empty());
        } else if affected {
            // Split both burns through the engine so fanout conserves each burn's amount.
            let mut tainted = initial.mint(id(1), holder.clone(), 7 * population as u128, victim.clone(), Timestamp::new(100)).unwrap();
            let mut clean = initial.mint(id(2), holder.clone(), 11 * population as u128, holder.clone(), Timestamp::new(100)).unwrap();
            for n in 0..population {
                let base = 3 + n * 5;
                let (first, tainted_change) = initial.transfer(&tainted, &holder, holder.clone(), 7, id(base), id(base + 1), Timestamp::new(100)).unwrap();
                let (second, clean_change) = initial.transfer(&clean, &holder, holder.clone(), 11, id(base + 2), id(base + 3), Timestamp::new(100)).unwrap();
                let merged = initial.merge(&[first, second], holder.clone(), id(base + 4), Timestamp::new(100)).unwrap();
                initial.track_token(merged);
                if n + 1 < population {
                    tainted = tainted_change.unwrap();
                    clean = clean_change.unwrap();
                } else {
                    assert!(tainted_change.is_none());
                    assert!(clean_change.is_none());
                }
            }
        } else {
        let mut head = initial.mint(id(1), holder.clone(), 1, holder.clone(), Timestamp::new(100)).unwrap();
        for n in 0..population {
            let next = initial.mint(id(2 + n * 2), holder.clone(), 1, holder.clone(), Timestamp::new(100)).unwrap();
            head = initial.merge(&[head, next], holder.clone(), id(3 + n * 2), Timestamp::new(100)).unwrap();
        }
        initial.track_token(head);
        let first = initial.mint(id(2 * population + 2), holder.clone(), 7, victim.clone(), Timestamp::new(100)).unwrap();
        let second = initial.mint(id(2 * population + 3), holder.clone(), 11, holder.clone(), Timestamp::new(100)).unwrap();
        let target = initial.merge(&[first, second], holder.clone(), id(2 * population + 4), Timestamp::new(100)).unwrap();
        initial.track_token(target);
        }
        let mut batch = store.tx_begin_write().unwrap();
        seed_stable(&mut batch, &initial);
        batch.commit().unwrap();
        drop(initial);
        let batch = store.tx_begin_write().unwrap();
        let start = Instant::now();
        batch.read_trst_engine(100).unwrap();
        let cold_ns = start.elapsed().as_nanos();
        batch.commit().unwrap();
        println!("revocation_graph_cold affected={affected} origins={origins} population={population} load_validate_index_ns={cold_ns}");
        for sample in 0..3 {
            for revoke in [true, false] {
                let mut batch = store.tx_begin_write().unwrap();
                let before = batch.read_trst_engine(100).unwrap();
                let start = Instant::now();
                let mut after = before.clone();
                if revoke {
                    let events = after.revoke_by_originator(&victim);
                    assert_eq!(events.len(), if affected { (population * origins) as usize } else { 1 });
                    assert!(events.iter().all(|event| event.revoked_amount == 7));
                } else {
                    let events = after.restore_by_originator(&victim, Timestamp::new(100)).unwrap();
                    assert_eq!(events.len(), if affected { (population * origins) as usize } else { 1 });
                    assert!(events.iter().all(|event| event.amount == 7));
                }
                let mutation_ns = start.elapsed().as_nanos();
                let start = Instant::now();
                TRANSITION_PROBE.with(|slot| {
                    assert!(slot.borrow().is_none());
                    *slot.borrow_mut() = Some(Vec::new());
                });
                let result = batch.persist_stable_trst_transition(&before, &after);
                let parts = TRANSITION_PROBE.with(|slot| slot.borrow_mut().take().unwrap());
                result.unwrap();
                assert_eq!(parts.len(), 1);
                let [proof_ns, graph_ns, holdings_ns] = parts[0];
                let persistence_ns = start.elapsed().as_nanos();
                let start = Instant::now();
                let cached = batch.read_trst_engine(100).unwrap();
                let read_ns = start.elapsed().as_nanos();
                assert!(std::ptr::eq(&*cached.wallets[&holder], &*after.wallets[&holder]));
                assert!(cached.merger_graph.shares_storage_with(&after.merger_graph));
                assert_eq!(cached.transferable_balance_snapshot(&holder), Some(if affected { population as u128 * if revoke { 11 } else { 7 * origins as u128 + 11 } } else { population as u128 + if revoke { 12 } else { 19 } }));
                let commit = crate::write_batch::probe_commit(batch);
                println!("revocation_graph_stage affected={affected} origins={origins} population={population} sample={sample} revoke={revoke} mutation_ns={mutation_ns} persistence_ns={persistence_ns} proof_ns={proof_ns} graph_ns={graph_ns} holdings_ns={holdings_ns} read_ns={read_ns} database_commit_ns={} auxiliary_publish_ns={} other_publish_ns={}", commit[0], commit[1], commit[2]);
                if origins > 1 {
                    // Independent full restoration after each committed direction,
                    // outside the measured mutation/persistence/cache-read stages.
                    let check = store.tx_begin_write().unwrap();
                    let mut raw = check.read_trst_holdings(100).unwrap();
                    raw.merger_graph = check.read_merger_graph().unwrap();
                    let full = RestoredTrstState::from_engine(raw).unwrap().into_engine();
                    assert_eq!(after.save_wallets(), full.save_wallets());
                    assert_eq!(after.merger_graph.to_bytes(), full.merger_graph.to_bytes());
                    assert_eq!(full.transferable_balance_snapshot(&holder),
                        Some(population as u128 * if revoke { 11 } else { 7 * origins as u128 + 11 }));
                }
            }
        }
        let batch = store.tx_begin_write().unwrap();
        let cached = batch.read_trst_engine(100).unwrap();
        let mut raw = batch.read_trst_holdings(100).unwrap();
        raw.merger_graph = batch.read_merger_graph().unwrap();
        let full = RestoredTrstState::from_engine(raw).unwrap().into_engine();
        assert_eq!(cached.save_wallets(), full.save_wallets());
        assert_eq!(cached.merger_graph.to_bytes(), full.merger_graph.to_bytes());
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines().filter(|line| line.starts_with("VmHWM:") || line.starts_with("VmRSS:")) {
                println!("revocation_graph_memory affected={affected} origins={origins} population={population} {line}");
            }
        }
    }
}

fn retained_cache_stage_scaling(distinct_deadlines: bool) {
    use std::time::Instant;
    for population in [1_000usize, 10_000, 100_000] {
        let (_dir, store, holder, token) = retained_scale_fixture_with_deadlines(population, distinct_deadlines);
        let origin = token.origin;
        let batch = store.tx_begin_write().unwrap();
        let start = Instant::now();
        let cold = batch.read_trst_engine(100).unwrap();
        let cold_ns = start.elapsed().as_nanos();
        assert_eq!(cold.transferable_balance_snapshot(&holder), Some(population as u128 * 100));
        batch.commit().unwrap();
        drop(cold);
        println!("retained_cache_cold {{\"population\":{population},\"load_validate_index_ns\":{cold_ns}}}");
        for operation in ["debit", "receive"] {
            for sample in 0..3u64 {
                let mut batch = store.tx_begin_write().unwrap();
                let start = Instant::now();
                let before = batch.read_trst_engine(100).unwrap();
                let read_ns = start.elapsed().as_nanos();
                let start = Instant::now();
                let admission = before.candidate_balances(&holder, &origin, Timestamp::new(100)).unwrap();
                let admission_ns = start.elapsed().as_nanos();
                if distinct_deadlines {
                    let removed = if operation == "debit" { sample } else { 3 };
                    let received = if operation == "receive" { sample } else { 0 };
                    assert_eq!(admission, (population as u128 * 100 - removed as u128 + received as u128 * 100,
                        100 - removed as u128 + received as u128 * 100));
                } else {
                    assert_eq!(admission.0, admission.1);
                }
                let start = Instant::now();
                let mut after = before.clone();
                let clone_ns = start.elapsed().as_nanos();
                let start = Instant::now();
                if operation == "debit" {
                    after.debit_wallet(&holder, &origin, 1);
                } else {
                    let mut incoming = token.clone();
                    let mut id = [0u8; 32];
                    id[..8].copy_from_slice(&(population as u64 + sample + 1).to_be_bytes());
                    incoming.id = TxHash::new(id);
                    after.receive_token(incoming, Timestamp::new(100));
                }
                let operation_ns = start.elapsed().as_nanos();
                let mutation_ns = clone_ns + operation_ns;
                println!("retained_mutation_parts {{\"population\":{population},\"operation\":\"{operation}\",\"sample\":{sample},\"clone_ns\":{clone_ns},\"operation_ns\":{operation_ns}}}");
                let start = Instant::now();
                batch.persist_stable_trst_delta(&before, &after).unwrap();
                let persistence_ns = start.elapsed().as_nanos();
                assert!(batch.auxiliary_cache.borrow().trst.as_ref().unwrap().dirty.is_empty());
                let start = Instant::now();
                let cached = batch.read_trst_engine(100).unwrap();
                let read_after_ns = start.elapsed().as_nanos();
                assert!(std::ptr::eq(&*cached.wallets[&holder], &*after.wallets[&holder]));
                // Query forward, at boundaries, then backwards on the changed root.
                // Keep the raw reconstruction independent so it cannot materialize
                // the retained portfolio or warm its counter cache.
                let clocks = [199, 200, 200 + population as u64 / 2,
                    200 + population as u64, 100];
                let start = Instant::now();
                let counters: Vec<_> = clocks.iter().map(|&now|
                    cached.token_counter_window(&holder, Timestamp::new(now)).unwrap()).collect();
                let counter_ns = start.elapsed().as_nanos();
                let commit = crate::write_batch::probe_commit(batch);
                let batch = store.tx_begin_write().unwrap();
                let start = Instant::now();
                let next = batch.read_trst_engine(100).unwrap();
                let next_writer_read_ns = start.elapsed().as_nanos();
                assert!(std::ptr::eq(&*next.wallets[&holder], &*after.wallets[&holder]));
                if distinct_deadlines {
                    let raw = batch.read_trst_holdings(100).unwrap();
                    for (&now, actual) in clocks.iter().zip(&counters) {
                        assert_eq!(*actual, raw.token_counter_window(&holder, Timestamp::new(now)).unwrap());
                    }
                    println!("retained_counter_stage {{\"population\":{population},\"operation\":\"{operation}\",\"sample\":{sample},\"queries\":5,\"counter_ns\":{counter_ns}}}");
                }
                drop(batch);
                println!("retained_cache_stage {{\"population\":{population},\"operation\":\"{operation}\",\"sample\":{sample},\"read_ns\":{read_ns},\"admission_ns\":{admission_ns},\"mutation_ns\":{mutation_ns},\"persistence_ns\":{persistence_ns},\"read_after_ns\":{read_after_ns},\"database_commit_ns\":{},\"auxiliary_publish_ns\":{},\"other_publish_ns\":{},\"next_writer_read_ns\":{next_writer_read_ns}}}", commit[0], commit[1], commit[2]);
            }
        }
        // Full reconstruction is an oracle outside all warm timing windows.
        let batch = store.tx_begin_write().unwrap();
        let cached = batch.read_trst_engine(100).unwrap();
        assert_eq!(cached.save_wallets(), batch.read_trst_holdings(100).unwrap().save_wallets());
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines().filter(|line| line.starts_with("VmHWM:") || line.starts_with("VmRSS:")) {
                println!("retained_cache_memory population={population} {line}");
            }
        }
    }
}

#[test]
#[ignore = "explicit capped-host cold-load phase attribution, not release qualification"]
fn stable_retained_cold_phase_attribution() {
    use std::time::Instant;
    let (_dir, store, holder, _) = retained_scale_fixture(100_000);
    let batch = store.tx_begin_write().unwrap();
    let start = Instant::now();
    let mut engine = batch.read_trst_holdings(100).unwrap();
    let holdings_ns = start.elapsed().as_nanos();
    let start = Instant::now();
    engine.merger_graph = batch.read_merger_graph().unwrap();
    let graph_ns = start.elapsed().as_nanos();
    let start = Instant::now();
    let mut state = RestoredTrstState::from_engine(engine).unwrap();
    let validation_ns = start.elapsed().as_nanos();
    let start = Instant::now();
    state.retain_portfolio_indexes().unwrap();
    let indexing_ns = start.elapsed().as_nanos();
    assert_eq!(state.engine().transferable_balance_snapshot(&holder), Some(10_000_000));
    println!("retained_cold_phases {{\"population\":100000,\"holdings_ns\":{holdings_ns},\"graph_ns\":{graph_ns},\"validation_ns\":{validation_ns},\"indexing_ns\":{indexing_ns}}}");
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines().filter(|line| line.starts_with("VmHWM:") || line.starts_with("VmRSS:")) {
            println!("retained_cold_memory {line}");
        }
    }
    assert_eq!(state.engine().save_wallets(), batch.read_trst_holdings(100).unwrap().save_wallets());
}

#[test]
#[ignore = "explicit capped-host cold holdings substage attribution"]
fn stable_cold_holdings_substages() {
    use std::time::Instant;
    let (_dir, store, holder, _) = retained_scale_fixture(100_000);
    let batch = store.tx_begin_write().unwrap();
    let (header_key, prefix) = holding_rows::portfolio_keys(&holder);
    let header = holding_rows::Row::from_storage_entry(&header_key,
        &batch.get_meta(&header_key).unwrap().unwrap()).unwrap();
    let start = Instant::now();
    let entries = batch.checkpoint_metadata_entries(prefix.as_bytes()).unwrap()
        .collect::<Result<Vec<_>, _>>().unwrap();
    let cursor_ns = start.elapsed().as_nanos();
    assert_eq!(entries.len(), 100_000);
    let start = Instant::now();
    let rows = entries.into_iter().map(|(key, bytes)|
        holding_rows::Row::from_storage_entry(std::str::from_utf8(key).unwrap(), bytes))
        .collect::<Result<Vec<_>, _>>().unwrap();
    let decode_ns = start.elapsed().as_nanos();
    let start = Instant::now();
    let portfolio = holding_rows::restore(&holder, header, rows.into_iter().map(Ok), 100).unwrap();
    let restore_ns = start.elapsed().as_nanos();
    let mut reader = WalletRecordReader::new(100);
    let start = Instant::now();
    reader.push(WalletRecord::Portfolio { holder: holder.clone(),
        cached_transferable: portfolio.cached_transferable, earliest_expiry: portfolio.earliest_expiry }).unwrap();
    for (position, token) in portfolio.into_canonical().tokens.into_iter().enumerate() {
        reader.push(WalletRecord::Holding { holder: holder.clone(), position: position as u64,
            token: Box::new(token) }).unwrap();
    }
    let positional_ns = start.elapsed().as_nanos();
    let start = Instant::now();
    let count = batch.checkpoint_metadata_entries(holding_rows::PREFIX.as_bytes()).unwrap()
        .map(|row| row.unwrap()).count();
    let row_count_ns = start.elapsed().as_nanos();
    assert_eq!(count, 100_001);
    for row in batch.checkpoint_metadata_entries(WalletRecord::STORAGE_PREFIX.as_bytes()).unwrap() {
        let (key, bytes) = row.unwrap();
        reader.push(WalletRecord::from_storage_entry(std::str::from_utf8(key).unwrap(), bytes).unwrap()).unwrap();
    }
    let start = Instant::now();
    let engine = reader.finish().unwrap();
    let finish_ns = start.elapsed().as_nanos();
    println!("cold_holding_substages {{\"population\":100000,\"cursor_ns\":{cursor_ns},\"decode_ns\":{decode_ns},\"restore_ns\":{restore_ns},\"positional_ns\":{positional_ns},\"row_count_ns\":{row_count_ns},\"finish_ns\":{finish_ns}}}");
    assert_eq!(engine.save_wallets(), batch.read_trst_holdings(100).unwrap().save_wallets());
}


fn fragmented_persistence_parts(population: usize) {
    use std::time::Instant;
    let (_dir, store, holder, token) = retained_scale_fixture(population);
    let batch = store.tx_begin_write().unwrap();
    let baseline = batch.read_trst_engine(100).unwrap().save_wallets();
    batch.commit().unwrap();
    for sample in 0..3 {
        let mut batch = store.tx_begin_write().unwrap();
        let before = batch.read_trst_engine(100).unwrap();
        let mut after = before.clone();
        after.debit_wallet(&holder, &token.origin, population as u128 * 100);
        assert_eq!(after.transferable_balance_snapshot(&holder).unwrap_or(0), 0);
        let block = BlockHash::new([91; 32]);
        batch.begin_block_undo(block, holder.clone(), 1).unwrap();
        HOLDING_PROBE.with(|slot| *slot.borrow_mut() = Some([0; 5]));
        let start = Instant::now();
        batch.persist_stable_trst_transition(&before, &after).unwrap();
        let total_ns = start.elapsed().as_nanos();
        let parts = HOLDING_PROBE.with(|slot| slot.borrow_mut().take().unwrap());
        assert_eq!(parts[4], population as u128 + 1, "deleted holdings plus portfolio header");
        assert_eq!(batch.read_trst_engine(100).unwrap().save_wallets(), after.save_wallets());
        assert_matches_full_restore(&batch, &after);
        if sample == 2 {
            batch.commit().unwrap();
            let batch = store.tx_begin_write().unwrap();
            let undo = batch.block_undo(&block).unwrap().unwrap();
            assert!(undo.changes.len() >= population + 1);
            assert_eq!(batch.read_trst_engine(100).unwrap().save_wallets(), after.save_wallets());
            assert_matches_full_restore(&batch, &after);
        } else {
            drop(batch);
            let batch = store.tx_begin_write().unwrap();
            assert_eq!(batch.read_trst_engine(100).unwrap().save_wallets(), baseline);
            assert!(batch.block_undo(&block).unwrap().is_none());
        }
        println!("fragmented_persistence population={population} sample={sample} total_ns={total_ns} proof_ns={} encode_ns={} cas_ns={} raw_write_ns={} holding_rows={}", parts[0], parts[1], parts[2], parts[3], parts[4]);
    }
}

#[test]
fn fragmented_persistence_probe_preserves_abort_and_commit() {
    fragmented_persistence_parts(8);
}

#[test]
#[ignore = "explicit capped-host fragmented persistence substages; synthetic storage fixture, not signed payment or release throughput"]
fn stable_fragmented_persistence_stage_scaling() {
    for population in [1, 10, 100, 1000] { fragmented_persistence_parts(population); }
}


#[test]
fn retained_successor_does_not_suppress_later_unknown_write_validation() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let holder = WalletAddress::new("brst_holder_1");
    let mut batch = store.tx_begin_write().unwrap();
    seed_stable(&mut batch, &fixture());
    let before = batch.read_trst_engine(100).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let mut after = before.clone();
    after.debit_wallet(&holder, &TxHash::new([1; 32]), 10);
    batch.persist_stable_trst_delta(&before, &after).unwrap();
    assert!(batch.auxiliary_cache.borrow().trst.as_ref().unwrap().dirty.is_empty());
    let (header, _) = holding_rows::portfolio_keys(&holder);
    batch.put_meta(&header, b"malformed after successful publication").unwrap();
    assert!(batch.auxiliary_cache.borrow().trst.is_none());
    assert!(batch.read_trst_engine(100).is_err());
    drop(batch);
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.read_trst_engine(100).unwrap().save_wallets(), before.save_wallets());
    assert_matches_full_restore(&batch, &before);
}

#[test]
fn graph_row_rewind_retains_validated_cut_and_independent_sibling_across_abort() {
    use burst_trst::merger_graph::{MergeNode, MergeSource};
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let account = WalletAddress::new("brst_holder_1");
    let untouched = WalletAddress::new("brst_holder_4");
    let mut batch = store.tx_begin_write().unwrap();
    seed_stable(&mut batch, &fixture());
    let before = batch.read_trst_engine(100).unwrap();
    batch.commit().unwrap();
    let append = |engine: &mut TrstEngine, id: u8| {
        engine.merger_graph.record_merge(MergeNode {
            merge_tx: TxHash::new([id; 32]),
            source_origins: vec![MergeSource { origin: TxHash::new([1; 32]), amount: 1 }],
            total_amount: 1, revoked_contribs: Default::default(),
        });
    };
    let block = BlockHash::new([91; 32]);
    let mut batch = store.tx_begin_write().unwrap();
    batch.begin_block_undo(block, account.clone(), 1).unwrap();
    let mut info = AccountInfo::unopened(account.clone());
    info.head = block;
    info.block_count = 1;
    batch.put_block_with_account(&block, b"block", &account, 1).unwrap();
    batch.put_account_info(&info, false).unwrap();
    batch.put_frontier(&account, &block).unwrap();
    let mut merged = before.clone();
    append(&mut merged, 80);
    batch.persist_stable_trst_transition(&before, &merged).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let current = batch.read_trst_engine(100).unwrap();
    let mut sibling = current.clone();
    append(&mut sibling, 81);
    batch.persist_stable_trst_transition(&current, &sibling).unwrap();
    batch.commit().unwrap();
    let mut expected = before;
    append(&mut expected, 81);
    for commit in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        let losing = batch.read_trst_engine(100).unwrap();
        assert!(losing.merger_graph == sibling.merger_graph);
        batch.rewind_block(&block).unwrap();
        assert!(batch.auxiliary_cache.borrow().trst.is_some());
        let rewound = batch.read_trst_engine(100).unwrap();
        assert!(rewound.merger_graph == expected.merger_graph);
        assert!(std::ptr::eq(&*rewound.wallets[&untouched], &*losing.wallets[&untouched]));
        assert_matches_full_restore(&batch, &rewound);
        if commit { batch.commit().unwrap(); }
    }
    drop(store);
    let reopened = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let batch = reopened.tx_begin_write().unwrap();
    let cold = batch.read_trst_engine(100).unwrap();
    assert!(cold.merger_graph == expected.merger_graph);
    assert_eq!(cold.save_wallets(), expected.save_wallets());
}

fn revision_rewind_fixture() -> TrstEngine {
    let owner = WalletAddress::new("brst_holder_1");
    let mut engine = fixture();
    let extra = engine.mint(TxHash::new([5; 32]), owner.clone(), 30,
        WalletAddress::new("brst_holder_2"), Timestamp::new(100)).unwrap();
    engine.track_token(extra);
    let ids = [TxHash::new([1; 32]), TxHash::new([5; 32])];
    let inputs = engine.wallets[&owner].select_merge_inputs(&ids).unwrap();
    let token = engine.merge(&inputs, owner.clone(), TxHash::new([90; 32]), Timestamp::new(100)).unwrap();
    engine.bulk_untrack(&owner, &ids.into_iter().collect()); engine.track_token(token);
    let extra = engine.mint(TxHash::new([6; 32]), WalletAddress::new("brst_holder_3"), 1,
        owner.clone(), Timestamp::new(100)).unwrap();
    engine.track_token(extra);
    engine.invalidate_outcome_origins(BlockHash::new([77; 32]), &[(owner.clone(), TxHash::new([6; 32]))]).unwrap();
    engine.revoke_by_originator(&owner);
    engine
}

#[test]
fn revision_row_rewind_restores_outcomes_and_retains_writer_cut_across_abort() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let owner = WalletAddress::new("brst_holder_1");
    let untouched = WalletAddress::new("brst_holder_4");
    let mut batch = store.tx_begin_write().unwrap();
    seed_stable(&mut batch, &revision_rewind_fixture());
    let before = batch.read_trst_engine(100).unwrap(); batch.commit().unwrap();
    let block = BlockHash::new([91; 32]);
    let mut batch = store.tx_begin_write().unwrap();
    batch.begin_block_undo(block, owner.clone(), 1).unwrap();
    let mut account = AccountInfo::unopened(owner.clone()); account.head = block; account.block_count = 1;
    batch.put_block_with_account(&block, b"block", &owner, 1).unwrap();
    batch.put_account_info(&account, false).unwrap(); batch.put_frontier(&owner, &block).unwrap();
    let mut after = before.clone(); after.restore_by_originator(&owner, Timestamp::new(100)).unwrap();
    batch.persist_stable_trst_transition(&before, &after).unwrap(); batch.commit().unwrap();
    for commit in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        let current = batch.read_trst_engine(100).unwrap();
        assert_eq!(current.save_wallets(), after.save_wallets());
        batch.rewind_block(&block).unwrap();
        assert!(batch.auxiliary_cache.borrow().trst.is_some());
        let rewound = batch.read_trst_engine(100).unwrap();
        assert_eq!(rewound.save_wallets(), before.save_wallets());
        assert!(rewound.merger_graph == before.merger_graph);
        assert!(std::ptr::eq(&*rewound.wallets[&untouched], &*current.wallets[&untouched]));
        assert_matches_full_restore(&batch, &rewound);
        if commit { batch.commit().unwrap(); }
    }
    drop(store);
    let reopened = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let cold = reopened.tx_begin_write().unwrap().read_trst_engine(100).unwrap();
    assert_eq!(cold.save_wallets(), before.save_wallets());
    assert!(cold.merger_graph == before.merger_graph);
}

#[test]
fn revision_row_rewind_rejects_surviving_pending_basis_and_cannot_commit_partial_undo() {
    use burst_store::pending::{PendingInfo, PendingProvenance};
    use burst_types::TrstBasis;
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let owner = WalletAddress::new("brst_holder_1");
    let mut batch = store.tx_begin_write().unwrap();
    seed_stable(&mut batch, &revision_rewind_fixture());
    let before = batch.read_trst_engine(100).unwrap(); batch.commit().unwrap();
    let block = BlockHash::new([91; 32]);
    let mut batch = store.tx_begin_write().unwrap();
    batch.begin_block_undo(block, owner.clone(), 1).unwrap();
    let mut account = AccountInfo::unopened(owner.clone()); account.head = block; account.block_count = 1;
    batch.put_block_with_account(&block, b"block", &owner, 1).unwrap();
    batch.put_account_info(&account, false).unwrap(); batch.put_frontier(&owner, &block).unwrap();
    let mut after = before.clone(); after.restore_by_originator(&owner, Timestamp::new(100)).unwrap();
    batch.persist_stable_trst_transition(&before, &after).unwrap(); batch.commit().unwrap();
    let mut revision = None;
    after.merger_graph.visit_records(|row| {
        if let GraphRecord::CurrentRevision { merge, revision: hash } = row {
            if merge == TxHash::new([90; 32]) { revision = Some(hash); }
        }
        Ok::<_, String>(())
    }).unwrap();
    let pending = PendingInfo { source: owner.clone(), amount: 1, timestamp: Timestamp::new(100), return_credit: false,
        provenance: vec![PendingProvenance { basis: TrstBasis::Remainder(revision.unwrap()), amount: 1,
            origin: TxHash::new([90; 32]), origin_wallet: owner.clone(), origin_timestamp: Timestamp::new(100),
            effective_origin_timestamp: Timestamp::new(100), revoked_amounts: Default::default() }] };
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_pending(&owner, TxHash::new([92; 32]).as_bytes(), &bincode::serialize(&pending).unwrap()).unwrap();
    batch.validate_pending_bases(&after.merger_graph).unwrap(); batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.read_trst_engine(100).unwrap();
    assert!(batch.rewind_block(&block).is_err());
    assert!(batch.commit().is_err());
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.get_account(&owner).unwrap().head, block);
    assert!(batch.block_undo(&block).unwrap().is_some());
    assert_eq!(batch.read_trst_engine(100).unwrap().save_wallets(), after.save_wallets());
}

#[test]
fn captured_trst_reader_preserves_all_formats_and_rejects_live_corruption() {
    for format in 0..3 {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let mut before = revision_rewind_fixture();
        before.wallets.insert(WalletAddress::new("brst_empty"), WalletPortfolio::default().into());
        let mut writer = store.tx_begin_write().unwrap();
        if format == 2 { seed_stable(&mut writer, &before); }
        else { seed(&mut writer, &before, format == 1); }
        let cached = writer.read_trst_engine(100).unwrap();
        writer.commit().unwrap();
        let reader = store.tx_begin_read().unwrap();
        let view = store.read_view(&reader).unwrap();
        let captured = view.read_trst_engine(100).unwrap();
        assert!(captured.wallet_snapshot_eq(&cached));
        assert!(captured.merger_graph == cached.merger_graph);
        let stages = std::cell::RefCell::new(std::collections::BTreeMap::new());
        let comparison = view.read_trst_engine_for_comparison_with_poll(100, &|stage| {
            *stages.borrow_mut().entry(stage).or_insert(0usize) += 1;
            Ok(())
        }).unwrap();
        assert!(comparison.wallet_snapshot_eq(&view.read_trst_engine_for_comparison(100).unwrap()));
        let stages = stages.into_inner();
        let tokens: usize = before.wallets.values().map(|portfolio| portfolio.tokens.len()).sum();
        assert_eq!(stages["trst:index-token"], tokens, "format={format}: rebuild holder indexes exactly once");
        for required in ["storage:trst-holdings", "storage:trst-graph", "storage:trst-validation", "storage:trst-restored"] {
            assert!(stages.contains_key(required));
        }
        for required in ["trst:graph-node", "trst:graph-contribution", "trst:basis-token",
            "trst:basis-wallet", "trst:restored-token", "trst:index-token", "trst:restored-origin", "trst:dependency-token"] {
            assert!(stages.contains_key(required), "unchecked validation stage {required}");
        }
        if format > 0 {
            assert!(stages["storage:trst-record"] >= 2);
            assert!(stages["storage:trst-graph-record"] >= 2);
        }
        if format == 2 {
            for required in ["storage:trst-header", "storage:trst-portfolio", "storage:trst-holding",
                "storage:trst-portfolio-finished", "storage:trst-restored-holding", "storage:trst-inventory"] {
                assert!(stages[required] >= 2, "missing repeated stage {required}");
            }
        }
        for (target, count) in stages {
            for stop_at in [1, count.min(2)] {
                let calls = std::cell::Cell::new(0);
                let result = view.read_trst_engine_for_comparison_with_poll(100, &|stage| {
                    if stage == target {
                        calls.set(calls.get() + 1);
                        if calls.get() == stop_at { return Err(StoreError::Backend("TRST cancellation sentinel".into())); }
                    }
                    Ok(())
                });
                assert!(matches!(result, Err(StoreError::Backend(ref message)) if message == "TRST cancellation sentinel"),
                    "format={format} stage={target} stop_at={stop_at}");
                assert_eq!(calls.get(), stop_at, "callback continued after error");
            }
        }
        assert!(comparison.wallet_snapshot_eq(&captured));
        assert!(comparison.merger_graph == captured.merger_graph);
        for period in [0, 50, 100, 200, u64::MAX] {
            for now in [0, 100, 199, 200, 300, u64::MAX] {
                let mut full = captured.clone();
                let mut raw = comparison.clone();
                assert_eq!(full.set_expiry_period(period, Timestamp::new(now)),
                    raw.set_expiry_period(period, Timestamp::new(now)));
                assert!(raw.wallet_snapshot_eq(&full), "format={format} period={period} now={now}");
                assert!(raw.merger_graph == full.merger_graph);
                assert_eq!(comparison.wallet_snapshot_eq_at_with_poll(&captured, period, Timestamp::new(now), &|_| Ok(())).unwrap(),
                    raw.wallet_snapshot_eq(&full));
                assert_eq!(comparison.merger_graph.snapshot_eq_with_poll(&captured.merger_graph, &|_| Ok(())).unwrap(),
                    raw.merger_graph == full.merger_graph);
            }
        }
        let mut writer = store.tx_begin_write().unwrap();
        writer.put_meta(GRAPH_FORMAT, &[99]).unwrap();
        writer.commit().unwrap();
        assert!(view.read_trst_engine(100).unwrap().wallet_snapshot_eq(&cached));
        assert!(view.read_trst_engine_for_comparison(100).unwrap().wallet_snapshot_eq(&cached));
        let writer = store.tx_begin_write().unwrap();
        assert!(writer.read_trst_engine(100).is_err());
        assert!(writer.read_view().read_trst_engine(100).is_err());
        assert!(writer.read_view().read_trst_engine_for_comparison(100).is_err());
    }
}

#[test]
fn captured_trst_comparison_rejects_invalid_portfolios_like_full_restoration() {
    for corruption in 0..6 {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let mut engine = fixture();
        let holder = WalletAddress::new("brst_holder_1");
        let portfolio = engine.wallets.get_mut(&holder).unwrap();
        match corruption {
            0 => portfolio.cached_transferable += 1,
            1 => portfolio.earliest_expiry = Some(Timestamp::new(201)),
            2 => portfolio.tokens[0].holder = WalletAddress::new("brst_wrong_holder"),
            3 => {
                let duplicate = portfolio.tokens[0].clone();
                portfolio.tokens.push(duplicate);
                portfolio.cached_transferable *= 2;
            }
            4 => portfolio.tokens[0].origin = TxHash::new([99; 32]),
            5 => portfolio.tokens[0].revoked_origin = Some(TxHash::new([99; 32])),
            _ => unreachable!(),
        }
        let mut writer = store.tx_begin_write().unwrap();
        // Legacy storage permits malformed snapshots to reach the shared cold
        // validator; record encoders can reject them before restoration.
        seed(&mut writer, &engine, false);
        let view = writer.read_view();
        assert!(view.read_trst_engine(100).is_err(), "full corruption={corruption}");
        assert!(view.read_trst_engine_for_comparison(100).is_err(), "comparison corruption={corruption}");
    }
}
