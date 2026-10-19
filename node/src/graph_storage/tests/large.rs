use super::*;
use blake2::{Blake2b512, Digest};
use std::{io::Write, time::Instant};

fn hash(domain: u8, id: u64) -> TxHash {
    let mut bytes = [0; 32];
    bytes[0] = domain;
    bytes[24..].copy_from_slice(&id.to_be_bytes());
    TxHash::new(bytes)
}

fn snapshot_identity(graph: &MergerGraph) -> (u64, Vec<u8>) {
    struct Sink {
        bytes: u64,
        digest: Blake2b512,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes += bytes.len() as u64;
            self.digest.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Sink {
        bytes: 0,
        digest: Blake2b512::new(),
    };
    graph.write_snapshot(&mut sink).unwrap();
    (sink.bytes, sink.digest.finalize().to_vec())
}

#[test]
#[ignore = "large-state qualification: run explicitly on the validation host"]
fn graph_rows_cross_legacy_snapshot_limit_reopen_admit_and_rewind() {
    let started = Instant::now();
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 2usize << 30).unwrap();
    let mut graph = MergerGraph::new();
    for id in 0..70_000 {
        graph.record_merge(MergeNode {
            merge_tx: hash(2, id),
            source_origins: (0..8)
                .map(|input| MergeSource {
                    origin: hash(1, id * 8 + input),
                    amount: 100,
                })
                .collect(),
            total_amount: 800,
            revoked_contribs: HashMap::new(),
        });
    }
    let expected = snapshot_identity(&graph);
    assert!(
        expected.0 > 64 * 1024 * 1024,
        "fixture must cross the old aggregate limit"
    );
    eprintln!(
        "graph fixture: {} legacy bytes, built in {:?}",
        expected.0,
        started.elapsed()
    );
    let mut batch = store.tx_begin_write().unwrap();
    stage_rows_for_test(&mut batch, &graph).unwrap();
    batch.commit().unwrap();
    store.force_sync().unwrap();
    drop(graph);
    drop(store);
    eprintln!("graph rows committed in {:?}", started.elapsed());

    let store = LmdbStore::open(directory.path(), 32, 2usize << 30).unwrap();
    let batch = store.tx_begin_write().unwrap();
    let before = load(&batch).unwrap();
    assert_eq!(snapshot_identity(&before), expected);
    drop(batch);
    eprintln!("graph reopened and matched in {:?}", started.elapsed());
    let candidate_started = Instant::now();
    let mut after = before.clone();
    after.record_merge(MergeNode {
        merge_tx: hash(9, 1),
        source_origins: vec![MergeSource {
            origin: hash(2, 0),
            amount: 1,
        }],
        total_amount: 1,
        revoked_contribs: HashMap::new(),
    });
    let account_address = WalletAddress::new("brst_large_graph_storage_fixture");
    let block = BlockHash::new([90; 32]);
    let mut account = AccountInfo::unopened(account_address.clone());
    account.head = block;
    account.block_count = 1;
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .begin_block_undo(block, account_address.clone(), 1)
        .unwrap();
    batch
        .put_block_with_account(
            &block,
            b"storage fixture, not a signed protocol block",
            &account_address,
            1,
        )
        .unwrap();
    batch.put_frontier(&account_address, &block).unwrap();
    batch.put_account_info(&account, false).unwrap();
    let delta_started = Instant::now();
    persist_delta(&mut batch, &before, &after).unwrap();
    batch.commit().unwrap();
    eprintln!(
        "large graph delta committed in {:?}",
        delta_started.elapsed()
    );
    eprintln!(
        "large graph candidate preparation plus commit: {:?}",
        candidate_started.elapsed()
    );
    let batch = store.tx_begin_write().unwrap();
    let journal = batch.block_undo(&block).unwrap().unwrap();
    let graph_changes: Vec<_> = journal
        .changes
        .iter()
        .filter(|change| {
            change
                .key
                .starts_with(GraphRecord::STORAGE_PREFIX.as_bytes())
        })
        .collect();
    assert_eq!(
        graph_changes.len(),
        4,
        "one node, one input and two reverse edges"
    );
    assert!(graph_changes.iter().all(|change| change.before.is_none()));
    eprintln!(
        "large graph journal: {} bytes, {} graph rows",
        bincode::serialized_size(&journal).unwrap(),
        graph_changes.len()
    );
    drop(batch);
    drop(before);
    let expected_after = snapshot_identity(&after);
    drop(after);
    drop(store);

    let store = LmdbStore::open(directory.path(), 32, 2usize << 30).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert_eq!(snapshot_identity(&load(&batch).unwrap()), expected_after);
    batch.rewind_block(&block).unwrap();
    assert_eq!(snapshot_identity(&load(&batch).unwrap()), expected);
    batch.commit().unwrap();
    eprintln!(
        "large graph second reopen and rollback matched in {:?}",
        started.elapsed()
    );
}
