use super::*;
use burst_brn::StakeKind;
use burst_store::brn::BrnStore;
use burst_store_lmdb::block_undo::UndoTable;
use burst_types::WalletAddress;

type StoredBrn = (Vec<(WalletAddress, Vec<u8>)>, Vec<u8>, Vec<u8>);

#[test]
fn stable_merge_candidate_retains_validated_graph_and_holdings() {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 4 * 1024 * 1024).unwrap();
    let (brn, mut initial, _, _) = fixture(1);
    let holder = initial.wallets.keys().next().unwrap().clone();
    let first = initial.wallets[&holder].tokens[0].id;
    let second = TxHash::new([77; 32]);
    let extra = initial.mint(second, holder.clone(), 20, holder.clone(),
        Timestamp::new(100)).unwrap();
    initial.track_token(extra);
    let period = initial.expiry_secs;
    let mut batch = store.tx_begin_write().unwrap();
    crate::graph_storage::stage_replayed(&mut batch, &initial.merger_graph).unwrap();
    crate::trst_storage::stage_replayed_stable(&mut batch, &initial).unwrap();
    let before = batch.read_trst_engine(period).unwrap();
    batch.commit().unwrap();
    let mut staged = StagedEngines::new(&brn, &before);
    let after = staged.trst_mut();
    let ids = [first, second];
    let inputs = after.wallets[&holder].select_merge_inputs(&ids).unwrap();
    let merged = after.merge(&inputs, holder.clone(), TxHash::new([78; 32]),
        Timestamp::new(101)).unwrap();
    after.bulk_untrack(&holder, &ids.into_iter().collect());
    after.track_token(merged);
    let expected = after.clone();
    let mut batch = store.tx_begin_write().unwrap();
    staged.persist(&mut batch).unwrap();
    let cached = batch.read_trst_engine(period).unwrap();
    assert!(std::ptr::eq(&*cached.wallets[&holder], &*expected.wallets[&holder]));
    assert!(cached.merger_graph.shares_storage_with(&expected.merger_graph));
    batch.commit().unwrap();
    drop(store);
    let store = LmdbStore::open(directory.path(), 32, 4 * 1024 * 1024).unwrap();
    let batch = store.tx_begin_write().unwrap();
    let restored = batch.read_trst_engine(period).unwrap();
    assert_eq!(restored.save_wallets(), expected.save_wallets());
    assert_eq!(restored.merger_graph.to_bytes(), expected.merger_graph.to_bytes());
}

fn stored_brn(store: &LmdbStore) -> StoredBrn {
    let brn = store.brn_store();
    let mut wallets = brn.all_wallet_states().unwrap();
    wallets.sort_by(|(a, _), (b, _)| a.as_str().cmp(b.as_str()));
    (
        wallets,
        brn.get_meta(b"rate_history").unwrap().unwrap_or_else(|| {
            let snapshot = brn.read_snapshot().unwrap();
            bincode::serialize(&burst_brn::rate_storage::read_snapshot(snapshot.as_ref()).unwrap().unwrap()).unwrap()
        }),
        brn.get_meta(b"next_stake_id").unwrap().unwrap(),
    )
}

#[test]
fn unchanged_brn_candidate_does_not_write_wallets_or_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 4 * 1024 * 1024).unwrap();
    let (brn, trst, block, _) = fixture(16);
    // Sentinels reveal attempted writes that equal-value undo filtering hides.
    let mut batch = store.tx_begin_write().unwrap();
    for address in brn.wallets.keys() {
        batch.put_brn_wallet(address, b"untouched wallet").unwrap();
    }
    batch
        .put_brn_meta(b"rate_history", b"untouched rates")
        .unwrap();
    batch
        .put_brn_meta(b"next_stake_id", b"untouched id")
        .unwrap();
    batch.commit().unwrap();
    let before = stored_brn(&store);
    let mut staged = StagedEngines::new(&brn, &trst);
    staged.brn_mut();
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .begin_block_undo(block.hash, block.account.clone(), 1)
        .unwrap();
    batch.put_meta("state-only-probe", &[1]).unwrap();
    staged.persist(&mut batch).unwrap();
    batch.commit().unwrap();
    assert_eq!(stored_brn(&store), before);
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(
        batch
            .block_undo(&block.hash)
            .unwrap()
            .unwrap()
            .changes
            .len(),
        1
    );
}

#[test]
fn brn_delta_matches_full_persistence_and_journals_insert_update_delete() {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 4 * 1024 * 1024).unwrap();
    let (mut brn, trst, block, _) = fixture(4);
    let outcome = BlockHash::new([9; 32]);
    brn.get_wallet_mut(&block.account)
        .unwrap()
        .record_release(outcome, 10, &[outcome], Timestamp::new(100))
        .unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    ledger_updater::persist_brn(&mut batch, &brn).unwrap();
    batch.commit().unwrap();
    let before = stored_brn(&store);
    let mut staged = StagedEngines::new(&brn, &trst);
    let candidate = staged.brn_mut();
    let removed = candidate
        .wallets
        .keys()
        .find(|address| **address != block.account)
        .unwrap()
        .clone();
    candidate.wallets.remove(&removed);
    let added = burst_crypto::derive_address(&PublicKey([91; 32]));
    assert!(!candidate.wallets.contains_key(&added));
    candidate.track_wallet(added.clone(), BrnWalletState::new(Timestamp::new(10)));
    let mut wallet = candidate.wallets.remove(&block.account).unwrap();
    candidate
        .stake(
            &block.account,
            &mut wallet,
            20,
            StakeKind::Challenge {
                target_wallet: added.clone(),
            },
            Timestamp::new(200),
        )
        .unwrap();
    candidate.track_wallet(block.account.clone(), wallet);
    candidate
        .apply_rate_change(2_000_000, Timestamp::new(200))
        .unwrap();
    let expected_rate_row = burst_brn::rate_storage::row_value(candidate.rate_history.segments().last().unwrap());
    let expected_directory = tempfile::tempdir().unwrap();
    let expected = LmdbStore::open(expected_directory.path(), 32, 4 * 1024 * 1024).unwrap();
    let mut batch = expected.tx_begin_write().unwrap();
    ledger_updater::persist_brn(&mut batch, candidate).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .begin_block_undo(block.hash, block.account.clone(), 1)
        .unwrap();
    staged.persist(&mut batch).unwrap();
    batch.commit().unwrap();
    assert_eq!(stored_brn(&store), stored_brn(&expected));
    let batch = store.tx_begin_write().unwrap();
    let undo = batch.block_undo(&block.hash).unwrap().unwrap();
    assert_eq!(
        undo.changes.len(),
        6,
        "only three wallets, allocator, rate count and appended rate row changed"
    );
    let rate_key = burst_brn::rate_storage::row_key(brn.rate_history.segments().len() as u64);
    let metadata: std::collections::BTreeSet<_> = undo.changes.iter()
        .filter(|change| change.table == UndoTable::BrnMeta)
        .map(|change| change.key.clone()).collect();
    assert_eq!(metadata, [b"next_stake_id".to_vec(), burst_brn::rate_storage::HEADER.to_vec(), rate_key.clone()]
        .into_iter().collect());
    let appended = undo.changes.iter().find(|change| change.table == UndoTable::BrnMeta && change.key == rate_key).unwrap();
    assert!(appended.before.is_none());
    assert_eq!(appended.after.as_deref(), Some(expected_rate_row.as_slice()));
    let deletion = undo
        .changes
        .iter()
        .find(|change| {
            change.table == UndoTable::BrnWallets && change.key == removed.as_str().as_bytes()
        })
        .unwrap();
    assert_eq!(
        deletion.before.as_ref(),
        Some(&bincode::serialize(brn.get_wallet(&removed).unwrap()).unwrap())
    );
    assert!(deletion.after.is_none());
    let insertion = undo
        .changes
        .iter()
        .find(|change| {
            change.table == UndoTable::BrnWallets && change.key == added.as_str().as_bytes()
        })
        .unwrap();
    assert!(insertion.before.is_none() && insertion.after.is_some());
    drop(batch);
    assert_eq!(brn.wallets.len(), before.0.len());
    assert!(brn.get_wallet(&removed).is_some() && brn.get_wallet(&added).is_none());
    assert_eq!(brn.next_stake_id(), 1);
}

#[test]
fn brn_delta_detects_release_contents_and_aborts_all_writes_on_failure() {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 4 * 1024 * 1024).unwrap();
    let (mut brn, trst, block, _) = fixture(4);
    let outcome = BlockHash::new([9; 32]);
    brn.get_wallet_mut(&block.account)
        .unwrap()
        .record_release(outcome, 10, &[outcome], Timestamp::new(100))
        .unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    ledger_updater::persist_brn(&mut batch, &brn).unwrap();
    batch.commit().unwrap();
    let before = stored_brn(&store);
    for field in 0..3 {
        let mut staged = StagedEngines::new(&brn, &trst);
        let candidate = staged.brn_mut();
        let wallet = candidate.get_wallet_mut(&block.account).unwrap();
        let mut releases: std::collections::BTreeMap<_, _> = wallet.pending_releases.iter()
            .map(|(key, value)| (*key, value.clone())).collect();
        let release = releases.get_mut(&outcome).unwrap();
        match field {
            0 => release.amount += 1,
            1 => release.sources.push(BlockHash::new([10; 32])),
            _ => release.completed_at = Timestamp::new(101),
        }
        wallet.pending_releases = releases.into();
        let expected = bincode::serialize(candidate.get_wallet(&block.account).unwrap()).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        staged.persist(&mut batch).unwrap();
        assert_eq!(
            batch
                .undo_value(UndoTable::BrnWallets, block.account.as_str().as_bytes())
                .unwrap(),
            Some(expected)
        );
        // Drop without commit, then exercise a poisoned transaction as well.
        drop(batch);
        assert_eq!(stored_brn(&store), before);
        let candidate = staged.brn_mut();
        let removed = candidate
            .wallets
            .keys()
            .find(|address| **address != block.account)
            .unwrap()
            .clone();
        candidate.wallets.remove(&removed);
        candidate
            .apply_rate_change(3_000_000, Timestamp::new(200))
            .unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        staged.persist(&mut batch).unwrap();
        assert!(batch
            .put_meta("force-map-full", &vec![0; 8 * 1024 * 1024])
            .is_err());
        assert!(batch.commit().is_err());
        drop(staged);
        assert_eq!(stored_brn(&store), before);
        assert_eq!(
            brn.get_wallet(&block.account).unwrap().pending_releases[&outcome].amount,
            10
        );
    }
}
