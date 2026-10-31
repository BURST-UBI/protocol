use super::*;
use std::collections::BTreeMap;

fn tx(id: u8) -> TxHash {
    TxHash::new([id; 32])
}

#[test]
fn edge_reconstruction_cancellation_stops_inside_one_large_parent() {
    for target in ["trst:graph-build-parent", "trst:graph-build-edge"] {
        let mut reader = EdgeReader::default();
        for position in 0..32 {
            reader.push(tx(1), position * 7, tx(10), position).unwrap();
        }
        let visits = std::cell::Cell::new(0);
        let stop_at = if target == "trst:graph-build-parent" { 1 } else { 2 };
        let result = reader.finish_with_poll(&|stage| {
            if stage == target {
                visits.set(visits.get() + 1);
                if visits.get() == stop_at { return Err("edge cancellation".into()); }
            }
            Ok(())
        });
        assert!(matches!(result, Err(ref error) if error == "edge cancellation"));
        assert_eq!(visits.get(), stop_at);
    }
}

#[test]
fn edge_reader_preserves_interleaved_gapped_positions_and_occurrences() {
    let mut reader = EdgeReader::default();
    // Storage keys order by child, while durable positions can have undo gaps.
    reader.push(tx(1), 40, tx(10), 1).unwrap();
    reader.push(tx(2), 900, tx(30), 0).unwrap();
    reader.push(tx(1), 10, tx(10), 0).unwrap();
    reader.push(tx(3), 2, tx(40), 0).unwrap();
    reader.push(tx(1), 25, tx(20), 0).unwrap();
    reader.push(tx(3), 1, tx(50), 0).unwrap();
    let restored = reader.finish().unwrap();
    assert_eq!(restored[&tx(1)], vec![tx(10), tx(20), tx(10)]);
    assert_eq!(restored[&tx(2)], vec![tx(30)]);
    assert_eq!(restored[&tx(3)], vec![tx(50), tx(40)]);
}

#[test]
fn edge_reader_rejects_duplicate_slots_and_invalid_occurrences_at_each_size() {
    for length in [1, 2, 32] {
        let mut reader = EdgeReader::default();
        for position in 0..length {
            reader.push(tx(1), position * 3, tx(10), position).unwrap();
        }
        assert!(reader.push(tx(1), 0, tx(20), 0).is_err());

        let mut reader = EdgeReader::default();
        for position in 0..length {
            reader
                .push(tx(1), position * 3, tx(10), position + 1)
                .unwrap();
        }
        assert!(reader.finish().is_err());
    }
    let mut reader = GraphRecordReader::default();
    let record = GraphRecord::OriginEdge {
        origin: tx(1),
        position: 99,
        child: tx(2),
        occurrence: 0,
    };
    reader.push(record.clone()).unwrap();
    assert!(reader.push(record).is_err());
    assert!(reader.finish().is_err(), "duplicate edge poisons publication");
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
    let restored = graph.apply_restoration(&tx(1));
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
    assert_eq!(graph.apply_restoration(&tx(2)).len(), 2);
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
        graph.apply_restoration(&tx(origin));
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

#[test]
fn one_merge_preserves_unrelated_graph_values_and_exact_record_delta() {
    let hash = |value: u64| TxHash::new(burst_crypto::blake2b_256(&value.to_be_bytes()));
    let mut before = MergerGraph::new();
    for n in 1..=2048u64 {
        before.record_merge(MergeNode {
            merge_tx: hash(2 * n + 1),
            source_origins: vec![MergeSource {
                origin: hash(2 * n),
                amount: 100,
            }],
            total_amount: 100,
            revoked_contribs: Default::default(),
        });
    }
    let mut after = before.clone();
    after.record_merge(MergeNode {
        merge_tx: hash(10000),
        source_origins: vec![MergeSource {
            origin: hash(2),
            amount: 40,
        }],
        total_amount: 40,
        revoked_contribs: Default::default(),
    });
    for n in 1..=2048u64 {
        assert!(std::ptr::eq(
            before.merge_nodes.get(&hash(2 * n + 1)).unwrap(),
            after.merge_nodes.get(&hash(2 * n + 1)).unwrap()
        ));
        if n > 1 {
            assert!(std::ptr::eq(
                before.origin_to_merges.get(&hash(2 * n)).unwrap(),
                after.origin_to_merges.get(&hash(2 * n)).unwrap()
            ));
        }
    }
    assert_eq!(before.origin_to_merges.get(&hash(2)).unwrap().len(), 1);
    assert_eq!(after.origin_to_merges.get(&hash(2)).unwrap().len(), 2);
    assert_delta(&before, &after);
    let mut revoked = after.clone();
    revoked.apply_revocation(hash(2));
    assert!(!after.is_origin_revoked(&hash(2)));
    assert_delta(&after, &revoked);
}

#[test]
fn compact_reader_matches_ordered_edges_across_arrival_orders() {
    for count in [1usize, 2, 8, 32] {
        let rows: Vec<_> = (0..count).map(|i| (i as u64 * 7 + 3, tx(10 + (i % 3) as u8), (i / 3) as u64)).collect();
        let expected: Vec<_> = rows.iter().map(|(_, child, _)| *child).collect();
        for reverse in [false, true] {
            for offset in 0..count {
                let mut arrivals = rows.clone();
                arrivals.rotate_left(offset);
                if reverse { arrivals.reverse(); }
                let mut reader = EdgeReader::default();
                for (position, child, occurrence) in arrivals {
                    reader.push(tx(1), position, child, occurrence).unwrap();
                    reader.push(tx(2), position, child, occurrence).unwrap();
                }
                let restored = reader.finish().unwrap();
                assert_eq!(restored[&tx(1)].iter().copied().collect::<Vec<_>>(), expected);
            }
        }
    }
}

#[test]
fn checked_graph_comparison_matches_full_equality_and_cancels_inner_walks() {
    let mut graph = MergerGraph::new();
    merge(&mut graph, 20, &[(1, 40), (2, 60)]);
    graph.apply_revocation(tx(1));
    for child in 30..50 { merge(&mut graph, child, &[(20, 10), (3, 5)]); }
    let independent = reload(&graph);
    let stages = std::cell::RefCell::new(BTreeMap::new());
    assert!(graph.snapshot_eq_with_poll(&independent, &|stage| {
        *stages.borrow_mut().entry(stage).or_insert(0usize) += 1; Ok(())
    }).unwrap());
    let stages = stages.into_inner();
    for required in ["trst:comparison-graph-edge", "trst:comparison-graph-source",
        "trst:comparison-graph-contribution", "trst:comparison-graph-revision"] {
        assert!(stages.contains_key(required));
    }
    for (target, count) in stages {
        for stop in [1, count.min(2)] {
            let visits = std::cell::Cell::new(0);
            let result = graph.snapshot_eq_with_poll(&independent, &|stage| {
                if stage == target {
                    visits.set(visits.get() + 1);
                    if visits.get() == stop { return Err("graph comparison stop".into()); }
                }
                Ok(())
            });
            assert_eq!(result, Err("graph comparison stop".into()));
            assert_eq!(visits.get(), stop);
        }
    }
    for field in 0..11 {
        let mut changed = independent.clone();
        match field {
            0 => changed.origin_to_merges.clear(),
            1 => changed.merge_to_downstream.clear(),
            2 => changed.merge_nodes.get_mut(&tx(20)).unwrap().merge_tx = tx(99),
            3 => changed.merge_nodes.get_mut(&tx(20)).unwrap().total_amount += 1,
            4 => changed.merge_nodes.get_mut(&tx(20)).unwrap().source_origins.reverse(),
            5 => changed.merge_nodes.get_mut(&tx(20)).unwrap().revoked_contribs.clear(),
            6 => { changed.revoked_origins.insert(tx(99)); },
            7 => changed.revisions.clear(),
            8 => { changed.current_revisions.insert(tx(99), tx(98)); },
            9 => { changed.source_revisions.insert(tx(99), vec![(0, TrstBasis::Single(tx(98)))]); },
            _ => { changed.merge_nodes.remove(&tx(20)); },
        }
        assert!(graph != changed, "no-op mutation {field}");
        assert_eq!(graph.snapshot_eq_with_poll(&changed, &|_| Ok(())).unwrap(), graph == changed);
    }
    assert!(graph == independent);
}

#[test]
fn changing_edge_slot_preserves_storage_identity_and_roundtrips() {
    for mut row in [
        GraphRecord::OriginEdge { origin: tx(1), position: 7, child: tx(2), occurrence: 3 },
        GraphRecord::DownstreamEdge { merge: tx(1), position: 7, child: tx(2), occurrence: 3 },
    ] {
        let (key, original) = row.storage_entry().unwrap();
        let (group, slot) = row.edge_group_position().unwrap();
        assert!(key.starts_with(&group));
        assert_eq!(slot, 7);
        row.set_edge_position(u64::MAX);
        let (new_key, encoded) = row.storage_entry().unwrap();
        assert_eq!(key, new_key);
        assert_ne!(original, encoded);
        let restored = GraphRecord::from_storage_entry(&key, &encoded).unwrap();
        assert_eq!(restored.edge_group_position(), Some((group, u64::MAX)));
        assert!(restored == row);
    }
    assert_eq!(GraphRecord::Node { hash: tx(1), total: 9 }.edge_group_position(), None);
}
