use super::*;

#[test]
fn structural_equality_covers_every_persisted_graph_field() {
    let mut graph = MergerGraph::new();
    graph.record_merge(node(20, &[(1, 60), (2, 40)]));
    graph.apply_revocation(tx(1));
    graph.record_merge(node(30, &[(20, 40)]));
    graph.apply_unrevocation(&tx(1));
    let bytes = graph.to_bytes();
    let decoded = MergerGraph::from_bytes(&bytes).unwrap();
    assert!(!decoded.shares_storage_with(&graph));
    assert!(decoded == graph);
    for field in 0..7 {
        let mut changed = graph.clone();
        match field {
            0 => {
                changed.origin_to_merges.insert(tx(99), vec![tx(20)]);
            }
            1 => {
                changed.merge_nodes.remove(&tx(20));
            }
            2 => {
                changed.merge_to_downstream.insert(tx(99), vec![tx(30)]);
            }
            3 => {
                changed.revoked_origins.insert(tx(99));
            }
            4 => {
                changed
                    .revisions
                    .values_mut()
                    .next()
                    .expect("revision fixture")
                    .remaining += 1;
            }
            5 => {
                changed.current_revisions.insert(tx(99), tx(30));
            }
            _ => {
                changed
                    .source_revisions
                    .insert(tx(99), vec![(0, TrstBasis::Entire)]);
            }
        }
        assert!(changed != graph, "field {field}");
        assert_ne!(changed.to_bytes(), bytes, "field {field}");
    }
}

fn tx(byte: u8) -> TxHash {
    TxHash::new([byte; 32])
}

fn node(id: u8, sources: &[(u8, u128)]) -> MergeNode {
    MergeNode {
        merge_tx: tx(id),
        source_origins: sources
            .iter()
            .map(|&(origin, amount)| MergeSource {
                origin: tx(origin),
                amount,
            })
            .collect(),
        total_amount: sources.iter().map(|(_, amount)| amount).sum(),
        revoked_contribs: HashMap::new(),
    }
}

#[test]
fn candidates_share_reads_but_merge_and_revocation_writes_are_isolated() {
    let mut live = MergerGraph::new();
    live.record_merge(node(20, &[(1, 60), (2, 40)]));
    let before = live.to_bytes();
    let mut candidate = live.clone();
    assert!(candidate.shares_storage_with(&live));
    assert!(candidate.get_merge(&tx(20)).is_some());
    assert!(candidate.shares_storage_with(&live));
    candidate.record_merge(node(30, &[(20, 100)]));
    assert!(!candidate.shares_storage_with(&live));
    assert!(candidate
        .revoked_origins
        .shares_storage_with(&live.revoked_origins));
    assert_eq!(live.to_bytes(), before);

    candidate.apply_revocation(tx(1));
    assert!(candidate.is_origin_revoked(&tx(1)));
    assert!(!live.is_origin_revoked(&tx(1)));
    assert_eq!(live.to_bytes(), before);
    let revoked = candidate.to_bytes();
    let mut restored = candidate.clone();
    restored.apply_unrevocation(&tx(1));
    assert!(!restored.is_origin_revoked(&tx(1)));
    assert_eq!(candidate.to_bytes(), revoked);
    assert_eq!(live.to_bytes(), before);

    live.apply_revocation(tx(2));
    assert!(!candidate.is_origin_revoked(&tx(2)));
    assert!(!restored.is_origin_revoked(&tx(2)));
    assert!(!candidate.shares_storage_with(&restored));
}

#[test]
fn shared_storage_preserves_the_existing_canonical_snapshot_bytes() {
    #[derive(Serialize)]
    struct LegacyGraph<'a> {
        #[serde(serialize_with = "sorted_map")]
        origin_to_merges: &'a HashMap<TxHash, Vec<TxHash>>,
        #[serde(serialize_with = "sorted_map")]
        merge_nodes: &'a HashMap<TxHash, MergeNode>,
        #[serde(serialize_with = "sorted_map")]
        merge_to_downstream: &'a HashMap<TxHash, Vec<TxHash>>,
        #[serde(serialize_with = "sorted_set")]
        revoked_origins: &'a HashSet<TxHash>,
        #[serde(serialize_with = "sorted_map")]
        revisions: &'a HashMap<TxHash, RevocationRevision>,
        #[serde(serialize_with = "sorted_map")]
        current_revisions: &'a HashMap<TxHash, TxHash>,
        #[serde(serialize_with = "sorted_map")]
        source_revisions: &'a HashMap<TxHash, Vec<(usize, TrstBasis)>>,
    }
    let mut graph = MergerGraph::new();
    graph.record_merge(node(20, &[(1, 60), (2, 40)]));
    graph.apply_revocation(tx(1));
    graph.record_merge(node(30, &[(20, 40)]));
    graph.apply_unrevocation(&tx(1));
    let legacy = LegacyGraph {
        origin_to_merges: &graph.origin_to_merges,
        merge_nodes: &graph.merge_nodes,
        merge_to_downstream: &graph.merge_to_downstream,
        revoked_origins: &graph.revoked_origins,
        revisions: &graph.revisions,
        current_revisions: &graph.current_revisions,
        source_revisions: &graph.source_revisions,
    };
    let mut bytes = SNAPSHOT_MAGIC.to_vec();
    bytes.extend(bincode::serialize(&legacy).unwrap());
    assert_eq!(graph.to_bytes(), bytes);
    let decoded = MergerGraph::from_bytes(&bytes).unwrap();
    assert_eq!(decoded.to_bytes(), bytes);
    assert!(!decoded.shares_storage_with(&graph));
    assert!(decoded.clone().shares_storage_with(&decoded));
}

#[test]
#[ignore = "manual snapshot cloning measurement; not transaction throughput"]
fn merger_snapshot_clone_cost() {
    let mut graph = MergerGraph::new();
    for n in 0..10_000u64 {
        let hash = |value: u64| TxHash::new(burst_crypto::blake2b_256(&value.to_le_bytes()));
        graph.record_merge(MergeNode {
            merge_tx: hash(2 * n + 1),
            source_origins: vec![MergeSource {
                origin: hash(2 * n),
                amount: 100,
            }],
            total_amount: 100,
            revoked_contribs: Default::default(),
        });
    }
    let start = std::time::Instant::now();
    for _ in 0..100 {
        // The seven owned collections copied by the previous Clone derive.
        std::hint::black_box((
            (*graph.origin_to_merges).clone(),
            (*graph.merge_nodes).clone(),
            (*graph.merge_to_downstream).clone(),
            (*graph.revoked_origins).clone(),
            (*graph.revisions).clone(),
            (*graph.current_revisions).clone(),
            (*graph.source_revisions).clone(),
        ));
    }
    eprintln!(
        "10,000 merges, owned indexes: {:?} per clone/drop",
        start.elapsed() / 100
    );
    let start = std::time::Instant::now();
    for _ in 0..100 {
        std::hint::black_box(graph.clone());
    }
    eprintln!(
        "10,000 merges, shared indexes: {:?} per clone/drop",
        start.elapsed() / 100
    );
}
