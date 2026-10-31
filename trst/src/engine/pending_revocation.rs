use super::{HashMap, MergerGraph, PendingRevocation, TrstBasis, TxHash};

/// Lookup tables over a borrowed sequence of pending revocation effects.
/// Positions preserve the caller's order, including repeated origins; building
/// the index visits every effect and its affected merge origins.
pub struct PendingRevocationIndex<'a> {
    effects: &'a [PendingRevocation],
    affected: HashMap<TxHash, Vec<usize>>,
    revoked: HashMap<TxHash, Vec<usize>>,
}

impl<'a> PendingRevocationIndex<'a> {
    pub fn new(effects: &'a [PendingRevocation]) -> Self {
        let mut affected: HashMap<TxHash, Vec<usize>> = HashMap::new();
        let mut revoked: HashMap<TxHash, Vec<usize>> = HashMap::new();
        for (position, effect) in effects.iter().enumerate() {
            revoked.entry(effect.origin).or_default().push(position);
            for origin in effect.affected_origins() {
                affected.entry(origin).or_default().push(position);
            }
        }
        Self {
            effects,
            affected,
            revoked,
        }
    }

    /// Distinct affected origins in unspecified order.
    pub fn affected_origins(&self) -> impl Iterator<Item = TxHash> + '_ {
        self.affected.keys().copied()
    }
}

/// A pending-payment transition with append-only remainder history. The exclusive
/// graph borrow prevents rewinds or replacement while revision answers are cached.
pub struct PendingRevocationBatch<'a> {
    graph: &'a mut MergerGraph,
    // None records the first remainder query without retaining its history.
    answers: HashMap<TxHash, Option<HashMap<TxHash, bool>>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merger_graph::{MergeNode, MergeSource};

    fn fixture(history: usize) -> (MergerGraph, Vec<TrstBasis>, Vec<PendingRevocation>) {
        let merge = TxHash::new([9; 32]);
        let first = TxHash::new([1; 32]);
        let second = TxHash::new([2; 32]);
        let third = TxHash::new([3; 32]);
        let mut graph = MergerGraph::new();
        graph.record_merge(MergeNode {
            merge_tx: merge,
            total_amount: 100,
            source_origins: vec![
                MergeSource {
                    origin: first,
                    amount: 40,
                },
                MergeSource {
                    origin: second,
                    amount: 30,
                },
                MergeSource {
                    origin: third,
                    amount: 30,
                },
            ],
            revoked_contribs: HashMap::new(),
        });
        let mut bases = vec![
            TrstBasis::Entire,
            TrstBasis::Single(first),
            TrstBasis::Single(second),
        ];
        for _ in 0..history / 2 {
            graph.apply_revocation(first);
            bases.push(graph.current_basis(&merge));
            graph.apply_restoration(&first);
            bases.push(graph.current_basis(&merge));
        }
        let effects = [second, third]
            .into_iter()
            .map(|origin| {
                let taints = graph
                    .apply_revocation(origin)
                    .into_iter()
                    .map(|event| (event.merge_tx, (event.tainted_amount, event.total_amount)))
                    .collect();
                PendingRevocation { origin, taints }
            })
            .collect();
        graph.validate_persisted().unwrap();
        (graph, bases, effects)
    }

    fn snapshot(graph: &MergerGraph) -> Vec<u8> {
        let mut bytes = Vec::new();
        graph.write_snapshot(&mut bytes).unwrap();
        bytes
    }

    fn unrelated(n: u64) -> PendingRevocation {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&n.to_be_bytes());
        PendingRevocation {
            origin: TxHash::new(bytes),
            taints: HashMap::new(),
        }
    }

    #[test]
    fn indexed_pending_effects_preserve_order_chunks_and_unrelated_basis_validation() {
        let (graph, bases, mut effects) = fixture(64);
        effects.insert(0, unrelated(100));
        effects.insert(2, unrelated(101));
        effects.push(PendingRevocation {
            origin: effects[1].origin,
            taints: effects[1].taints.clone(),
        });
        let index = PendingRevocationIndex::new(&effects);
        let merge = TxHash::new([9; 32]);
        let chunk_origin = effects[2].origin;
        let mut direct = graph.clone();
        let mut indexed = graph.clone();
        {
            let mut batch = PendingRevocationBatch::new(&mut indexed);
            for basis in &bases {
                for amount in [0, 1, 7, 100, u128::MAX] {
                    let (mut a, mut b) = (*basis, *basis);
                    let (mut clean_a, mut clean_b) = (amount, amount);
                    let (mut restored_a, mut restored_b) = (true, true);
                    let (mut chunk_cuts_a, mut chunk_cuts_b) = (0, 0);
                    let selected = batch
                        .select_effects(&index, &merge, b, amount, std::iter::once(chunk_origin))
                        .unwrap();
                    assert!(!selected.iter().any(|e| e.origin == effects[0].origin));
                    for effect in &effects {
                        if effect.origin == chunk_origin && restored_a {
                            restored_a = false;
                            chunk_cuts_a += 3;
                        }
                        clean_a -= effect
                            .cut_basis(&mut direct, &merge, &mut a, clean_a)
                            .unwrap();
                    }
                    for effect in selected {
                        if effect.origin == chunk_origin && restored_b {
                            restored_b = false;
                            chunk_cuts_b += 3;
                        }
                        clean_b -= batch.cut_basis(effect, &merge, &mut b, clean_b).unwrap();
                    }
                    assert_eq!(
                        (a, clean_a, restored_a, chunk_cuts_a),
                        (b, clean_b, restored_b, chunk_cuts_b)
                    );
                }
            }
            let only_unrelated = [unrelated(900)];
            let unrelated_index = PendingRevocationIndex::new(&only_unrelated);
            assert!(batch
                .select_effects(
                    &unrelated_index,
                    &merge,
                    TrstBasis::Remainder(TxHash::ZERO),
                    1,
                    std::iter::empty()
                )
                .is_err());
            assert!(batch
                .select_effects(
                    &unrelated_index,
                    &merge,
                    TrstBasis::Remainder(TxHash::ZERO),
                    0,
                    std::iter::empty()
                )
                .unwrap()
                .is_empty());
            assert!(batch
                .select_effects(
                    &PendingRevocationIndex::new(&[]),
                    &merge,
                    TrstBasis::Single(TxHash::ZERO),
                    1,
                    std::iter::empty()
                )
                .unwrap()
                .is_empty());
            let selected = batch
                .select_effects(
                    &index,
                    &merge,
                    TrstBasis::Single(effects[1].origin),
                    10,
                    std::iter::empty(),
                )
                .unwrap();
            assert_eq!(selected.len(), 2);
            assert!(std::ptr::eq(selected[0], &effects[1]));
            assert!(std::ptr::eq(selected[1], &effects[4]));
        }
        assert_eq!(snapshot(&direct), snapshot(&indexed));
        indexed.validate_persisted().unwrap();
    }

    #[test]
    #[ignore = "explicit capped-host sparse pending effect selection scaling"]
    fn pending_effect_selection_scaling() {
        use std::time::Instant;
        let merge = TxHash::new([9; 32]);
        for count in [10, 100, 1_000, 10_000] {
            let (graph, _, mut effects) = fixture(0);
            effects.extend((2..count).map(|n| unrelated(n as u64 + 100)));
            for sample in 0..3 {
                let mut direct = graph.clone();
                let mut indexed = graph.clone();
                let start = Instant::now();
                let expected: Vec<_> = (0..100)
                    .map(|_| {
                        let mut basis = TrstBasis::Entire;
                        let mut clean = 10;
                        for effect in &effects {
                            clean -= effect
                                .cut_basis(&mut direct, &merge, &mut basis, clean)
                                .unwrap();
                        }
                        (basis, clean)
                    })
                    .collect();
                let direct_ns = start.elapsed().as_nanos();
                let start = Instant::now();
                let index = PendingRevocationIndex::new(&effects);
                let build_ns = start.elapsed().as_nanos();
                let start = Instant::now();
                let mut batch = PendingRevocationBatch::new(&mut indexed);
                let actual: Vec<_> = (0..100)
                    .map(|_| {
                        let mut basis = TrstBasis::Entire;
                        let mut clean = 10;
                        let selected = batch
                            .select_effects(&index, &merge, basis, clean, std::iter::empty())
                            .unwrap();
                        assert_eq!(selected.len(), 2);
                        for effect in selected {
                            clean -= batch.cut_basis(effect, &merge, &mut basis, clean).unwrap();
                        }
                        (basis, clean)
                    })
                    .collect();
                let indexed_ns = start.elapsed().as_nanos();
                drop(batch);
                assert_eq!(actual, expected);
                assert!(actual.iter().all(|(_, clean)| *clean == 4));
                assert_eq!(snapshot(&direct), snapshot(&indexed));
                indexed.validate_persisted().unwrap();
                println!("pending_effects effects={count} portions=100 sample={sample} direct_ns={direct_ns} build_ns={build_ns} indexed_ns={indexed_ns}");
            }
        }
    }

    #[test]
    fn pending_batch_defers_history_memo_until_repeated_remainder_query() {
        let (mut graph, bases, effects) = fixture(64);
        let merge = TxHash::new([9; 32]);
        let mut direct = graph.clone();
        let mut batch = PendingRevocationBatch::new(&mut graph);
        for initial in [TrstBasis::Entire, TrstBasis::Single(effects[0].origin)] {
            let mut basis = initial;
            batch
                .cut_basis(&effects[0], &merge, &mut basis, 10)
                .unwrap();
        }
        assert!(batch.answers.is_empty());
        for query in 0..2 {
            let (mut a, mut b) = (*bases.last().unwrap(), *bases.last().unwrap());
            let (mut clean_a, mut clean_b) = (10, 10);
            for effect in &effects {
                clean_a -= effect
                    .cut_basis(&mut direct, &merge, &mut a, clean_a)
                    .unwrap();
                clean_b -= batch.cut_basis(effect, &merge, &mut b, clean_b).unwrap();
                let answers = batch.answers.get(&effect.origin).unwrap();
                assert_eq!(answers.is_some(), query > 0);
                if let Some(answers) = answers {
                    assert!(!answers.is_empty());
                }
            }
            assert_eq!((a, clean_a), (b, clean_b));
        }
    }

    #[test]
    fn pending_batch_preserves_sequential_cuts_rounding_errors_and_graph_history() {
        let (graph, bases, effects) = fixture(64);
        let merge = TxHash::new([9; 32]);
        let mut direct = graph.clone();
        let mut cached = graph.clone();
        {
            let mut batch = PendingRevocationBatch::new(&mut cached);
            for basis in bases.iter().rev().chain(bases.iter()) {
                for amount in [0, 1, 7, 99, 100, u128::MAX] {
                    let (mut a, mut b) = (*basis, *basis);
                    let (mut clean_a, mut clean_b) = (amount, amount);
                    for effect in effects.iter().chain(effects.iter().rev()) {
                        let cut_a = effect
                            .cut_basis(&mut direct, &merge, &mut a, clean_a)
                            .unwrap();
                        let cut_b = batch.cut_basis(effect, &merge, &mut b, clean_b).unwrap();
                        assert_eq!((cut_a, a), (cut_b, b));
                        clean_a -= cut_a;
                        clean_b -= cut_b;
                    }
                    assert_eq!(clean_a, clean_b);
                }
            }
            for invalid in [
                TrstBasis::Single(TxHash::ZERO),
                TrstBasis::Remainder(TxHash::ZERO),
            ] {
                for amount in [0, 1] {
                    let (mut a, mut b) = (invalid, invalid);
                    assert_eq!(
                        effects[0].cut_basis(&mut direct, &merge, &mut a, amount),
                        batch.cut_basis(&effects[0], &merge, &mut b, amount)
                    );
                    assert_eq!(a, b);
                }
            }
            assert_eq!(batch.answers.len(), 2);
        }
        assert_eq!(snapshot(&direct), snapshot(&cached));
        direct.validate_persisted().unwrap();
        cached.validate_persisted().unwrap();
        // A new batch must not retain answers across a rewind to a different cut.
        cached = graph;
        let mut batch = PendingRevocationBatch::new(&mut cached);
        assert!(batch.answers.is_empty());
        let mut basis = bases[3];
        assert_eq!(
            batch.cut_basis(&effects[0], &merge, &mut basis, 7).unwrap(),
            4
        );
    }

    #[test]
    #[ignore = "explicit capped-host sequential pending history scaling"]
    fn pending_batch_history_scaling() {
        use std::time::Instant;
        let merge = TxHash::new([9; 32]);
        for history in [100, 1_000, 10_000] {
            let (graph, bases, effects) = fixture(history);
            let basis = *bases.last().unwrap();
            for portions in [1, 100, 1_000] {
                for sample in 0..3 {
                    let mut direct = graph.clone();
                    let mut cached = graph.clone();
                    let start = Instant::now();
                    let expected: Vec<_> = (0..portions)
                        .map(|_| {
                            let mut basis = basis;
                            let mut clean = 10;
                            for effect in &effects {
                                clean -= effect
                                    .cut_basis(&mut direct, &merge, &mut basis, clean)
                                    .unwrap();
                            }
                            (basis, clean)
                        })
                        .collect();
                    let direct_ns = start.elapsed().as_nanos();
                    let start = Instant::now();
                    let mut batch = PendingRevocationBatch::new(&mut cached);
                    let actual: Vec<_> = (0..portions)
                        .map(|_| {
                            let mut basis = basis;
                            let mut clean = 10;
                            for effect in &effects {
                                clean -=
                                    batch.cut_basis(effect, &merge, &mut basis, clean).unwrap();
                            }
                            (basis, clean)
                        })
                        .collect();
                    let cached_ns = start.elapsed().as_nanos();
                    let memo_entries: usize = batch
                        .answers
                        .values()
                        .filter_map(Option::as_ref)
                        .map(HashMap::len)
                        .sum();
                    if portions == 1 {
                        assert_eq!(memo_entries, 0);
                    }
                    assert!(memo_entries <= history * 2 + 2);
                    drop(batch);
                    assert_eq!(expected, actual);
                    assert!(actual.iter().all(|(_, clean)| *clean == 4));
                    assert_eq!(snapshot(&direct), snapshot(&cached));
                    cached.validate_persisted().unwrap();
                    println!("pending_history history={history} portions={portions} effects=2 sample={sample} direct_ns={direct_ns} cached_ns={cached_ns} memo_entries={memo_entries}");
                }
            }
        }
    }
}

impl<'a> PendingRevocationBatch<'a> {
    pub fn new(graph: &'a mut MergerGraph) -> Self {
        Self {
            graph,
            answers: HashMap::new(),
        }
    }

    /// Select effects for the clean portion and any restored chunks, preserving
    /// input order. Deduplication removes repeated positions, not repeated events.
    /// The composition basis is checked only for a nonzero clean amount and a
    /// nonempty effect list. Selection does not apply cuts or settle the payment.
    pub fn select_effects<'e>(
        &self,
        index: &PendingRevocationIndex<'e>,
        origin: &TxHash,
        basis: TrstBasis,
        clean: u128,
        chunk_origins: impl Iterator<Item = TxHash>,
    ) -> Result<Vec<&'e PendingRevocation>, String> {
        if clean > 0 && !index.effects.is_empty() && !self.graph.valid_basis(origin, basis) {
            return Err("invalid pending composition basis".into());
        }
        let mut positions = Vec::new();
        if clean > 0 {
            let found = match basis {
                TrstBasis::Single(single) => index.revoked.get(&single),
                TrstBasis::Entire | TrstBasis::Remainder(_) => index.affected.get(origin),
            };
            if let Some(found) = found {
                positions.extend_from_slice(found);
            }
        }
        for origin in chunk_origins {
            if let Some(found) = index.revoked.get(&origin) {
                positions.extend_from_slice(found);
            }
        }
        positions.sort_unstable();
        positions.dedup();
        Ok(positions
            .into_iter()
            .map(|position| &index.effects[position])
            .collect())
    }

    /// Return the revoked amount and update the basis after a partial cut.
    /// The caller subtracts the result from the remaining payment amount. Revision
    /// answers are cached only after a second remainder query for a revoked origin;
    /// the cache belongs to this batch and must not survive a graph rewind.
    pub fn cut_basis(
        &mut self,
        effect: &PendingRevocation,
        origin: &TxHash,
        basis: &mut TrstBasis,
        amount: u128,
    ) -> Result<u128, String> {
        effect.cut_basis_using(
            self.graph,
            origin,
            basis,
            amount,
            |graph, origin, basis, amount, revoked, tainted| {
                if !matches!(basis, TrstBasis::Remainder(_)) {
                    return graph.basis_cut(origin, basis, amount, revoked, tainted);
                }
                match self.answers.entry(*revoked) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(None);
                        graph.basis_cut(origin, basis, amount, revoked, tainted)
                    }
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        let answers = entry.get_mut().get_or_insert_with(HashMap::new);
                        graph.basis_cut_cached(origin, basis, amount, revoked, tainted, answers)
                    }
                }
            },
        )
    }
}
