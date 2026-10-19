use super::*;
use burst_store::account::AccountInfo;
use burst_store_lmdb::LmdbStore;
use burst_trst::{MergeNode, MergeSource};
use burst_types::{BlockHash, TxHash, WalletAddress};
mod large;

fn tx(id: u8) -> TxHash {
    TxHash::new([id; 32])
}

fn merge(graph: &mut MergerGraph, id: u8, sources: &[(u8, u128)]) {
    graph.record_merge(MergeNode {
        merge_tx: tx(id),
        source_origins: sources
            .iter()
            .map(|(id, amount)| MergeSource {
                origin: tx(*id),
                amount: *amount,
            })
            .collect(),
        total_amount: sources.iter().map(|(_, amount)| amount).sum(),
        revoked_contribs: HashMap::new(),
    });
}

fn journal(store: &LmdbStore, id: u8, graph: &MergerGraph) -> BlockHash {
    let hash = BlockHash::new([id; 32]);
    let mut account = AccountInfo::unopened(WalletAddress::new(format!("brst_graph_{id}")));
    account.head = hash;
    account.block_count = 1;
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .begin_block_undo(hash, account.address.clone(), 1)
        .unwrap();
    batch
        .put_block_with_account(
            &hash,
            b"storage fixture, not a signed protocol block",
            &account.address,
            1,
        )
        .unwrap();
    batch.put_frontier(&account.address, &hash).unwrap();
    batch.put_account_info(&account, false).unwrap();
    let before = load(&batch).unwrap();
    persist_delta(&mut batch, &before, graph).unwrap();
    batch.commit().unwrap();
    hash
}

#[test]
fn independent_graph_journals_survive_non_tail_undo_reopen_and_new_consumer() {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
    let mut base = MergerGraph::new();
    merge(&mut base, 20, &[(1, 40), (2, 60)]);
    let mut batch = store.tx_begin_write().unwrap();
    stage_rows_for_test(&mut batch, &base).unwrap();
    batch.commit().unwrap();
    let mut a = base.clone();
    merge(&mut a, 30, &[(20, 10), (20, 20)]);
    let block_a = journal(&store, 30, &a);
    let mut ab = a.clone();
    merge(&mut ab, 40, &[(20, 30)]);
    let block_b = journal(&store, 40, &ab);
    let mut batch = store.tx_begin_write().unwrap();
    let b_journal = batch.block_undo(&block_b).unwrap().unwrap();
    batch.rewind_block(&block_a).unwrap();
    let mut expected = base.clone();
    merge(&mut expected, 40, &[(20, 30)]);
    assert_eq!(load(&batch).unwrap().to_bytes(), expected.to_bytes());
    batch.commit().unwrap();
    drop(store);
    let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(load(&batch).unwrap().to_bytes(), expected.to_bytes());
    assert_eq!(batch.block_undo(&block_b).unwrap().unwrap(), b_journal);
    drop(batch);
    merge(&mut expected, 50, &[(20, 20)]);
    let block_c = journal(&store, 50, &expected);
    let mut batch = store.tx_begin_write().unwrap();
    let c_journal = batch.block_undo(&block_c).unwrap().unwrap();
    for change in c_journal.changes.iter().filter(|change| {
        change
            .key
            .starts_with(GraphRecord::STORAGE_PREFIX.as_bytes())
    }) {
        assert!(
            change.before.is_none(),
            "new merge must not rewrite existing graph rows"
        );
    }
    assert_eq!(batch.block_undo(&block_b).unwrap().unwrap(), b_journal);
    batch.rewind_block(&block_b).unwrap();
    let mut c = base.clone();
    merge(&mut c, 50, &[(20, 20)]);
    assert_eq!(load(&batch).unwrap().to_bytes(), c.to_bytes());
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.rewind_block(&block_c).unwrap();
    assert_eq!(load(&batch).unwrap().to_bytes(), base.to_bytes());
    batch.commit().unwrap();
}

#[test]
fn graph_rows_preserve_revocation_restore_and_aborted_conversion() {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
    let mut graph = MergerGraph::new();
    merge(&mut graph, 20, &[(1, 40), (2, 60)]);
    let mut batch = store.tx_begin_write().unwrap();
    persist(&mut batch, &graph).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    stage_rows_for_test(&mut batch, &graph).unwrap();
    drop(batch);
    let mut batch = store.tx_begin_write().unwrap();
    assert!(!row_format(&batch).unwrap());
    stage_rows_for_test(&mut batch, &graph).unwrap();
    batch.commit().unwrap();
    graph.apply_revocation(tx(1));
    merge(&mut graph, 30, &[(20, 60), (3, 40)]);
    let mut batch = store.tx_begin_write().unwrap();
    persist(&mut batch, &graph).unwrap();
    batch.commit().unwrap();
    let batch = store.tx_begin_write().unwrap();
    let mut graph = load(&batch).unwrap();
    drop(batch);
    assert_eq!(graph.apply_unrevocation(&tx(1)).len(), 1);
    let mut batch = store.tx_begin_write().unwrap();
    persist(&mut batch, &graph).unwrap();
    assert_eq!(load(&batch).unwrap().to_bytes(), graph.to_bytes());
    batch.commit().unwrap();
}

#[test]
fn mixed_and_unmarked_graph_rows_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
    let mut graph = MergerGraph::new();
    merge(&mut graph, 20, &[(1, 40), (2, 60)]);
    let mut batch = store.tx_begin_write().unwrap();
    stage_rows_for_test(&mut batch, &graph).unwrap();
    batch.delete_meta(FORMAT_KEY).unwrap();
    assert!(load(&batch).is_err());
    batch.put_meta(FORMAT_KEY, &[1]).unwrap();
    batch.put_meta(LEGACY_KEY, &graph.to_bytes()).unwrap();
    assert!(load(&batch).is_err());
}

#[test]
fn ordinary_graph_write_cannot_silently_reorder_surviving_consumers() {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
    let mut original = MergerGraph::new();
    merge(&mut original, 20, &[(1, 40), (2, 60)]);
    merge(&mut original, 30, &[(20, 30)]);
    merge(&mut original, 40, &[(20, 30)]);
    let mut batch = store.tx_begin_write().unwrap();
    stage_rows_for_test(&mut batch, &original).unwrap();
    batch.commit().unwrap();
    let mut reordered = MergerGraph::new();
    merge(&mut reordered, 20, &[(1, 40), (2, 60)]);
    merge(&mut reordered, 40, &[(20, 30)]);
    merge(&mut reordered, 30, &[(20, 30)]);
    let mut batch = store.tx_begin_write().unwrap();
    assert!(persist(&mut batch, &reordered).is_err());
    drop(batch);
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(load(&batch).unwrap().to_bytes(), original.to_bytes());
}

#[test]
fn startup_migration_converts_historical_journals_after_independent_rollback() {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
    let mut base = MergerGraph::new();
    merge(&mut base, 20, &[(1, 40), (2, 60)]);
    let mut batch = store.tx_begin_write().unwrap();
    persist(&mut batch, &base).unwrap();
    batch.commit().unwrap();
    let mut a = base.clone();
    merge(&mut a, 30, &[(20, 10), (20, 20)]);
    let block_a = journal(&store, 30, &a);
    let mut ab = a;
    merge(&mut ab, 40, &[(20, 30)]);
    let block_b = journal(&store, 40, &ab);
    let mut batch = store.tx_begin_write().unwrap();
    let legacy_b = batch.block_undo(&block_b).unwrap().unwrap();
    batch
        .rewind_block_with_snapshots(&block_a, |key, before, after, current| {
            assert_eq!(key, LEGACY_KEY.as_bytes());
            MergerGraph::rewind_snapshot(before, after, current).map_err(invalid)
        })
        .unwrap();
    batch.commit().unwrap();
    assert!(migrate(&store).unwrap());
    assert!(!migrate(&store).unwrap());
    drop(store);
    let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
    let batch = store.tx_begin_write().unwrap();
    assert!(row_format(&batch).unwrap());
    assert!(batch.get_meta(LEGACY_KEY).unwrap().is_none());
    let mut bc = load(&batch).unwrap();
    let before_c = bc.to_bytes();
    drop(batch);
    merge(&mut bc, 50, &[(20, 20)]);
    let block_c = journal(&store, 50, &bc);
    let mut batch = store.tx_begin_write().unwrap();
    let original_c = batch.block_undo(&block_c).unwrap().unwrap();
    let mut legacy_c = original_c.clone();
    legacy_c.changes.retain(|change| {
        !change
            .key
            .starts_with(GraphRecord::STORAGE_PREFIX.as_bytes())
    });
    legacy_c
        .changes
        .push(burst_store_lmdb::block_undo::UndoChange {
            table: UndoTable::Meta,
            key: LEGACY_KEY.as_bytes().to_vec(),
            before: Some(before_c),
            after: Some(bc.to_bytes()),
        });
    // Replay emits a fresh dense graph and aggregate economic images. Install
    // their translated journals with the graph, never with historical slots.
    stage_replayed(&mut batch, &bc).unwrap();
    let journals = [
        convert_undo(&batch, legacy_b.clone()).unwrap(),
        convert_undo(&batch, legacy_c.clone()).unwrap(),
    ];
    batch.replace_checkpoint_undo(&journals).unwrap();
    drop(batch);
    let mut batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.block_undo(&block_c).unwrap().unwrap(), original_c);
    stage_replayed(&mut batch, &bc).unwrap();
    let journals = [
        convert_undo(&batch, legacy_b).unwrap(),
        convert_undo(&batch, legacy_c).unwrap(),
    ];
    batch.replace_checkpoint_undo(&journals).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.rewind_block(&block_b).unwrap();
    let mut c = base.clone();
    merge(&mut c, 50, &[(20, 20)]);
    assert_eq!(load(&batch).unwrap().to_bytes(), c.to_bytes());
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.rewind_block(&block_c).unwrap();
    assert_eq!(load(&batch).unwrap().to_bytes(), base.to_bytes());
    batch.commit().unwrap();
}

#[test]
fn migration_missing_journal_aborts_rows_and_marker() {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
    let mut graph = MergerGraph::new();
    merge(&mut graph, 20, &[(1, 40), (2, 60)]);
    let mut batch = store.tx_begin_write().unwrap();
    persist(&mut batch, &graph).unwrap();
    batch.commit().unwrap();
    merge(&mut graph, 30, &[(20, 30)]);
    let block = journal(&store, 30, &graph);
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .delete_meta(&format!(
            "block_undo/v1/{}",
            std::str::from_utf8(block.as_bytes()).unwrap()
        ))
        .unwrap();
    assert!(batch.block_undo(&block).unwrap().is_none());
    batch.commit().unwrap();
    assert!(migrate(&store).is_err());
    let batch = store.tx_begin_write().unwrap();
    assert!(!row_format(&batch).unwrap());
    assert_eq!(load(&batch).unwrap().to_bytes(), graph.to_bytes());
}

#[test]
fn graph_delta_skips_unrelated_rows_and_rejects_stale_changed_rows() {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
    let mut before = MergerGraph::new();
    merge(&mut before, 20, &[(1, 40), (2, 60)]);
    merge(&mut before, 21, &[(3, 40), (4, 60)]);
    let mut batch = store.tx_begin_write().unwrap();
    stage_rows_for_test(&mut batch, &before).unwrap();
    let key = GraphRecord::Node {
        hash: tx(21),
        total: 100,
    }
    .storage_entry()
    .unwrap()
    .0;
    let original = batch.get_meta(&key).unwrap().unwrap();
    batch
        .put_meta(&key, b"unrelated row must not be read or rewritten")
        .unwrap();
    batch.commit().unwrap();
    let mut after = before.clone();
    merge(&mut after, 30, &[(20, 10), (20, 20)]);
    let mut batch = store.tx_begin_write().unwrap();
    persist_delta(&mut batch, &before, &after).unwrap();
    assert_eq!(
        batch.get_meta(&key).unwrap().unwrap(),
        b"unrelated row must not be read or rewritten"
    );
    batch.put_meta(&key, &original).unwrap();
    assert_eq!(load(&batch).unwrap().to_bytes(), after.to_bytes());
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(persist_delta(&mut batch, &before, &after).is_err());
    drop(batch);
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(load(&batch).unwrap().to_bytes(), after.to_bytes());
}
