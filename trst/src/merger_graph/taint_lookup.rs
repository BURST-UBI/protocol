use super::{HashMap, MergerGraph, TrstBasis, TxHash};

/// Answers belong to one immutable graph cut and one revoked origin.
/// The caller may append remainder revisions to a separate graph clone.
#[cfg(test)]
pub(crate) struct BasisCutLookup<'a> {
    graph: &'a MergerGraph,
    revoked: TxHash,
    answers: HashMap<TxHash, bool>,
}

#[cfg(test)]
impl BasisCutLookup<'_> {
    pub(crate) fn cut(&mut self, origin: &TxHash, basis: TrstBasis, amount: u128, tainted: u128) -> u128 {
        let graph = self.graph;
        let revoked = self.revoked;
        let answers = &mut self.answers;
        graph.basis_cut_using(origin, basis, amount, &revoked, tainted,
            |revision| graph.revoked_at_cached(revision, &revoked, answers))
    }
}

impl MergerGraph {
    /// Check all settled origins against one immutable composition basis. The
    /// first revision for each origin is authoritative; older reversals cannot
    /// change it. Walk the revision chain once for the requested set of origins.
    /// Requires an already validated, acyclic revision chain: missing records
    /// panic, and this lookup does not check that the basis belongs to a merge.
    pub(crate) fn basis_excludes_all(
        &self, basis: TrstBasis, origins: impl IntoIterator<Item = TxHash>,
    ) -> bool {
        let mut origins = origins.into_iter();
        match basis {
            TrstBasis::Entire => origins.next().is_none(),
            TrstBasis::Single(single) => origins.all(|origin| origin != single),
            TrstBasis::Remainder(mut revision) => {
                let mut unresolved: std::collections::HashSet<_> = origins.collect();
                while !unresolved.is_empty() && revision != TxHash::ZERO {
                    #[cfg(test)]
                    super::REVISION_VISITS.with(|probe| {
                        if let Some(count) = probe.get() { probe.set(Some(count + 1)); }
                    });
                    let state = &self.revisions[&revision];
                    if unresolved.remove(&state.origin) && !state.revoked {
                        return false;
                    }
                    revision = state.previous;
                }
                unresolved.is_empty()
            }
        }
    }

    pub(crate) fn basis_cut_cached(
        &self, origin: &TxHash, basis: TrstBasis, amount: u128,
        revoked: &TxHash, tainted: u128, answers: &mut HashMap<TxHash, bool>,
    ) -> u128 {
        self.basis_cut_using(origin, basis, amount, revoked, tainted,
            |revision| self.revoked_at_cached(revision, revoked, answers))
    }

    #[cfg(test)]
    pub(crate) fn basis_cut_lookup(&self, revoked: TxHash) -> BasisCutLookup<'_> {
        BasisCutLookup { graph: self, revoked, answers: HashMap::new() }
    }
}

/// Expand sparse source positions. The first entry wins for duplicates; entries
/// outside the requested count are ignored. This is a lookup, not validation.
pub(super) fn source_bases(
    count: usize,
    revisions: &[(usize, TrstBasis)],
) -> Vec<Option<TrstBasis>> {
    let mut bases = vec![None; count];
    for &(position, basis) in revisions {
        if let Some(slot) = bases.get_mut(position) {
            slot.get_or_insert(basis);
        }
    }
    bases
}

impl MergerGraph {
    /// Cache answers for one origin against immutable revision records. Reusing
    /// this cache for another origin or a rewound graph gives invalid answers.
    /// An uncached query follows predecessors until the origin, a cached answer,
    /// or the chain end. Missing records panic; callers validate the chain first.
    pub(super) fn revoked_at_cached(
        &self,
        mut revision: TxHash,
        origin: &TxHash,
        cache: &mut HashMap<TxHash, bool>,
    ) -> bool {
        let mut path = Vec::new();
        let revoked = loop {
            if revision == TxHash::ZERO {
                break false;
            }
            if let Some(&known) = cache.get(&revision) {
                break known;
            }
            #[cfg(test)]
            super::REVISION_VISITS.with(|probe| {
                if let Some(count) = probe.get() { probe.set(Some(count + 1)); }
            });
            path.push(revision);
            let state = &self.revisions[&revision];
            if state.origin == *origin {
                break state.revoked;
            }
            revision = state.previous;
        };
        for revision in path {
            cache.insert(revision, revoked);
        }
        revoked
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merger_graph::RevocationRevision;

    #[test]
    fn indexed_bases_match_first_search_for_sparse_duplicate_and_raw_rows() {
        let rows = [
            (255, TrstBasis::Single(TxHash::new([1; 32]))),
            (0, TrstBasis::Entire),
            (0, TrstBasis::Single(TxHash::new([2; 32]))),
            (1, TrstBasis::Remainder(TxHash::new([3; 32]))),
            (usize::MAX, TrstBasis::Entire),
        ];
        for count in [0, 1, 2, 255, 256, 4096] {
            let indexed = source_bases(count, &rows);
            for (position, basis) in indexed.into_iter().enumerate() {
                assert_eq!(
                    basis,
                    rows.iter()
                        .find(|(index, _)| *index == position)
                        .map(|(_, basis)| *basis)
                );
            }
        }
    }

    #[test]
    fn revision_memo_matches_history_and_reuses_shared_suffixes() {
        let mut graph = MergerGraph::new();
        let mut previous = TxHash::ZERO;
        let mut history = vec![previous];
        for n in 0..256 {
            let state = RevocationRevision {
                merge_tx: TxHash::new([9; 32]),
                previous,
                origin: TxHash::new([if n % 17 == 0 { 1 } else { 2 }; 32]),
                revoked: n % 2 == 0,
                remaining: 100,
            };
            previous = state.hash();
            graph.revisions.insert(previous, state);
            history.push(previous);
        }
        for origin in [
            TxHash::new([1; 32]),
            TxHash::new([2; 32]),
            TxHash::new([3; 32]),
        ] {
            let mut cache = HashMap::new();
            for revision in history.iter().rev() {
                assert_eq!(
                    graph.revoked_at_cached(*revision, &origin, &mut cache),
                    graph.revoked_at(*revision, &origin)
                );
            }
            assert_eq!(cache.len(), 256);
            let before = cache.clone();
            for revision in &history {
                assert_eq!(
                    graph.revoked_at_cached(*revision, &origin, &mut cache),
                    graph.revoked_at(*revision, &origin)
                );
            }
            assert_eq!(cache, before);
        }
    }

    #[test]
    fn settled_origin_checks_match_individual_history_queries_with_one_walk() {
        let mut graph = MergerGraph::new();
        let hash = |n: u64| {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&n.to_be_bytes());
            TxHash::new(bytes)
        };
        let mut previous = TxHash::ZERO;
        for n in 0..1024 {
            let state = RevocationRevision {
                merge_tx: hash(2000), previous, origin: hash(n % 256 + 1),
                revoked: n >= 768, remaining: 100,
            };
            previous = state.hash();
            graph.revisions.insert(previous, state);
        }
        let basis = TrstBasis::Remainder(previous);
        let origins: Vec<_> = (1..=256).map(hash).collect();
        super::super::REVISION_VISITS.with(|probe| probe.set(Some(0)));
        assert!(graph.basis_excludes_all(basis, origins.iter().copied()));
        let visits = super::super::REVISION_VISITS.with(|probe| probe.replace(None).unwrap());
        assert_eq!(visits, 256, "stop when every origin has its latest answer");
        for basis in [basis, TrstBasis::Remainder(TxHash::ZERO),
            TrstBasis::Entire, TrstBasis::Single(hash(1))] {
            for queries in [vec![], origins.clone(), vec![hash(1), hash(1)],
                vec![hash(4000)], vec![hash(1), hash(4000)]] {
                assert_eq!(graph.basis_excludes_all(basis, queries.iter().copied()),
                    queries.iter().all(|origin| graph.basis_excludes(basis, origin)));
            }
        }
        // A newer restoration takes precedence over all older revocations.
        let state = RevocationRevision {
            merge_tx: hash(2000), previous, origin: hash(1), revoked: false, remaining: 100,
        };
        previous = state.hash();
        graph.revisions.insert(previous, state);
        let basis = TrstBasis::Remainder(previous);
        assert!(!graph.basis_excludes_all(basis, origins.iter().copied()));
        assert!(graph.basis_excludes_all(basis, origins.iter().copied().skip(1)));
    }

    #[test]
    fn batch_cuts_match_uncached_rounding_and_keep_their_graph_cut() {
        use crate::merger_graph::{MergeNode, MergeSource};
        let first = TxHash::new([1; 32]);
        let second = TxHash::new([2; 32]);
        let merge = TxHash::new([9; 32]);
        let mut graph = MergerGraph::new();
        graph.record_merge(MergeNode {
            merge_tx: merge, total_amount: 100,
            source_origins: vec![MergeSource { origin: first, amount: 40 }, MergeSource { origin: second, amount: 60 }],
            revoked_contribs: HashMap::new(),
        });
        let mut bases = vec![TrstBasis::Entire, TrstBasis::Single(first), TrstBasis::Single(second)];
        for _ in 0..64 {
            graph.apply_revocation(first);
            bases.push(graph.current_basis(&merge));
            graph.apply_restoration(&first);
            bases.push(graph.current_basis(&merge));
        }
        graph.validate_persisted().unwrap();
        for revoked in [first, second, TxHash::new([3; 32])] {
            let mut lookup = graph.basis_cut_lookup(revoked);
            let mut changed = graph.clone();
            for basis in bases.iter().rev().chain(bases.iter()) {
                for amount in [0, 1, 7, 100, u128::MAX] {
                    for tainted in [0, 1, 40, 60, 100] {
                        assert_eq!(lookup.cut(&merge, *basis, amount, tainted),
                            graph.basis_cut(&merge, *basis, amount, &revoked, tainted));
                    }
                }
                // Appending generated remainders cannot change the lookup's prior cut.
                changed.reduced_basis(&merge, *basis, revoked, 1);
            }
            assert!(lookup.answers.len() <= graph.revisions.len());
            assert_eq!(graph.revisions.len(), 128);
        }
    }

    #[test]
    #[ignore = "explicit capped-host shared revision lookup scaling, not full revocation"]
    fn shared_remainder_history_lookup_scaling() {
        use crate::merger_graph::{MergeNode, MergeSource};
        use std::time::Instant;
        let first = TxHash::new([1; 32]);
        let target = TxHash::new([2; 32]);
        let merge = TxHash::new([9; 32]);
        for history in [100usize, 1_000, 10_000] {
            let mut graph = MergerGraph::new();
            graph.record_merge(MergeNode {
                merge_tx: merge, total_amount: 100,
                source_origins: vec![
                    MergeSource { origin: first, amount: 40 },
                    MergeSource { origin: target, amount: 60 },
                ],
                revoked_contribs: HashMap::new(),
            });
            for _ in 0..history / 2 {
                assert_eq!(graph.apply_revocation(first).len(), 1);
                assert_eq!(graph.apply_restoration(&first).len(), 1);
            }
            graph.validate_persisted().unwrap();
            assert_eq!(graph.revisions.len(), history);
            let basis = graph.current_basis(&merge);
            assert!(matches!(basis, TrstBasis::Remainder(_)));
            for queries in [1usize, 100, 1_000] {
                for sample in 0..3 {
                    let start = Instant::now();
                    let direct: u128 = (0..queries).map(|_| {
                        std::hint::black_box(&graph).basis_cut(&merge, basis, 10, &target, 60)
                    }).sum();
                    let direct_ns = start.elapsed().as_nanos();
                    let mut lookup = graph.basis_cut_lookup(target);
                    let start = Instant::now();
                    let cached: u128 = (0..queries).map(|_| {
                        lookup.cut(&merge, basis, 10, 60)
                    }).sum();
                    let cached_ns = start.elapsed().as_nanos();
                    assert_eq!(direct, 6 * queries as u128);
                    assert_eq!(cached, direct);
                    assert_eq!(lookup.answers.len(), history);
                    println!("remainder_history history={history} queries={queries} sample={sample} direct_ns={direct_ns} cached_ns={cached_ns} memo_entries={}", lookup.answers.len());
                }
            }
        }
    }

}
