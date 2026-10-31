use super::*;
use burst_store_lmdb::block_undo::UndoTable;
use burst_types::{BlockHash, Timestamp, WalletAddress};

fn record(before: &RateHistory, after: &RateHistory) -> BlockUndo {
    BlockUndo { block: BlockHash::new([31; 32]), account: WalletAddress::new("brst_rate_migration"), height: 1,
        changes: vec![UndoChange { table: UndoTable::BrnMeta, key: b"rate_history".to_vec(),
            before: Some(bincode::serialize(before).unwrap()), after: Some(bincode::serialize(after).unwrap()) }] }
}

#[test]
fn complete_rate_images_convert_to_exact_suffix_without_repairing_corruption() {
    let mut before = RateHistory::new(3, Timestamp::EPOCH);
    for at in 1..1024 { before.apply_rate_change(3, Timestamp::new(at)).unwrap(); }
    let mut after = before.clone();
    after.apply_rate_change(7, Timestamp::new(1024)).unwrap();
    let legacy = record(&before, &after);
    let converted = convert_undo(legacy.clone()).unwrap();
    assert_eq!(converted.changes.len(), 2);
    assert!(converted.changes.iter().any(|row| row.key == rate_storage::HEADER
        && row.before == Some(1024u64.to_be_bytes().to_vec()) && row.after == Some(1025u64.to_be_bytes().to_vec())));
    assert!(converted.changes.iter().any(|row| row.key == rate_storage::row_key(1024)
        && row.before.is_none() && row.after == Some(rate_storage::row_value(after.segments().last().unwrap()).to_vec())));
    assert!(convert_undo(record(&before, &before)).unwrap().changes.is_empty());
    let mut mixed = legacy.clone();
    mixed.changes.push(UndoChange { table: UndoTable::BrnMeta, key: rate_storage::HEADER.to_vec(), before: None, after: Some(vec![1]) });
    assert!(convert_undo(mixed).is_err());
    let mut trailing = legacy.clone();
    trailing.changes[0].before.as_mut().unwrap().push(0);
    assert!(convert_undo(trailing).is_err());
    let mut bad_segments = before.segments().iter().cloned().collect::<Vec<_>>();
    bad_segments[0].end = None;
    assert!(convert_undo(record(&RateHistory::from_segments(bad_segments), &after)).is_err());
    let mut duplicate = legacy.clone();
    duplicate.changes.push(legacy.changes[0].clone());
    assert!(convert_undo(duplicate).is_err());
}

#[test]
fn startup_migration_preserves_finality_and_rewinds_converted_provisional_rates() {
    for confirmed in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let before = RateHistory::new(3, Timestamp::EPOCH);
        let mut after = before.clone();
        after.apply_rate_change(7, Timestamp::new(10)).unwrap();
        let identity = record(&before, &after);
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_brn_meta(b"next_stake_id", &1u64.to_be_bytes()).unwrap();
        batch.put_brn_meta(b"rate_history", &bincode::serialize(&before).unwrap()).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.begin_block_undo(identity.block, identity.account.clone(), 1).unwrap();
        let mut account = burst_store::account::AccountInfo::unopened(identity.account.clone());
        account.head = identity.block;
        account.block_count = 1;
        batch.put_block_with_account(&identity.block, b"local storage fixture", &identity.account, 1).unwrap();
        batch.put_frontier(&identity.account, &identity.block).unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch.persist_brn_rate_history(&after).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let original_journal = batch.block_undo(&identity.block).unwrap().unwrap();
        if confirmed { account.confirmation_height = 1; batch.put_account_info(&account, false).unwrap(); }
        batch.commit().unwrap();
        assert!(migrate(&store).unwrap());
        assert!(!migrate(&store).unwrap());
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch.uses_brn_rate_records().unwrap());
        assert!(batch.undo_value_ref(UndoTable::BrnMeta, b"rate_history").unwrap().is_none());
        assert_eq!(batch.read_brn_engine(0, 0).unwrap().rate_history, after);
        assert_eq!(batch.get_account(&identity.account).unwrap().confirmation_height, u64::from(confirmed));
        if confirmed {
            assert_eq!(batch.block_undo(&identity.block).unwrap().unwrap(), original_journal);
            assert!(batch.rewind_block(&identity.block).unwrap_err().to_string().contains("unconfirmed"));
        } else {
            assert!(batch.block_undo(&identity.block).unwrap().unwrap().changes.iter()
                .all(|change| change.table != UndoTable::BrnMeta || change.key != b"rate_history"));
            batch.rewind_block(&identity.block).unwrap();
            assert_eq!(batch.read_brn_engine(0, 0).unwrap().rate_history, before);
            batch.commit().unwrap();
        }
    }
}

#[test]
fn missing_provisional_journal_aborts_before_changing_rate_format() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let rates = RateHistory::new(3, Timestamp::EPOCH);
    let identity = record(&rates, &rates);
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_brn_meta(b"next_stake_id", &1u64.to_be_bytes()).unwrap();
    batch.put_brn_meta(b"rate_history", &bincode::serialize(&rates).unwrap()).unwrap();
    let mut account = burst_store::account::AccountInfo::unopened(identity.account.clone());
    account.head = identity.block;
    account.block_count = 1;
    batch.put_account_info(&account, false).unwrap();
    batch.put_block_with_account(&identity.block, b"local storage fixture", &identity.account, 1).unwrap();
    batch.put_frontier(&identity.account, &identity.block).unwrap();
    batch.commit().unwrap();
    assert!(migrate(&store).unwrap_err().to_string().contains("recovered provisional journals"));
    let batch = store.tx_begin_write().unwrap();
    assert!(!batch.uses_brn_rate_records().unwrap());
    assert_eq!(batch.read_brn_engine(0, 0).unwrap().rate_history, rates);
}
