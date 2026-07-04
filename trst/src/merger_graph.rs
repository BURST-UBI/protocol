//! The Merger Graph — forward index for proactive revocation.
//!
//! Normal transaction chains are backward-linked (holder → link → origin).
//! The merger graph is the **inverse**: origin → [merges containing it] → current balances.
//!
//! Per the whitepaper (§The Merger Graph) and IMPLEMENTATION_DECISIONS 6.17(b),
//! each merge node records only its **immediate inputs** — the merge transaction's
//! input list. Ancestry is never flattened onto tokens or nodes; it is discovered
//! by following the chain. A source origin may itself be an earlier merge tx,
//! which is what forms the multi-level graph:
//!
//! ```text
//! origin (burn) → [merges containing it] → [merges of merges] → current balances
//! ```
//!
//! Without the merger graph, every transaction requires O(n) backward traversal to
//! check for revoked origins. With it, revocation is a one-time O(k) forward
//! traversal at catch time, and every subsequent transaction is O(1).

use burst_types::TxHash;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

/// A node in the merger graph representing a merge operation.
///
/// `source_origins` lists the merge's immediate inputs only. Current holders of
/// tokens descended from this merge are tracked by the engine's origin-holders
/// index, not here — a holder recorded at merge time would go stale on the first
/// transfer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeNode {
    /// Hash of the merge transaction.
    pub merge_tx: TxHash,
    /// Immediate inputs consumed by this merge: the origin of each input token
    /// (a burn tx, or an earlier merge tx) with the amount consumed.
    pub source_origins: Vec<MergeSource>,
    /// Total amount of the merged token.
    pub total_amount: u128,
    /// Amounts already revoked from this merge, keyed by the revoked burn origin.
    /// Kept so sequential revocations of different origins split against the
    /// correct remaining (unrevoked) denominator, and so un-revocation (6.15b)
    /// can restore exactly what was taken.
    #[serde(default)]
    pub revoked_contribs: HashMap<TxHash, u128>,
}

impl MergeNode {
    /// Sum of all amounts already revoked from this merge.
    pub fn revoked_total(&self) -> u128 {
        self.revoked_contribs
            .values()
            .fold(0u128, |acc, v| acc.saturating_add(*v))
    }
}

/// One source in a merge operation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeSource {
    pub origin: TxHash,
    pub amount: u128,
}

/// The merger graph — maps origins forward to all merges that consumed them.
#[derive(Serialize, Deserialize, Default)]
pub struct MergerGraph {
    /// Maps an input origin (burn tx or merge tx) to all merges that consumed it.
    origin_to_merges: HashMap<TxHash, Vec<TxHash>>,

    /// Maps a merge TxHash to its full merge node data.
    merge_nodes: HashMap<TxHash, MergeNode>,

    /// Maps a merge TxHash to downstream merges that consumed it.
    merge_to_downstream: HashMap<TxHash, Vec<TxHash>>,

    /// Set of burn origins that are currently revoked.
    revoked_origins: HashSet<TxHash>,
}

/// Multiply-then-ceil-divide with saturation: `ceil(amount * num / den)`.
/// The "harsh" rounding of IMPLEMENTATION_DECISIONS 6.18(b) — the holder
/// loses the fractional raw.
pub(crate) fn ceil_proportion(amount: u128, num: u128, den: u128) -> u128 {
    if den == 0 || num == 0 || amount == 0 {
        return 0;
    }
    let prod = amount.saturating_mul(num);
    let out = prod / den + u128::from(!prod.is_multiple_of(den));
    out.min(amount)
}

/// A taint computed for one merge node when a burn origin is revoked.
#[derive(Clone, Debug)]
pub struct TaintEvent {
    /// The merge whose descendants contain tainted TRST.
    pub merge_tx: TxHash,
    /// Amount of this merge attributable to the revoked origin.
    pub tainted_amount: u128,
    /// Sum of amounts revoked from this merge by *earlier* revocations
    /// (before this one). Live tokens split against `total_amount - prior_revoked`.
    pub prior_revoked: u128,
    /// Total amount of the merged token at merge time.
    pub total_amount: u128,
}

/// Reversal of a taint when a burn origin is un-revoked (6.15b).
#[derive(Clone, Debug)]
pub struct UnTaintEvent {
    pub merge_tx: TxHash,
    /// The amount that had been revoked from this merge for the origin.
    pub restored_amount: u128,
}

impl MergerGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a new merge operation in the graph.
    ///
    /// Automatically links downstream when a source origin is itself an earlier
    /// merge (multi-level merges). Because sources are the immediate inputs,
    /// this fires whenever a merged token is consumed by a subsequent merge.
    pub fn record_merge(&mut self, node: MergeNode) {
        let merge_tx = node.merge_tx;
        for source in &node.source_origins {
            self.origin_to_merges
                .entry(source.origin)
                .or_default()
                .push(merge_tx);

            if self.merge_nodes.contains_key(&source.origin) {
                self.record_downstream(source.origin, merge_tx);
            }
        }
        self.merge_nodes.insert(merge_tx, node);
    }

    /// Record that a downstream merge consumed an earlier merge.
    pub fn record_downstream(&mut self, parent_merge: TxHash, child_merge: TxHash) {
        self.merge_to_downstream
            .entry(parent_merge)
            .or_default()
            .push(child_merge);
    }

    /// Whether `tx` is a known merge transaction (i.e. tokens with this origin
    /// are merged tokens).
    pub fn contains_merge(&self, tx: &TxHash) -> bool {
        self.merge_nodes.contains_key(tx)
    }

    /// Get a merge node by its transaction hash.
    pub fn get_merge(&self, tx: &TxHash) -> Option<&MergeNode> {
        self.merge_nodes.get(tx)
    }

    /// Collect all merges affected by an origin: its direct consumers plus the
    /// full downstream closure, in a topological order (parents before children).
    ///
    /// Topological ordering is possible without timestamps because a merge can
    /// only ever consume tokens that already exist — the graph is a DAG.
    fn affected_merges_topo(&self, origin: &TxHash) -> Vec<TxHash> {
        let mut affected: HashSet<TxHash> = HashSet::new();
        let mut queue: VecDeque<TxHash> = VecDeque::new();

        if let Some(direct) = self.origin_to_merges.get(origin) {
            for &m in direct {
                if affected.insert(m) {
                    queue.push_back(m);
                }
            }
        }
        while let Some(m) = queue.pop_front() {
            if let Some(children) = self.merge_to_downstream.get(&m) {
                for &c in children {
                    if affected.insert(c) {
                        queue.push_back(c);
                    }
                }
            }
        }

        // Kahn's algorithm restricted to the affected set.
        let mut in_degree: HashMap<TxHash, usize> = HashMap::new();
        for &m in &affected {
            let node = match self.merge_nodes.get(&m) {
                Some(n) => n,
                None => continue,
            };
            let deg = node
                .source_origins
                .iter()
                .filter(|s| affected.contains(&s.origin))
                .count();
            in_degree.insert(m, deg);
        }

        let mut ready: VecDeque<TxHash> = in_degree
            .iter()
            .filter(|(_, &d)| d == 0)
            .map(|(&m, _)| m)
            .collect();
        let mut order = Vec::with_capacity(affected.len());
        while let Some(m) = ready.pop_front() {
            order.push(m);
            if let Some(children) = self.merge_to_downstream.get(&m) {
                for c in children {
                    if let Some(d) = in_degree.get_mut(c) {
                        *d = d.saturating_sub(1);
                        if *d == 0 {
                            ready.push_back(*c);
                        }
                    }
                }
            }
        }
        order
    }

    /// Compute the tainted amount per affected merge for `origin`, walking the
    /// graph forward level by level. Does not mutate state.
    ///
    /// A merge's taint is its direct intake from `origin` plus, for every
    /// source that is itself a tainted merge, the consumed amount scaled by the
    /// parent's tainted fraction (rounded up per 6.18b).
    fn compute_taints(&self, origin: &TxHash) -> Vec<(TxHash, u128)> {
        let order = self.affected_merges_topo(origin);
        let mut taint: HashMap<TxHash, u128> = HashMap::new();
        let mut out = Vec::new();

        for m in order {
            let node = match self.merge_nodes.get(&m) {
                Some(n) => n,
                None => continue,
            };
            let mut t: u128 = 0;
            for s in &node.source_origins {
                if s.origin == *origin {
                    t = t.saturating_add(s.amount);
                } else if let (Some(&pt), Some(parent)) =
                    (taint.get(&s.origin), self.merge_nodes.get(&s.origin))
                {
                    t = t.saturating_add(ceil_proportion(s.amount, pt, parent.total_amount));
                }
            }
            let t = t.min(node.total_amount);
            if t > 0 {
                taint.insert(m, t);
                out.push((m, t));
            }
        }
        out
    }

    /// Mark a burn origin as revoked and propagate the taint forward through
    /// every merge that (directly or transitively) consumed it.
    ///
    /// Records the per-origin revoked contribution on each affected node and
    /// returns one `TaintEvent` per affected merge so the engine can split the
    /// live tokens proportionally (7.2c). Idempotent: re-revoking an already
    /// revoked origin returns no events.
    pub fn apply_revocation(&mut self, origin: TxHash) -> Vec<TaintEvent> {
        if !self.revoked_origins.insert(origin) {
            return Vec::new();
        }
        let taints = self.compute_taints(&origin);
        let mut events = Vec::with_capacity(taints.len());
        for (merge_tx, tainted) in taints {
            if let Some(node) = self.merge_nodes.get_mut(&merge_tx) {
                let prior = node.revoked_total();
                let tainted = tainted.min(node.total_amount.saturating_sub(prior));
                if tainted == 0 {
                    continue;
                }
                node.revoked_contribs.insert(origin, tainted);
                events.push(TaintEvent {
                    merge_tx,
                    tainted_amount: tainted,
                    prior_revoked: prior,
                    total_amount: node.total_amount,
                });
            }
        }
        events
    }

    /// Un-mark a burn origin and remove its recorded contribution from every
    /// affected merge (6.15b). Returns what was restored per merge. Idempotent.
    pub fn apply_unrevocation(&mut self, origin: &TxHash) -> Vec<UnTaintEvent> {
        if !self.revoked_origins.remove(origin) {
            return Vec::new();
        }
        let order = self.affected_merges_topo(origin);
        let mut events = Vec::new();
        for m in order {
            if let Some(node) = self.merge_nodes.get_mut(&m) {
                if let Some(restored) = node.revoked_contribs.remove(origin) {
                    events.push(UnTaintEvent {
                        merge_tx: m,
                        restored_amount: restored,
                    });
                }
            }
        }
        events
    }

    /// Check if a specific burn origin is currently revoked — the O(1)
    /// per-transaction validity check from the whitepaper.
    pub fn is_origin_revoked(&self, origin: &TxHash) -> bool {
        self.revoked_origins.contains(origin)
    }

    /// Get the set of all currently revoked burn origins.
    pub fn revoked_origins(&self) -> &HashSet<TxHash> {
        &self.revoked_origins
    }

    /// Check whether a merge node has ANY remaining revoked contributions.
    pub fn merge_has_revoked_origins(&self, merge_tx: &TxHash) -> bool {
        self.merge_nodes
            .get(merge_tx)
            .is_some_and(|n| !n.revoked_contribs.is_empty())
    }

    /// Serialize the entire graph to bytes for LMDB persistence.
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).expect("merger graph serialization should not fail")
    }

    /// Deserialize a graph from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        bincode::deserialize(bytes).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(id: u8) -> TxHash {
        TxHash::new([id; 32])
    }

    fn node(merge: TxHash, sources: &[(TxHash, u128)], total: u128) -> MergeNode {
        MergeNode {
            merge_tx: merge,
            source_origins: sources
                .iter()
                .map(|(o, a)| MergeSource {
                    origin: *o,
                    amount: *a,
                })
                .collect(),
            total_amount: total,
            revoked_contribs: HashMap::new(),
        }
    }

    #[test]
    fn record_merge_indexes_by_origin() {
        let mut graph = MergerGraph::new();
        let (o1, o2, m1) = (tx(1), tx(2), tx(10));
        graph.record_merge(node(m1, &[(o1, 50), (o2, 50)], 100));

        assert_eq!(graph.origin_to_merges[&o1], vec![m1]);
        assert_eq!(graph.origin_to_merges[&o2], vec![m1]);
        assert!(graph.contains_merge(&m1));
    }

    #[test]
    fn auto_downstream_links_multi_level_merges() {
        let mut graph = MergerGraph::new();
        let (o1, o2, o3, m1, m2) = (tx(1), tx(2), tx(3), tx(10), tx(20));

        graph.record_merge(node(m1, &[(o1, 50), (o2, 50)], 100));
        // The merged token's origin IS m1 (whitepaper), so a second-level merge
        // lists m1 as an immediate source.
        graph.record_merge(node(m2, &[(m1, 60), (o3, 40)], 100));

        assert_eq!(graph.merge_to_downstream[&m1], vec![m2]);
    }

    #[test]
    fn no_downstream_for_plain_burn_origins() {
        let mut graph = MergerGraph::new();
        graph.record_merge(node(tx(10), &[(tx(1), 50), (tx(2), 50)], 100));
        assert!(graph.merge_to_downstream.is_empty());
    }

    #[test]
    fn taint_propagates_proportionally_through_levels() {
        let mut graph = MergerGraph::new();
        let (o1, o2, o3, m1, m2) = (tx(1), tx(2), tx(3), tx(10), tx(20));

        graph.record_merge(node(m1, &[(o1, 50), (o2, 50)], 100));
        // m2 consumed only 60 of m1's 100.
        graph.record_merge(node(m2, &[(m1, 60), (o3, 40)], 100));

        let events = graph.apply_revocation(o1);
        assert_eq!(events.len(), 2);

        let m1_ev = events.iter().find(|e| e.merge_tx == m1).unwrap();
        assert_eq!(m1_ev.tainted_amount, 50);

        // m1 is 50% tainted; m2 consumed 60 of it → 30 tainted.
        let m2_ev = events.iter().find(|e| e.merge_tx == m2).unwrap();
        assert_eq!(m2_ev.tainted_amount, 30);
    }

    #[test]
    fn three_level_taint() {
        let mut graph = MergerGraph::new();
        let (o1, o2, o3, o4) = (tx(1), tx(2), tx(3), tx(4));
        let (m1, m2, m3) = (tx(10), tx(20), tx(30));

        graph.record_merge(node(m1, &[(o1, 50), (o2, 50)], 100));
        graph.record_merge(node(m2, &[(m1, 60), (o3, 40)], 100));
        graph.record_merge(node(m3, &[(m2, 70), (o4, 30)], 100));

        let events = graph.apply_revocation(o1);
        let m3_ev = events.iter().find(|e| e.merge_tx == m3).unwrap();
        // m1: 50/100 tainted. m2: ceil(60*50/100)=30 of 100. m3: ceil(70*30/100)=21.
        assert_eq!(m3_ev.tainted_amount, 21);
    }

    #[test]
    fn rounding_is_harsh_per_6_18_b() {
        let mut graph = MergerGraph::new();
        let (o1, o2, m1, m2) = (tx(1), tx(2), tx(10), tx(20));

        graph.record_merge(node(m1, &[(o1, 1), (o2, 2)], 3));
        graph.record_merge(node(m2, &[(m1, 2), (o2, 8)], 10));

        let events = graph.apply_revocation(o1);
        let m2_ev = events.iter().find(|e| e.merge_tx == m2).unwrap();
        // exact: 2 * 1/3 = 0.66… → rounds UP to 1.
        assert_eq!(m2_ev.tainted_amount, 1);
    }

    #[test]
    fn sequential_revocations_track_prior_revoked() {
        let mut graph = MergerGraph::new();
        let (o1, o2, m1) = (tx(1), tx(2), tx(10));
        graph.record_merge(node(m1, &[(o1, 60), (o2, 40)], 100));

        let ev1 = graph.apply_revocation(o1);
        assert_eq!(ev1[0].tainted_amount, 60);
        assert_eq!(ev1[0].prior_revoked, 0);

        let ev2 = graph.apply_revocation(o2);
        assert_eq!(ev2[0].tainted_amount, 40);
        assert_eq!(ev2[0].prior_revoked, 60);
    }

    #[test]
    fn revocation_is_idempotent() {
        let mut graph = MergerGraph::new();
        let (o1, o2, m1) = (tx(1), tx(2), tx(10));
        graph.record_merge(node(m1, &[(o1, 50), (o2, 50)], 100));

        assert_eq!(graph.apply_revocation(o1).len(), 1);
        assert!(graph.apply_revocation(o1).is_empty());
    }

    #[test]
    fn unrevocation_restores_contribs() {
        let mut graph = MergerGraph::new();
        let (o1, o2, m1) = (tx(1), tx(2), tx(10));
        graph.record_merge(node(m1, &[(o1, 50), (o2, 50)], 100));

        graph.apply_revocation(o1);
        assert!(graph.is_origin_revoked(&o1));
        assert!(graph.merge_has_revoked_origins(&m1));

        let restored = graph.apply_unrevocation(&o1);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].restored_amount, 50);
        assert!(!graph.is_origin_revoked(&o1));
        assert!(!graph.merge_has_revoked_origins(&m1));
    }

    #[test]
    fn serialization_roundtrip() {
        let mut graph = MergerGraph::new();
        let (o1, o2, m1) = (tx(1), tx(2), tx(10));
        graph.record_merge(node(m1, &[(o1, 50), (o2, 50)], 100));
        graph.apply_revocation(o1);

        let bytes = graph.to_bytes();
        let restored = MergerGraph::from_bytes(&bytes).unwrap();
        assert!(restored.is_origin_revoked(&o1));
        assert!(restored.contains_merge(&m1));
        assert_eq!(restored.get_merge(&m1).unwrap().revoked_contribs[&o1], 50);
    }
}
