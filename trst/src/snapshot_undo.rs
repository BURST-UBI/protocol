//! Three-way restoration for disjoint local snapshot entries. No peer input or
//! consensus authorization is implied by a before/after pair.
//! Each helper checks conflicts before changing its destination. A sequence of
//! helper calls is not atomic: stage the combined result before publishing it.
//! These generic comparisons scan snapshot entries, not only changed tree paths.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

pub(crate) trait SnapshotMap<K, V> {
    fn get(&self, key: &K) -> Option<&V>;
    fn keys<'a>(&'a self) -> impl Iterator<Item = &'a K>
    where
        K: 'a;
    fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }
    fn put(&mut self, key: K, value: V);
    fn delete(&mut self, key: &K);
}

impl<K: Eq + Hash, V> SnapshotMap<K, V> for HashMap<K, V> {
    fn get(&self, key: &K) -> Option<&V> {
        self.get(key)
    }
    fn keys<'a>(&'a self) -> impl Iterator<Item = &'a K>
    where
        K: 'a,
    {
        self.keys()
    }
    fn put(&mut self, key: K, value: V) {
        self.insert(key, value);
    }
    fn delete(&mut self, key: &K) {
        self.remove(key);
    }
}

impl<K: Ord + Clone, V: Clone> SnapshotMap<K, V> for crate::SharedMap<K, V> {
    fn get(&self, key: &K) -> Option<&V> {
        self.get(key)
    }
    fn keys<'a>(&'a self) -> impl Iterator<Item = &'a K>
    where
        K: 'a,
    {
        self.keys()
    }
    fn put(&mut self, key: K, value: V) {
        self.insert(key, value);
    }
    fn delete(&mut self, key: &K) {
        self.remove(key);
    }
}

impl<K, V, T: SnapshotMap<K, V> + Clone> SnapshotMap<K, V> for crate::Shared<T> {
    fn get(&self, key: &K) -> Option<&V> {
        (**self).get(key)
    }
    fn keys<'a>(&'a self) -> impl Iterator<Item = &'a K>
    where
        K: 'a,
    {
        (**self).keys()
    }
    fn put(&mut self, key: K, value: V) {
        (**self).put(key, value);
    }
    fn delete(&mut self, key: &K) {
        (**self).delete(key);
    }
}

/// Restore entries changed between before and after only if current still matches
/// after at those keys. Unrelated keys retain their current values. This compares
/// whole values; it does not merge concurrent edits inside one value.
pub(crate) fn rewind_map<K: Clone + Eq + Hash, V: Clone + Eq>(
    before: &impl SnapshotMap<K, V>,
    after: &impl SnapshotMap<K, V>,
    current: &mut impl SnapshotMap<K, V>,
) -> Result<(), String> {
    let changed: HashSet<_> = before
        .keys()
        .chain(after.keys())
        .filter(|key| before.get(*key) != after.get(*key))
        .collect();
    if changed
        .iter()
        .any(|key| current.get(*key) != after.get(*key))
    {
        return Err("snapshot undo conflicts with a changed entry".into());
    }
    for key in changed {
        match before.get(key) {
            Some(value) => {
                current.put(key.clone(), value.clone());
            }
            None => {
                current.delete(key);
            }
        }
    }
    Ok(())
}

pub(crate) trait SnapshotSet<K> {
    fn members<'a>(&'a self) -> impl Iterator<Item = &'a K>
    where
        K: 'a;
    fn contains(&self, key: &K) -> bool;
    fn add(&mut self, key: K);
    fn discard(&mut self, key: &K);
}
impl<K: Eq + Hash> SnapshotSet<K> for HashSet<K> {
    fn members<'a>(&'a self) -> impl Iterator<Item = &'a K>
    where
        K: 'a,
    {
        self.iter()
    }
    fn contains(&self, key: &K) -> bool {
        self.contains(key)
    }
    fn add(&mut self, key: K) {
        self.insert(key);
    }
    fn discard(&mut self, key: &K) {
        self.remove(key);
    }
}
impl<K: Ord + Clone> SnapshotSet<K> for crate::SharedSet<K> {
    fn members<'a>(&'a self) -> impl Iterator<Item = &'a K>
    where
        K: 'a,
    {
        self.iter()
    }
    fn contains(&self, key: &K) -> bool {
        self.contains(key)
    }
    fn add(&mut self, key: K) {
        self.insert(key);
    }
    fn discard(&mut self, key: &K) {
        self.remove(key);
    }
}

/// Undo changed memberships after checking that current agrees with after for
/// every affected member. Memberships untouched by the transition are preserved.
pub(crate) fn rewind_set<K: Clone + Eq + Hash>(
    before: &impl SnapshotSet<K>,
    after: &impl SnapshotSet<K>,
    current: &mut impl SnapshotSet<K>,
) -> Result<(), String> {
    let changed: HashSet<_> = before
        .members()
        .chain(after.members())
        .filter(|key| before.contains(key) != after.contains(key))
        .collect();
    if changed
        .iter()
        .any(|key| current.contains(key) != after.contains(key))
    {
        return Err("snapshot undo conflicts with changed membership".into());
    }
    for key in changed {
        if before.contains(key) {
            current.add(key.clone());
        } else {
            current.discard(key);
        }
    }
    Ok(())
}

/// Undo origin membership per owner, preserving unrelated origins of the same
/// owner. Stage all replacements before changing current. Empty owners that
/// existed in before remain represented; newly created empty owners are removed.
pub(crate) fn rewind_owned_sets<K: Clone + Eq + Hash, V: Clone + Eq + Hash, S>(
    before: &impl SnapshotMap<K, S>,
    after: &impl SnapshotMap<K, S>,
    current: &mut impl SnapshotMap<K, S>,
) -> Result<(), String>
where
    S: SnapshotSet<V> + Clone + Default + PartialEq,
{
    let empty = S::default();
    let mut replacements = Vec::new();
    for key in before.keys().chain(after.keys()).collect::<HashSet<_>>() {
        if before.get(key) == after.get(key) {
            continue;
        }
        let previous = before.get(key).unwrap_or(&empty);
        let next = after.get(key).unwrap_or(&empty);
        let mut live = current.get(key).cloned().unwrap_or_default();
        rewind_set(previous, next, &mut live)?;
        let restored = if live.members().next().is_none() && !before.contains_key(key) {
            None
        } else {
            Some(live)
        };
        replacements.push((key.clone(), restored));
    }
    for (key, restored) in replacements {
        match restored {
            Some(value) => {
                current.put(key, value);
            }
            None => {
                current.delete(&key);
            }
        }
    }
    Ok(())
}

/// Ordered child sequence used by reverse-edge undo. Duplicates are significant.
pub(crate) trait SnapshotSequence<K>: Clone + PartialEq + From<Vec<K>> {
    fn sequence_len(&self) -> usize;
    fn sequence<'a>(&'a self) -> impl Iterator<Item = &'a K>
    where
        K: 'a;
}
impl<K: Clone + PartialEq> SnapshotSequence<K> for Vec<K> {
    fn sequence_len(&self) -> usize {
        self.len()
    }
    fn sequence<'a>(&'a self) -> impl Iterator<Item = &'a K>
    where
        K: 'a,
    {
        self.iter()
    }
}

/// Remove this transition's appended consumers while preserving the order and
/// multiplicity of other current edges. Before must be a prefix of after, and
/// appended consumers must not already occur in that prefix. Their occurrence
/// counts in current must still match the appended counts.
///
/// All replacements are staged before mutation. This does not validate graph
/// references or prove that removing a merge leaves no dependent holdings.
pub(crate) fn rewind_appended_edges<K: Clone + Eq + Hash, V: SnapshotSequence<K>>(
    before: &impl SnapshotMap<K, V>,
    after: &impl SnapshotMap<K, V>,
    current: &mut impl SnapshotMap<K, V>,
) -> Result<(), String> {
    let mut replacements = Vec::new();
    for key in before.keys().chain(after.keys()).collect::<HashSet<_>>() {
        if before.get(key) == after.get(key) {
            continue;
        }
        let previous = before.get(key);
        let previous_len = previous.map_or(0, |v| v.sequence_len());
        let next = after
            .get(key)
            .ok_or("snapshot undo removed reverse edges")?;
        if next.sequence_len() < previous_len
            || !next
                .sequence()
                .take(previous_len)
                .eq(previous.into_iter().flat_map(|v| v.sequence()))
        {
            return Err("snapshot undo changed existing reverse edges".into());
        }
        let mut added = HashMap::<&K, usize>::new();
        for edge in next.sequence().skip(previous_len) {
            if previous.is_some_and(|v| v.sequence().any(|old| old == edge)) {
                return Err("snapshot undo appended an existing consumer".into());
            }
            *added.entry(edge).or_default() += 1;
        }
        let live = current
            .get(key)
            .ok_or("snapshot undo is missing reverse edges")?;
        let mut observed = HashMap::<&K, usize>::new();
        for edge in live.sequence() {
            if added.contains_key(edge) {
                *observed.entry(edge).or_default() += 1;
            }
        }
        if observed != added {
            return Err("snapshot undo conflicts with changed reverse edges".into());
        }
        let remaining: Vec<_> = live
            .sequence()
            .filter(|edge| !added.contains_key(edge))
            .cloned()
            .collect();
        replacements.push((
            key.clone(),
            remaining.is_empty()
                && !before
                    .get(key)
                    .is_some_and(|v| v.sequence().next().is_none()),
            remaining,
        ));
    }
    for (key, remove, edges) in replacements {
        if remove {
            current.delete(&key);
        } else {
            current.put(key, V::from(edges));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MergeNode, MergeSource, MergerGraph, TrstEngine};
    use burst_types::{BlockHash, Timestamp, TxHash, WalletAddress};

    fn wallet(n: u8) -> WalletAddress {
        WalletAddress::new(format!("brst_{n}"))
    }
    fn hash(n: u8) -> TxHash {
        TxHash::new([n; 32])
    }
    fn mint(engine: &mut TrstEngine, n: u8) {
        let token = engine
            .mint(hash(n), wallet(n), 100, wallet(n), Timestamp::new(100))
            .unwrap();
        engine.track_token(token);
    }

    fn restored(before: &TrstEngine, after: &TrstEngine, current: &TrstEngine) -> TrstEngine {
        let wallets = TrstEngine::rewind_wallet_snapshot(
            &before.save_wallets(),
            &after.save_wallets(),
            &current.save_wallets(),
        )
        .unwrap();
        let graph = MergerGraph::rewind_snapshot(
            &before.merger_graph.to_bytes(),
            &after.merger_graph.to_bytes(),
            &current.merger_graph.to_bytes(),
        )
        .unwrap();
        let mut result = TrstEngine::load_wallets(&wallets, u64::MAX).unwrap();
        result.merger_graph = MergerGraph::from_bytes(&graph).unwrap();
        result.validate_bases().unwrap();
        result
    }

    #[test]
    fn wallet_rewind_preserves_unrelated_paid_balances_and_revocation_causes() {
        let mut before = TrstEngine::new();
        mint(&mut before, 1);
        mint(&mut before, 2);
        let mut after = before.clone();
        after.debit_wallet_with_provenance(&wallet(1), &hash(1), 40);
        let mut current = after.clone();
        current.debit_wallet_with_provenance(&wallet(2), &hash(2), 30);
        mint(&mut current, 3);
        current
            .invalidate_outcome_origins(BlockHash::new([9; 32]), &[(wallet(3), hash(3))])
            .unwrap();
        let result = restored(&before, &after, &current);
        assert_eq!(
            result.get_portfolio(&wallet(1)),
            before.get_portfolio(&wallet(1))
        );
        assert_eq!(
            result.get_portfolio(&wallet(2)),
            current.get_portfolio(&wallet(2))
        );
        assert_eq!(
            result.get_portfolio(&wallet(3)),
            current.get_portfolio(&wallet(3))
        );
        assert!(result.merger_graph.is_origin_revoked(&hash(3)));
        let mut expected = before.clone();
        expected.debit_wallet_with_provenance(&wallet(2), &hash(2), 30);
        mint(&mut expected, 3);
        expected
            .invalidate_outcome_origins(BlockHash::new([9; 32]), &[(wallet(3), hash(3))])
            .unwrap();
        assert_eq!(result.save_wallets(), expected.save_wallets());
        assert_eq!(
            result.merger_graph.to_bytes(),
            expected.merger_graph.to_bytes()
        );
    }

    #[test]
    fn wallet_rewind_preserves_disjoint_origins_owned_by_the_same_creator() {
        let add = |engine: &mut TrstEngine, seed: u8| {
            let token = engine
                .mint(
                    hash(seed),
                    wallet(seed),
                    100,
                    wallet(1),
                    Timestamp::new(100),
                )
                .unwrap();
            engine.track_token(token);
        };
        let mut before = TrstEngine::new();
        add(&mut before, 10);
        let mut after = before.clone();
        add(&mut after, 11);
        let mut current = after.clone();
        add(&mut current, 12);
        let mut expected = before.clone();
        add(&mut expected, 12);
        let result = restored(&before, &after, &current);
        assert_eq!(result.save_wallets(), expected.save_wallets());
        assert_eq!(
            result.wallet_origins[&wallet(1)],
            [hash(10), hash(12)].into_iter().collect::<crate::SharedSet<_>>()
        );
        assert!(result.get_portfolio(&wallet(11)).is_none());
    }

    #[test]
    fn owned_set_rewind_handles_owner_creation_and_preserves_later_members() {
        for before in [HashMap::new(), HashMap::from([(1, HashSet::new())])] {
            let after = HashMap::from([(1, HashSet::from([10]))]);
            let mut current = HashMap::from([(1, HashSet::from([10, 20]))]);
            rewind_owned_sets(&before, &after, &mut current).unwrap();
            assert_eq!(current, HashMap::from([(1, HashSet::from([20]))]));
            let mut exact = after.clone();
            rewind_owned_sets(&before, &after, &mut exact).unwrap();
            assert_eq!(exact, before);
        }
    }

    #[test]
    fn owned_set_rewind_rejects_changed_membership_without_partial_updates() {
        let before = HashMap::from([(1, HashSet::from([10])), (2, HashSet::from([30]))]);
        let after = HashMap::from([(1, HashSet::from([20])), (2, HashSet::from([40]))]);
        for live in [HashSet::from([10, 20]), HashSet::from([50]), HashSet::new()] {
            let mut current = HashMap::from([(1, live), (2, HashSet::from([40, 50]))]);
            let unchanged = current.clone();
            assert!(rewind_owned_sets(&before, &after, &mut current).is_err());
            assert_eq!(current, unchanged);
        }
    }

    #[test]
    fn expiry_aware_rewind_restores_provenance_without_restoring_spending_rights() {
        let mut before = TrstEngine::with_expiry(100);
        mint(&mut before, 1);
        mint(&mut before, 2);
        let mut after = before.clone();
        after.debit_wallet_with_provenance(&wallet(1), &hash(1), 40);
        let mut current = after.clone();
        current.debit_wallet_with_provenance(&wallet(2), &hash(2), 30);
        current.flush_all_expired(Timestamp::new(250));
        let unchanged = current.save_wallets();
        assert!(TrstEngine::rewind_wallet_snapshot(
            &before.save_wallets(),
            &after.save_wallets(),
            &unchanged
        )
        .is_err());
        let bytes = TrstEngine::rewind_wallet_snapshot_at(
            &before.save_wallets(),
            &after.save_wallets(),
            &unchanged,
            100,
            Timestamp::new(250),
        )
        .unwrap();
        let restored = TrstEngine::load_wallets(&bytes, 100).unwrap();
        let mut expected = before.clone();
        expected.debit_wallet_with_provenance(&wallet(2), &hash(2), 30);
        expected.set_expiry_period(100, Timestamp::new(250)).unwrap();
        assert_eq!(bytes, expected.save_wallets());
        assert_eq!(
            restored
                .get_portfolio(&wallet(1))
                .unwrap()
                .cached_transferable,
            0
        );
        assert_eq!(
            restored.get_portfolio(&wallet(1)).unwrap().tokens[0].amount,
            100
        );
        assert_eq!(current.save_wallets(), unchanged);
    }

    #[test]
    fn expiry_aware_rewind_obeys_governed_period_without_reviving_revoked_tokens() {
        let mut before = TrstEngine::with_expiry(100);
        mint(&mut before, 1);
        mint(&mut before, 2);
        before.revoke_by_originator(&wallet(2));
        let mut after = before.clone();
        after.debit_wallet_with_provenance(&wallet(1), &hash(1), 40);
        let mut current = after.clone();
        current.flush_all_expired(Timestamp::new(250));
        let original = current.save_wallets();

        // The supplied period also applies to restored holdings.
        for (period, transferable) in [(100, 0), (10_000, 100), (u64::MAX, 100)] {
            let bytes = TrstEngine::rewind_wallet_snapshot_at(
                &before.save_wallets(),
                &after.save_wallets(),
                &original,
                period,
                Timestamp::new(250),
            )
            .unwrap();
            let result = TrstEngine::load_wallets(&bytes, period).unwrap();
            let mut expected = before.clone();
            expected.set_expiry_period(period, Timestamp::new(250)).unwrap();
            assert_eq!(bytes, expected.save_wallets());
            assert_eq!(
                result
                    .get_portfolio(&wallet(1))
                    .unwrap()
                    .cached_transferable,
                transferable
            );
            let revoked = result.get_portfolio(&wallet(2)).unwrap();
            assert_eq!(revoked.cached_transferable, 0);
            assert_eq!(revoked.tokens[0].amount, 100);
            assert_eq!(current.save_wallets(), original);
        }
    }

    #[test]
    fn expiry_aware_rewind_rejects_noncanonical_inputs_before_normalizing() {
        let mut before = TrstEngine::with_expiry(100);
        mint(&mut before, 1);
        let mut after = before.clone();
        after.debit_wallet_with_provenance(&wallet(1), &hash(1), 40);
        let mut current = after.clone();
        current.flush_all_expired(Timestamp::new(250));
        let originals = [
            before.save_wallets(),
            after.save_wallets(),
            current.save_wallets(),
        ];
        for index in 0..3 {
            for truncate in [false, true] {
                let mut snapshots = originals.clone();
                if truncate {
                    snapshots[index].pop();
                } else {
                    snapshots[index].push(0);
                }
                assert!(TrstEngine::rewind_wallet_snapshot_at(
                    &snapshots[0],
                    &snapshots[1],
                    &snapshots[2],
                    10_000,
                    Timestamp::new(250),
                )
                .is_err());
            }
        }
    }

    #[test]
    fn expiry_aware_rewind_still_rejects_later_spending_ownership_and_revocation() {
        let mut before = TrstEngine::with_expiry(100);
        mint(&mut before, 1);
        let mut after = before.clone();
        after.debit_wallet_with_provenance(&wallet(1), &hash(1), 40);
        for case in 0..4 {
            let mut current = after.clone();
            match case {
                0 => {
                    current.debit_wallet_with_provenance(&wallet(1), &hash(1), 10);
                }
                1 => {
                    current.revoke_by_originator(&wallet(1));
                }
                2 => current.get_portfolio_mut(&wallet(1)).unwrap().tokens[0].holder = wallet(2),
                _ => {
                    current.get_portfolio_mut(&wallet(1)).unwrap().tokens[0].origin_wallet =
                        wallet(2)
                }
            }
            current.flush_all_expired(Timestamp::new(250));
            assert!(
                TrstEngine::rewind_wallet_snapshot_at(
                    &before.save_wallets(),
                    &after.save_wallets(),
                    &current.save_wallets(),
                    100,
                    Timestamp::new(250)
                )
                .is_err(),
                "case {case}"
            );
        }
    }

    #[test]
    fn wallet_rewind_rejects_later_spending_revocation_and_corrupt_snapshots() {
        let mut before = TrstEngine::new();
        mint(&mut before, 1);
        let mut after = before.clone();
        after.debit_wallet_with_provenance(&wallet(1), &hash(1), 40);
        for revoke in [false, true] {
            let mut current = after.clone();
            if revoke {
                current.revoke_by_originator(&wallet(1));
            } else {
                current.debit_wallet_with_provenance(&wallet(1), &hash(1), 10);
            }
            assert!(TrstEngine::rewind_wallet_snapshot(
                &before.save_wallets(),
                &after.save_wallets(),
                &current.save_wallets()
            )
            .is_err());
        }
        let mut corrupt = after.save_wallets();
        corrupt.push(0);
        assert!(TrstEngine::rewind_wallet_snapshot(
            &before.save_wallets(),
            &after.save_wallets(),
            &corrupt
        )
        .is_err());
    }

    fn merge(graph: &mut MergerGraph, n: u8, origin: u8) {
        graph.record_merge(MergeNode {
            merge_tx: hash(n),
            source_origins: vec![MergeSource {
                origin: hash(origin),
                amount: 100,
            }],
            total_amount: 100,
            revoked_contribs: HashMap::new(),
        });
    }

    #[test]
    fn graph_rewind_preserves_independent_merges_including_shared_origins() {
        let before = MergerGraph::new();
        let mut after = before.clone();
        merge(&mut after, 10, 1);
        let mut current = after.clone();
        merge(&mut current, 20, 2);
        let bytes = MergerGraph::rewind_snapshot(
            &before.to_bytes(),
            &after.to_bytes(),
            &current.to_bytes(),
        )
        .unwrap();
        let mut expected = before.clone();
        merge(&mut expected, 20, 2);
        assert_eq!(bytes, expected.to_bytes());
        for origin in [1, 2] {
            let mut current = after.clone();
            merge(&mut current, 20, origin);
            let bytes = MergerGraph::rewind_snapshot(
                &before.to_bytes(),
                &after.to_bytes(),
                &current.to_bytes(),
            )
            .unwrap();
            let mut expected = before.clone();
            merge(&mut expected, 20, origin);
            assert_eq!(bytes, expected.to_bytes());
        }
        let mut downstream = after.clone();
        merge(&mut downstream, 20, 10);
        assert!(MergerGraph::rewind_snapshot(
            &before.to_bytes(),
            &after.to_bytes(),
            &downstream.to_bytes()
        )
        .is_err());
    }

    #[test]
    fn shared_parent_rewind_preserves_later_edges_and_proportional_revocation() {
        let mut before = MergerGraph::new();
        before.record_merge(MergeNode {
            merge_tx: hash(10),
            source_origins: vec![
                MergeSource {
                    origin: hash(1),
                    amount: 60,
                },
                MergeSource {
                    origin: hash(2),
                    amount: 40,
                },
            ],
            total_amount: 100,
            revoked_contribs: HashMap::new(),
        });
        let child = |graph: &mut MergerGraph, id| {
            graph.record_merge(MergeNode {
                merge_tx: hash(id),
                source_origins: vec![MergeSource {
                    origin: hash(10),
                    amount: 50,
                }],
                total_amount: 50,
                revoked_contribs: HashMap::new(),
            })
        };
        let mut after = before.clone();
        child(&mut after, 20);
        let mut current = after.clone();
        child(&mut current, 30);
        let mut expected = before.clone();
        child(&mut expected, 30);
        let bytes = MergerGraph::rewind_snapshot(
            &before.to_bytes(),
            &after.to_bytes(),
            &current.to_bytes(),
        )
        .unwrap();
        assert_eq!(bytes, expected.to_bytes());
        let mut result = MergerGraph::from_bytes(&bytes).unwrap();
        result.apply_revocation(hash(1));
        expected.apply_revocation(hash(1));
        assert_eq!(result.to_bytes(), expected.to_bytes());
        assert_eq!(result.get_merge(&hash(30)).unwrap().revoked_total(), 30);
        // A later revocation changed the losing merge itself, not just an index.
        current.apply_revocation(hash(1));
        assert!(MergerGraph::rewind_snapshot(
            &before.to_bytes(),
            &after.to_bytes(),
            &current.to_bytes()
        )
        .is_err());
    }

    #[test]
    fn reverse_edge_undo_checks_multiplicity_and_never_partially_changes_a_map() {
        let before = HashMap::from([(1, vec![10])]);
        let after = HashMap::from([(1, vec![10, 20, 20]), (2, vec![20])]);
        let mut current = HashMap::from([(1, vec![10, 20, 20, 30]), (2, vec![20, 40])]);
        rewind_appended_edges(&before, &after, &mut current).unwrap();
        assert_eq!(current, HashMap::from([(1, vec![10, 30]), (2, vec![40])]));
        for bad in [vec![10, 20, 30], vec![10, 20, 20, 20, 30], vec![10, 30]] {
            let original = HashMap::from([(1, bad), (2, vec![20, 40])]);
            let mut current = original.clone();
            assert!(rewind_appended_edges(&before, &after, &mut current).is_err());
            assert_eq!(current, original);
        }
        for changed in [
            HashMap::from([(1, vec![20])]),
            HashMap::new(),
            HashMap::from([(1, vec![10, 10])]),
        ] {
            let mut current = changed.clone();
            assert!(rewind_appended_edges(&before, &changed, &mut current).is_err());
            assert_eq!(current, changed);
        }
    }

    #[test]
    fn successive_shared_origin_rewinds_do_not_resurrect_previously_removed_edges() {
        let empty = MergerGraph::new().to_bytes();
        let mut graph = MergerGraph::new();
        merge(&mut graph, 10, 1);
        let first = graph.to_bytes();
        merge(&mut graph, 20, 1);
        let second = graph.to_bytes();
        let remaining = MergerGraph::rewind_snapshot(&empty, &first, &second).unwrap();
        let restored = MergerGraph::rewind_snapshot(&first, &second, &remaining).unwrap();
        assert_eq!(restored, empty);
    }

    #[test]
    fn disjoint_additions_deletions_and_updates_survive() {
        let before = HashMap::from([(1, 10), (2, 20), (4, 40)]);
        let after = HashMap::from([(1, 11), (3, 30), (4, 40)]);
        let mut current = HashMap::from([(1, 11), (3, 30), (4, 41), (5, 50)]);
        rewind_map(&before, &after, &mut current).unwrap();
        assert_eq!(current, HashMap::from([(1, 10), (2, 20), (4, 41), (5, 50)]));
    }

    #[test]
    fn overlapping_additions_deletions_and_updates_reject_without_mutation() {
        let before = HashMap::from([(1, 10), (2, 20)]);
        let after = HashMap::from([(1, 11), (3, 30)]);
        for conflict in [
            HashMap::from([(1, 12), (3, 30)]),
            HashMap::from([(1, 11), (2, 21), (3, 30)]),
            HashMap::from([(1, 11)]),
        ] {
            let mut current = conflict.clone();
            assert!(rewind_map(&before, &after, &mut current).is_err());
            assert_eq!(current, conflict);
        }
    }

    #[test]
    fn set_restoration_preserves_unrelated_membership_and_rejects_overlap() {
        let before = HashSet::from([1, 2]);
        let after = HashSet::from([2, 3]);
        let mut current = HashSet::from([3, 4]);
        rewind_set(&before, &after, &mut current).unwrap();
        assert_eq!(current, HashSet::from([1, 4]));
        let mut conflict = HashSet::from([1, 3, 4]);
        let original = conflict.clone();
        assert!(rewind_set(&before, &after, &mut conflict).is_err());
        assert_eq!(conflict, original);
    }
}
