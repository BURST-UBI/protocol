use super::*;

type Visit<'a> = dyn FnMut(Option<GraphRecord>, Option<GraphRecord>) -> Result<(), String> + 'a;

fn changed<T: Clone + PartialEq>(
    before: &SharedMap<TxHash, T>,
    after: &SharedMap<TxHash, T>,
    mut visit: impl FnMut(TxHash, Option<&T>, Option<&T>) -> Result<(), String>,
) -> Result<(), String> {
    if before.shares_storage_with(after) {
        return Ok(());
    }
    for key in after.changed_keys(before) {
        let old = before.get(key);
        let new = after.get(key);
        if old != new {
            visit(*key, old, new)?;
        }
    }
    Ok(())
}

fn rows(
    before: Vec<GraphRecord>,
    after: Vec<GraphRecord>,
    visit: &mut Visit<'_>,
) -> Result<(), String> {
    let mut before: BTreeMap<_, _> = before
        .into_iter()
        .map(|record| (record.key(), record))
        .collect();
    for record in after {
        let old = before.remove(&record.key());
        if old.as_ref() != Some(&record) {
            visit(old, Some(record))?;
        }
    }
    for (_, record) in before {
        visit(Some(record), None)?;
    }
    Ok(())
}

fn node_rows(hash: TxHash, node: &MergeNode) -> Vec<GraphRecord> {
    let mut records = vec![GraphRecord::Node {
        hash,
        total: node.total_amount,
    }];
    records.extend(
        node.source_origins
            .iter()
            .enumerate()
            .map(|(position, source)| GraphRecord::Source {
                merge: hash,
                position: position as u64,
                source: source.clone(),
            }),
    );
    records.extend(node.revoked_contribs.iter().map(|(origin, amount)| {
        GraphRecord::Contribution {
            merge: hash,
            origin: *origin,
            amount: *amount,
        }
    }));
    records
}

fn appended_edges(
    before: &SharedMap<TxHash, EdgeList>,
    after: &SharedMap<TxHash, EdgeList>,
    downstream: bool,
    visit: &mut Visit<'_>,
) -> Result<(), String> {
    changed(before, after, |parent, old, new| {
        let empty = EdgeList::default();
        let old = old.unwrap_or(&empty);
        let new = new.unwrap_or(&empty);
        new.visit_appended(old, |position, child, occurrence| {
            let record = if downstream {
                GraphRecord::DownstreamEdge {
                    merge: parent,
                    position: position as u64,
                    child,
                    occurrence,
                }
            } else {
                GraphRecord::OriginEdge {
                    origin: parent,
                    position: position as u64,
                    child,
                    occurrence,
                }
            };
            visit(None, Some(record))
        })
    })
}

impl MergerGraph {
    /// Emit old/new record pairs for merges, revocations and restorations. Shared
    /// map storage is skipped, but changed nodes build their full row lists before
    /// comparison. Reverse edges must be append-only; ancestry removal needs the
    /// checked replacement path.
    ///
    /// A late edge or callback error can follow earlier emissions. Callers stage
    /// the entire delta before committing. This visitor does not validate graph
    /// structure, and record order is not a canonical serialization order.
    pub fn visit_record_delta(
        &self,
        before: &Self,
        mut visit: impl FnMut(Option<GraphRecord>, Option<GraphRecord>) -> Result<(), String>,
    ) -> Result<(), String> {
        changed(&before.merge_nodes, &self.merge_nodes, |hash, old, new| {
            rows(
                old.map_or_else(Vec::new, |node| node_rows(hash, node)),
                new.map_or_else(Vec::new, |node| node_rows(hash, node)),
                &mut visit,
            )
        })?;
        appended_edges(
            &before.origin_to_merges,
            &self.origin_to_merges,
            false,
            &mut visit,
        )?;
        appended_edges(
            &before.merge_to_downstream,
            &self.merge_to_downstream,
            true,
            &mut visit,
        )?;
        for origin in self.revoked_origins.changed_keys(&before.revoked_origins) {
            let old = before
                .revoked_origins
                .contains(origin)
                .then_some(GraphRecord::RevokedOrigin(*origin));
            let new = self
                .revoked_origins
                .contains(origin)
                .then_some(GraphRecord::RevokedOrigin(*origin));
            visit(old, new)?;
        }
        changed(&before.revisions, &self.revisions, |hash, old, new| {
            let record = |r: &RevocationRevision| GraphRecord::Revision {
                hash,
                merge: r.merge_tx,
                previous: r.previous,
                origin: r.origin,
                revoked: r.revoked,
                remaining: r.remaining,
            };
            visit(old.map(record), new.map(record))
        })?;
        changed(
            &before.current_revisions,
            &self.current_revisions,
            |merge, old, new| {
                let record = |revision: &TxHash| GraphRecord::CurrentRevision {
                    merge,
                    revision: *revision,
                };
                visit(old.map(record), new.map(record))
            },
        )?;
        changed(
            &before.source_revisions,
            &self.source_revisions,
            |merge, old, new| {
                let records =
                    |sources: &Vec<(usize, TrstBasis)>| {
                        let mut records = vec![GraphRecord::SourceRevisions(merge)];
                        records.extend(sources.iter().enumerate().map(
                            |(position, (index, basis))| GraphRecord::SourceRevision {
                                merge,
                                position: position as u64,
                                index: *index as u64,
                                basis: *basis,
                            },
                        ));
                        records
                    };
                rows(
                    old.map_or_else(Vec::new, records),
                    new.map_or_else(Vec::new, records),
                    &mut visit,
                )
            },
        )
    }
}
