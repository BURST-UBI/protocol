use super::*;
use std::collections::BTreeMap;

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

fn entries(graph: &MergerGraph) -> BTreeMap<String, Vec<u8>> {
    let mut rows = BTreeMap::new();
    graph
        .visit_records(|record| {
            let (key, value) = record.storage_entry()?;
            assert!(rows.insert(key, value).is_none());
            Ok::<_, String>(())
        })
        .unwrap();
    assert_eq!(graph.record_count(), Some(rows.len()));
    rows
}

fn reload(graph: &MergerGraph) -> MergerGraph {
    let rows = entries(graph);
    let restored = MergerGraph::from_records(
        rows.iter()
            .map(|(key, bytes)| GraphRecord::from_storage_entry(key, bytes)),
    )
    .unwrap();
    assert_eq!(restored.to_bytes(), graph.to_bytes());
    assert_eq!(entries(&restored), rows);
    // The streamed visitor need not be lexicographically sorted, but it must
    // also be accepted directly without relying on the storage cursor's order.
    let mut reader = GraphRecordReader::default();
    graph.visit_records(|record| reader.push(record)).unwrap();
    assert_eq!(reader.finish().unwrap().to_bytes(), graph.to_bytes());
    restored
}

#[test]
fn records_preserve_remerge_exclusion_and_later_restoration() {
    let mut graph = MergerGraph::new();
    merge(&mut graph, 20, &[(1, 40), (2, 60)]);
    graph.apply_revocation(tx(1));
    merge(&mut graph, 30, &[(20, 60), (3, 40)]);
    graph = reload(&graph);
    let restored = graph.apply_unrevocation(&tx(1));
    assert_eq!(
        restored.len(),
        1,
        "the remerge never consumed the excluded share"
    );
    graph = reload(&graph);
    assert_eq!(graph.apply_revocation(tx(1)).len(), 1);
    graph = reload(&graph);
    let second = graph.apply_revocation(tx(2));
    assert_eq!(second.len(), 2);
    assert!(second.iter().all(|event| event.tainted_amount == 60));
    graph = reload(&graph);
    assert_eq!(graph.apply_unrevocation(&tx(2)).len(), 2);
    reload(&graph);
}

#[test]
fn records_preserve_repeated_edges_and_empty_revision_headers() {
    let mut graph = MergerGraph::new();
    merge(&mut graph, 20, &[(1, 40), (1, 60)]);
    merge(&mut graph, 30, &[(20, 10), (20, 20)]);
    graph.source_revisions.insert(tx(30), Vec::new());
    let graph = reload(&graph);
    assert_eq!(graph.origin_to_merges[&tx(1)], vec![tx(20), tx(20)]);
    assert_eq!(graph.merge_to_downstream[&tx(20)], vec![tx(30), tx(30)]);
    reload(&MergerGraph::new());
}

#[test]
fn records_reject_wrong_keys_versions_trailing_bytes_and_missing_edges() {
    let record = GraphRecord::Node {
        hash: tx(20),
        total: 100,
    };
    let (key, bytes) = record.storage_entry().unwrap();
    assert!(GraphRecord::from_storage_entry(
        &key.replace(&tx(20).to_string(), &tx(21).to_string()),
        &bytes
    )
    .is_err());
    let mut bad_version = bytes.clone();
    bad_version[0] = 2;
    assert!(GraphRecord::from_storage_entry(&key, &bad_version).is_err());
    let mut trailing = bytes;
    trailing.push(0);
    assert!(GraphRecord::from_storage_entry(&key, &trailing).is_err());
    assert!(GraphRecord::from_storage_entry(&key, &[0; 1025]).is_err());
    let mut graph = MergerGraph::new();
    merge(&mut graph, 20, &[(1, 40), (2, 60)]);
    let mut rows = entries(&graph);
    rows.retain(|key, _| !key.starts_with(&format!("{}3/", GraphRecord::STORAGE_PREFIX)));
    assert!(MergerGraph::from_records(
        rows.iter()
            .map(|(key, bytes)| GraphRecord::from_storage_entry(key, bytes))
    )
    .is_err());
}

#[test]
fn record_reader_failure_cannot_publish_partial_state() {
    let mut reader = GraphRecordReader::default();
    reader
        .push(GraphRecord::Node {
            hash: tx(20),
            total: 100,
        })
        .unwrap();
    assert!(reader
        .push(GraphRecord::Source {
            merge: tx(20),
            position: 1,
            source: MergeSource {
                origin: tx(1),
                amount: 100
            }
        })
        .is_err());
    assert!(reader
        .push(GraphRecord::Source {
            merge: tx(20),
            position: 0,
            source: MergeSource {
                origin: tx(1),
                amount: 100
            }
        })
        .is_err());
    assert!(reader.finish().is_err());
    let mut reader = GraphRecordReader::default();
    reader
        .push(GraphRecord::Node {
            hash: tx(20),
            total: 0,
        })
        .unwrap();
    assert!(reader
        .push(GraphRecord::Node {
            hash: tx(20),
            total: 0
        })
        .is_err());
    assert!(reader.finish().is_err());
}

#[test]
fn visitor_stops_on_storage_failure() {
    let mut graph = MergerGraph::new();
    merge(&mut graph, 20, &[(1, 40), (2, 60)]);
    let mut count = 0;
    assert!(graph
        .visit_records(|_| {
            count += 1;
            Err::<(), _>("disk failure")
        })
        .is_err());
    assert_eq!(count, 1);
}

fn assert_delta(before: &MergerGraph, after: &MergerGraph) {
    let mut rows = entries(before);
    after
        .visit_record_delta(before, |old, new| {
            if let Some(old) = old {
                let (key, bytes) = old.storage_entry()?;
                assert_eq!(rows.remove(&key), Some(bytes));
            }
            if let Some(new) = new {
                let (key, bytes) = new.storage_entry()?;
                assert!(rows.insert(key, bytes).is_none());
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(rows, entries(after));
    let rebuilt = MergerGraph::from_records(
        rows.iter()
            .map(|(key, bytes)| GraphRecord::from_storage_entry(key, bytes)),
    )
    .unwrap();
    assert_eq!(rebuilt.to_bytes(), after.to_bytes());
}

#[test]
fn graph_deltas_match_full_projection_through_remerge_revocation_and_restoration() {
    let mut graph = MergerGraph::new();
    let before = graph.clone();
    merge(&mut graph, 20, &[(1, 20), (1, 20), (2, 60)]);
    assert_delta(&before, &graph);
    let before = graph.clone();
    graph.apply_revocation(tx(1));
    assert_delta(&before, &graph);
    let before = graph.clone();
    merge(&mut graph, 30, &[(20, 60), (3, 40)]);
    assert_delta(&before, &graph);
    for origin in [1, 2, 3] {
        let before = graph.clone();
        graph.apply_revocation(tx(origin));
        assert_delta(&before, &graph);
    }
    for origin in [2, 1, 3] {
        let before = graph.clone();
        graph.apply_unrevocation(&tx(origin));
        assert_delta(&before, &graph);
    }
    let mut count = 0;
    graph
        .visit_record_delta(&graph.clone(), |_, _| {
            count += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(count, 0);
    let before = graph.clone();
    merge(&mut graph, 40, &[(20, 10), (20, 20)]);
    assert!(graph
        .visit_record_delta(&before, |_, _| {
            count += 1;
            Err("disk failure".into())
        })
        .is_err());
    assert_eq!(count, 1);
}

#[test]
fn row_reconstruction_and_rewind_preserve_independent_shared_origin_consumers() {
    let mut before = MergerGraph::new();
    merge(&mut before, 20, &[(1, 40), (2, 60)]);
    let mut after = before.clone();
    merge(&mut after, 30, &[(20, 10), (20, 20), (3, 5)]);
    let mut current = after.clone();
    merge(&mut current, 40, &[(20, 30), (4, 20)]);
    let mut expected = before.clone();
    merge(&mut expected, 40, &[(20, 30), (4, 20)]);
    let recovered = reload(&current)
        .rewind_changes(&reload(&before), &reload(&after))
        .unwrap();
    assert_eq!(recovered.to_bytes(), expected.to_bytes());
    assert_eq!(
        MergerGraph::rewind_snapshot(&before.to_bytes(), &after.to_bytes(), &current.to_bytes())
            .unwrap(),
        recovered.to_bytes()
    );
    let recovered = reload(&recovered);
    let twice_recovered = recovered
        .rewind_changes(&reload(&after), &reload(&current))
        .unwrap();
    assert_eq!(twice_recovered.to_bytes(), before.to_bytes());

    // A later consumer of the losing merge is not an independent transition.
    merge(&mut current, 50, &[(30, 5), (4, 5)]);
    let unchanged = current.to_bytes();
    assert!(current.rewind_changes(&before, &after).is_err());
    assert_eq!(current.to_bytes(), unchanged);
}
