//! Checked inverse of revocation metadata; graph topology remains fixed.
use super::*;

/// Candidate graph and updated dependency counts, not a committed ledger change.
pub(crate) struct RevocationUndo {
    pub graph: MergerGraph,
    pub contributions: SharedMap<TxHash, usize>,
    pub revisions: RevisionIndex,
    pub removed_revisions: Vec<TxHash>,
}

impl MergerGraph {
    fn revocation_record(&self, template: &GraphRecord) -> Option<GraphRecord> {
        match template {
            GraphRecord::Contribution { merge, origin, .. } => self.merge_nodes.get(merge)?
                .revoked_contribs.get(origin).map(|amount| GraphRecord::Contribution {
                    merge: *merge, origin: *origin, amount: *amount }),
            GraphRecord::RevokedOrigin(origin) => self.revoked_origins.contains(origin)
                .then_some(GraphRecord::RevokedOrigin(*origin)),
            GraphRecord::Revision { hash, .. } => self.revisions.get(hash).map(|r| GraphRecord::Revision {
                hash: *hash, merge: r.merge_tx, previous: r.previous, origin: r.origin,
                revoked: r.revoked, remaining: r.remaining }),
            GraphRecord::CurrentRevision { merge, .. } => self.current_revisions.get(merge)
                .map(|revision| GraphRecord::CurrentRevision { merge: *merge, revision: *revision }),
            _ => None,
        }
    }

    /// `changes` are (current, restored) records already compared with the
    /// writer's exact journal bytes. Revisions are immutable: an inverse may
    /// remove appended revisions, never add or replace them. Callers must still
    /// validate ownership, outcome causes and holding references before publish.
    /// Both supplied indexes must describe this exact validated graph. Graph
    /// references prevent removal of a still-used revision; token references
    /// must be checked by the caller using the returned removed_revisions list.
    ///
    /// None requests full restoration for unsupported changes. Success returns
    /// a candidate without modifying self; removed revision order follows input.
    pub(crate) fn rewind_revocation_records(
        &self,
        changes: &[(Option<GraphRecord>, Option<GraphRecord>)],
        contributions: &SharedMap<TxHash, usize>,
        revisions: &RevisionIndex,
    ) -> Result<Option<RevocationUndo>, String> {
        let mut candidate = self.clone();
        let mut seen = HashSet::new();
        let mut removed = Vec::new();
        for (old, new) in changes {
            let row = old.as_ref().or(new.as_ref()).ok_or("empty graph undo change")?;
            if !matches!(row, GraphRecord::Contribution { .. } | GraphRecord::RevokedOrigin(_)
                | GraphRecord::Revision { .. } | GraphRecord::CurrentRevision { .. }) {
                return Ok(None);
            }
            let key = row.key();
            if new.as_ref().is_some_and(|new| new.key() != key) || !seen.insert(key) {
                return Err("graph undo changes a key or repeats a row".into());
            }
            if self.revocation_record(row).as_ref() != old.as_ref() {
                return Err("graph undo differs from retained revision cut".into());
            }
            match row {
                GraphRecord::Contribution { merge, origin, .. } => {
                    let node = candidate.merge_nodes.get_mut(merge).ok_or("graph undo contribution lacks node")?;
                    if let Some(GraphRecord::Contribution { amount, .. }) = new {
                        node.revoked_contribs.insert(*origin, *amount);
                    } else { node.revoked_contribs.remove(origin); }
                }
                GraphRecord::RevokedOrigin(origin) => {
                    if new.is_some() { candidate.revoked_origins.insert(*origin); }
                    else { candidate.revoked_origins.remove(origin); }
                }
                GraphRecord::Revision { hash, .. } => {
                    if new.is_some() { return Ok(None); }
                    candidate.revisions.remove(hash);
                    removed.push(*hash);
                }
                GraphRecord::CurrentRevision { merge, .. } => {
                    if let Some(GraphRecord::CurrentRevision { revision, .. }) = new {
                        candidate.current_revisions.insert(*merge, *revision);
                    } else { candidate.current_revisions.remove(merge); }
                }
                _ => unreachable!("supported rows checked above"),
            }
        }
        if candidate.revoked_origins.contains(&TxHash::ZERO) { return Err("invalid revoked origin".into()); }
        // Reuse the fixed-topology contribution proof. Retaining old revision
        // metadata in this temporary context avoids assuming the yet-unproved
        // deletion is valid. Its dependency proof is checked separately below.
        let mut contribution_context = candidate.clone();
        contribution_context.revisions = self.revisions.clone();
        contribution_context.current_revisions = self.current_revisions.clone();
        let counts = contribution_context.validated_revocation_transition(self, contributions)?
            .ok_or("graph undo changed fixed topology")?;
        for merge in candidate.current_revisions.changed_keys(&self.current_revisions) {
            if let Some(revision) = candidate.current_revisions.get(merge) {
                candidate.validate_current_revision(merge, revision)?;
            }
        }
        let index = revisions.updated(self, &candidate)?;
        for revision in &removed {
            if index.references(revision) {
                return Err("graph undo would orphan a revision dependency".into());
            }
        }
        Ok(Some(RevocationUndo { graph: candidate, contributions: counts,
            revisions: index, removed_revisions: removed }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(n: u8) -> TxHash { TxHash::new([n; 32]) }
    fn base() -> MergerGraph {
        let mut graph = MergerGraph::new();
        graph.record_merge(MergeNode { merge_tx: id(10), source_origins: vec![
            MergeSource { origin: id(1), amount: 5 }, MergeSource { origin: id(2), amount: 5 }],
            total_amount: 10, revoked_contribs: HashMap::new() });
        graph
    }
    fn inverse(before: &MergerGraph, after: &MergerGraph) -> Vec<(Option<GraphRecord>, Option<GraphRecord>)> {
        let mut rows = Vec::new();
        after.visit_record_delta(before, |old, new| { rows.push((new, old)); Ok(()) }).unwrap();
        rows
    }
    fn undo(current: &MergerGraph, rows: &[(Option<GraphRecord>, Option<GraphRecord>)]) -> Result<Option<RevocationUndo>, String> {
        current.rewind_revocation_records(rows, &current.revocation_counts()?, &RevisionIndex::new(current)?)
    }
    #[test]
    fn revocation_and_restoration_inverse_match_full_graph_validation() {
        let original = base();
        let mut revoked = original.clone(); revoked.apply_revocation(id(1));
        let mut restored = revoked.clone(); restored.apply_restoration(&id(1));
        for (before, current) in [(&original, &revoked), (&revoked, &restored)] {
            let rows = inverse(before, current);
            let saved = current.to_bytes();
            let result = undo(current, &rows).unwrap().unwrap();
            assert!(result.graph == *before);
            result.graph.validate_persisted().unwrap();
            assert_eq!(result.contributions, before.revocation_counts().unwrap());
            assert_eq!(result.revisions, RevisionIndex::new(before).unwrap());
            assert!(!result.removed_revisions.is_empty());
            assert_eq!(current.to_bytes(), saved);
        }
    }
    #[test]
    fn surviving_source_basis_and_incomplete_metadata_fail_without_mutation() {
        let mut before = base(); before.apply_revocation(id(1));
        let mut current = before.clone(); current.apply_restoration(&id(1));
        let rows = inverse(&before, &current);
        let mut consumed = current.clone();
        consumed.record_merge(MergeNode { merge_tx: id(11), source_origins: vec![
            MergeSource { origin: id(10), amount: 1 }], total_amount: 1, revoked_contribs: HashMap::new() });
        assert!(undo(&consumed, &rows).is_err());
        let no_marker: Vec<_> = rows.iter().filter(|(_, restored)|
            !matches!(restored, Some(GraphRecord::RevokedOrigin(_)))).cloned().collect();
        assert!(undo(&current, &no_marker).is_err());
        let mut duplicated = rows.clone(); duplicated.push(rows[0].clone());
        assert!(undo(&current, &duplicated).is_err());
        let mut wrong = rows.clone();
        let record = wrong.iter_mut().find_map(|(old, _)| match old {
            Some(GraphRecord::Revision { remaining, .. }) => Some(remaining), _ => None }).unwrap();
        *record += 1;
        assert!(undo(&current, &wrong).is_err());
        current.validate_persisted().unwrap(); consumed.validate_persisted().unwrap();
    }
}
