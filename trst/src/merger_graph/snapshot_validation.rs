use super::*;

type Edges = HashMap<(TxHash, TxHash), usize>;

fn check_index(index: &HashMap<TxHash, Vec<TxHash>>, mut expected: Edges) -> Result<(), String> {
    for (origin, consumers) in index {
        if consumers.is_empty() {
            return Err("empty merger graph reverse index".into());
        }
        for consumer in consumers {
            let key = (*origin, *consumer);
            let count = expected
                .get_mut(&key)
                .ok_or("unexpected merger graph reverse edge")?;
            *count -= 1;
            if *count == 0 {
                expected.remove(&key);
            }
        }
    }
    if !expected.is_empty() {
        return Err("missing merger graph reverse edge".into());
    }
    Ok(())
}

impl MergerGraph {
    /// Check stored indexes against the immediate inputs before any revocation
    /// traverses them. Repeated inputs retain their edge multiplicity.
    pub(super) fn validate_structure(&self) -> Result<(), String> {
        let mut origins = Edges::new();
        let mut downstream = Edges::new();
        let mut indegrees = HashMap::new();
        for (hash, node) in &self.merge_nodes {
            if *hash == TxHash::ZERO
                || *hash != node.merge_tx
                || node.total_amount == 0
                || node.source_origins.is_empty()
                || node.source_origins.len() > crate::MAX_MERGE_SOURCES
            {
                return Err("invalid merger graph node".into());
            }
            let mut total = 0u128;
            let mut indegree = 0usize;
            for source in &node.source_origins {
                if source.origin == TxHash::ZERO || source.amount == 0 {
                    return Err("invalid merger graph source".into());
                }
                total = total
                    .checked_add(source.amount)
                    .ok_or("merger graph source total overflow")?;
                *origins.entry((source.origin, *hash)).or_default() += 1;
                if self.merge_nodes.contains_key(&source.origin) {
                    *downstream.entry((source.origin, *hash)).or_default() += 1;
                    indegree += 1;
                }
            }
            if total != node.total_amount {
                return Err("merger graph source total mismatch".into());
            }
            let mut revoked = 0u128;
            for (origin, amount) in &node.revoked_contribs {
                if *amount == 0 || !self.revoked_origins.contains(origin) {
                    return Err("invalid merger graph revocation contribution".into());
                }
                revoked = revoked
                    .checked_add(*amount)
                    .ok_or("merger graph revocation total overflow")?;
            }
            if revoked > total {
                return Err("merger graph revocation exceeds its amount".into());
            }
            indegrees.insert(*hash, indegree);
        }
        if self.revoked_origins.contains(&TxHash::ZERO) {
            return Err("invalid revoked origin".into());
        }
        check_index(&self.origin_to_merges, origins)?;
        check_index(&self.merge_to_downstream, downstream)?;

        let mut ready: VecDeque<_> = indegrees
            .iter()
            .filter_map(|(hash, count)| (*count == 0).then_some(*hash))
            .collect();
        let mut visited = 0usize;
        while let Some(hash) = ready.pop_front() {
            visited += 1;
            if let Some(children) = self.merge_to_downstream.get(&hash) {
                for child in children {
                    // Both endpoints and multiplicity were checked above.
                    let count = indegrees.get_mut(child).expect("validated reverse edge");
                    *count -= 1;
                    if *count == 0 {
                        ready.push_back(*child);
                    }
                }
            }
        }
        if visited != self.merge_nodes.len() {
            return Err("cyclic merger graph snapshot".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(value: u8) -> TxHash {
        TxHash::new([value; 32])
    }

    fn merge(graph: &mut MergerGraph, id: u8, sources: &[(u8, u128)]) {
        graph.record_merge(MergeNode {
            merge_tx: hash(id),
            source_origins: sources
                .iter()
                .map(|(origin, amount)| MergeSource {
                    origin: hash(*origin),
                    amount: *amount,
                })
                .collect(),
            total_amount: sources.iter().map(|(_, amount)| amount).sum(),
            revoked_contribs: HashMap::new(),
        });
    }

    fn fixture() -> MergerGraph {
        let mut graph = MergerGraph::new();
        merge(&mut graph, 10, &[(1, 30), (1, 20), (2, 50)]);
        merge(&mut graph, 20, &[(10, 40), (10, 10), (3, 50)]);
        graph
    }

    #[test]
    fn repeated_inputs_preserve_exact_revocation_through_reload() {
        let mut original = fixture();
        let mut restored = MergerGraph::from_bytes(&original.to_bytes()).unwrap();
        for origin in [1, 2, 3] {
            original.apply_revocation(hash(origin));
            restored.apply_revocation(hash(origin));
            assert_eq!(original.to_bytes(), restored.to_bytes());
            restored = MergerGraph::from_bytes(&restored.to_bytes()).unwrap();
        }
        for origin in [2, 1, 3] {
            original.apply_unrevocation(&hash(origin));
            restored.apply_unrevocation(&hash(origin));
            assert_eq!(original.to_bytes(), restored.to_bytes());
            restored = MergerGraph::from_bytes(&restored.to_bytes()).unwrap();
        }
    }

    #[test]
    fn missing_extra_and_wrong_multiplicity_edges_are_rejected() {
        for mutation in 0..8 {
            let mut graph = fixture();
            match mutation {
                0 => {
                    graph.origin_to_merges.remove(&hash(1));
                }
                1 => {
                    graph.origin_to_merges.get_mut(&hash(1)).unwrap().pop();
                }
                2 => {
                    graph
                        .origin_to_merges
                        .get_mut(&hash(1))
                        .unwrap()
                        .push(hash(20));
                }
                3 => {
                    graph.merge_to_downstream.remove(&hash(10));
                }
                4 => {
                    graph.merge_to_downstream.get_mut(&hash(10)).unwrap().pop();
                }
                5 => {
                    graph
                        .merge_to_downstream
                        .get_mut(&hash(10))
                        .unwrap()
                        .push(hash(99));
                }
                6 => {
                    graph.origin_to_merges.insert(hash(99), vec![]);
                }
                _ => {
                    graph.merge_to_downstream.insert(hash(99), vec![]);
                }
            }
            assert!(
                MergerGraph::from_bytes(&graph.to_bytes()).is_err(),
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn malformed_nodes_and_revocation_totals_are_rejected() {
        for mutation in 0..9 {
            let mut graph = fixture();
            let node = graph.merge_nodes.get_mut(&hash(10)).unwrap();
            match mutation {
                0 => node.merge_tx = hash(99),
                1 => node.total_amount += 1,
                2 => node.source_origins[0].amount = u128::MAX,
                3 => node.source_origins[0].amount = 0,
                4 => node.source_origins[0].origin = TxHash::ZERO,
                5 => {
                    node.revoked_contribs.insert(hash(1), 10);
                }
                6 => {
                    node.revoked_contribs.insert(hash(1), 101);
                    graph.revoked_origins.insert(hash(1));
                }
                7 => {
                    node.revoked_contribs.insert(hash(1), 0);
                    graph.revoked_origins.insert(hash(1));
                }
                _ => {
                    graph.revoked_origins.insert(TxHash::ZERO);
                }
            }
            assert!(
                MergerGraph::from_bytes(&graph.to_bytes()).is_err(),
                "mutation {mutation}"
            );
        }
    }

    #[test]
    fn cycles_are_rejected_even_with_matching_reverse_indexes() {
        let mut graph = MergerGraph::new();
        merge(&mut graph, 10, &[(20, 100)]);
        merge(&mut graph, 20, &[(10, 100)]);
        graph.record_downstream(hash(20), hash(10));
        assert_eq!(
            MergerGraph::from_bytes(&graph.to_bytes()).err().as_deref(),
            Some("cyclic merger graph snapshot")
        );
        let mut graph = MergerGraph::new();
        merge(&mut graph, 10, &[(10, 100)]);
        graph.record_downstream(hash(10), hash(10));
        assert_eq!(
            MergerGraph::from_bytes(&graph.to_bytes()).err().as_deref(),
            Some("cyclic merger graph snapshot")
        );
    }

    #[test]
    fn duplicate_serialized_map_keys_are_not_silently_collapsed() {
        let mut graph = MergerGraph::new();
        merge(&mut graph, 10, &[(1, 100)]);
        let mut origins: Vec<_> = graph.origin_to_merges.iter().collect();
        origins.push(origins[0]);
        let mut bytes = SNAPSHOT_MAGIC.to_vec();
        bytes.extend(
            bincode::serialize(&(
                origins,
                &graph.merge_nodes,
                &graph.merge_to_downstream,
                &graph.revoked_origins,
                &graph.revisions,
                &graph.current_revisions,
                &graph.source_revisions,
            ))
            .unwrap(),
        );
        assert_eq!(
            MergerGraph::from_bytes(&bytes).err().as_deref(),
            Some("noncanonical merger graph snapshot")
        );
    }

    #[test]
    fn deep_graph_restoration_uses_iterative_traversal() {
        let mut graph = MergerGraph::new();
        let id = |n: u64| TxHash::new(burst_crypto::blake2b_256(&n.to_be_bytes()));
        for n in 1..=10_000 {
            graph.record_merge(MergeNode {
                merge_tx: id(n),
                source_origins: vec![MergeSource {
                    origin: id(n - 1),
                    amount: 1,
                }],
                total_amount: 1,
                revoked_contribs: HashMap::new(),
            });
        }
        let bytes = graph.to_bytes();
        assert_eq!(MergerGraph::from_bytes(&bytes).unwrap().to_bytes(), bytes);
    }
}
