use super::*;
use burst_store::MetaStore;

fn seed(batch: &mut LmdbWriteTransaction<'_>) {
    batch
        .put_meta(
            DelegationEngine::meta_key(),
            &DelegationEngine::new(8).save_state(),
        )
        .unwrap();
    batch
        .put_meta(
            "consti_engine",
            &bincode::serialize(&ConstiEngine::bootstrap()).unwrap(),
        )
        .unwrap();
}

#[test]
fn committed_roots_survive_unrelated_writes_and_isolate_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch.read_delegation_engine().unwrap().is_none());
    assert!(batch.read_constitution_engine().unwrap().is_none());
    seed(&mut batch);
    let delegation = batch.read_delegation_engine().unwrap().unwrap();
    let constitution = batch.read_constitution_engine().unwrap().unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(delegation.shares_storage_with(&batch.read_delegation_engine().unwrap().unwrap()));
    assert!(constitution.shares_storage_with(&batch.read_constitution_engine().unwrap().unwrap()));
    let mut candidate = batch.read_delegation_engine().unwrap().unwrap();
    candidate
        .delegate(
            &WalletAddress::new("brst_alice"),
            &WalletAddress::new("brst_bob"),
        )
        .unwrap();
    assert!(delegation
        .get_delegate(&WalletAddress::new("brst_alice"))
        .is_none());
    let mut candidate = batch.read_constitution_engine().unwrap().unwrap();
    candidate.get_amendment_mut(&TxHash::new([1; 32]));
    assert!(!candidate.shares_storage_with(&constitution));
    batch.put_meta("unrelated", b"payment").unwrap();
    batch.commit().unwrap();
    let batch = store.tx_begin_write().unwrap();
    assert!(delegation.shares_storage_with(&batch.read_delegation_engine().unwrap().unwrap()));
    assert!(constitution.shares_storage_with(&batch.read_constitution_engine().unwrap().unwrap()));
    drop(batch);
    store.meta_store().put_meta("external", b"write").unwrap();
    let batch = store.tx_begin_write().unwrap();
    assert!(!delegation.shares_storage_with(&batch.read_delegation_engine().unwrap().unwrap()));
    assert!(!constitution.shares_storage_with(&batch.read_constitution_engine().unwrap().unwrap()));
}

#[test]
fn staged_changes_abort_and_failed_commit_never_publish() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch);
    let original = batch.read_delegation_engine().unwrap().unwrap();
    let constitution = batch.read_constitution_engine().unwrap().unwrap();
    batch.commit().unwrap();
    for poison in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_meta(
                DelegationEngine::meta_key(),
                &DelegationEngine::new(3).save_state(),
            )
            .unwrap();
        batch
            .put_meta(
                "consti_engine",
                &bincode::serialize(&ConstiEngine::new()).unwrap(),
            )
            .unwrap();
        assert_eq!(
            batch.read_delegation_engine().unwrap().unwrap().max_depth(),
            3
        );
        assert_eq!(
            batch.read_constitution_engine().unwrap().unwrap(),
            ConstiEngine::new()
        );
        if poison {
            assert!(batch
                .put_block_meta(
                    &BlockHash::new([1; 32]),
                    &WalletAddress::new("brst_alice"),
                    1,
                    "bad",
                    b"bad"
                )
                .is_err());
            assert!(batch.commit().is_err());
        }
    }
    let batch = store.tx_begin_write().unwrap();
    assert!(original.shares_storage_with(&batch.read_delegation_engine().unwrap().unwrap()));
    assert!(constitution.shares_storage_with(&batch.read_constitution_engine().unwrap().unwrap()));
}

#[test]
fn mutations_deletion_corruption_and_reopen_use_current_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        seed(&mut batch);
        batch.read_delegation_engine().unwrap();
        batch.read_constitution_engine().unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.delete_meta(DelegationEngine::meta_key()).unwrap();
        batch.delete_meta("consti_engine").unwrap();
        assert!(batch.read_delegation_engine().unwrap().is_none());
        assert!(batch.read_constitution_engine().unwrap().is_none());
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch.read_delegation_engine().unwrap().is_none());
        assert!(batch.read_constitution_engine().unwrap().is_none());
        seed(&mut batch);
        batch.commit().unwrap();
        for (key, mut bytes) in [
            (
                DelegationEngine::meta_key(),
                DelegationEngine::new(8).save_state(),
            ),
            (
                "consti_engine",
                bincode::serialize(&ConstiEngine::bootstrap()).unwrap(),
            ),
        ] {
            bytes.push(0);
            store.meta_store().put_meta(key, &bytes).unwrap();
            let batch = store.tx_begin_write().unwrap();
            if key == "consti_engine" {
                assert!(batch.read_constitution_engine().is_err());
            } else {
                assert!(batch.read_delegation_engine().is_err());
            }
        }
        let mut batch = store.tx_begin_write().unwrap();
        seed(&mut batch);
        batch
            .put_meta(
                DelegationEngine::meta_key(),
                &DelegationEngine::new(4).save_state(),
            )
            .unwrap();
        batch.commit().unwrap();
    }
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(
        batch.read_delegation_engine().unwrap().unwrap().max_depth(),
        4
    );
    assert_eq!(
        batch.read_constitution_engine().unwrap().unwrap(),
        ConstiEngine::bootstrap()
    );
}

#[test]
fn block_rewind_invalidates_both_auxiliary_images() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch);
    batch.commit().unwrap();
    let block = BlockHash::new([9; 32]);
    let account = WalletAddress::new("brst_alice");
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
    batch
        .put_meta(
            DelegationEngine::meta_key(),
            &DelegationEngine::new(2).save_state(),
        )
        .unwrap();
    batch
        .put_meta(
            "consti_engine",
            &bincode::serialize(&ConstiEngine::new()).unwrap(),
        )
        .unwrap();
    batch.read_delegation_engine().unwrap();
    batch.read_constitution_engine().unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.rewind_block(&block).unwrap();
    assert_eq!(
        batch.read_delegation_engine().unwrap().unwrap().max_depth(),
        8
    );
    assert_eq!(
        batch.read_constitution_engine().unwrap().unwrap(),
        ConstiEngine::bootstrap()
    );
    batch.commit().unwrap();
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(
        batch.read_delegation_engine().unwrap().unwrap().max_depth(),
        8
    );
    assert_eq!(
        batch.read_constitution_engine().unwrap().unwrap(),
        ConstiEngine::bootstrap()
    );
}

#[test]
fn pinned_auxiliary_reads_reuse_roots_and_do_not_replace_newer_publications() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch);
    batch.commit().unwrap();
    let old = store.tx_begin_read().unwrap();
    let delegation = store.read_delegation_snapshot(&old).unwrap().unwrap();
    let constitution = store.read_constitution_snapshot(&old).unwrap().unwrap();
    assert!(delegation.shares_storage_with(&store.read_delegation_snapshot(&old).unwrap().unwrap()));
    assert!(
        constitution.shares_storage_with(&store.read_constitution_snapshot(&old).unwrap().unwrap())
    );
    let mut batch = store.tx_begin_write().unwrap();
    assert!(delegation.shares_storage_with(&batch.read_delegation_engine().unwrap().unwrap()));
    batch
        .put_meta(
            DelegationEngine::meta_key(),
            &DelegationEngine::new(3).save_state(),
        )
        .unwrap();
    batch
        .put_meta(
            "consti_engine",
            &bincode::serialize(&ConstiEngine::new()).unwrap(),
        )
        .unwrap();
    let next_delegation = batch.read_delegation_engine().unwrap().unwrap();
    let next_constitution = batch.read_constitution_engine().unwrap().unwrap();
    batch.commit().unwrap();
    assert_eq!(
        store
            .read_delegation_snapshot(&old)
            .unwrap()
            .unwrap()
            .max_depth(),
        8
    );
    assert_eq!(
        store.read_constitution_snapshot(&old).unwrap().unwrap(),
        constitution
    );
    drop(old);
    let new = store.tx_begin_read().unwrap();
    assert!(next_delegation
        .shares_storage_with(&store.read_delegation_snapshot(&new).unwrap().unwrap()));
    assert!(next_constitution
        .shares_storage_with(&store.read_constitution_snapshot(&new).unwrap().unwrap()));
}

#[test]
fn pinned_auxiliary_unknown_identity_and_external_corruption_cannot_hit_cache() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let empty = store.tx_begin_read().unwrap();
    assert!(store.read_delegation_snapshot(&empty).unwrap().is_none());
    assert!(store.read_constitution_snapshot(&empty).unwrap().is_none());
    drop(empty);
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch);
    batch.commit().unwrap();
    let mut view = store.tx_begin_read().unwrap();
    let original = store.read_delegation_snapshot(&view).unwrap().unwrap();
    let constitution = store.read_constitution_snapshot(&view).unwrap().unwrap();
    view.generation = None;
    assert!(!original.shares_storage_with(&store.read_delegation_snapshot(&view).unwrap().unwrap()));
    assert!(!constitution
        .shares_storage_with(&store.read_constitution_snapshot(&view).unwrap().unwrap()));
    drop(view);
    store
        .meta_store()
        .put_meta(DelegationEngine::meta_key(), b"corrupt")
        .unwrap();
    store
        .meta_store()
        .put_meta("consti_engine", b"corrupt")
        .unwrap();
    let view = store.tx_begin_read().unwrap();
    assert!(store.read_delegation_snapshot(&view).is_err());
    assert!(store.read_constitution_snapshot(&view).is_err());
}

fn electorate(store: &crate::LmdbStore) -> std::sync::Arc<[WalletAddress]> {
    store
        .read_verified_snapshot(&store.tx_begin_read().unwrap())
        .unwrap()
}

#[test]
fn electorate_cache_tracks_membership_not_balances_and_preserves_old_views() {
    use burst_types::WalletState;
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut row = burst_store::account::AccountInfo::unopened(WalletAddress::new("brst_elector"));
    row.state = WalletState::Verified;
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_account_info(&row, false).unwrap();
    batch
        .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &[1; 32])
        .unwrap();
    batch.commit().unwrap();
    let original = electorate(&store);
    assert_eq!(&*original, &[row.address.clone()]);
    assert!(std::sync::Arc::ptr_eq(&original, &electorate(&store)));
    reserve_test_votes(&store);
    assert!(std::sync::Arc::ptr_eq(&original, &electorate(&store)));
    row.trst_balance = 99;
    row.state = WalletState::Challenged; // Still in the eligible electorate.
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_account_info(&row, false).unwrap();
    batch.commit().unwrap();
    assert!(std::sync::Arc::ptr_eq(&original, &electorate(&store)));
    row.state = WalletState::Unverified;
    {
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_account_info(&row, false).unwrap();
        // Abort must not publish membership removal.
    }
    assert!(std::sync::Arc::ptr_eq(&original, &electorate(&store)));
    let old = store.tx_begin_read().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_account_info(&row, false).unwrap();
    batch.commit().unwrap();
    assert_eq!(&*store.read_verified_snapshot(&old).unwrap(), &*original);
    drop(old);
    assert!(electorate(&store).is_empty());
    assert_eq!(original.len(), 1);
}

#[test]
fn electorate_cache_rewinds_membership_and_clears_checkpoint_state() {
    use burst_types::WalletState;
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let address = WalletAddress::new("brst_elector_undo");
    let mut row = burst_store::account::AccountInfo::unopened(address.clone());
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_account_info(&row, false).unwrap();
    batch.commit().unwrap();
    assert!(electorate(&store).is_empty());
    let hash = BlockHash::new([7; 32]);
    let mut batch = store.tx_begin_write().unwrap();
    batch.begin_block_undo(hash, address.clone(), 1).unwrap();
    batch
        .put_block_with_account(&hash, b"electorate-block", &address, 1)
        .unwrap();
    batch.put_frontier(&address, &hash).unwrap();
    row.state = WalletState::Verified;
    row.head = hash;
    row.block_count = 1;
    batch.put_account_info(&row, false).unwrap();
    batch.commit().unwrap();
    assert_eq!(electorate(&store).len(), 1);
    let before_rewind = store
        .read_governance_projection_identity(&store.tx_begin_read().unwrap())
        .unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.rewind_block(&hash).unwrap();
    batch.commit().unwrap();
    assert!(electorate(&store).is_empty());
    assert_ne!(
        before_rewind,
        store
            .read_governance_projection_identity(&store.tx_begin_read().unwrap())
            .unwrap()
    );
    let mut batch = store.tx_begin_write().unwrap();
    row.head = BlockHash::ZERO;
    row.block_count = 0;
    batch.put_account_info(&row, false).unwrap();
    batch.commit().unwrap();
    assert_eq!(electorate(&store).len(), 1);
    let before_clear = store
        .read_governance_projection_identity(&store.tx_begin_read().unwrap())
        .unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .clear_checkpoint_rows(UndoTable::Accounts, None)
        .unwrap();
    batch.commit().unwrap();
    assert!(electorate(&store).is_empty());
    assert_ne!(
        before_clear,
        store
            .read_governance_projection_identity(&store.tx_begin_read().unwrap())
            .unwrap()
    );
}

#[test]
fn electorate_retention_budget_and_unknown_writes_preserve_correctness() {
    use burst_types::WalletState;
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut row =
        burst_store::account::AccountInfo::unopened(WalletAddress::new("brst_elector_budget"));
    row.state = WalletState::Verified;
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_account_info(&row, false).unwrap();
    batch.commit().unwrap();
    let mut view = store.tx_begin_read().unwrap();
    let first = store.read_verified_snapshot_with_limit(&view, 0).unwrap();
    let second = store.read_verified_snapshot_with_limit(&view, 0).unwrap();
    assert_eq!(first, second);
    assert!(!std::sync::Arc::ptr_eq(&first, &second));
    let retained = store.read_verified_snapshot(&view).unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &retained,
        &store.read_verified_snapshot(&view).unwrap()
    ));
    view.generation = None;
    assert!(!std::sync::Arc::ptr_eq(
        &retained,
        &store.read_verified_snapshot(&view).unwrap()
    ));
    drop(view);
    store.meta_store().put_meta("unknown", b"write").unwrap();
    assert!(!std::sync::Arc::ptr_eq(&retained, &electorate(&store)));
    let mut raw = store.env().write_txn().unwrap();
    store
        .verified_accounts_db
        .put(&mut raw, row.address.as_str().as_bytes(), &[2])
        .unwrap();
    raw.commit().unwrap();
    assert!(store
        .read_verified_snapshot(&store.tx_begin_read().unwrap())
        .is_err());
}

#[test]
fn projection_identity_survives_unrelated_commits_but_not_input_changes() {
    use burst_types::WalletState;
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let identity = || {
        store
            .read_governance_projection_identity(&store.tx_begin_read().unwrap())
            .unwrap()
    };
    let mut row =
        burst_store::account::AccountInfo::unopened(WalletAddress::new("brst_projection_identity"));
    row.state = WalletState::Verified;
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_account_info(&row, false).unwrap();
    batch.commit().unwrap();
    let original = identity();
    let old = store.tx_begin_read().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    row.trst_balance = 7;
    batch.put_account_info(&row, false).unwrap();
    batch.put_meta("unrelated", b"payment").unwrap();
    batch.commit().unwrap();
    assert_eq!(
        store.read_governance_projection_identity(&old),
        None,
        "old views must not take the newer cache identity"
    );
    drop(old);
    assert_eq!(identity(), original);
    {
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_meta(DelegationEngine::meta_key(), b"aborted")
            .unwrap();
    }
    assert_eq!(identity(), original);
    for key in [
        DelegationEngine::meta_key(),
        "protocol_params",
        "governance_records_format",
        "governance_state/v1/test",
    ] {
        let before = identity();
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(key, b"changed-input").unwrap();
        batch.commit().unwrap();
        assert_ne!(identity(), before, "{key}");
    }
    let before = identity();
    row.state = WalletState::Revoked;
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_account_info(&row, false).unwrap();
    batch.commit().unwrap();
    assert_ne!(identity(), before);
    let before = identity();
    store.meta_store().put_meta("external", b"write").unwrap();
    assert_ne!(identity(), before);
    let mut unknown = store.tx_begin_read().unwrap();
    unknown.generation = None;
    assert_eq!(store.read_governance_projection_identity(&unknown), None);
}

#[test]
fn payment_authority_identity_tracks_dependencies_and_committed_views() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let identity = || {
        store
            .read_payment_authority_identity(&store.tx_begin_read().unwrap())
            .unwrap()
    };
    let original = identity();
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta("unrelated_payment", b"one").unwrap();
    batch
        .put_meta("payment_epoch-other", b"near prefix")
        .unwrap();
    batch.commit().unwrap();
    assert_eq!(identity(), original);
    for key in [
        "checkpoint_installed/v1",
        "checkpoint_installed/pages/v1/00000000",
        "payment_epoch/v1",
        "payment_epoch/v1/cutoff",
        "payment_epoch/v1/vote_limit",
        "payment_epoch/v1/counting_limit",
        "payment_epoch/v1/closed/test",
        "payment_epoch/v2",
        "payment_epoch/v2/closed/test",
        "payment_epoch/v1/unknown",
        "confirmed_governance/v2",
    ] {
        let before = identity();
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(key, b"aborted").unwrap();
        drop(batch);
        assert_eq!(identity(), before, "{key} abort");
        let old = store.tx_begin_read().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(key, b"committed").unwrap();
        batch.commit().unwrap();
        // LMDB uses one reader slot per thread; keep the old view here and
        // obtain the new committed view on a different reader thread.
        let after = std::thread::scope(|scope| scope.spawn(&identity).join().unwrap());
        assert_ne!(after, before, "{key} commit");
        assert_eq!(store.read_payment_authority_identity(&old), None);
        drop(old);
        let mut batch = store.tx_begin_write().unwrap();
        batch.delete_meta(key).unwrap();
        batch.commit().unwrap();
        assert_ne!(identity(), after, "{key} deletion");
    }
    let before = identity();
    store.meta_store().put_meta("untracked", b"write").unwrap();
    assert_ne!(identity(), before);
    let before = identity();
    let schema = store.meta_store().get_meta("schema_version").unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.clear_checkpoint_rows(UndoTable::Meta, None).unwrap();
    // Also make a tracked write: the identity must not survive when cache
    // publication advances to the cleared transaction's actual generation.
    batch.put_meta("schema_version", &schema).unwrap();
    batch.put_meta("replacement", b"checkpoint").unwrap();
    batch.commit().unwrap();
    assert_ne!(identity(), before);
    let raw = crate::LmdbReadTransaction::from_ro(store.env().read_txn().unwrap());
    assert_eq!(store.read_payment_authority_identity(&raw), None);
    drop(raw);
    let before = identity();
    drop(store);
    let reopened = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    assert_ne!(
        reopened.read_payment_authority_identity(&reopened.tx_begin_read().unwrap()),
        Some(before)
    );
}

#[test]
fn confirmed_authority_snapshots_reuse_only_valid_committed_roots() {
    use crate::confirmed_authority::{Authority, Snapshot, KEY};
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let account = burst_store::account::AccountInfo::unopened(WalletAddress::new("brst_authority"));
    let legacy = std::collections::BTreeMap::from([(
        account.address.to_string(),
        Authority::from(&account),
    )]);
    let genesis = BlockHash::new([1; 32]);
    let bytes = bincode::serialize(&(genesis, &legacy)).unwrap();
    let original = Snapshot::decode(&bytes).unwrap();
    assert_eq!(bincode::serialize(&original).unwrap(), bytes);
    let mut batch = store.tx_begin_write().unwrap();
    batch.write_confirmed_authority(original.clone()).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let current = batch.read_confirmed_authority().unwrap();
    assert!(current.accounts.shares_storage_with(&original.accounts));
    batch.write_confirmed_authority(current).unwrap();
    assert!(!batch.changed_value);
    batch.put_meta("unrelated", b"payment").unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let mut changed = batch.read_confirmed_authority().unwrap();
    assert!(changed.accounts.shares_storage_with(&original.accounts));
    let key = account.address.to_string();
    changed.accounts.get_mut(&key).unwrap().evicted = true;
    batch.write_confirmed_authority(changed).unwrap();
    drop(batch);
    let mut batch = store.tx_begin_write().unwrap();
    assert!(!batch.read_confirmed_authority().unwrap().accounts[&key].evicted);
    batch.put_meta(KEY, b"corrupt").unwrap();
    assert!(batch.read_confirmed_authority().is_err());
    drop(batch);
    assert_eq!(
        bincode::serialize(&store.read_confirmed_authority_snapshot().unwrap()).unwrap(),
        bytes
    );
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let reopened = batch.read_confirmed_authority().unwrap();
    assert_eq!(bincode::serialize(&reopened).unwrap(), bytes);
    assert!(!reopened.accounts.shares_storage_with(&original.accounts));
    batch.clear_checkpoint_rows(UndoTable::Meta, None).unwrap();
    assert!(batch.read_confirmed_authority().is_err());
    drop(batch);
    let mut batch = store.tx_begin_write().unwrap();
    let mut replacement = batch.read_confirmed_authority().unwrap();
    replacement.accounts.get_mut(&key).unwrap().evicted = true;
    batch.write_confirmed_authority(replacement).unwrap();
    batch.commit().unwrap();
    assert!(
        store
            .tx_begin_write()
            .unwrap()
            .read_confirmed_authority()
            .unwrap()
            .accounts[&key]
            .evicted
    );
    store.meta_store().put_meta(KEY, b"corrupt").unwrap();
    assert!(store
        .tx_begin_write()
        .unwrap()
        .read_confirmed_authority()
        .is_err());
}

#[test]
fn confirmed_authority_rows_convert_atomically_and_write_only_changed_accounts() {
    use crate::confirmed_authority::{Authority, Snapshot, HEADER, KEY, ROW_PREFIX};
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let accounts = (0..1024)
        .map(|i| {
            let account = burst_store::account::AccountInfo::unopened(WalletAddress::new(format!(
                "brst_holder_{i:04}"
            )));
            (account.address.to_string(), Authority::from(&account))
        })
        .collect();
    let original = Snapshot {
        genesis: BlockHash::new([1; 32]),
        accounts,
    };
    let bytes = bincode::serialize(&original).unwrap();
    store.meta_store().put_meta(KEY, &bytes).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let loaded = batch.read_confirmed_authority().unwrap();
    batch.write_confirmed_authority(loaded).unwrap();
    assert!(batch.get_meta(KEY).unwrap().is_none());
    assert!(batch.get_meta(HEADER).unwrap().is_some());
    drop(batch);
    assert_eq!(store.meta_store().get_meta(KEY).unwrap(), bytes);
    assert!(store.meta_store().get_meta(HEADER).is_err());
    let mut batch = store.tx_begin_write().unwrap();
    let loaded = batch.read_confirmed_authority().unwrap();
    batch.write_confirmed_authority(loaded).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.auxiliary_cache.get_mut().confirmed_authority = None;
    batch.write_confirmed_authority(original.clone()).unwrap();
    assert!(
        !batch.changed_value,
        "cold identical replacement must retain existing rows"
    );
    drop(batch);
    let mut batch = store.tx_begin_write().unwrap();
    batch.auxiliary_cache.get_mut().confirmed_authority = None;
    let mut reduced = original.clone();
    for i in 0..600 {
        reduced.accounts.remove(&format!("brst_holder_{i:04}"));
    }
    reduced
        .accounts
        .get_mut(&"brst_holder_0700".to_owned())
        .unwrap()
        .evicted = true;
    batch.write_confirmed_authority(reduced.clone()).unwrap();
    assert_eq!(
        batch
            .checkpoint_metadata(ROW_PREFIX.as_bytes())
            .unwrap()
            .len(),
        424
    );
    batch.auxiliary_cache.get_mut().confirmed_authority = None;
    assert_eq!(
        bincode::serialize(&batch.read_confirmed_authority().unwrap()).unwrap(),
        bincode::serialize(&reduced).unwrap()
    );
    drop(batch);
    assert_eq!(
        bincode::serialize(&store.read_confirmed_authority_snapshot().unwrap()).unwrap(),
        bytes
    );
    let mut batch = store.tx_begin_write().unwrap();
    let before = batch.checkpoint_metadata(ROW_PREFIX.as_bytes()).unwrap();
    let mut changed = batch.read_confirmed_authority().unwrap();
    changed
        .accounts
        .get_mut(&"brst_holder_0500".to_owned())
        .unwrap()
        .representative = WalletAddress::new("brst_changed_representative");
    batch.write_confirmed_authority(changed.clone()).unwrap();
    let after = batch.checkpoint_metadata(ROW_PREFIX.as_bytes()).unwrap();
    assert_eq!(before.len(), 1024);
    assert_eq!(before.iter().zip(&after).filter(|(a, b)| a != b).count(), 1);
    batch.commit().unwrap();
    assert_eq!(
        bincode::serialize(&store.read_confirmed_authority_snapshot().unwrap()).unwrap(),
        bincode::serialize(&changed).unwrap()
    );
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    assert_eq!(
        bincode::serialize(&store.read_confirmed_authority_snapshot().unwrap()).unwrap(),
        bincode::serialize(&changed).unwrap()
    );
    // A legacy image beside normalized rows is ambiguous, even if independently valid.
    store.meta_store().put_meta(KEY, &bytes).unwrap();
    assert!(store.read_confirmed_authority_snapshot().is_err());
    store.meta_store().delete_meta(KEY).unwrap();
    store.meta_store().delete_meta(HEADER).unwrap();
    assert!(store.read_confirmed_authority_snapshot().is_err());
}

#[test]
fn confirmed_authority_rows_validate_manifest_replacement_and_legacy_fallback() {
    use crate::confirmed_authority::{Authority, Snapshot, HEADER, KEY, ROW_PREFIX};
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let a = burst_store::account::AccountInfo::unopened(WalletAddress::new("brst_a"));
    let b = burst_store::account::AccountInfo::unopened(WalletAddress::new("brst_b"));
    let original = Snapshot {
        genesis: BlockHash::new([1; 32]),
        accounts: [
            (a.address.to_string(), Authority::from(&a)),
            (b.address.to_string(), Authority::from(&b)),
        ]
        .into_iter()
        .collect(),
    };
    let mut batch = store.tx_begin_write().unwrap();
    assert!(!batch.has_confirmed_authority_storage().unwrap());
    batch.write_confirmed_authority(original.clone()).unwrap();
    assert!(batch.has_confirmed_authority_storage().unwrap());
    batch.commit().unwrap();
    let manifest = store.meta_store().get_meta(HEADER).unwrap();
    let (genesis, count, bytes): (BlockHash, u64, u64) = bincode::deserialize(&manifest).unwrap();
    for bad in [
        (genesis, count + 1, bytes),
        (genesis, count, bytes + 1),
        (BlockHash::ZERO, count, bytes),
        (genesis, 1_000_001, bytes),
        (genesis, count, (64 << 20) + 1),
    ] {
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_meta(HEADER, &bincode::serialize(&bad).unwrap())
            .unwrap();
        assert!(batch.read_confirmed_authority().is_err());
        drop(batch);
        assert!(store.read_confirmed_authority_snapshot().is_ok());
    }
    let mut batch = store.tx_begin_write().unwrap();
    let mut reduced = batch.read_confirmed_authority().unwrap();
    reduced.accounts.remove(&a.address.to_string());
    batch.write_confirmed_authority(reduced).unwrap();
    assert!(batch
        .get_meta(&format!("{ROW_PREFIX}{}", a.address))
        .unwrap()
        .is_none());
    batch.commit().unwrap();
    assert_eq!(
        store
            .read_confirmed_authority_snapshot()
            .unwrap()
            .accounts
            .len(),
        1
    );
    // No warm snapshot is needed for checkpoint replacement of malformed metadata.
    store.meta_store().put_meta(HEADER, b"bad").unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.write_confirmed_authority(original.clone()).unwrap();
    batch.commit().unwrap();
    assert_eq!(
        store
            .read_confirmed_authority_snapshot()
            .unwrap()
            .accounts
            .len(),
        2
    );
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .begin_block_undo(BlockHash::new([2; 32]), a.address.clone(), 2)
        .unwrap();
    assert!(batch.write_confirmed_authority(original.clone()).is_err());
    assert!(batch.commit().is_err());
    let long = burst_store::account::AccountInfo::unopened(WalletAddress::new(format!(
        "brst_{}",
        "x".repeat(600)
    )));
    let mut fallback = original.clone();
    fallback
        .accounts
        .insert(long.address.to_string(), Authority::from(&long));
    let mut batch = store.tx_begin_write().unwrap();
    batch.read_confirmed_authority().unwrap();
    batch.write_confirmed_authority(fallback).unwrap();
    assert!(batch.get_meta(KEY).unwrap().is_some());
    assert!(batch.get_meta(HEADER).unwrap().is_none());
    assert!(batch
        .checkpoint_metadata(ROW_PREFIX.as_bytes())
        .unwrap()
        .is_empty());
    batch.commit().unwrap();
    assert_eq!(
        store
            .read_confirmed_authority_snapshot()
            .unwrap()
            .accounts
            .len(),
        3
    );
    // Removing the incompatible legacy key permits atomic normalization again.
    let mut batch = store.tx_begin_write().unwrap();
    let mut normalized = batch.read_confirmed_authority().unwrap();
    normalized.accounts.remove(&long.address.to_string());
    batch.write_confirmed_authority(normalized).unwrap();
    batch.commit().unwrap();
    assert!(store.meta_store().get_meta(KEY).is_err());
    assert_eq!(
        store
            .read_confirmed_authority_snapshot()
            .unwrap()
            .accounts
            .len(),
        2
    );
}

#[test]
fn confirmed_authority_readers_reuse_only_their_pinned_generation() {
    use crate::confirmed_authority::{Authority, Snapshot, HEADER, KEY};
    for legacy in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let account =
            burst_store::account::AccountInfo::unopened(WalletAddress::new("brst_reader"));
        let key = account.address.to_string();
        let mut accounts = burst_utils::SharedMap::default();
        accounts.insert(key.clone(), Authority::from(&account));
        let original = Snapshot {
            genesis: BlockHash::new([1; 32]),
            accounts,
        };
        let mut batch = store.tx_begin_write().unwrap();
        if legacy {
            batch
                .put_meta(KEY, &bincode::serialize(&original).unwrap())
                .unwrap();
        } else {
            batch.write_confirmed_authority(original).unwrap();
        }
        batch.commit().unwrap();
        // Exercise cold publication as well as warm sharing for both formats.
        *store.auxiliary_cache.lock().unwrap() = None;
        let first = store.read_confirmed_authority_snapshot().unwrap();
        let second = store.read_confirmed_authority_snapshot().unwrap();
        assert!(first.accounts.shares_storage_with(&second.accounts));
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_meta("unrelated-reader-input", b"keep roots")
            .unwrap();
        batch.commit().unwrap();
        assert!(first
            .accounts
            .shares_storage_with(&store.read_confirmed_authority_snapshot().unwrap().accounts));

        let old = store.tx_begin_read().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let mut changed = batch.read_confirmed_authority().unwrap();
        changed.accounts.get_mut(&key).unwrap().evicted = true;
        batch.write_confirmed_authority(changed.clone()).unwrap();
        batch.commit().unwrap();
        let old_snapshot = store.read_confirmed_authority_view(&old).unwrap();
        assert!(!old_snapshot.accounts[&key].evicted);
        drop(old);
        let current = store.read_confirmed_authority_snapshot().unwrap();
        assert!(current.accounts[&key].evicted);
        assert!(current.accounts.shares_storage_with(&changed.accounts));

        let mut unknown = store.tx_begin_read().unwrap();
        unknown.generation = None;
        let uncached = store.read_confirmed_authority_view(&unknown).unwrap();
        assert!(!uncached.accounts.shares_storage_with(&current.accounts));
        assert!(uncached.accounts[&key].evicted);
        drop(unknown);
        assert!(current
            .accounts
            .shares_storage_with(&store.read_confirmed_authority_snapshot().unwrap().accounts));

        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(HEADER, b"corrupt").unwrap();
        drop(batch);
        assert!(current
            .accounts
            .shares_storage_with(&store.read_confirmed_authority_snapshot().unwrap().accounts));
        // Untracked writes change the database generation, bypassing old roots.
        store.meta_store().put_meta(HEADER, b"corrupt").unwrap();
        assert!(store.read_confirmed_authority_snapshot().is_err());
    }
}

#[test]
fn speculative_payment_cache_defers_while_vote_publication_owns_cache() {
    use std::sync::{mpsc, Arc};
    use std::time::Duration;
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(crate::LmdbStore::open(directory.path(), 32, 16 << 20).unwrap());
    let generation = store.env().info().last_txn_id;
    *store.auxiliary_cache.lock().unwrap() = Some((generation, AuxiliaryCache::default()));
    // This is the same mutex held across commit_vote_reservation. A speculative
    // read must finish while its publisher remains blocked, even on a cold cut.
    let publisher = store.auxiliary_cache.lock().unwrap();
    let reader = store.clone();
    let (sent, received) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let view = reader.tx_begin_read().unwrap();
        sent.send(
            reader
                .cached_payment_engines(&view, u64::MAX, 131_072, 64 << 20)
                .map(|value| value.is_none()),
        )
        .unwrap();
    });
    let result = received.recv_timeout(Duration::from_secs(2));
    drop(publisher); // Release a broken blocking implementation before joining it.
    worker.join().unwrap();
    assert!(result
        .expect("speculation waited for cache publication")
        .unwrap());
    assert_eq!(
        store.auxiliary_cache.lock().unwrap().as_ref().unwrap().0,
        generation
    );
    assert_eq!(store.env().info().last_txn_id, generation);
}

#[test]
fn snapshot_base_identity_tracks_pages_descriptor_and_environment() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let identity = || {
        store
            .read_snapshot_base_identity(&store.tx_begin_read().unwrap())
            .unwrap()
    };
    let original = identity();
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta("unrelated_snapshot", b"one").unwrap();
    batch
        .put_meta("checkpoint_snapshot-other", b"near prefix")
        .unwrap();
    batch.commit().unwrap();
    assert_eq!(identity(), original);
    for key in [
        "checkpoint_snapshot/v1/base",
        "checkpoint_snapshot/pages/v1/00000000",
        "checkpoint_snapshot/pages/v1/00000001",
        "checkpoint_snapshot/future/unknown",
    ] {
        let before = identity();
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(key, b"aborted").unwrap();
        drop(batch);
        assert_eq!(identity(), before, "{key} abort");
        let old = store.tx_begin_read().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(key, b"committed").unwrap();
        batch.commit().unwrap();
        // LMDB uses one reader slot per thread; keep the old view here and
        // obtain the new committed view on a different reader thread.
        let after = std::thread::scope(|scope| scope.spawn(&identity).join().unwrap());
        assert_ne!(after, before, "{key} commit");
        assert_eq!(store.read_snapshot_base_identity(&old), None);
        drop(old);
        let mut batch = store.tx_begin_write().unwrap();
        batch.delete_meta(key).unwrap();
        batch.commit().unwrap();
        assert_ne!(identity(), after, "{key} deletion");
    }
    let before = identity();
    store.meta_store().put_meta("untracked", b"write").unwrap();
    assert_ne!(identity(), before);
    let before = identity();
    let schema = store.meta_store().get_meta("schema_version").unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.clear_checkpoint_rows(UndoTable::Meta, None).unwrap();
    // Also make a tracked write: the identity must not survive when cache
    // publication advances to the cleared transaction's actual generation.
    batch.put_meta("schema_version", &schema).unwrap();
    batch.put_meta("replacement", b"checkpoint").unwrap();
    batch.commit().unwrap();
    assert_ne!(identity(), before);
    let raw = crate::LmdbReadTransaction::from_ro(store.env().read_txn().unwrap());
    assert_eq!(store.read_snapshot_base_identity(&raw), None);
    drop(raw);
    let foreign_dir = tempfile::tempdir().unwrap();
    let foreign = crate::LmdbStore::open(foreign_dir.path(), 32, 16 << 20).unwrap();
    assert_eq!(
        store.read_snapshot_base_identity(&foreign.tx_begin_read().unwrap()),
        None
    );
    let view = store.tx_begin_read().unwrap();
    let held = store.auxiliary_cache.lock().unwrap();
    assert_eq!(store.read_snapshot_base_identity(&view), None);
    drop(held);
    drop(view);
    let before = identity();
    drop(store);
    let reopened = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    assert_ne!(
        reopened.read_snapshot_base_identity(&reopened.tx_begin_read().unwrap()),
        Some(before)
    );
}
