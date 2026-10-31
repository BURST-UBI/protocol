//! Incremental graph checks against an already validated snapshot.
//! Appends and revocation changes have separate checks. Neither authenticates
//! ledger operations or establishes ownership of burn origins.
use super::*;

type Counts = HashMap<(TxHash, TxHash), usize>;

fn appended_edges(
    before: &SharedMap<TxHash, EdgeList>,
    after: &SharedMap<TxHash, EdgeList>,
    mut expected: Counts,
) -> Result<(), String> {
    for parent in after.changed_keys(before) {
        let empty = EdgeList::default();
        let old = before.get(parent).unwrap_or(&empty);
        let new = after.get(parent).unwrap_or(&empty);
        new.visit_appended(old, |_, child, _| {
            let key = (*parent, child);
            let count = expected
                .get_mut(&key)
                .ok_or("unexpected appended merger edge")?;
            *count -= 1;
            if *count == 0 {
                expected.remove(&key);
            }
            Ok(())
        })?;
        if new.is_empty() {
            return Err("empty merger graph reverse index".into());
        }
    }
    if !expected.is_empty() {
        return Err("missing appended merger edge".into());
    }
    Ok(())
}

impl MergerGraph {
    /// Merge two append-only cuts without replacing the current reverse-edge
    /// prefixes. The caller must separately check burn ownership and holdings.
    /// Revocations, revisions and edits to old nodes require fresh preparation.
    /// Returns a new graph without changing either branch. None means this path
    /// cannot validate the changes; it is not permission to install them unchecked.
    pub(crate) fn rebase_appends(
        &self,
        before: &Self,
        after: &Self,
    ) -> Result<Option<Self>, String> {
        if self.shares_storage_with(before) && after.shares_storage_with(before) {
            return Ok(Some(self.clone()));
        }
        if self.validated_append_ids(before)?.is_none() {
            return Ok(None);
        }
        let Some(added) = after.validated_append_ids(before)? else {
            return Ok(None);
        };
        let mut rebased = self.clone();
        for id in added {
            if self.merge_nodes.contains_key(&id) {
                return Err("prepared merge ID already exists".into());
            }
            rebased
                .merge_nodes
                .insert(id, after.merge_nodes[&id].clone());
            if let Some(bases) = after.source_revisions.get(&id) {
                rebased.source_revisions.insert(id, bases.clone());
            }
        }
        fn append(
            current: &mut SharedMap<TxHash, EdgeList>,
            before: &SharedMap<TxHash, EdgeList>,
            after: &SharedMap<TxHash, EdgeList>,
        ) -> Result<(), String> {
            for parent in after.changed_keys(before) {
                let empty = EdgeList::default();
                let old = before.get(parent).unwrap_or(&empty);
                let new = after
                    .get(parent)
                    .ok_or("prepared merge removes reverse edges")?;
                // Preserve arrival order, including repeated parent occurrences.
                new.visit_appended(old, |_, child, _| {
                    current.entry(*parent).or_default().push(child);
                    Ok(())
                })?;
            }
            Ok(())
        }
        append(
            &mut rebased.origin_to_merges,
            &before.origin_to_merges,
            &after.origin_to_merges,
        )?;
        append(
            &mut rebased.merge_to_downstream,
            &before.merge_to_downstream,
            &after.merge_to_downstream,
        )?;
        // Concurrent appends must not reinterpret a prepared burn as a merge,
        // or promote an existing burn source to the candidate's new merge ID.
        if rebased.validated_append_ids(self)?.is_none() {
            return Err("prepared merge conflicts with current graph".into());
        }
        Ok(Some(rebased))
    }

    /// Count merge nodes with a contribution entry for each revoked origin.
    #[cfg(test)]
    pub(crate) fn revocation_counts(&self) -> Result<SharedMap<TxHash, usize>, String> {
        self.revocation_counts_with_poll(&|_| Ok(()))
    }

    pub(crate) fn revocation_counts_with_poll(
        &self,
        poll: &dyn Fn(&'static str) -> Result<(), String>,
    ) -> Result<SharedMap<TxHash, usize>, String> {
        self.build_revocation_counts_with_poll(poll)
    }

    /// Count contribution entries, not their amounts. This scans every merge node
    /// and contribution; it does not validate the entries being counted.
    pub(crate) fn build_revocation_counts_with_poll<C: crate::dependency_counts::Counts>(
        &self,
        poll: &dyn Fn(&'static str) -> Result<(), String>,
    ) -> Result<C, String> {
        let mut counts = C::default();
        for node in self.merge_nodes.values() {
            poll("trst:revocation-count-node")?;
            for origin in node.revoked_contribs.keys() {
                poll("trst:revocation-count-origin")?;
                crate::dependency_counts::adjust(
                    &mut counts,
                    *origin,
                    true,
                    "revocation contribution count overflow",
                )?;
            }
        }
        Ok(counts)
    }

    /// Return updated counts for the supplied added nodes. The caller must pass
    /// each newly counted ID once; repeated IDs would be counted repeatedly.
    pub(crate) fn add_revocation_counts(
        &self,
        before: &SharedMap<TxHash, usize>,
        ids: impl IntoIterator<Item = TxHash>,
    ) -> Result<SharedMap<TxHash, usize>, String> {
        let mut counts = before.clone();
        for id in ids {
            for origin in self
                .merge_nodes
                .get(&id)
                .ok_or("missing counted merge")?
                .revoked_contribs
                .keys()
            {
                let count = counts
                    .get(origin)
                    .copied()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or("revocation contribution count overflow")?;
                counts.insert(*origin, count);
            }
        }
        Ok(counts)
    }

    /// The caller owns counts for the validated before cut. Fixed graph topology
    /// and immutable old revisions let unchanged nodes/bases inherit that proof.
    /// Some returns updated contribution counts after structural checks. None
    /// requires full validation. This does not authorize the revocation or replay
    /// its amount calculations from ledger history.
    pub(crate) fn validated_revocation_transition(
        &self,
        before: &Self,
        before_counts: &SharedMap<TxHash, usize>,
    ) -> Result<Option<SharedMap<TxHash, usize>>, String> {
        if !self
            .origin_to_merges
            .shares_storage_with(&before.origin_to_merges)
            || !self
                .merge_to_downstream
                .shares_storage_with(&before.merge_to_downstream)
            || !self
                .source_revisions
                .shares_storage_with(&before.source_revisions)
        {
            return Ok(None);
        }
        let mut counts = before_counts.clone();
        for id in self.merge_nodes.changed_keys(&before.merge_nodes) {
            let (Some(old), Some(new)) = (before.merge_nodes.get(id), self.merge_nodes.get(id))
            else {
                return Ok(None);
            };
            if old.merge_tx != new.merge_tx
                || old.total_amount != new.total_amount
                || old.source_origins != new.source_origins
            {
                return Ok(None);
            }
            snapshot_validation::validate_node(id, new, &self.revoked_origins)?;
            for origin in old
                .revoked_contribs
                .keys()
                .filter(|o| !new.revoked_contribs.contains_key(o))
            {
                let count = counts
                    .get(origin)
                    .copied()
                    .unwrap_or(0)
                    .checked_sub(1)
                    .ok_or("revocation contribution count underflow")?;
                if count == 0 {
                    counts.remove(origin);
                } else {
                    counts.insert(*origin, count);
                }
            }
            for origin in new
                .revoked_contribs
                .keys()
                .filter(|o| !old.revoked_contribs.contains_key(o))
            {
                let count = counts
                    .get(origin)
                    .copied()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or("revocation contribution count overflow")?;
                counts.insert(*origin, count);
            }
        }
        for origin in self.revoked_origins.changed_keys(&before.revoked_origins) {
            if !self.revoked_origins.contains(origin) && counts.contains_key(origin) {
                return Err("invalid merger graph revocation contribution".into());
            }
        }
        for id in self.revisions.changed_keys(&before.revisions) {
            if before.revisions.contains_key(id) || !self.revisions.contains_key(id) {
                return Ok(None);
            }
            self.validate_revision(id, &self.revisions[id])?;
        }
        for merge in self
            .current_revisions
            .changed_keys(&before.current_revisions)
        {
            if let Some(revision) = self.current_revisions.get(merge) {
                self.validate_current_revision(merge, revision)?;
            }
        }
        Ok(Some(counts))
    }

    /// The caller must already hold a validated `before` cut and separately
    /// check new merge IDs and burn sources against its ownership registry.
    /// None requires full validation; it never grants an incremental proof.
    /// Shared subtrees avoid rescanning. Reverse-edge checks can still compare a
    /// full prefix when internal slot layouts differ. Returned IDs are sorted by
    /// hash, not by ancestry or transaction order.
    pub(crate) fn validated_append_ids(
        &self,
        before: &Self,
    ) -> Result<Option<Vec<TxHash>>, String> {
        if !self
            .revoked_origins
            .shares_storage_with(&before.revoked_origins)
            || !self.revisions.shares_storage_with(&before.revisions)
            || !self
                .current_revisions
                .shares_storage_with(&before.current_revisions)
        {
            return Ok(None);
        }
        let mut added = HashSet::new();
        for id in self.merge_nodes.changed_keys(&before.merge_nodes) {
            if before.merge_nodes.contains_key(id) || !self.merge_nodes.contains_key(id) {
                return Ok(None);
            }
            // A former burn source cannot become a merge while leaving old
            // downstream edges and revision interpretations unchanged.
            if before.origin_to_merges.contains_key(id) || before.revoked_origins.contains(id) {
                return Ok(None);
            }
            added.insert(*id);
        }
        let mut origins = Counts::new();
        let mut downstream = Counts::new();
        let mut indegrees = HashMap::new();
        let mut children: HashMap<TxHash, Vec<TxHash>> = HashMap::new();
        for id in &added {
            let node = self.merge_nodes.get(id).expect("added node");
            snapshot_validation::validate_node(id, node, &self.revoked_origins)?;
            let mut degree = 0usize;
            for source in &node.source_origins {
                *origins.entry((source.origin, *id)).or_default() += 1;
                if self.merge_nodes.contains_key(&source.origin) {
                    *downstream.entry((source.origin, *id)).or_default() += 1;
                }
                if added.contains(&source.origin) {
                    degree += 1;
                    children.entry(source.origin).or_default().push(*id);
                }
            }
            indegrees.insert(*id, degree);
        }
        let mut ready: Vec<_> = indegrees
            .iter()
            .filter_map(|(id, count)| (*count == 0).then_some(*id))
            .collect();
        let mut visited = 0;
        while let Some(id) = ready.pop() {
            visited += 1;
            if let Some(next) = children.get(&id) {
                for child in next {
                    let count = indegrees.get_mut(child).expect("new child");
                    *count -= 1;
                    if *count == 0 {
                        ready.push(*child);
                    }
                }
            }
        }
        if visited != added.len() {
            return Err("cyclic appended merger graph".into());
        }
        appended_edges(&before.origin_to_merges, &self.origin_to_merges, origins)?;
        appended_edges(
            &before.merge_to_downstream,
            &self.merge_to_downstream,
            downstream,
        )?;
        for id in self.source_revisions.changed_keys(&before.source_revisions) {
            if !added.contains(id) {
                return Ok(None);
            }
            let Some(sources) = self.source_revisions.get(id) else {
                return Ok(None);
            };
            let node = self.merge_nodes.get(id).expect("added revised node");
            if sources.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
                return Err("noncanonical merge source revisions".into());
            }
            for (index, basis) in sources {
                if *basis == TrstBasis::Entire
                    || !node
                        .source_origins
                        .get(*index)
                        .is_some_and(|source| self.valid_basis(&source.origin, *basis))
                {
                    return Err("invalid merge source revision".into());
                }
            }
        }
        let mut added: Vec<_> = added.into_iter().collect();
        added.sort_unstable();
        Ok(Some(added))
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
    #[test]
    fn prepared_appends_preserve_shared_parents_order_and_reject_changed_context() {
        let before = baseline();
        let after = appended(&before);
        let mut current = before.clone();
        current.record_merge(node(200, &[(1, 1), (10, 1)]));
        let saved = current.to_bytes();
        let rebased = current.rebase_appends(&before, &after).unwrap().unwrap();
        let expected = appended(&current);
        assert_eq!(rebased.to_bytes(), expected.to_bytes());
        rebased.validate_persisted().unwrap();
        assert_eq!(current.to_bytes(), saved);
        assert!(rebased.rebase_appends(&before, &after).is_err());
        let mut changed = current.clone();
        changed.apply_revocation(id(1));
        assert!(changed.rebase_appends(&before, &after).unwrap().is_none());
        let mut changed = after.clone();
        changed.merge_nodes.get_mut(&id(10)).unwrap().total_amount += 1;
        assert!(current.rebase_appends(&before, &changed).unwrap().is_none());
        let mut changed = after.clone();
        changed
            .origin_to_merges
            .get_mut(&id(1))
            .unwrap()
            .push(id(99));
        assert!(current.rebase_appends(&before, &changed).is_err());
        // Both inputs can independently be valid while their union promotes a
        // burn identifier used only by the other concurrent append.
        let mut current = before.clone();
        current.record_merge(node(201, &[(100, 1)]));
        current.validate_persisted().unwrap();
        assert!(current.rebase_appends(&before, &after).is_err());
        // Preserve explicit revision bases when appending to an already revised cut.
        let mut before = before;
        before.apply_revocation(id(1));
        let mut after = before.clone();
        after.record_merge_with_bases(node(100, &[(10, 1)]), vec![before.current_basis(&id(10))]);
        let mut current = before.clone();
        current.record_merge_with_bases(node(200, &[(11, 1)]), vec![before.current_basis(&id(11))]);
        let rebased = current.rebase_appends(&before, &after).unwrap().unwrap();
        rebased.validate_persisted().unwrap();
        assert_eq!(
            rebased.source_revisions[&id(100)],
            after.source_revisions[&id(100)]
        );
        assert_eq!(
            rebased.source_revisions[&id(200)],
            current.source_revisions[&id(200)]
        );
    }

    #[test]
    fn revocation_transition_preserves_old_nodes_and_revision_references() {
        let before = baseline();
        let mut after = before.clone();
        after.apply_revocation(id(1));
        assert!(after
            .validated_revocation_transition(&before, &before.revocation_counts().unwrap())
            .unwrap()
            .is_some());
        let mut restored = after.clone();
        restored.apply_restoration(&id(1));
        assert!(restored
            .validated_revocation_transition(&after, &after.revocation_counts().unwrap())
            .unwrap()
            .is_some());
        let mut corrupt = after.clone();
        corrupt.revoked_origins.remove(&id(1));
        assert!(corrupt
            .validated_revocation_transition(&before, &before.revocation_counts().unwrap())
            .is_err());
        let mut removed = restored.clone();
        removed.revisions.clear();
        assert!(!removed
            .validated_revocation_transition(&after, &after.revocation_counts().unwrap())
            .unwrap()
            .is_some());
        let mut changed = before.clone();
        changed.merge_nodes.get_mut(&id(10)).unwrap().source_origins[0].amount += 1;
        assert!(!changed
            .validated_revocation_transition(&before, &before.revocation_counts().unwrap())
            .unwrap()
            .is_some());
    }

    #[test]
    fn counted_revocation_proof_rejects_untouched_contributions_and_bad_revision_deltas() {
        let mut before = baseline();
        before.apply_revocation(id(1));
        let counts = before.revocation_counts().unwrap();
        let mut missing = before.clone();
        missing.revoked_origins.remove(&id(1));
        assert!(missing
            .validated_revocation_transition(&before, &counts)
            .is_err());
        assert!(missing.validate_persisted().is_err());
        let mut restored = before.clone();
        restored.apply_restoration(&id(1));
        let next = restored
            .validated_revocation_transition(&before, &counts)
            .unwrap()
            .unwrap();
        assert_eq!(next, restored.revocation_counts().unwrap());
        assert!(next.is_empty());
        for bad_head in [false, true] {
            let mut corrupt = before.clone();
            if bad_head {
                corrupt.current_revisions.insert(id(10), id(99));
            } else {
                let mut revision = corrupt.revisions.values().next().unwrap().clone();
                revision.previous = id(99);
                corrupt.revisions.insert(revision.hash(), revision);
            }
            assert!(corrupt
                .validated_revocation_transition(&before, &counts)
                .is_err());
            assert!(corrupt.validate_persisted().is_err());
        }
        assert!(restored
            .validated_revocation_transition(&before, &SharedMap::default())
            .is_err());
    }

    fn baseline() -> MergerGraph {
        let mut graph = MergerGraph::new();
        for n in 10..50 {
            graph.record_merge(node(n, &[(1, 1), (2, 1)]));
        }
        graph.validate_persisted().unwrap();
        graph
    }
    fn appended(before: &MergerGraph) -> MergerGraph {
        let mut graph = before.clone();
        graph.record_merge(node(100, &[(1, 1), (1, 1), (10, 2)]));
        graph.record_merge(node(101, &[(100, 2), (11, 1)]));
        graph
    }
    #[test]
    fn appended_batches_match_full_validation_with_repeated_edges_and_revision_bases() {
        let before = baseline();
        let after = appended(&before);
        assert_eq!(
            after.validated_append_ids(&before).unwrap(),
            Some(vec![id(100), id(101)])
        );
        after.validate_persisted().unwrap();
        assert_eq!(before.validated_append_ids(&before).unwrap(), Some(vec![]));
        let mut revoked = before.clone();
        revoked.apply_revocation(id(1));
        revoked.validate_persisted().unwrap();
        let mut after = revoked.clone();
        after.record_merge_with_bases(node(100, &[(10, 1)]), vec![revoked.current_basis(&id(10))]);
        assert_eq!(
            after.validated_append_ids(&revoked).unwrap(),
            Some(vec![id(100)])
        );
        after.validate_persisted().unwrap();
    }
    #[test]
    fn appended_corruption_is_rejected_with_persistent_old_edge_prefixes() {
        let before = baseline();
        let saved = before.to_bytes();
        for mutation in 0..6 {
            let mut after = appended(&before);
            match mutation {
                0 => after.origin_to_merges.get_mut(&id(1)).unwrap().push(id(77)),
                1 => {
                    after.origin_to_merges.get_mut(&id(1)).unwrap().pop();
                }
                2 => {
                    after.merge_to_downstream.remove(&id(10));
                }
                3 => {
                    after.merge_nodes.get_mut(&id(100)).unwrap().total_amount += 1;
                }
                4 => {
                    after
                        .source_revisions
                        .insert(id(100), vec![(99, TrstBasis::Single(id(2)))]);
                }
                _ => {
                    after.origin_to_merges.insert(id(2), EdgeList::default());
                }
            }
            assert!(
                after.validated_append_ids(&before).is_err(),
                "mutation={mutation}"
            );
            assert!(
                after.validate_persisted().is_err(),
                "full mutation={mutation}"
            );
            assert_eq!(before.to_bytes(), saved);
        }
    }
    #[test]
    fn existing_mutations_and_burn_promotions_require_full_validation() {
        let before = baseline();
        let mut replaced = before.clone();
        replaced.merge_nodes.get_mut(&id(10)).unwrap().total_amount += 1;
        assert!(replaced.validated_append_ids(&before).unwrap().is_none());
        let mut revoked = before.clone();
        revoked.apply_revocation(id(1));
        assert!(revoked.validated_append_ids(&before).unwrap().is_none());
        let mut promoted = before.clone();
        promoted.record_merge(node(1, &[(3, 2)]));
        assert!(promoted.validated_append_ids(&before).unwrap().is_none());
    }
    #[test]
    fn appended_cycles_are_rejected_even_with_matching_reverse_edges() {
        let before = baseline();
        let mut after = before.clone();
        after.record_merge(node(100, &[(101, 1)]));
        after.record_merge(node(101, &[(100, 1)]));
        after.record_downstream(id(101), id(100));
        assert!(after
            .validated_append_ids(&before)
            .unwrap_err()
            .contains("cyclic"));
        assert!(after.validate_persisted().unwrap_err().contains("cyclic"));
    }
}
