use super::*;
use crate::LmdbStore;
use burst_store::{account::AccountStore, block::BlockStore, meta::MetaStore};

fn hash(n: u8) -> BlockHash {
    BlockHash::new([n; 32])
}

fn row(height: u64, n: u8) -> CheckpointBlock {
    CheckpointBlock {
        hash: hash(n),
        account: WalletAddress::new("brst_compact_account"),
        height,
        bytes: vec![n],
    }
}

#[test]
fn account_map_projection_matches_slice_and_preserves_abort_and_reopen() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let reference = LmdbStore::open(left.path(), 32, 16 << 20).unwrap();
    let indexed = LmdbStore::open(right.path(), 32, 16 << 20).unwrap();
    let (blocks, accounts) = fixture(&reference);
    fixture(&indexed);
    let accounts_by_address: HashMap<_, _> = accounts
        .iter()
        .map(|account| (account.address.clone(), account.clone()))
        .collect();
    let view = AccountTargets::Indexed(&accounts_by_address);
    assert!(std::ptr::eq(
        view.get(&accounts[0].address).unwrap(),
        &accounts_by_address[&accounts[0].address]
    ));
    let before = contents(&indexed);
    for commit in [false, true] {
        let mut batch = indexed.tx_begin_write().unwrap();
        let projection = batch
            .replace_compact_checkpoint_ledger_account_map(
                hash(1),
                blocks
                    .iter()
                    .map(|block| Ok(std::borrow::Cow::Borrowed(block))),
                &accounts_by_address,
                None,
            )
            .unwrap();
        assert_eq!(projection.retired_blocks, vec![hash(3)]);
        if commit {
            batch.commit().unwrap();
        } else {
            drop(batch);
            assert_eq!(contents(&indexed), before);
        }
    }
    let mut batch = reference.tx_begin_write().unwrap();
    batch
        .replace_compact_checkpoint_ledger(hash(1), &blocks, &accounts)
        .unwrap();
    batch.commit().unwrap();
    assert_eq!(contents(&indexed), contents(&reference));
    indexed.force_sync().unwrap();
    drop(indexed);
    let reopened = LmdbStore::open(right.path(), 32, 16 << 20).unwrap();
    assert_eq!(contents(&reopened), contents(&reference));
}

#[test]
fn account_map_projection_rejects_misbound_accounts_and_late_input_errors() {
    for wrong_key in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let (blocks, accounts) = fixture(&store);
        let before = contents(&store);
        let accounts: HashMap<_, _> = accounts
            .into_iter()
            .map(|account| {
                let key = if wrong_key {
                    WalletAddress::new("brst_wrong_key")
                } else {
                    account.address.clone()
                };
                (key, account)
            })
            .collect();
        let rows = blocks
            .into_iter()
            .map(|block| Ok(std::borrow::Cow::Owned(block)))
            .chain(std::iter::once(Err(invalid("late source failure"))));
        let mut batch = store.tx_begin_write().unwrap();
        let error = batch
            .replace_compact_checkpoint_ledger_account_map(hash(1), rows, &accounts, None)
            .err()
            .unwrap();
        assert!(error.to_string().contains(if wrong_key {
            "invalid compact checkpoint account"
        } else {
            "late source failure"
        }));
        assert!(batch.commit().is_err());
        assert_eq!(contents(&store), before);
    }
}

#[test]
fn sparse_projection_preserves_history_above_old_row_and_byte_limits() {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 256 << 20).unwrap();
    let block_hash = |height: u64| {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&height.to_be_bytes());
        BlockHash::new(bytes)
    };
    let count = MAX_ROWS as u64 + 1;
    let body = vec![7; 512];
    assert!(count * body.len() as u64 > MAX_BYTES as u64);
    let mut account = AccountInfo::unopened(WalletAddress::new("brst_retained_history"));
    account.block_count = count;
    account.confirmation_height = count;
    account.head = block_hash(count);
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, hash(1).as_bytes())
        .unwrap();
    for height in 1..=count {
        batch
            .put_block_with_account(&block_hash(height), &body, &account.address, height)
            .unwrap();
    }
    batch.put_account_info(&account, false).unwrap();
    batch.put_frontier(&account.address, &account.head).unwrap();
    batch.commit().unwrap();
    // Confirmation work for an old retained body need not be resupplied as a witness.
    store
        .meta_store()
        .enqueue_confirmation(&block_hash(2))
        .unwrap();
    account.block_count += 1;
    account.confirmation_height += 1;
    account.head = block_hash(count + 1);
    let rows = [count, count + 1].map(|height| CheckpointBlock {
        hash: block_hash(height),
        account: account.address.clone(),
        height,
        bytes: body.clone(),
    });
    for commit in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        let projection = batch
            .replace_compact_checkpoint_ledger(hash(1), &rows, std::slice::from_ref(&account))
            .unwrap();
        assert_eq!(projection.retained_blocks, count + 1);
        assert_eq!(projection.ledger_blocks, count + 1);
        assert!(projection.retired_blocks.is_empty());
        for height in [1, 2, count / 2, count, count + 1] {
            assert_eq!(
                batch.block_at_height(&account.address, height).unwrap(),
                Some(block_hash(height))
            );
            assert_eq!(
                batch
                    .block_height(&account.address, &block_hash(height))
                    .unwrap(),
                height
            );
            assert_eq!(batch.get_block(&block_hash(height)).unwrap(), body);
        }
        if commit {
            batch.commit().unwrap();
        } else {
            drop(batch);
            assert_eq!(store.block_store().block_count().unwrap(), count);
            assert_eq!(
                store.meta_store().pending_confirmations().unwrap(),
                vec![block_hash(2)]
            );
        }
    }
    store.force_sync().unwrap();
    drop(store);
    let store = LmdbStore::open(directory.path(), 32, 256 << 20).unwrap();
    assert_eq!(store.block_store().block_count().unwrap(), count + 1);
    assert_eq!(
        store
            .account_store()
            .get_account(&account.address)
            .unwrap()
            .head,
        account.head
    );
    assert!(store
        .meta_store()
        .pending_confirmations()
        .unwrap()
        .is_empty());
    assert_eq!(store.block_store().get_block(&block_hash(1)).unwrap(), body);
}

fn fixture(store: &LmdbStore) -> (Vec<CheckpointBlock>, Vec<AccountInfo>) {
    let mut account = AccountInfo::unopened(row(1, 1).account);
    account.block_count = 3;
    account.confirmation_height = 2;
    account.head = hash(3);
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, hash(1).as_bytes())
        .unwrap();
    for n in 1..=3 {
        let block = row(n as u64, n);
        batch
            .put_block_with_account(&block.hash, &block.bytes, &block.account, block.height)
            .unwrap();
    }
    batch.put_account_info(&account, false).unwrap();
    batch.put_frontier(&account.address, &account.head).unwrap();
    batch.put_meta("application-preference", b"keep").unwrap();
    batch.commit().unwrap();
    store.meta_store().enqueue_confirmation(&hash(2)).unwrap();
    account.block_count = 10;
    account.confirmation_height = 10;
    account.head = hash(10);
    (vec![row(2, 2), row(10, 10)], vec![account])
}

#[test]
fn sparse_projection_aborts_when_row_source_fails_after_staging() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let (blocks, accounts) = fixture(&store);
    let rows = blocks
        .into_iter()
        .map(|row| Ok(std::borrow::Cow::Owned(row)))
        .chain(std::iter::once(Err(invalid("injected row read failure"))));
    let mut batch = store.tx_begin_write().unwrap();
    let error = batch
        .replace_compact_checkpoint_ledger_rows(hash(1), rows, &accounts)
        .err()
        .expect("late row failure must abort projection");
    assert!(error.to_string().contains("injected row read failure"));
    assert!(batch.commit().is_err());
    assert!(store.block_store().get_block(&hash(10)).is_err());
    assert_eq!(store.block_store().block_count().unwrap(), 3);
    assert_eq!(
        store
            .account_store()
            .get_account(&accounts[0].address)
            .unwrap()
            .head,
        hash(3)
    );
    assert_eq!(
        store.meta_store().pending_confirmations().unwrap(),
        vec![hash(2)]
    );

    let (blocks, accounts) = fixture(&store);
    let mut batch = store.tx_begin_write().unwrap();
    let projected = batch
        .replace_compact_checkpoint_ledger_rows(
            hash(1),
            blocks
                .into_iter()
                .map(|row| Ok(std::borrow::Cow::Owned(row))),
            &accounts,
        )
        .unwrap();
    assert_eq!(projected.retained_blocks, 3);
    assert_eq!(projected.retired_blocks, vec![hash(3)]);
    batch.commit().unwrap();
    assert_eq!(store.block_store().get_block(&hash(10)).unwrap(), vec![10]);
    assert!(store
        .meta_store()
        .pending_confirmations()
        .unwrap()
        .is_empty());
}

#[test]
fn sparse_projection_preserves_cemented_bodies_and_reopens_without_invented_heights() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let (blocks, accounts) = fixture(&store);
    for commit in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        let projected = batch
            .replace_compact_checkpoint_ledger(hash(1), &blocks, &accounts)
            .unwrap();
        assert_eq!(projected.retired_blocks, vec![hash(3)]);
        assert_eq!(projected.retained_blocks, 3);
        assert_eq!(projected.ledger_blocks, 10);
        assert_eq!(
            batch.block_at_height(&accounts[0].address, 1).unwrap(),
            Some(hash(1))
        );
        assert_eq!(
            batch.block_at_height(&accounts[0].address, 3).unwrap(),
            None
        );
        assert_eq!(
            batch.block_at_height(&accounts[0].address, 10).unwrap(),
            Some(hash(10))
        );
        assert_eq!(
            batch
                .get_meta(&format!("checkpoint_orphan/v1/{}/{}", hash(1), hash(3)))
                .unwrap(),
            Some(vec![3])
        );
        assert!(!batch.confirmation_pending(&hash(2)).unwrap());
        if commit {
            batch.commit().unwrap();
        } else {
            drop(batch);
            assert_eq!(
                store
                    .account_store()
                    .get_account(&accounts[0].address)
                    .unwrap()
                    .head,
                hash(3)
            );
            assert_eq!(
                store.meta_store().pending_confirmations().unwrap(),
                vec![hash(2)]
            );
            assert!(store.block_store().get_block(&hash(10)).is_err());
        }
    }
    store.env().force_sync().unwrap();
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    assert_eq!(
        store
            .account_store()
            .get_account(&accounts[0].address)
            .unwrap()
            .block_count,
        10
    );
    assert_eq!(store.block_store().block_count().unwrap(), 3);
    assert_eq!(store.block_store().get_block(&hash(1)).unwrap(), vec![1]);
    assert_eq!(
        store
            .meta_store()
            .get_meta("application-preference")
            .unwrap(),
        b"keep"
    );
    let mut next = accounts.clone();
    next[0].block_count = 12;
    next[0].confirmation_height = 12;
    next[0].head = hash(12);
    let mut batch = store.tx_begin_write().unwrap();
    let projected = batch
        .replace_compact_checkpoint_ledger(hash(1), &[row(10, 10), row(12, 12)], &next)
        .unwrap();
    assert_eq!(projected.retained_blocks, 4);
    assert_eq!(projected.ledger_blocks, 12);
    assert!(projected.retired_blocks.is_empty());
    assert_eq!(batch.get_block(&hash(2)).unwrap(), vec![2]);
}

#[test]
fn sparse_projection_rejects_inconsistent_inputs_and_poisoned_writers() {
    for case in 0..19 {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let (mut blocks, mut accounts) = fixture(&store);
        match case {
            0 => {
                blocks.pop();
            }
            1 => {
                blocks.remove(0);
            }
            2 => blocks.push(row(1, 19)),
            3 => blocks[0].bytes = vec![99],
            4 => blocks[0].height = 0,
            5 => blocks.push(row(4, 2)),
            6 => blocks.push(row(10, 11)),
            7 => blocks[0].account = WalletAddress::new("brst_unknown"),
            8 => accounts[0].confirmation_height = 9,
            9 => store.meta_store().enqueue_confirmation(&hash(99)).unwrap(),
            10 => accounts[0].head = BlockHash::ZERO,
            11 => accounts.push(accounts[0].clone()),
            12 => store
                .meta_store()
                .put_meta(
                    &format!("checkpoint_orphan/v1/{}/{}", hash(1), hash(3)),
                    b"different",
                )
                .unwrap(),
            13 => {
                let mut txn = store.env().write_txn().unwrap();
                store
                    .block_height_db
                    .put(&mut txn, hash(1).as_bytes(), b"broken")
                    .unwrap();
                txn.commit().unwrap();
            }
            14 => {
                let mut txn = store.env().write_txn().unwrap();
                store
                    .height_db
                    .put(&mut txn, b"bad", hash(1).as_bytes())
                    .unwrap();
                txn.commit().unwrap();
            }
            15 => {
                let mut txn = store.env().write_txn().unwrap();
                store
                    .transactions_db
                    .put(&mut txn, b"legacy", b"unsupported")
                    .unwrap();
                txn.commit().unwrap();
            }
            16 => blocks.push(row(4, 1)),
            17 => {
                let mut txn = store.env().write_txn().unwrap();
                store
                    .blocks_db
                    .delete(&mut txn, hash(2).as_bytes())
                    .unwrap();
                txn.commit().unwrap();
            }
            18 => {
                let mut txn = store.env().write_txn().unwrap();
                let mut key = accounts[0].address.as_str().as_bytes().to_vec();
                key.extend_from_slice(&9u64.to_be_bytes());
                store
                    .block_height_db
                    .put(&mut txn, hash(99).as_bytes(), &key)
                    .unwrap();
                txn.commit().unwrap();
            }
            _ => unreachable!(),
        }
        let mut batch = store.tx_begin_write().unwrap();
        assert!(
            batch
                .replace_compact_checkpoint_ledger(hash(1), &blocks, &accounts)
                .is_err(),
            "case {case}"
        );
        let _ = batch.put_meta("must-not-commit", b"no");
        assert!(batch.commit().is_err(), "case {case}");
        assert!(store.meta_store().get_meta("must-not-commit").is_err());
        assert_eq!(store.block_store().get_block(&hash(3)).unwrap(), vec![3]);
        assert_eq!(
            store
                .account_store()
                .get_account(&row(1, 1).account)
                .unwrap()
                .head,
            hash(3)
        );
    }
}

#[test]
fn sparse_projection_refuses_stale_dirty_or_undo_boundaries() {
    for case in 0..3 {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let (blocks, accounts) = fixture(&store);
        let mut batch = store.tx_begin_write().unwrap();
        match case {
            1 => batch.put_meta("dirty", b"no").unwrap(),
            2 => batch
                .begin_block_undo(hash(4), accounts[0].address.clone(), 4)
                .unwrap(),
            _ => (),
        }
        assert!(batch
            .replace_compact_checkpoint_ledger(
                hash(if case == 0 { 99 } else { 1 }),
                &blocks,
                &accounts
            )
            .is_err());
        assert!(batch.commit().is_err());
        assert!(store.meta_store().get_meta("dirty").is_err());
        assert_eq!(store.block_store().get_block(&hash(3)).unwrap(), vec![3]);
    }
}

#[test]
fn sparse_projection_bounds_retained_work_not_certified_account_height() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let (mut blocks, mut accounts) = fixture(&store);
    accounts[0].block_count = u64::MAX;
    accounts[0].confirmation_height = u64::MAX;
    blocks[1].height = u64::MAX;
    let mut batch = store.tx_begin_write().unwrap();
    let projected = batch
        .replace_compact_checkpoint_ledger(hash(1), &blocks, &accounts)
        .unwrap();
    assert_eq!(projected.retained_blocks, 3);
    assert_eq!(projected.ledger_blocks, u64::MAX);
}

#[test]
fn sparse_projection_late_failure_aborts_archiving_and_preserves_signer_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let (blocks, accounts) = fixture(&store);
    let archive = |n| format!("checkpoint_orphan/v1/{}/{}", hash(1), hash(n));
    let mut batch = store.tx_begin_write().unwrap();
    let mut local = batch.get_account(&accounts[0].address).unwrap();
    local.head = hash(4);
    local.block_count = 4;
    batch
        .put_block_with_account(&hash(4), &[4], &local.address, 4)
        .unwrap();
    batch.put_account_info(&local, false).unwrap();
    batch.put_frontier(&local.address, &local.head).unwrap();
    batch
        .put_meta(&archive(4), b"conflict after first retirement")
        .unwrap();
    batch
        .put_meta(
            "consensus_final/v2/retained-test-promise",
            hash(3).as_bytes(),
        )
        .unwrap();
    batch
        .put_meta("checkpoint_round/retained-test-lock", b"lock")
        .unwrap();
    batch.commit().unwrap();
    let before = contents(&store);
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch
        .replace_compact_checkpoint_ledger(hash(1), &blocks, &accounts)
        .is_err());
    // LMDB key order processes block 3 before the conflicting block 4 archive.
    assert_eq!(batch.get_meta(&archive(3)).unwrap(), Some(vec![3]));
    assert!(batch.commit().is_err());
    assert_eq!(contents(&store), before);
    store.meta_store().delete_meta(&archive(4)).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .replace_compact_checkpoint_ledger(hash(1), &blocks, &accounts)
        .unwrap();
    assert_eq!(
        batch
            .get_meta("consensus_final/v2/retained-test-promise")
            .unwrap(),
        Some(hash(3).as_bytes().to_vec())
    );
    assert_eq!(
        batch
            .get_meta("checkpoint_round/retained-test-lock")
            .unwrap(),
        Some(b"lock".to_vec())
    );
    assert_eq!(batch.payment_epoch().unwrap(), hash(1));
}

type DatabaseContents = std::collections::BTreeMap<String, CheckpointRows>;

fn contents(store: &LmdbStore) -> DatabaseContents {
    use heed::types::Bytes;
    let env = store.env();
    let txn = env.read_txn().unwrap();
    let directory = env
        .open_database::<Bytes, Bytes>(&txn, None)
        .unwrap()
        .unwrap();
    directory
        .iter(&txn)
        .unwrap()
        .map(|entry| {
            let (name, _) = entry.unwrap();
            let name = std::str::from_utf8(name).unwrap();
            let db = env
                .open_database::<Bytes, Bytes>(&txn, Some(name))
                .unwrap()
                .unwrap();
            let rows = db
                .iter(&txn)
                .unwrap()
                .map(|row| {
                    let (key, value) = row.unwrap();
                    (key.to_vec(), value.to_vec())
                })
                .collect();
            (name.to_owned(), rows)
        })
        .collect()
}

#[test]
fn compact_account_reconciliation_repairs_indexes_and_removes_stale_rows_atomically() {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
    let (blocks, mut accounts) = fixture(&store);
    accounts[0].state = burst_types::WalletState::Verified;
    let mut stale = AccountInfo::unopened(WalletAddress::new("brst_stale_account"));
    stale.state = burst_types::WalletState::Verified;
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_account_info(&stale, false).unwrap();
    batch.commit().unwrap();
    let mut raw = store.env().write_txn().unwrap();
    store.verified_accounts_db.put(&mut raw, accounts[0].address.as_str().as_bytes(), b"wrong").unwrap();
    store.verified_accounts_db.delete(&mut raw, stale.address.as_str().as_bytes()).unwrap();
    store.verified_accounts_db.put(&mut raw, b"orphan", &[1]).unwrap();
    store.frontiers_db.put(&mut raw, accounts[0].address.as_str().as_bytes(), b"malformed").unwrap();
    store.frontiers_db.put(&mut raw, b"orphan", b"malformed").unwrap();
    raw.commit().unwrap();
    let before = contents(&store);
    for commit in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        batch.replace_compact_checkpoint_ledger(hash(1), &blocks, &accounts).unwrap();
        assert!(matches!(batch.get_account(&stale.address), Err(StoreError::NotFound(_))));
        assert_eq!(batch.env.accounts_db.len(&batch.txn).unwrap(), 1);
        assert_eq!(batch.env.frontiers_db.len(&batch.txn).unwrap(), 1);
        assert_eq!(batch.env.verified_accounts_db.len(&batch.txn).unwrap(), 1);
        assert_eq!(batch.env.verified_accounts_db.get(&batch.txn, accounts[0].address.as_str().as_bytes()).unwrap(), Some(&[1][..]));
        assert_eq!(batch.env.frontiers_db.get(&batch.txn, accounts[0].address.as_str().as_bytes()).unwrap(), Some(accounts[0].head.as_bytes().as_slice()));
        if commit { batch.commit().unwrap(); } else {
            drop(batch);
            assert_eq!(contents(&store), before);
        }
    }
    store.force_sync().unwrap();
    drop(store);
    let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
    assert_eq!(store.account_store().get_account(&accounts[0].address).unwrap(), accounts[0]);
    assert!(store.account_store().get_account(&stale.address).is_err());
    let txn = store.env().read_txn().unwrap();
    assert_eq!(store.verified_accounts_db.len(&txn).unwrap(), 1);
}
