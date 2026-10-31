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
    run_graph_fixture(70_000, false);
}

#[test]
#[ignore = "large-state qualification: run explicitly on the validation host"]
fn high_fanout_graph_reopen_admit_and_rewind() {
    for merges in [1_024, 8_192, 32_768] {
        run_graph_fixture(merges, true);
    }
}

fn graph_fixture(merges: u64, shared_parent: bool) -> (MergerGraph, TxHash) {
    let mut graph = MergerGraph::new();
    let parent = if shared_parent {
        let parent = hash(2, u64::MAX);
        graph.record_merge(MergeNode {
            merge_tx: parent,
            source_origins: vec![MergeSource {
                origin: hash(1, u64::MAX),
                amount: 100,
            }],
            total_amount: 100,
            revoked_contribs: HashMap::new(),
        });
        parent
    } else {
        hash(2, 0)
    };
    for id in 0..merges {
        graph.record_merge(MergeNode {
            merge_tx: hash(2, id),
            source_origins: (0..if shared_parent { 2 } else { 8 })
                .map(|input| MergeSource {
                    origin: if shared_parent {
                        parent
                    } else {
                        hash(1, id * 8 + input)
                    },
                    amount: 100,
                })
                .collect(),
            total_amount: if shared_parent { 200 } else { 800 },
            revoked_contribs: HashMap::new(),
        });
    }
    (graph, parent)
}

fn run_graph_fixture(merges: u64, shared_parent: bool) {
    let started = Instant::now();
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 2usize << 30).unwrap();
    let (graph, parent) = graph_fixture(merges, shared_parent);
    let expected = snapshot_identity(&graph);
    if !shared_parent {
        assert!(
            expected.0 > 64 * 1024 * 1024,
            "fixture must cross the old aggregate limit"
        );
    }
    eprintln!(
        "graph fixture: {merges} merges, shared_parent={shared_parent}, {} legacy bytes, built in {:?}",
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
    let old_tail = batch.next_graph_edge_position(&parent, true).unwrap();
    assert_eq!(old_tail, if shared_parent { 2 * merges } else { 0 });
    drop(batch);
    eprintln!("graph reopened and matched in {:?}", started.elapsed());
    let candidate_started = Instant::now();
    let mut after = before.clone();
    after.record_merge(MergeNode {
        merge_tx: hash(9, 1),
        source_origins: vec![MergeSource {
            origin: parent,
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
    assert_eq!(
        batch.next_graph_edge_position(&parent, true).unwrap(),
        old_tail + 1
    );
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
    assert_eq!(
        batch.next_graph_edge_position(&parent, true).unwrap(),
        old_tail
    );
    batch.commit().unwrap();
    eprintln!(
        "large graph second reopen and rollback matched in {:?}",
        started.elapsed()
    );
}

fn graph_memory(stage: &str) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let value = |prefix: &str| -> u64 {
        status
            .lines()
            .find(|line| line.starts_with(prefix))
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap()
    };
    println!("graph_process_memory {{\"stage\":\"{stage}\",\"rss_kib\":{},\"hwm_kib\":{},\"anon_kib\":{},\"file_kib\":{}}}",
        value("VmRSS:"), value("VmHWM:"), value("RssAnon:"), value("RssFile:"));
}

fn prepare_retained_graph(directory: &std::path::Path, merges: u64) {
    // Refuse to replace an existing baseline; recovery runs reuse its identity.
    std::fs::create_dir(directory).unwrap();
    let (graph, _) = graph_fixture(merges, false);
    let expected = snapshot_identity(&graph);
    if merges == 70_000 {
        assert!(expected.0 > 64 * 1024 * 1024);
    }
    let store = LmdbStore::open(&directory.join("ledger"), 32, 2usize << 30).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    stage_rows_for_test(&mut batch, &graph).unwrap();
    batch.commit().unwrap();
    store.force_sync().unwrap();
    std::fs::write(
        directory.join("identity.bin"),
        bincode::serialize(&(merges, expected)).unwrap(),
    )
    .unwrap();
}

fn recover_retained_graph(directory: &std::path::Path, reader_phases: bool) {
    let (merges, expected): (u64, (u64, Vec<u8>)) =
        bincode::deserialize(&std::fs::read(directory.join("identity.bin")).unwrap()).unwrap();
    if merges == 70_000 {
        assert!(expected.0 > 64 * 1024 * 1024);
    }
    graph_memory("before_open");
    let store = LmdbStore::open(&directory.join("ledger"), 32, 2usize << 30).unwrap();
    let batch = store.tx_begin_write().unwrap();
    graph_memory("before_load");
    let start = Instant::now();
    let graph = if reader_phases {
        assert!(batch.uses_graph_records().unwrap());
        let mut reader = burst_trst::merger_graph::GraphRecordReader::default();
        // Same record decoder and reader used by read_merger_graph; retain its
        // full finish validation and the independent snapshot digest below.
        for row in batch
            .checkpoint_metadata_entries(GraphRecord::STORAGE_PREFIX.as_bytes())
            .unwrap()
        {
            let (key, bytes) = row.unwrap();
            reader
                .push(
                    GraphRecord::from_storage_entry(std::str::from_utf8(key).unwrap(), bytes)
                        .unwrap(),
                )
                .unwrap();
        }
        println!(
            "graph_process_decode_ms {}",
            start.elapsed().as_secs_f64() * 1000.0
        );
        graph_memory("before_reader_finish");
        let finish_start = Instant::now();
        let graph = reader.finish().unwrap();
        println!(
            "graph_process_finish_ms {}",
            finish_start.elapsed().as_secs_f64() * 1000.0
        );
        graph
    } else {
        load(&batch).unwrap()
    };
    println!(
        "graph_process_load_ms {}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    graph_memory("after_load");
    assert_eq!(snapshot_identity(&graph), expected);
    graph_memory("after_digest");
    drop(batch);
    graph_memory("after_transaction_drop");
    drop(graph);
    graph_memory("after_graph_drop");
    drop(store);
    graph_memory("after_store_drop");
}

#[test]
fn retained_graph_fixture_recovers_matching_identity() {
    let directory = tempfile::tempdir().unwrap();
    let baseline = directory.path().join("baseline");
    prepare_retained_graph(&baseline, 16);
    recover_retained_graph(&baseline, false);
    recover_retained_graph(&baseline, true);
}

#[test]
#[ignore = "explicit capped-host one-time retained graph baseline generation"]
fn prepare_large_retained_graph_memory_fixture() {
    let directory =
        std::path::PathBuf::from(std::env::var_os("BURST_GRAPH_MEMORY_FIXTURE").unwrap());
    prepare_retained_graph(&directory, 70_000);
}

#[test]
#[ignore = "explicit capped-host fresh-process recovery of retained graph baseline"]
fn recover_large_retained_graph_memory_fixture() {
    let directory =
        std::path::PathBuf::from(std::env::var_os("BURST_GRAPH_MEMORY_FIXTURE").unwrap());
    recover_retained_graph(
        &directory,
        std::env::var_os("BURST_GRAPH_READER_PHASES").is_some(),
    );
}
