use burst_store::{account::AccountInfo, MetaStore};
use burst_trst::merger_graph::GraphRecord;
use burst_types::{BlockHash, TxHash, WalletAddress};

fn row(parent: u8, child: u8, position: u64) -> (String, Vec<u8>) {
    GraphRecord::OriginEdge {
        origin: TxHash::new([parent; 32]),
        child: TxHash::new([child; 32]),
        position,
        occurrence: 0,
    }
    .storage_entry()
    .unwrap()
}

#[test]
fn positions_follow_external_writes_abort_delete_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let (a, av) = row(1, 2, 5);
        let (b, bv) = row(1, 3, 9);
        store.meta_store().put_meta(&a, &av).unwrap();
        store.meta_store().put_meta(&b, &bv).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(
            batch
                .next_graph_edge_position(&TxHash::new([1; 32]), false)
                .unwrap(),
            10
        );
        assert_eq!(
            batch
                .next_graph_edge_position(&TxHash::new([1; 32]), true)
                .unwrap(),
            0
        );
        batch.delete_meta(&b).unwrap();
        assert_eq!(
            batch
                .next_graph_edge_position(&TxHash::new([1; 32]), false)
                .unwrap(),
            6
        );
        drop(batch);
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(
            batch
                .next_graph_edge_position(&TxHash::new([1; 32]), false)
                .unwrap(),
            10
        );
        batch.delete_meta(&a).unwrap();
        batch.commit().unwrap();
    }
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(
        batch
            .next_graph_edge_position(&TxHash::new([1; 32]), false)
            .unwrap(),
        10
    );
}

#[test]
fn shared_parent_undo_has_no_shared_index_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    for id in [2, 3] {
        let mut batch = store.tx_begin_write().unwrap();
        let block = BlockHash::new([id; 32]);
        let account = WalletAddress::new(format!("brst_edge_{id}"));
        batch.begin_block_undo(block, account.clone(), 1).unwrap();
        let mut info = AccountInfo::unopened(account.clone());
        info.head = block;
        info.block_count = 1;
        batch
            .put_block_with_account(&block, b"fixture", &account, 1)
            .unwrap();
        batch.put_account_info(&info, false).unwrap();
        batch.put_frontier(&account, &block).unwrap();
        let (key, value) = row(1, id, id as u64);
        batch.put_meta(&key, &value).unwrap();
        batch.commit().unwrap();
    }
    let mut batch = store.tx_begin_write().unwrap();
    let survivor = batch.block_undo(&BlockHash::new([3; 32])).unwrap().unwrap();
    batch.rewind_block(&BlockHash::new([2; 32])).unwrap();
    assert_eq!(
        batch
            .next_graph_edge_position(&TxHash::new([1; 32]), false)
            .unwrap(),
        4
    );
    assert_eq!(
        batch.block_undo(&BlockHash::new([3; 32])).unwrap().unwrap(),
        survivor
    );
    batch.rewind_block(&BlockHash::new([3; 32])).unwrap();
    assert_eq!(
        batch
            .next_graph_edge_position(&TxHash::new([1; 32]), false)
            .unwrap(),
        0
    );
    batch.commit().unwrap();
}

#[test]
fn duplicate_and_exhausted_positions_fail_without_committing_partial_writes() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let (a, av) = row(1, 2, u64::MAX);
    store.meta_store().put_meta(&a, &av).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch
        .next_graph_edge_position(&TxHash::new([1; 32]), false)
        .is_err());
    let (b, bv) = row(1, 3, u64::MAX);
    batch.put_meta("probe", b"abort").unwrap();
    assert!(batch.put_meta(&b, &bv).is_err());
    assert!(batch.commit().is_err());
    assert!(store.meta_store().get_meta("probe").is_err());
}

#[test]
fn schema_four_rebuilds_gapped_positions_atomically() {
    let dir = tempfile::tempdir().unwrap();
    {
        let env = crate::LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let mut txn = env.env().write_txn().unwrap();
        let (key, value) = row(1, 2, 17);
        env.meta_db.put(&mut txn, key.as_bytes(), &value).unwrap();
        env.meta_db
            .put(&mut txn, b"schema_version", &4u32.to_le_bytes())
            .unwrap();
        env.graph_position_db.clear(&mut txn).unwrap();
        txn.commit().unwrap();
    }
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    assert_eq!(
        store.meta_store().get_schema_version().unwrap(),
        crate::CURRENT_SCHEMA_VERSION
    );
    assert_eq!(
        store
            .tx_begin_write()
            .unwrap()
            .next_graph_edge_position(&TxHash::new([1; 32]), false)
            .unwrap(),
        18
    );
}

#[test]
fn invalid_legacy_position_rebuild_does_not_advance_schema() {
    let dir = tempfile::tempdir().unwrap();
    let env = crate::LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
    let mut txn = env.env().write_txn().unwrap();
    for id in [2, 3] {
        let (key, value) = row(1, id, 17);
        env.meta_db.put(&mut txn, key.as_bytes(), &value).unwrap();
    }
    env.meta_db
        .put(&mut txn, b"schema_version", &4u32.to_le_bytes())
        .unwrap();
    txn.commit().unwrap();
    assert!(crate::LmdbEnvironment::open(dir.path(), 32, 16 << 20).is_err());
    assert_eq!(env.meta_store().get_schema_version().unwrap(), 4);
}
