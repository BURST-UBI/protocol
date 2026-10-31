//! Counts of revisions per merge and references to retained revisions.
//! Restoration uses these indexes to check whether records can be removed.
use super::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RevisionIndex {
    merges: SharedMap<TxHash, usize>,
    references: SharedMap<TxHash, usize>,
}

fn count(map: &mut SharedMap<TxHash, usize>, key: TxHash, add: bool) -> Result<(), String> {
    crate::dependency_counts::adjust(map, key, add, "revision dependency count overflow or underflow")
}

impl RevisionIndex {
    #[cfg(test)]
    pub(crate) fn new(graph: &MergerGraph) -> Result<Self, String> {
        Self::new_with_poll(graph, &|_| Ok(()))
    }

    pub(crate) fn new_with_poll(graph: &MergerGraph, poll: &dyn Fn(&'static str) -> Result<(), String>) -> Result<Self, String> {
        let (merges, references) = Self::build_counts_with_poll(graph, poll)?;
        Ok(Self { merges, references })
    }

    /// Count records and references without checking target existence, hashes or
    /// cycles. The poll callback can cancel construction without publishing a result.
    pub(crate) fn build_counts_with_poll<C: crate::dependency_counts::Counts>(graph: &MergerGraph,
        poll: &dyn Fn(&'static str) -> Result<(), String>) -> Result<(C, C), String> {
        let mut merges = C::default();
        let mut references = C::default();
        let error = "revision dependency count overflow or underflow";
        for revision in graph.revisions.values() {
            poll("trst:revision-index")?;
            crate::dependency_counts::adjust(&mut merges, revision.merge_tx, true, error)?;
            if revision.previous != TxHash::ZERO {
                crate::dependency_counts::adjust(&mut references, revision.previous, true, error)?;
            }
        }
        for revision in graph.current_revisions.values() {
            poll("trst:revision-current-index")?;
            crate::dependency_counts::adjust(&mut references, *revision, true, error)?;
        }
        for sources in graph.source_revisions.values() {
            poll("trst:revision-source-index")?;
            for (_, basis) in sources {
                poll("trst:revision-basis-index")?;
                if let TrstBasis::Remainder(revision) = basis {
                    crate::dependency_counts::adjust(&mut references, *revision, true, error)?;
                }
            }
        }
        Ok((merges, references))
    }

    pub(crate) fn has_merge(&self, merge: &TxHash) -> bool { self.merges.contains_key(merge) }

    pub(crate) fn references(&self, revision: &TxHash) -> bool {
        self.references.contains_key(revision)
    }

    fn revision(&mut self, revision: &RevocationRevision, add: bool) -> Result<(), String> {
        count(&mut self.merges, revision.merge_tx, add)?;
        if revision.previous != TxHash::ZERO { count(&mut self.references, revision.previous, add)?; }
        Ok(())
    }

    fn sources(&mut self, sources: &[(usize, TrstBasis)], add: bool) -> Result<(), String> {
        for (_, basis) in sources {
            if let TrstBasis::Remainder(revision) = basis {
                count(&mut self.references, *revision, add)?;
            }
        }
        Ok(())
    }

    /// The caller binds `before` to this exact validated cut. This updates only
    /// dependency counts; it neither validates nor authorizes the graph change.
    /// Work follows changed keys. Errors discard the candidate and leave this
    /// index unchanged; success returns a new index without installing it.
    pub(crate) fn updated(&self, before: &MergerGraph, after: &MergerGraph) -> Result<Self, String> {
        let mut candidate = self.clone();
        for id in after.revisions.changed_keys(&before.revisions) {
            if let Some(old) = before.revisions.get(id) { candidate.revision(old, false)?; }
            if let Some(new) = after.revisions.get(id) { candidate.revision(new, true)?; }
        }
        for merge in after.current_revisions.changed_keys(&before.current_revisions) {
            if let Some(old) = before.current_revisions.get(merge) { count(&mut candidate.references, *old, false)?; }
            if let Some(new) = after.current_revisions.get(merge) { count(&mut candidate.references, *new, true)?; }
        }
        for merge in after.source_revisions.changed_keys(&before.source_revisions) {
            if let Some(old) = before.source_revisions.get(merge) { candidate.sources(old, false)?; }
            if let Some(new) = after.source_revisions.get(merge) { candidate.sources(new, true)?; }
        }
        Ok(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(n: u8) -> TxHash { TxHash::new([n; 32]) }
    fn baseline() -> MergerGraph {
        let mut graph = MergerGraph::new();
        graph.record_merge(MergeNode { merge_tx: id(10), source_origins: vec![
            MergeSource { origin: id(1), amount: 5 }, MergeSource { origin: id(2), amount: 5 }],
            total_amount: 10, revoked_contribs: HashMap::new() });
        graph.apply_revocation(id(1));
        graph
    }

    #[test]
    fn transient_revision_counts_match_persistent_construction() {
        let mut graph = baseline();
        for n in 11..20 {
            graph.record_merge(MergeNode { merge_tx: id(n),
                source_origins: vec![MergeSource { origin: id(10), amount: 1 }],
                total_amount: 1, revoked_contribs: HashMap::new() });
        }
        let full = RevisionIndex::new(&graph).unwrap();
        let (merges, references) = RevisionIndex::build_counts_with_poll::<HashMap<TxHash, usize>>(&graph, &|_| Ok(())).unwrap();
        assert_eq!(merges, full.merges.iter().map(|(k, v)| (*k, *v)).collect());
        assert_eq!(references, full.references.iter().map(|(k, v)| (*k, *v)).collect());
        assert!(references.values().any(|count| *count > 1));
    }

    #[test]
    fn revision_heads_predecessors_and_source_bases_keep_distinct_references() {
        let before = baseline();
        before.validate_persisted().unwrap();
        let first = before.current_revisions[&id(10)];
        let initial = RevisionIndex::new(&before).unwrap();
        assert_eq!(initial.references.get(&first), Some(&1));
        let mut graph = before.clone();
        graph.record_merge(MergeNode { merge_tx: id(11), source_origins: vec![
            MergeSource { origin: id(10), amount: 1 }, MergeSource { origin: id(10), amount: 1 }],
            total_amount: 2, revoked_contribs: HashMap::new() });
        let index = initial.updated(&before, &graph).unwrap();
        assert_eq!(index, RevisionIndex::new(&graph).unwrap());
        assert_eq!(index.references.get(&first), Some(&3));
        let before_restore = graph.clone();
        graph.apply_restoration(&id(1));
        let second = graph.current_revisions[&id(10)];
        let restored = index.updated(&before_restore, &graph).unwrap();
        assert_eq!(restored, RevisionIndex::new(&graph).unwrap());
        assert_eq!(restored.references.get(&first), Some(&3));
        assert!(restored.references(&second));
        // Missing heads do not erase retained revision history or source bases.
        let with_head = graph.clone();
        graph.current_revisions.remove(&id(10));
        let headless = restored.updated(&with_head, &graph).unwrap();
        graph.validate_persisted().unwrap();
        assert!(headless.has_merge(&id(10)));
        assert!(!headless.references(&second));
        assert!(headless.references(&first));
        assert_eq!(headless, RevisionIndex::new(&graph).unwrap());
        assert_eq!(initial.references.get(&first), Some(&1));
    }

    #[test]
    fn removed_source_bases_release_counts_and_failed_updates_leave_prior_index() {
        let before = baseline();
        let first = before.current_revisions[&id(10)];
        let mut graph = before.clone();
        graph.record_merge(MergeNode { merge_tx: id(11), source_origins: vec![
            MergeSource { origin: id(10), amount: 1 }], total_amount: 1, revoked_contribs: HashMap::new() });
        let index = RevisionIndex::new(&graph).unwrap();
        let mut rows = Vec::new();
        graph.visit_record_delta(&before, |old, new| { assert!(old.is_none()); rows.push(new.unwrap()); Ok(()) }).unwrap();
        let (rewound, _) = graph.remove_appended_records(&rows, &index).unwrap().unwrap();
        let next = index.updated(&graph, &rewound).unwrap();
        assert_eq!(next, RevisionIndex::new(&before).unwrap());
        assert_eq!(next.references.get(&first), Some(&1));
        assert_eq!(index.references.get(&first), Some(&2));
        let empty = RevisionIndex::default();
        assert!(empty.updated(&graph, &before).is_err());
        assert_eq!(empty, RevisionIndex::default());
    }
}
