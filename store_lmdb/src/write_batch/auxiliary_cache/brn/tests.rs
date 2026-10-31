use super::*;
use burst_store::BrnStore;

#[test]
fn normalized_wallet_writes_are_scoped_and_rewind_with_exact_physical_limits() {
    use burst_brn::wallet_storage;
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    // Exercise the normalized format used by fresh genesis and replay migration.
    batch
        .put_brn_meta(wallet_storage::FORMAT_KEY, &[1])
        .unwrap();
    let address = seed(&mut batch);
    let mut state = BrnWalletState::new(Timestamp::EPOCH);
    for id in 1u64..=1024 {
        let mut hash = [0; 32];
        hash[..8].copy_from_slice(&id.to_be_bytes());
        state
            .record_release(
                BlockHash::new(hash),
                id as u128,
                &[BlockHash::new(hash)],
                Timestamp::new(10),
            )
            .unwrap();
    }
    for index in 0..1024 {
        state
            .paused_intervals
            .push((Timestamp::new(index * 2 + 1), Timestamp::new(index * 2 + 2)));
    }
    batch.put_brn_wallet_state(&address, &state).unwrap();
    let before = batch.read_brn_engine(1, 1 << 20).unwrap();
    let physical_bytes = batch
        .auxiliary_cache
        .borrow()
        .brn
        .as_ref()
        .unwrap()
        .wallet_bytes;
    let expected_bytes = address.as_str().len()
        + batch
            .undo_value_ref(UndoTable::BrnWallets, address.as_str().as_bytes())
            .unwrap()
            .unwrap()
            .len()
        + batch
            .env
            .brn_meta_db
            .prefix_iter(&batch.txn, wallet_storage::ROW_PREFIX)
            .unwrap()
            .map(|row| {
                let (key, value) = row.unwrap();
                key.len() + value.len()
            })
            .sum::<usize>();
    assert_eq!(physical_bytes, expected_bytes);
    assert!(batch.read_brn_engine(1, physical_bytes).is_ok());
    assert!(batch.read_brn_engine(1, physical_bytes - 1).is_err());
    batch
        .read_view()
        .verify_brn_engine_with_poll(&before, 1, physical_bytes, &|_| Ok(()))
        .unwrap();
    assert!(batch
        .read_view()
        .verify_brn_engine_with_poll(&before, 1, physical_bytes - 1, &|_| Ok(()))
        .is_err());
    batch.commit().unwrap();
    let brn_store = store.brn_store();
    let pinned = brn_store.read_snapshot().unwrap();
    for change in 0..3 {
        let block = BlockHash::new([70 + change; 32]);
        let mut batch = store.tx_begin_write().unwrap();
        batch.begin_block_undo(block, address.clone(), 1).unwrap();
        batch
            .put_block_with_account(&block, b"block", &address, 1)
            .unwrap();
        let mut info = AccountInfo::unopened(address.clone());
        info.head = block;
        info.block_count = 1;
        batch.put_account_info(&info, false).unwrap();
        batch.put_frontier(&address, &block).unwrap();
        let mut after = state.clone();
        after.total_burned = 5;
        if change == 1 {
            after
                .record_unproven_release(block, 9, Timestamp::new(20))
                .unwrap();
        }
        if change == 2 {
            after
                .paused_intervals
                .push((Timestamp::new(3000), Timestamp::new(4000)));
        }
        batch.put_brn_wallet_state(&address, &after).unwrap();
        let cached = batch.read_brn_engine(1, 1 << 20).unwrap();
        assert_eq!(cached.get_wallet(&address), Some(&after));
        for (hash, release) in after.pending_releases.iter() {
            assert!(std::ptr::eq(
                release,
                cached
                    .get_wallet(&address)
                    .unwrap()
                    .pending_releases
                    .get(hash)
                    .unwrap()
            ));
        }
        batch.commit().unwrap();
        assert_eq!(
            pinned
                .wallet_entries()
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .1
                .as_ref(),
            bincode::serialize(&state).unwrap()
        );
        let mut batch = store.tx_begin_write().unwrap();
        let undo = batch.block_undo(&block).unwrap().unwrap();
        let rows: Vec<_> = undo
            .changes
            .iter()
            .filter(|row| matches!(row.table, UndoTable::BrnWallets | UndoTable::BrnMeta))
            .collect();
        assert_eq!(rows.len(), if change == 0 { 1 } else { 2 });
        assert!(
            rows.iter()
                .map(|row| row.after.as_ref().map_or(0, Vec::len))
                .sum::<usize>()
                < 256
        );
        batch.rewind_block(&block).unwrap();
        assert_eq!(
            batch
                .read_brn_engine(1, physical_bytes)
                .unwrap()
                .get_wallet(&address),
            before.get_wallet(&address)
        );
        batch.commit().unwrap();
    }
    drop(pinned);
    // Self-committing canonical-byte adapters must update records and generation.
    let mut after = state.clone();
    after.total_burned = 17;
    brn_store
        .put_wallet_state(&address, &bincode::serialize(&after).unwrap())
        .unwrap();
    assert_eq!(
        brn_store.get_wallet_state(&address).unwrap(),
        Some(bincode::serialize(&after).unwrap())
    );
    assert_eq!(
        store
            .tx_begin_write()
            .unwrap()
            .read_brn_engine(1, physical_bytes)
            .unwrap()
            .get_wallet(&address),
        Some(&after)
    );
    assert_eq!(
        BrnEngine::load_from_store(&brn_store)
            .unwrap()
            .get_wallet(&address),
        Some(&after)
    );
    drop(brn_store);
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    assert!(store
        .tx_begin_write()
        .unwrap()
        .read_brn_engine(1, physical_bytes - 1)
        .is_err());
    assert_eq!(
        store
            .tx_begin_write()
            .unwrap()
            .read_brn_engine(1, physical_bytes)
            .unwrap()
            .get_wallet(&address),
        Some(&after)
    );
    let mut batch = store.tx_begin_write().unwrap();
    batch.delete_brn_wallet(&address).unwrap();
    assert!(batch.read_brn_engine(0, 0).unwrap().wallets.is_empty());
    drop(batch);
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .clear_checkpoint_rows(UndoTable::BrnWallets, None)
        .unwrap();
    assert!(batch.read_brn_engine(0, 0).unwrap().wallets.is_empty());
    batch.commit().unwrap();
    assert!(BrnEngine::load_from_store(&store.brn_store())
        .unwrap()
        .wallets
        .is_empty());
}

#[test]
fn normalized_wallet_corruption_cannot_hide_behind_a_cached_cut() {
    use burst_brn::wallet_storage;
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .put_brn_meta(wallet_storage::FORMAT_KEY, &[1])
        .unwrap();
    let address = seed(&mut batch);
    let expected = batch.read_brn_engine(1, 1 << 20).unwrap();
    batch.commit().unwrap();
    for case in 0..5 {
        let mut batch = store.tx_begin_write().unwrap();
        let mut key = wallet_storage::row_prefix(&address).unwrap();
        key.extend_from_slice(wallet_storage::HEADER);
        match case {
            0 => batch.put_brn_meta(&key, b"bad").unwrap(),
            1 => batch
                .put_brn_meta(wallet_storage::FORMAT_KEY, &[2])
                .unwrap(),
            2 => {
                let mut key =
                    wallet_storage::row_prefix(&WalletAddress::new("brst_orphan")).unwrap();
                key.extend_from_slice(&[2; 33]);
                batch.put_brn_meta(&key, b"bad").unwrap();
            }
            3 => batch
                .put_raw(UndoTable::BrnWallets, address.as_str().as_bytes(), b"bad")
                .unwrap(),
            _ => {
                batch.put_brn_meta(&key, b"bad").unwrap();
                batch.delete_brn_meta(wallet_storage::FORMAT_KEY).unwrap();
            }
        }
        assert!(batch.auxiliary_cache.borrow().brn.is_none());
        assert!(batch.read_brn_engine(1, 1 << 20).is_err(), "case {case}");
        assert!(
            batch
                .read_view()
                .verify_brn_engine_with_poll(&expected, 1, 1 << 20, &|_| Ok(()))
                .is_err(),
            "case {case}"
        );
        drop(batch);
        assert!(store
            .tx_begin_write()
            .unwrap()
            .read_brn_engine(1, 1 << 20)
            .is_ok());
    }
}

fn seed(batch: &mut LmdbWriteTransaction<'_>) -> WalletAddress {
    let address = WalletAddress::new("brst_cached");
    batch
        .put_brn_meta(b"next_stake_id", &1u64.to_be_bytes())
        .unwrap();
    batch
        .put_brn_meta(
            b"rate_history",
            &bincode::serialize(&burst_brn::RateHistory::default()).unwrap(),
        )
        .unwrap();
    batch
        .put_brn_wallet(
            &address,
            &bincode::serialize(&BrnWalletState::new(Timestamp::EPOCH)).unwrap(),
        )
        .unwrap();
    address
}

#[test]
fn wallet_record_format_requires_empty_state_or_atomic_projection_and_never_block_undo() {
    use burst_brn::wallet_storage;
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let address = seed(&mut batch);
    assert!(!batch.initialize_brn_wallet_records_if_empty().unwrap());
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.begin_brn_wallet_records_projection().unwrap();
    assert!(batch.uses_brn_wallet_records().unwrap());
    assert!(batch.read_brn_engine(0, 0).unwrap().wallets.is_empty());
    drop(batch);
    let mut batch = store.tx_begin_write().unwrap();
    assert!(!batch.uses_brn_wallet_records().unwrap());
    assert!(batch
        .read_brn_engine(1, 1 << 20)
        .unwrap()
        .get_wallet(&address)
        .is_some());
    batch
        .begin_block_undo(BlockHash::new([85; 32]), address.clone(), 1)
        .unwrap();
    assert!(batch
        .put_brn_meta(wallet_storage::FORMAT_KEY, &[1])
        .is_err());
    assert!(batch.commit().is_err());
    let mut batch = store.tx_begin_write().unwrap();
    batch.delete_brn_wallet(&address).unwrap();
    batch
        .put_meta("block_undo/v1/fixture", b"existing journal")
        .unwrap();
    assert!(!batch.initialize_brn_wallet_records_if_empty().unwrap());
    drop(batch);
    let empty = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(empty.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch.initialize_brn_wallet_records_if_empty().unwrap());
    let address = seed(&mut batch);
    assert!(batch
        .read_brn_engine(1, 1 << 20)
        .unwrap()
        .get_wallet(&address)
        .is_some());
    batch.commit().unwrap();
    assert!(store
        .tx_begin_write()
        .unwrap()
        .uses_brn_wallet_records()
        .unwrap());
}

#[test]
fn typed_wallet_writes_preserve_proofs_bytes_limits_and_cold_reader_parity() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let address = seed(&mut batch);
    batch.read_brn_engine(1, 1 << 20).unwrap();
    let mut state = BrnWalletState::new(Timestamp::EPOCH);
    for id in 1u64..=1024 {
        let mut hash = [0; 32];
        hash[..8].copy_from_slice(&id.to_be_bytes());
        state
            .record_release(
                BlockHash::new(hash),
                id as u128,
                &[BlockHash::new([2; 32]), BlockHash::new(hash)],
                Timestamp::new(10),
            )
            .unwrap();
    }
    for second in [false, true] {
        let address = if second {
            WalletAddress::new("brst_second")
        } else {
            address.clone()
        };
        batch.put_brn_wallet_state(&address, &state).unwrap();
        let bytes = bincode::serialize(&state).unwrap();
        assert_eq!(
            batch
                .undo_value_ref(UndoTable::BrnWallets, address.as_str().as_bytes())
                .unwrap(),
            Some(bytes.as_slice())
        );
        let cached = batch.read_brn_engine(2, 1 << 20).unwrap();
        let wallet = cached.get_wallet(&address).unwrap();
        assert_eq!(
            wallet,
            &BrnWalletState::from_persisted_bytes(&bytes).unwrap()
        );
        for (hash, release) in state.pending_releases.iter() {
            assert!(
                std::ptr::eq(release, wallet.pending_releases.get(hash).unwrap()),
                "typed write rebuilt an unchanged release proof"
            );
        }
        let budget: usize = cached
            .wallets
            .iter()
            .map(|(address, wallet)| {
                address.as_str().len() + bincode::serialize(wallet).unwrap().len()
            })
            .sum();
        assert!(batch.read_brn_engine(2, budget).is_ok());
        assert!(batch.read_brn_engine(2, budget - 1).is_err());
        batch.put_brn_wallet_state(&address, &state).unwrap(); // no-op cannot double-count
        assert!(batch.read_brn_engine(2, budget).is_ok());
    }
    batch
        .delete_brn_wallet(&WalletAddress::new("brst_second"))
        .unwrap();
    let expected = batch.read_brn_engine(1, 1 << 20).unwrap();
    batch.commit().unwrap();
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert_eq!(
        batch
            .read_brn_engine(1, 1 << 20)
            .unwrap()
            .get_wallet(&address),
        expected.get_wallet(&address)
    );
    // A typed write cannot hide corruption previously introduced by a raw writer.
    batch
        .put_raw(
            UndoTable::BrnWallets,
            b"invalid",
            &bincode::serialize(&state).unwrap(),
        )
        .unwrap();
    batch.put_brn_wallet_state(&address, &state).unwrap();
    assert!(batch.auxiliary_cache.borrow().brn.is_none());
    assert!(batch.read_brn_engine(2, 1 << 20).is_err());
    drop(batch);
    assert_eq!(
        store
            .tx_begin_write()
            .unwrap()
            .read_brn_engine(1, 1 << 20)
            .unwrap()
            .get_wallet(&address),
        Some(&state)
    );
}

#[test]
fn roots_survive_commits_and_wallet_changes_preserve_isolated_views() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let address = seed(&mut batch);
    let original = batch.read_brn_engine(100, 1 << 20).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let cached = batch.read_brn_engine(100, 1 << 20).unwrap();
    assert!(std::ptr::eq(
        original.get_wallet(&address).unwrap(),
        cached.get_wallet(&address).unwrap()
    ));
    let mut candidate = cached.clone();
    candidate.get_wallet_mut(&address).unwrap().total_burned = 12;
    assert_eq!(cached.get_wallet(&address).unwrap().total_burned, 0);
    batch
        .put_brn_wallet_state(&address, candidate.get_wallet(&address).unwrap())
        .unwrap();
    assert_eq!(
        batch
            .read_brn_engine(100, 1 << 20)
            .unwrap()
            .get_wallet(&address)
            .unwrap()
            .total_burned,
        12
    );
    batch
        .put_brn_meta(b"next_stake_id", &7u64.to_be_bytes())
        .unwrap();
    assert_eq!(
        batch.read_brn_engine(100, 1 << 20).unwrap().next_stake_id(),
        7
    );
    drop(batch); // Abort both changes; the committed root remains untouched.
    let mut batch = store.tx_begin_write().unwrap();
    let restored = batch.read_brn_engine(100, 1 << 20).unwrap();
    assert_eq!(restored.next_stake_id(), 1);
    assert!(std::ptr::eq(
        original.get_wallet(&address).unwrap(),
        restored.get_wallet(&address).unwrap()
    ));
    batch.put_meta("unrelated", b"value").unwrap();
    batch.commit().unwrap();
    let batch = store.tx_begin_write().unwrap();
    assert!(std::ptr::eq(
        original.get_wallet(&address).unwrap(),
        batch
            .read_brn_engine(100, 1 << 20)
            .unwrap()
            .get_wallet(&address)
            .unwrap()
    ));
    drop(batch);
    // A self-committing adapter bypasses this writer; generation mismatch must miss.
    store
        .brn_store()
        .put_wallet_state(
            &address,
            &bincode::serialize(candidate.get_wallet(&address).unwrap()).unwrap(),
        )
        .unwrap();
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(
        batch
            .read_brn_engine(100, 1 << 20)
            .unwrap()
            .get_wallet(&address)
            .unwrap()
            .total_burned,
        12
    );
}

#[test]
fn limits_corruption_and_projection_clear_apply_to_cached_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let address = seed(&mut batch);
    let engine = batch.read_brn_engine(100, 1 << 20).unwrap();
    let bytes = bincode::serialize(engine.get_wallet(&address).unwrap()).unwrap();
    let budget = address.as_str().len() + bytes.len();
    assert!(batch.read_brn_engine(1, budget).is_ok());
    assert!(batch.read_brn_engine(0, budget).is_err());
    assert!(batch.read_brn_engine(1, budget - 1).is_err());
    let second = WalletAddress::new("brst_second");
    batch.put_brn_wallet(&second, &bytes).unwrap();
    assert!(batch.read_brn_engine(1, usize::MAX).is_err());
    assert!(batch.read_brn_engine(2, budget).is_err());
    batch.delete_brn_wallet(&second).unwrap();
    assert!(batch.read_brn_engine(1, budget).is_ok());
    batch.put_brn_wallet(&address, b"corrupt").unwrap();
    assert!(batch.read_brn_engine(100, 1 << 20).is_err());
    batch.put_brn_wallet(&address, &bytes).unwrap();
    assert!(batch.read_brn_engine(1, budget).is_ok());
    batch
        .put_brn_meta(b"next_stake_id", &0u64.to_be_bytes())
        .unwrap();
    assert!(batch.read_brn_engine(1, budget).is_err());
    batch
        .put_brn_meta(b"next_stake_id", &9u64.to_be_bytes())
        .unwrap();
    assert_eq!(batch.read_brn_engine(1, budget).unwrap().next_stake_id(), 9);
    batch
        .clear_checkpoint_rows(UndoTable::BrnWallets, None)
        .unwrap();
    assert_eq!(batch.read_brn_engine(0, 0).unwrap().wallets.len(), 0);
    batch.commit().unwrap();
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    assert_eq!(
        store
            .tx_begin_write()
            .unwrap()
            .read_brn_engine(0, 0)
            .unwrap()
            .next_stake_id(),
        9
    );
}

#[test]
fn rewind_patches_the_writer_cut_and_failed_commit_keeps_committed_root() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let address = seed(&mut batch);
    batch.read_brn_engine(100, 1 << 20).unwrap();
    batch.commit().unwrap();
    let block = BlockHash::new([9; 32]);
    let mut batch = store.tx_begin_write().unwrap();
    batch.begin_block_undo(block, address.clone(), 1).unwrap();
    let mut info = AccountInfo::unopened(address.clone());
    info.head = block;
    info.block_count = 1;
    batch
        .put_block_with_account(&block, b"block", &address, 1)
        .unwrap();
    batch.put_account_info(&info, false).unwrap();
    batch.put_frontier(&address, &block).unwrap();
    let mut state = BrnWalletState::new(Timestamp::EPOCH);
    state.total_burned = 5;
    batch.put_brn_wallet_state(&address, &state).unwrap();
    batch
        .put_brn_meta(b"next_stake_id", &2u64.to_be_bytes())
        .unwrap();
    let losing = batch.read_brn_engine(100, 1 << 20).unwrap();
    batch.commit().unwrap();
    for abort in [true, false] {
        let mut batch = store.tx_begin_write().unwrap();
        batch.rewind_block(&block).unwrap();
        let rewound = batch.read_brn_engine(100, 1 << 20).unwrap();
        assert_eq!(rewound.get_wallet(&address).unwrap().total_burned, 0);
        assert_eq!(rewound.next_stake_id(), 1);
        assert_eq!(losing.get_wallet(&address).unwrap().total_burned, 5);
        if abort {
            assert!(batch
                .put_block_meta(&block, &address, 1, "bad", b"bad")
                .is_err());
            assert!(batch.commit().is_err());
            let batch = store.tx_begin_write().unwrap();
            assert_eq!(
                batch
                    .read_brn_engine(100, 1 << 20)
                    .unwrap()
                    .get_wallet(&address)
                    .unwrap()
                    .total_burned,
                5
            );
        } else {
            batch.commit().unwrap();
        }
    }
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(
        batch
            .read_brn_engine(100, 1 << 20)
            .unwrap()
            .get_wallet(&address)
            .unwrap()
            .total_burned,
        0
    );
}

#[test]
fn stake_sequence_refresh_keeps_rates_shared_and_still_rejects_changed_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let address = seed(&mut batch);
    let mut rates = burst_brn::RateHistory::default();
    for n in 1..=1024 {
        rates
            .apply_rate_change(n as u128, Timestamp::new(n))
            .unwrap();
    }
    let encoded = bincode::serialize(&rates).unwrap();
    batch.put_brn_meta(b"rate_history", &encoded).unwrap();
    let original = batch.read_brn_engine(1, 1 << 20).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    for next in [9u64, 2, u64::MAX, 1] {
        batch
            .put_brn_meta(b"next_stake_id", &next.to_be_bytes())
            .unwrap();
        let updated = batch.read_brn_engine(1, 1 << 20).unwrap();
        assert_eq!(updated.next_stake_id(), next);
        assert!(std::ptr::eq(
            original.rate_history.segments(),
            updated.rate_history.segments()
        ));
        assert!(std::ptr::eq(
            original.get_wallet(&address).unwrap(),
            updated.get_wallet(&address).unwrap()
        ));
        assert_eq!(original.next_stake_id(), 1);
    }
    for bad in [vec![], vec![1; 7], vec![1; 9], vec![0; 8]] {
        batch.put_brn_meta(b"next_stake_id", &bad).unwrap();
        assert!(batch.read_brn_engine(1, 1 << 20).is_err());
    }
    batch
        .delete_raw(UndoTable::BrnMeta, b"next_stake_id")
        .unwrap();
    assert!(batch.read_brn_engine(1, 1 << 20).is_err());
    batch
        .put_brn_meta(b"next_stake_id", &2u64.to_be_bytes())
        .unwrap();
    batch.put_brn_meta(b"rate_history", b"corrupt").unwrap();
    assert!(batch.read_brn_engine(1, 1 << 20).is_err());
    batch.put_brn_meta(b"rate_history", &encoded).unwrap();
    let refreshed = batch.read_brn_engine(1, 1 << 20).unwrap();
    assert_eq!(refreshed.next_stake_id(), 2);
    assert!(!std::ptr::eq(
        original.rate_history.segments(),
        refreshed.rate_history.segments()
    ));
    drop(batch);
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(
        batch.read_brn_engine(1, 1 << 20).unwrap().next_stake_id(),
        1
    );
    drop(batch);
    store
        .brn_store()
        .put_meta(b"rate_history", b"corrupt")
        .unwrap();
    assert!(store
        .tx_begin_write()
        .unwrap()
        .read_brn_engine(1, 1 << 20)
        .is_err());
}

#[test]
fn normalized_rate_readers_share_one_cut_and_reject_mixed_or_orphaned_rows() {
    use burst_brn::{rate_storage, RateHistory};
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 64 << 20).unwrap();
    let mut rates = RateHistory::new(3, Timestamp::EPOCH);
    rates.apply_rate_change(5, Timestamp::new(10)).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let address = seed(&mut batch);
    batch
        .delete_raw(UndoTable::BrnMeta, b"rate_history")
        .unwrap();
    for (key, value) in rate_storage::encode(&rates).unwrap() {
        batch.put_brn_meta(&key, &value).unwrap();
    }
    batch
        .put_brn_meta(rate_storage::HEADER, &2u64.to_be_bytes())
        .unwrap();
    let expected = batch.read_brn_engine(100, 1 << 20).unwrap();
    assert_eq!(expected.rate_history, rates);
    batch
        .read_view()
        .verify_brn_engine_with_poll(&expected, 100, 1 << 20, &|_| Ok(()))
        .unwrap();
    let polls = std::cell::Cell::new(0);
    let error = batch
        .read_view()
        .verify_brn_engine_with_poll(&expected, 100, 1 << 20, &|stage| {
            if stage == "storage:brn-rate" {
                polls.set(polls.get() + 1);
                if polls.get() == 2 {
                    return Err(StoreError::Backend("rate comparison sentinel".into()));
                }
            }
            Ok(())
        })
        .unwrap_err();
    assert!(matches!(error, StoreError::Backend(ref text) if text == "rate comparison sentinel"));
    assert_eq!(polls.get(), 2);
    for case in 0..3 {
        let mut wrong = expected.clone();
        let mut segments: Vec<_> = wrong.rate_history.segments().iter().cloned().collect();
        match case {
            0 => segments.clear(),
            1 => segments[0].end = None,
            _ => segments[1].rate = u128::MAX,
        }
        wrong.rate_history = burst_brn::RateHistory::from_segments(segments);
        assert!(batch
            .read_view()
            .verify_brn_engine_with_poll(&wrong, 100, 1 << 20, &|_| Ok(()))
            .is_err());
    }
    batch.commit().unwrap();
    let brn_store = store.brn_store();
    let original = BrnEngine::load_from_store(&brn_store).unwrap();
    let snapshot = brn_store.read_snapshot().unwrap();
    let pinned_rows = snapshot
        .meta_entries(rate_storage::ROW_PREFIX)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(original.rate_history, rates);
    assert!(original.get_wallet(&address).is_some());
    rates.apply_rate_change(7, Timestamp::new(10)).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let cached = batch.read_brn_engine(100, 1 << 20).unwrap();
    batch
        .put_brn_meta(
            &rate_storage::row_key(2),
            &rate_storage::row_value(rates.segments().last().unwrap()),
        )
        .unwrap();
    assert!(batch.read_brn_engine(100, 1 << 20).is_err()); // count still old
    batch
        .put_brn_meta(rate_storage::HEADER, &3u64.to_be_bytes())
        .unwrap();
    let current = batch.read_brn_engine(100, 1 << 20).unwrap();
    assert_eq!(current.rate_history, rates);
    batch
        .read_view()
        .verify_brn_engine_with_poll(&current, 100, 1 << 20, &|_| Ok(()))
        .unwrap();
    assert!(batch
        .read_view()
        .verify_brn_engine_with_poll(&original, 100, 1 << 20, &|_| Ok(()))
        .is_err());
    assert_eq!(cached.rate_history, original.rate_history);
    batch.commit().unwrap();
    assert_eq!(pinned_rows.len(), 2);
    assert_eq!(
        rate_storage::read_snapshot(snapshot.as_ref())
            .unwrap()
            .unwrap(),
        original.rate_history
    );
    drop(snapshot);
    assert_eq!(
        BrnEngine::load_from_store(&brn_store).unwrap().rate_history,
        rates
    );
    for (key, value) in [
        (
            b"rate_history".to_vec(),
            Some(bincode::serialize(&rates).unwrap()),
        ),
        (rate_storage::HEADER.to_vec(), None),
        (
            rate_storage::HEADER.to_vec(),
            Some(4u64.to_be_bytes().to_vec()),
        ),
        (rate_storage::row_key(1), None),
        (rate_storage::row_key(1), Some(vec![0; 23])),
        (b"next_stake_id".to_vec(), None),
        (b"next_stake_id".to_vec(), Some(0u64.to_be_bytes().to_vec())),
        (
            rate_storage::HEADER.to_vec(),
            Some(0u64.to_be_bytes().to_vec()),
        ),
        (
            rate_storage::HEADER.to_vec(),
            Some(u64::MAX.to_be_bytes().to_vec()),
        ),
        (rate_storage::HEADER.to_vec(), Some(vec![0; 7])),
        (rate_storage::row_key(3), Some(vec![0; 24])),
        (rate_storage::row_key(1), Some(vec![0; 25])),
    ] {
        let mut batch = store.tx_begin_write().unwrap();
        let expected = batch.read_brn_engine(100, 1 << 20).unwrap();
        match value {
            Some(value) => batch.put_brn_meta(&key, &value).unwrap(),
            None => batch.delete_raw(UndoTable::BrnMeta, &key).unwrap(),
        }
        assert!(batch.read_brn_engine(100, 1 << 20).is_err());
        assert!(batch
            .read_view()
            .verify_brn_engine_with_poll(&expected, 100, 1 << 20, &|_| Ok(()))
            .is_err());
        // Aborted corrupt edits cannot replace the committed cache.
        drop(batch);
        assert_eq!(
            BrnEngine::load_from_store(&brn_store).unwrap().rate_history,
            rates
        );
    }
    // Unknown-generation writes must also be diagnosed by the cold reader.
    brn_store
        .put_meta(b"rate_history", &bincode::serialize(&rates).unwrap())
        .unwrap();
    assert!(BrnEngine::load_from_store(&brn_store).is_err());
    assert!(store
        .tx_begin_write()
        .unwrap()
        .read_brn_engine(100, 1 << 20)
        .is_err());
}

#[test]
fn normalized_rate_writer_records_only_appended_rows_and_rewinds_conversion() {
    use burst_brn::{rate_storage, RateHistory};
    for normalized in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 64 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let address = seed(&mut batch);
        let mut rates = RateHistory::new(3, Timestamp::EPOCH);
        for at in 1..1024 {
            rates
                .apply_rate_change(3 + u128::from(at), Timestamp::new(at))
                .unwrap();
        }
        let legacy = bincode::serialize(&rates).unwrap();
        batch.put_brn_meta(b"rate_history", &legacy).unwrap();
        batch.read_brn_engine(100, 1 << 20).unwrap();
        batch.commit().unwrap();
        // Aborting migration retains both the raw legacy format and cached cut.
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_brn_rate_history(&rates).unwrap();
        assert!(batch
            .undo_value_ref(UndoTable::BrnMeta, b"rate_history")
            .unwrap()
            .is_none());
        drop(batch);
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(
            batch
                .undo_value_ref(UndoTable::BrnMeta, b"rate_history")
                .unwrap(),
            Some(legacy.as_slice())
        );
        if normalized {
            batch.put_brn_rate_history(&rates).unwrap();
        }
        batch.commit().unwrap();
        let block = BlockHash::new([19; 32]);
        let mut batch = store.tx_begin_write().unwrap();
        let before = batch.read_brn_engine(100, 1 << 20).unwrap();
        let mut after = before.clone();
        after
            .rate_history
            .apply_rate_change(7, Timestamp::new(1024))
            .unwrap();
        batch.begin_block_undo(block, address.clone(), 1).unwrap();
        let mut info = AccountInfo::unopened(address.clone());
        info.head = block;
        info.block_count = 1;
        batch
            .put_block_with_account(&block, b"block", &address, 1)
            .unwrap();
        batch.put_account_info(&info, false).unwrap();
        batch.put_frontier(&address, &block).unwrap();
        batch.put_brn_rate_history(&after.rate_history).unwrap();
        let restored = batch.read_brn_engine(100, 1 << 20).unwrap();
        assert!(std::ptr::eq(
            restored.rate_history.segments(),
            after.rate_history.segments()
        ));
        assert_eq!(before.rate_history, rates);
        batch.put_brn_rate_history(&after.rate_history).unwrap(); // unchanged root is a no-op
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let undo = batch.block_undo(&block).unwrap().unwrap();
        let rate_changes: Vec<_> = undo
            .changes
            .iter()
            .filter(|change| change.table == UndoTable::BrnMeta)
            .collect();
        assert_eq!(rate_changes.len(), if normalized { 2 } else { 1027 });
        if normalized {
            assert!(rate_changes
                .iter()
                .all(|change| change.key == rate_storage::HEADER
                    || change.key == rate_storage::row_key(1024)));
        }
        batch.rewind_block(&block).unwrap();
        assert_eq!(
            batch.read_brn_engine(100, 1 << 20).unwrap().rate_history,
            rates
        );
        assert_eq!(
            batch
                .undo_value_ref(UndoTable::BrnMeta, rate_storage::HEADER)
                .unwrap()
                .is_some(),
            normalized
        );
        batch.commit().unwrap();
        drop(store);
        let store = crate::LmdbStore::open(dir.path(), 32, 64 << 20).unwrap();
        assert_eq!(
            BrnEngine::load_from_store(&store.brn_store())
                .unwrap()
                .rate_history,
            rates
        );
        let mut batch = store.tx_begin_write().unwrap();
        let invalid = RateHistory::from_segments(vec![]);
        assert!(batch.put_brn_rate_history(&invalid).is_err());
        assert_eq!(
            batch.read_brn_engine(100, 1 << 20).unwrap().rate_history,
            rates
        );
        batch.commit().unwrap();
    }
}

#[test]
fn normal_rate_persistence_preserves_legacy_journal_format_and_normalizes_fresh_state() {
    use burst_brn::{rate_storage, RateHistory};
    for legacy in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let rates = RateHistory::new(3, Timestamp::EPOCH);
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_brn_meta(b"next_stake_id", &1u64.to_be_bytes())
            .unwrap();
        if legacy {
            batch
                .put_brn_meta(b"rate_history", &bincode::serialize(&rates).unwrap())
                .unwrap();
        }
        batch.persist_brn_rate_history(&rates).unwrap();
        assert_eq!(batch.uses_brn_rate_records().unwrap(), !legacy);
        let before = batch.read_brn_engine(0, 0).unwrap();
        let mut after = before.rate_history.clone();
        after.apply_rate_change(7, Timestamp::new(10)).unwrap();
        batch.persist_brn_rate_history(&after).unwrap();
        assert_eq!(batch.read_brn_engine(0, 0).unwrap().rate_history, after);
        assert_eq!(batch.uses_brn_rate_records().unwrap(), !legacy);
        assert_eq!(
            batch
                .undo_value_ref(UndoTable::BrnMeta, b"rate_history")
                .unwrap()
                .is_some(),
            legacy
        );
        assert_eq!(
            batch
                .undo_value_ref(UndoTable::BrnMeta, rate_storage::HEADER)
                .unwrap()
                .is_some(),
            !legacy
        );
        batch.commit().unwrap();
        assert_eq!(
            BrnEngine::load_from_store(&store.brn_store())
                .unwrap()
                .rate_history,
            after
        );
    }
}

#[test]
fn rate_only_publication_preserves_root_without_loading_wallet_cache() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut rates = burst_brn::RateHistory::new(3, Timestamp::EPOCH);
    for at in 1..1024 {
        rates.apply_rate_change(3, Timestamp::new(at)).unwrap();
    }
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .put_brn_meta(b"next_stake_id", &1u64.to_be_bytes())
        .unwrap();
    batch.persist_brn_rate_history(&rates).unwrap();
    assert!(batch.auxiliary_cache.borrow().brn.is_none());
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch.auxiliary_cache.borrow().brn.is_none());
    assert!(std::ptr::eq(
        batch
            .auxiliary_cache
            .borrow()
            .brn_rates
            .as_ref()
            .unwrap()
            .segments(),
        rates.segments()
    ));
    let mut after = rates.clone();
    after.apply_rate_change(7, Timestamp::new(1024)).unwrap();
    let hash = BlockHash::new([24; 32]);
    batch
        .begin_block_undo(hash, WalletAddress::new("brst_rate_only"), 1)
        .unwrap();
    batch.persist_brn_rate_history(&after).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.block_undo(&hash).unwrap().unwrap().changes.len(), 2);
    batch
        .put_brn_meta(burst_brn::rate_storage::HEADER, b"bad")
        .unwrap();
    assert!(batch.auxiliary_cache.borrow().brn_rates.is_none());
    assert!(batch.read_brn_engine(0, 0).is_err());
    drop(batch);
    assert_eq!(
        BrnEngine::load_from_store(&store.brn_store())
            .unwrap()
            .rate_history,
        after
    );
    store
        .brn_store()
        .put_meta(burst_brn::rate_storage::HEADER, b"bad")
        .unwrap();
    let batch = store.tx_begin_write().unwrap();
    assert!(batch.auxiliary_cache.borrow().brn_rates.is_none());
    assert!(batch.read_brn_engine(0, 0).is_err());
}

#[test]
fn captured_brn_reader_preserves_formats_limits_and_old_state() {
    for normalized in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let mut writer = store.tx_begin_write().unwrap();
        if normalized {
            writer
                .put_brn_meta(burst_brn::wallet_storage::FORMAT_KEY, &[1])
                .unwrap();
        }
        let owner = seed(&mut writer);
        let before = writer.read_brn_engine(1, 1 << 20).unwrap();
        writer.commit().unwrap();
        let reader = store.tx_begin_read().unwrap();
        let view = store.read_view(&reader).unwrap();
        assert_eq!(
            view.read_brn_engine(1, 1 << 20).unwrap().get_wallet(&owner),
            before.get_wallet(&owner)
        );
        assert!(view.read_brn_engine(0, 1 << 20).is_err());
        assert!(view.read_brn_engine(1, 1).is_err());
        view.verify_brn_engine_with_poll(&before, 1, 1 << 20, &|_| Ok(()))
            .unwrap();
        assert!(view
            .verify_brn_engine_with_poll(&before, 0, 1 << 20, &|_| Ok(()))
            .is_err());
        assert!(view
            .verify_brn_engine_with_poll(&before, 1, 1, &|_| Ok(()))
            .is_err());
        for stage in ["storage:brn-metadata", "storage:brn-wallet"] {
            let error = view
                .verify_brn_engine_with_poll(&before, 1, 1 << 20, &|at| {
                    if at == stage {
                        return Err(StoreError::Backend("BRN comparison sentinel".into()));
                    }
                    Ok(())
                })
                .unwrap_err();
            assert!(
                matches!(error, StoreError::Backend(ref text) if text == "BRN comparison sentinel")
            );
        }
        for case in 0..4 {
            let mut wrong = before.clone();
            match case {
                0 => {
                    wrong.wallets.remove(&owner);
                }
                1 => {
                    wrong.wallets.insert(
                        WalletAddress::new("brst_missing"),
                        BrnWalletState::new(Timestamp::EPOCH),
                    );
                }
                2 => {
                    wrong
                        .rate_history
                        .apply_rate_change(17, Timestamp::new(10))
                        .unwrap();
                }
                _ => {
                    wrong.restore_stake_sequence(&7u64.to_be_bytes()).unwrap();
                }
            }
            assert!(
                view.verify_brn_engine_with_poll(&wrong, 2, 1 << 20, &|_| Ok(()))
                    .is_err(),
                "case {case}"
            );
        }
        let mut writer = store.tx_begin_write().unwrap();
        let mut after = before.get_wallet(&owner).unwrap().clone();
        after.total_burned = 19;
        writer.put_brn_wallet_state(&owner, &after).unwrap();
        assert_eq!(
            writer
                .read_brn_engine(1, 1 << 20)
                .unwrap()
                .get_wallet(&owner),
            Some(&after)
        );
        writer.commit().unwrap();
        assert_eq!(
            view.read_brn_engine(1, 1 << 20).unwrap().get_wallet(&owner),
            before.get_wallet(&owner)
        );
        view.verify_brn_engine_with_poll(&before, 1, 1 << 20, &|_| Ok(()))
            .unwrap();
        let mut writer = store.tx_begin_write().unwrap();
        assert!(writer
            .read_view()
            .verify_brn_engine_with_poll(&before, 1, 1 << 20, &|_| Ok(()))
            .is_err());
        writer.put_brn_meta(b"rate_history", b"corrupt").unwrap();
        assert!(writer.read_brn_engine(1, 1 << 20).is_err());
        assert!(writer.read_view().read_brn_engine(1, 1 << 20).is_err());
        assert!(writer
            .read_view()
            .verify_brn_engine_with_poll(&before, 1, 1 << 20, &|_| Ok(()))
            .is_err());
        assert!(view.read_brn_engine(1, 1 << 20).is_ok());
        view.verify_brn_engine_with_poll(&before, 1, 1 << 20, &|_| Ok(()))
            .unwrap();
    }
}
