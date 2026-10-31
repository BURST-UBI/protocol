//! Inverse of appended graph rows against a retained, validated graph cut.
use super::*;

fn logical_record(mut record: GraphRecord) -> GraphRecord {
    // Durable edge positions may have gaps after earlier removals. The reader
    // retains their order, not their numeric positions. The storage caller must
    // compare the complete journal bytes with the current writer before using
    // this proof; key, parent, child and occurrence remain checked here.
    match &mut record {
        GraphRecord::OriginEdge { position, .. } | GraphRecord::DownstreamEdge { position, .. } => {
            *position = 0
        }
        _ => {}
    }
    record
}

impl MergerGraph {
    /// Remove complete appended nodes, preserving independent later consumers
    /// of their inputs. `self` and `revision_index` must belong to the same
    /// validated cut; the latter includes historical revisions without heads.
    /// The supplied rows must describe each complete current node. Edge positions
    /// are normalized here, so the storage caller must first compare the exact
    /// journal bytes with its current records. This does not validate holdings
    /// or authorize a ledger rewind.
    ///
    /// None requires full restoration because of unsupported row kinds or retained
    /// revision history. Success returns a candidate and hash-sorted removed IDs;
    /// this graph is never modified. Work includes node rows, outgoing dependency
    /// checks and indexed edge removal. Large parents preserve unrelated siblings
    /// through persistent trees; surviving descendants prevent removal.
    pub(crate) fn remove_appended_records(
        &self,
        records: &[GraphRecord],
        revision_index: &RevisionIndex,
    ) -> Result<Option<(Self, Vec<TxHash>)>, String> {
        if records.iter().any(|row| {
            matches!(
                row,
                GraphRecord::RevokedOrigin(_)
                    | GraphRecord::Revision { .. }
                    | GraphRecord::CurrentRevision { .. }
            )
        }) {
            return Ok(None);
        }
        let mut supplied = BTreeMap::new();
        let mut removed = HashSet::new();
        for record in records {
            if supplied
                .insert(record.key(), logical_record(record.clone()))
                .is_some()
            {
                return Err("duplicate removed merger graph row".into());
            }
            if let GraphRecord::Node { hash, .. } = record {
                removed.insert(*hash);
            }
        }
        let mut expected = BTreeMap::new();
        let mut add = |record: GraphRecord| {
            expected.insert(record.key(), logical_record(record));
        };
        let mut parents = HashMap::<TxHash, HashSet<TxHash>>::new();
        let mut downstream_parents = HashMap::<TxHash, HashSet<TxHash>>::new();
        for id in &removed {
            let node = self
                .merge_nodes
                .get(id)
                .ok_or("removed merger graph node is missing")?;
            if revision_index.has_merge(id) || self.current_revisions.contains_key(id) {
                return Ok(None);
            }
            if self
                .origin_to_merges
                .get(id)
                .is_some_and(|edges| edges.iter().any(|child| !removed.contains(child)))
            {
                return Err("graph row undo would orphan a downstream merge".into());
            }
            add(GraphRecord::Node {
                hash: *id,
                total: node.total_amount,
            });
            let mut occurrences = HashMap::<TxHash, u64>::new();
            for (position, source) in node.source_origins.iter().enumerate() {
                add(GraphRecord::Source {
                    merge: *id,
                    position: position as u64,
                    source: source.clone(),
                });
                let occurrence = occurrences.entry(source.origin).or_default();
                add(GraphRecord::OriginEdge {
                    origin: source.origin,
                    position: 0,
                    child: *id,
                    occurrence: *occurrence,
                });
                parents.entry(source.origin).or_default().insert(*id);
                if self.merge_nodes.contains_key(&source.origin) {
                    add(GraphRecord::DownstreamEdge {
                        merge: source.origin,
                        position: 0,
                        child: *id,
                        occurrence: *occurrence,
                    });
                    downstream_parents
                        .entry(source.origin)
                        .or_default()
                        .insert(*id);
                }
                *occurrence += 1;
            }
            for (origin, amount) in &node.revoked_contribs {
                add(GraphRecord::Contribution {
                    merge: *id,
                    origin: *origin,
                    amount: *amount,
                });
            }
            if let Some(sources) = self.source_revisions.get(id) {
                add(GraphRecord::SourceRevisions(*id));
                for (position, (index, basis)) in sources.iter().enumerate() {
                    add(GraphRecord::SourceRevision {
                        merge: *id,
                        position: position as u64,
                        index: *index as u64,
                        basis: *basis,
                    });
                }
            }
        }
        if supplied != expected {
            return Err("removed merger graph rows do not describe complete current nodes".into());
        }
        let mut candidate = self.clone();
        for (map, parents) in [
            (&mut candidate.origin_to_merges, parents),
            (&mut candidate.merge_to_downstream, downstream_parents),
        ] {
            for (parent, children) in parents {
                let edges = map
                    .get_mut(&parent)
                    .ok_or("removed merger graph edge parent is missing")?;
                edges.remove_children(&children);
                if edges.is_empty() {
                    map.remove(&parent);
                }
            }
        }
        for id in &removed {
            candidate.merge_nodes.remove(id);
            candidate.source_revisions.remove(id);
        }
        let mut removed: Vec<_> = removed.into_iter().collect();
        removed.sort_unstable();
        Ok(Some((candidate, removed)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(n: u8) -> TxHash {
        TxHash::new([n; 32])
    }
    fn node(n: u8, sources: &[(u8, u128)]) -> MergeNode {
        MergeNode {
            merge_tx: id(n),
            source_origins: sources
                .iter()
                .map(|(origin, amount)| MergeSource {
                    origin: id(*origin),
                    amount: *amount,
                })
                .collect(),
            total_amount: sources.iter().map(|(_, amount)| amount).sum(),
            revoked_contribs: HashMap::new(),
        }
    }
    fn added_rows(before: &MergerGraph, after: &MergerGraph) -> Vec<GraphRecord> {
        let mut records = Vec::new();
        after
            .visit_record_delta(before, |old, new| {
                assert!(old.is_none());
                records.push(new.unwrap());
                Ok(())
            })
            .unwrap();
        records
    }
    fn revision_merges(graph: &MergerGraph) -> RevisionIndex {
        RevisionIndex::new(graph).unwrap()
    }
    #[test]
    fn removal_preserves_later_siblings_duplicates_and_unrelated_roots() {
        let mut before = MergerGraph::new();
        for n in 10..40 {
            before.record_merge(node(n, &[(1, 1), (2, 1)]));
        }
        let mut appended = before.clone();
        appended.record_merge(node(80, &[(1, 1), (1, 1), (10, 2)]));
        appended.record_merge(node(81, &[(80, 2), (11, 1)]));
        let rows = added_rows(&before, &appended);
        let mut current = appended.clone();
        current.record_merge(node(90, &[(1, 1), (10, 1)]));
        current.validate_persisted().unwrap();
        let saved = current.clone();
        let (rewound, removed) = current
            .remove_appended_records(&rows, &revision_merges(&current))
            .unwrap()
            .unwrap();
        assert_eq!(removed, vec![id(80), id(81)]);
        let mut expected = before;
        expected.record_merge(node(90, &[(1, 1), (10, 1)]));
        assert!(rewound == expected);
        rewound.validate_persisted().unwrap();
        assert!(current == saved);
        assert!(rewound.revisions.shares_storage_with(&current.revisions));
        assert!(rewound
            .revoked_origins
            .shares_storage_with(&current.revoked_origins));
        // Durable positions need not equal the compacted in-memory sequence.
        let mut gapped = rows.clone();
        for row in &mut gapped {
            match row {
                GraphRecord::OriginEdge { position, .. }
                | GraphRecord::DownstreamEdge { position, .. } => *position += 100,
                _ => {}
            }
        }
        assert!(
            current
                .remove_appended_records(&gapped, &revision_merges(&current))
                .unwrap()
                .unwrap()
                .0
                == expected
        );
    }
    #[test]
    fn incomplete_duplicate_changed_and_extra_rows_cannot_remove_nodes() {
        let before = MergerGraph::new();
        let mut current = before.clone();
        current.record_merge(node(80, &[(1, 1), (1, 1)]));
        let rows = added_rows(&before, &current);
        for index in 0..rows.len() {
            let mut incomplete = rows.clone();
            incomplete.remove(index);
            assert!(current
                .remove_appended_records(&incomplete, &RevisionIndex::default())
                .is_err());
        }
        let mut duplicate = rows.clone();
        duplicate.push(rows[0].clone());
        assert!(current
            .remove_appended_records(&duplicate, &RevisionIndex::default())
            .is_err());
        let mut changed = rows.clone();
        if let GraphRecord::Node { total, .. } = &mut changed[0] {
            *total += 1;
        } else {
            panic!("node first");
        }
        assert!(current
            .remove_appended_records(&changed, &RevisionIndex::default())
            .is_err());
        let mut extra = rows;
        extra.push(GraphRecord::SourceRevisions(id(80)));
        assert!(current
            .remove_appended_records(&extra, &RevisionIndex::default())
            .is_err());
        current.validate_persisted().unwrap();
    }
    #[test]
    fn surviving_consumers_and_headless_revision_history_prevent_removal() {
        let before = MergerGraph::new();
        let mut current = before.clone();
        current.record_merge(node(80, &[(1, 2)]));
        let rows = added_rows(&before, &current);
        let mut dependent = current.clone();
        dependent.record_merge(node(81, &[(80, 1)]));
        assert!(dependent
            .remove_appended_records(&rows, &revision_merges(&dependent))
            .is_err());
        current.apply_revocation(id(1));
        current.apply_restoration(&id(1));
        current.current_revisions.remove(&id(80));
        current.validate_persisted().unwrap();
        assert!(!current.revisions.is_empty());
        assert!(current
            .remove_appended_records(&rows, &revision_merges(&current))
            .unwrap()
            .is_none());
    }
    #[test]
    fn revised_source_basis_rows_are_required_but_unrelated_history_survives() {
        let mut before = MergerGraph::new();
        before.record_merge(node(10, &[(1, 1), (2, 1)]));
        before.apply_revocation(id(1));
        let mut current = before.clone();
        current.record_merge_with_bases(node(80, &[(10, 1)]), vec![before.current_basis(&id(10))]);
        current.validate_persisted().unwrap();
        let rows = added_rows(&before, &current);
        let (rewound, _) = current
            .remove_appended_records(&rows, &revision_merges(&current))
            .unwrap()
            .unwrap();
        assert!(rewound == before);
        rewound.validate_persisted().unwrap();
        let partial: Vec<_> = rows
            .into_iter()
            .filter(|row| !matches!(row, GraphRecord::SourceRevision { .. }))
            .collect();
        assert!(current
            .remove_appended_records(&partial, &revision_merges(&current))
            .is_err());
    }
}
