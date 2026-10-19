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

use crate::shared::Shared;
use bincode::Options;
use burst_types::{TrstBasis, TxHash};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

const SNAPSHOT_MAGIC: &[u8; 8] = b"BRSTMG03";

mod records;
mod snapshot_validation;
pub use records::{GraphRecord, GraphRecordReader};

/// One change, shared by every later merge using this revision. A source edge
/// stores a pointer, not a copied list of all excluded burn origins.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RevocationRevision {
    merge_tx: TxHash,
    previous: TxHash,
    origin: TxHash,
    revoked: bool,
    remaining: u128,
}

impl RevocationRevision {
    fn hash(&self) -> TxHash {
        TxHash::new(burst_crypto::blake2b_256_multi(&[
            b"BURST/merge-revocation-revision/v1\0",
            self.merge_tx.as_bytes(),
            self.previous.as_bytes(),
            self.origin.as_bytes(),
            &[u8::from(self.revoked)],
            &self.remaining.to_be_bytes(),
        ]))
    }
}

// Local snapshots must survive decode/encode without randomized map order
// appearing as an unrelated economic change to an undo journal.
fn sorted_map<V: Serialize, S: serde::Serializer>(
    map: &HashMap<TxHash, V>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    map.iter()
        .map(|(hash, value)| (hash.as_bytes(), value))
        .collect::<std::collections::BTreeMap<_, _>>()
        .serialize(serializer)
}

fn sorted_set<S: serde::Serializer>(
    set: &HashSet<TxHash>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    set.iter()
        .map(TxHash::as_bytes)
        .collect::<std::collections::BTreeSet<_>>()
        .serialize(serializer)
}

/// A node in the merger graph representing a merge operation.
///
/// `source_origins` lists the merge's immediate inputs only. Current holders of
/// tokens descended from this merge are tracked by the engine's origin-holders
/// index, not here — a holder recorded at merge time would go stale on the first
/// transfer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    #[serde(default, serialize_with = "sorted_map")]
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeSource {
    pub origin: TxHash,
    pub amount: u128,
}

/// The merger graph — maps origins forward to all merges that consumed them.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MergerGraph {
    /// Maps an input origin (burn tx or merge tx) to all merges that consumed it.
    #[serde(serialize_with = "sorted_map")]
    origin_to_merges: Shared<HashMap<TxHash, Vec<TxHash>>>,

    /// Maps a merge TxHash to its full merge node data.
    #[serde(serialize_with = "sorted_map")]
    merge_nodes: Shared<HashMap<TxHash, MergeNode>>,

    /// Maps a merge TxHash to downstream merges that consumed it.
    #[serde(serialize_with = "sorted_map")]
    merge_to_downstream: Shared<HashMap<TxHash, Vec<TxHash>>>,

    /// Set of burn origins that are currently revoked.
    #[serde(serialize_with = "sorted_set")]
    revoked_origins: Shared<HashSet<TxHash>>,

    #[serde(serialize_with = "sorted_map")]
    revisions: Shared<HashMap<TxHash, RevocationRevision>>,
    #[serde(serialize_with = "sorted_map")]
    current_revisions: Shared<HashMap<TxHash, TxHash>>,
    /// Child merge -> immediate source/revision pairs, only for changed sources.
    #[serde(serialize_with = "sorted_map")]
    source_revisions: Shared<HashMap<TxHash, Vec<(usize, TrstBasis)>>>,
}

/// Exact ceil-divide, capped at `amount`: `ceil(amount * num / den)`.
/// The "harsh" rounding of IMPLEMENTATION_DECISIONS 6.18(b) — the holder
/// loses the fractional raw.
pub(crate) fn ceil_proportion(amount: u128, num: u128, den: u128) -> u128 {
    if den == 0 || num == 0 || amount == 0 {
        return 0;
    }
    if num >= den {
        return amount;
    }
    if let Some(product) = amount.checked_mul(num) {
        return product / den + u128::from(product % den != 0);
    }
    // Binary long division of the conceptual 256-bit product. Keep a
    // remainder below den; subtraction comparisons avoid overflowing 2*r.
    // Saturating the product would severely under-revoke ordinary 18-decimal
    // amounts, not merely unrealistic u128-limit balances.
    let mut quotient = 0u128;
    let mut remainder = 0u128;
    for bit in (0..128).rev() {
        let mut carry = 0u128;
        if remainder >= den - remainder {
            remainder -= den - remainder;
            carry = 1;
        } else {
            remainder += remainder;
        }
        if (amount >> bit) & 1 == 1 {
            if remainder >= den - num {
                remainder -= den - num;
                carry += 1;
            } else {
                remainder += num;
            }
        }
        quotient = quotient * 2 + carry;
    }
    quotient + u128::from(remainder != 0)
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
    /// True only when both snapshots still share every unchanged index.
    /// False does not imply different contents (for example, after decoding).
    pub fn shares_storage_with(&self, other: &Self) -> bool {
        self.origin_to_merges
            .shares_storage_with(&other.origin_to_merges)
            && self.merge_nodes.shares_storage_with(&other.merge_nodes)
            && self
                .merge_to_downstream
                .shares_storage_with(&other.merge_to_downstream)
            && self
                .revoked_origins
                .shares_storage_with(&other.revoked_origins)
            && self.revisions.shares_storage_with(&other.revisions)
            && self
                .current_revisions
                .shares_storage_with(&other.current_revisions)
            && self
                .source_revisions
                .shares_storage_with(&other.source_revisions)
    }

    pub fn iter_merges(&self) -> impl Iterator<Item = &MergeNode> {
        self.merge_nodes.values()
    }
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a new merge operation in the graph.
    ///
    /// Automatically links downstream when a source origin is itself an earlier
    /// merge (multi-level merges). Because sources are the immediate inputs,
    /// this fires whenever a merged token is consumed by a subsequent merge.
    pub fn record_merge(&mut self, node: MergeNode) {
        let bases = node
            .source_origins
            .iter()
            .map(|source| {
                self.current_revisions
                    .get(&source.origin)
                    .map_or(TrstBasis::Entire, |revision| {
                        TrstBasis::Remainder(*revision)
                    })
            })
            .collect();
        self.record_merge_with_bases(node, bases);
    }

    pub(crate) fn record_merge_with_bases(&mut self, node: MergeNode, bases: Vec<TrstBasis>) {
        let merge_tx = node.merge_tx;
        assert_eq!(node.source_origins.len(), bases.len());
        let revisions: Vec<_> = bases
            .into_iter()
            .enumerate()
            .filter(|(_, basis)| *basis != TrstBasis::Entire)
            .collect();
        if !revisions.is_empty() {
            self.source_revisions.insert(merge_tx, revisions);
        }
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
            for (index, s) in node.source_origins.iter().enumerate() {
                let basis = self
                    .source_revisions
                    .get(&m)
                    .and_then(|sources| {
                        sources
                            .iter()
                            .find(|(source, _)| *source == index)
                            .map(|(_, basis)| *basis)
                    })
                    .unwrap_or_default();
                if let TrstBasis::Single(single) = basis {
                    if single == *origin {
                        t = t.saturating_add(s.amount);
                    }
                } else if s.origin == *origin {
                    t = t.saturating_add(s.amount);
                } else if let (Some(&pt), Some(parent)) =
                    (taint.get(&s.origin), self.merge_nodes.get(&s.origin))
                {
                    let denominator = match basis {
                        TrstBasis::Remainder(revision) => {
                            if self.revoked_at(revision, origin) {
                                continue;
                            }
                            self.revisions[&revision].remaining
                        }
                        _ => parent.total_amount,
                    };
                    t = t.saturating_add(ceil_proportion(s.amount, pt, denominator));
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

    fn revoked_at(&self, mut revision: TxHash, origin: &TxHash) -> bool {
        while revision != TxHash::ZERO {
            let state = &self.revisions[&revision];
            if state.origin == *origin {
                return state.revoked;
            }
            revision = state.previous;
        }
        false
    }

    fn record_revision(&mut self, merge_tx: TxHash, origin: TxHash, revoked: bool) {
        let node = &self.merge_nodes[&merge_tx];
        let revision = RevocationRevision {
            merge_tx,
            origin,
            revoked,
            previous: self
                .current_revisions
                .get(&merge_tx)
                .copied()
                .unwrap_or(TxHash::ZERO),
            remaining: node.total_amount.saturating_sub(node.revoked_total()),
        };
        let hash = revision.hash();
        self.revisions.insert(hash, revision);
        self.current_revisions.insert(merge_tx, hash);
    }

    pub fn valid_basis(&self, origin: &TxHash, basis: TrstBasis) -> bool {
        match basis {
            TrstBasis::Entire => true,
            TrstBasis::Single(single) => {
                single != TxHash::ZERO && (single == *origin || self.contains_merge(origin))
            }
            TrstBasis::Remainder(revision) => self
                .revisions
                .get(&revision)
                .is_some_and(|state| state.merge_tx == *origin && state.remaining > 0),
        }
    }

    pub(crate) fn current_basis(&self, origin: &TxHash) -> TrstBasis {
        self.current_revisions
            .get(origin)
            .map_or(TrstBasis::Entire, |hash| TrstBasis::Remainder(*hash))
    }

    pub(crate) fn basis_excludes(&self, basis: TrstBasis, origin: &TxHash) -> bool {
        match basis {
            TrstBasis::Entire => false,
            TrstBasis::Single(single) => single != *origin,
            TrstBasis::Remainder(revision) => self.revoked_at(revision, origin),
        }
    }

    pub(crate) fn basis_cut(
        &self,
        origin: &TxHash,
        basis: TrstBasis,
        amount: u128,
        revoked: &TxHash,
        tainted: u128,
    ) -> u128 {
        match basis {
            TrstBasis::Single(single) => {
                if single == *revoked {
                    amount
                } else {
                    0
                }
            }
            TrstBasis::Remainder(revision) => {
                if self.revoked_at(revision, revoked) {
                    return 0;
                }
                ceil_proportion(amount, tainted, self.revisions[&revision].remaining)
            }
            TrstBasis::Entire => self
                .merge_nodes
                .get(origin)
                .map_or(if origin == revoked { amount } else { 0 }, |node| {
                    ceil_proportion(amount, tainted, node.total_amount)
                }),
        }
    }

    pub(crate) fn reduced_basis(
        &mut self,
        origin: &TxHash,
        basis: TrstBasis,
        revoked: TxHash,
        tainted: u128,
    ) -> TrstBasis {
        let (previous, capacity) = match basis {
            TrstBasis::Entire => (TxHash::ZERO, self.merge_nodes[origin].total_amount),
            TrstBasis::Remainder(revision) => (revision, self.revisions[&revision].remaining),
            TrstBasis::Single(_) => return basis,
        };
        let revision = RevocationRevision {
            merge_tx: *origin,
            previous,
            origin: revoked,
            revoked: true,
            remaining: capacity.saturating_sub(tainted),
        };
        let hash = revision.hash();
        self.revisions.entry(hash).or_insert(revision);
        TrstBasis::Remainder(hash)
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
                self.record_revision(merge_tx, origin, true);
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
                    self.record_revision(m, *origin, false);
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
        let mut bytes = Vec::new();
        self.write_snapshot(&mut bytes)
            .expect("merger graph serialization should not fail");
        bytes
    }

    /// Write the existing snapshot format directly to a bounded caller's sink.
    pub fn write_snapshot(&self, mut writer: impl std::io::Write) -> Result<(), bincode::Error> {
        writer.write_all(SNAPSHOT_MAGIC)?;
        bincode::serialize_into(writer, self)
    }

    /// Restore only entries changed by a local journal. Independent consumers
    /// of a shared origin survive. Changes to the same merge or revision still
    /// conflict. Finality and dependency checks belong to the caller.
    pub fn rewind_snapshot(before: &[u8], after: &[u8], current: &[u8]) -> Result<Vec<u8>, String> {
        const MAX_BYTES: usize = 64 * 1024 * 1024;
        if [before, after, current]
            .iter()
            .any(|bytes| bytes.len() > MAX_BYTES)
        {
            return Err("merger graph undo exceeds size limit".into());
        }
        let before = Self::from_bytes(before)?;
        let after = Self::from_bytes(after)?;
        let current = Self::from_bytes(current)?;
        let bytes = current.rewind_changes(&before, &after)?.to_bytes();
        if bytes.len() > MAX_BYTES {
            return Err("merger graph undo exceeds size limit".into());
        }
        Ok(bytes)
    }

    /// Reconcile a local transition against current state without constructing
    /// aggregate snapshots. The returned candidate is unpublished on failure.
    pub fn rewind_changes(&self, before: &Self, after: &Self) -> Result<Self, String> {
        before.validate_persisted()?;
        after.validate_persisted()?;
        self.validate_persisted()?;
        let mut current = self.clone();
        use crate::snapshot_undo::{rewind_appended_edges, rewind_map, rewind_set};
        rewind_appended_edges(
            &before.origin_to_merges,
            &after.origin_to_merges,
            &mut current.origin_to_merges,
        )?;
        rewind_map(
            &before.merge_nodes,
            &after.merge_nodes,
            &mut current.merge_nodes,
        )?;
        rewind_appended_edges(
            &before.merge_to_downstream,
            &after.merge_to_downstream,
            &mut current.merge_to_downstream,
        )?;
        rewind_set(
            &before.revoked_origins,
            &after.revoked_origins,
            &mut current.revoked_origins,
        )?;
        rewind_map(&before.revisions, &after.revisions, &mut current.revisions)?;
        rewind_map(
            &before.current_revisions,
            &after.current_revisions,
            &mut current.current_revisions,
        )?;
        rewind_map(
            &before.source_revisions,
            &after.source_revisions,
            &mut current.source_revisions,
        )?;
        let removed: HashSet<_> = after
            .merge_nodes
            .keys()
            .filter(|hash| !before.merge_nodes.contains_key(hash))
            .collect();
        if current.merge_nodes.values().any(|node| {
            node.source_origins
                .iter()
                .any(|source| removed.contains(&source.origin))
        }) {
            return Err("snapshot undo would orphan a downstream merge".into());
        }
        current.validate_persisted()?;
        Ok(current)
    }

    /// Deserialize a graph from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 64 * 1024 * 1024 {
            return Err("merger graph snapshot exceeds size limit".into());
        }
        let body = bytes
            .strip_prefix(SNAPSHOT_MAGIC)
            .ok_or("merger graph requires explicit ledger replay to upgrade")?;
        let graph: Self = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .with_limit(body.len() as u64)
            .deserialize(body)
            .map_err(|e| e.to_string())?;
        graph.validate_persisted()?;
        if !crate::snapshot_encoding::matches(bytes, |writer| graph.write_snapshot(writer))
            .map_err(|error| error.to_string())?
        {
            return Err("noncanonical merger graph snapshot".into());
        }
        Ok(graph)
    }

    fn validate_persisted(&self) -> Result<(), String> {
        let graph = self;
        graph.validate_structure()?;
        for (hash, revision) in &graph.revisions {
            if *hash != revision.hash()
                || revision.origin == TxHash::ZERO
                || !graph
                    .merge_nodes
                    .get(&revision.merge_tx)
                    .is_some_and(|node| revision.remaining <= node.total_amount)
                || (revision.previous != TxHash::ZERO
                    && !graph
                        .revisions
                        .get(&revision.previous)
                        .is_some_and(|parent| parent.merge_tx == revision.merge_tx))
            {
                return Err("invalid merger graph revision".into());
            }
        }
        for (merge_tx, revision) in &graph.current_revisions {
            if !graph
                .revisions
                .get(revision)
                .is_some_and(|revision| revision.merge_tx == *merge_tx)
            {
                return Err("invalid current merger graph revision".into());
            }
        }
        for (merge_tx, sources) in &graph.source_revisions {
            let node = graph
                .merge_nodes
                .get(merge_tx)
                .ok_or("missing revised merge")?;
            if sources.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
                return Err("noncanonical merge source revisions".into());
            }
            for (index, basis) in sources {
                if *basis == TrstBasis::Entire
                    || !node
                        .source_origins
                        .get(*index)
                        .is_some_and(|source| graph.valid_basis(&source.origin, *basis))
                {
                    return Err("invalid merge source revision".into());
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod shared_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proportional_revocation_is_exact_above_u128_product_range() {
        let thousand = 1_000 * 10u128.pow(18);
        assert_eq!(
            ceil_proportion(thousand, thousand / 2, thousand),
            thousand / 2
        );
        assert_eq!(
            ceil_proportion(u128::MAX, u128::MAX - 1, u128::MAX),
            u128::MAX - 1
        );
        assert_eq!(ceil_proportion(u128::MAX, 1, 2), (u128::MAX / 2) + 1);
        assert_eq!(ceil_proportion(3, 1, 2), 2);
        assert_eq!(ceil_proportion(3, 5, 2), 3);
    }

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
    fn graph_snapshot_bytes_survive_rehashed_roundtrips() {
        let mut graph = MergerGraph::new();
        for id in 10..30 {
            let mut merge = node(tx(id), &[(tx(id - 1), 60), (tx(1), 40)], 100);
            merge.revoked_contribs.insert(tx(1), 10);
            merge.revoked_contribs.insert(tx(2), 5);
            graph.record_merge(merge);
        }
        graph.revoked_origins.extend([tx(1), tx(2), tx(3)]);
        let bytes = graph.to_bytes();
        for _ in 0..16 {
            graph = MergerGraph::from_bytes(&bytes).unwrap();
            assert_eq!(graph.to_bytes(), bytes);
        }
    }

    #[test]
    fn remerges_share_revision_history_instead_of_copying_excluded_origins() {
        let mut graph = MergerGraph::new();
        graph.record_merge(node(tx(20), &[(tx(1), 40), (tx(2), 60)], 100));
        graph.apply_revocation(tx(1));
        let revision = graph.current_revisions[&tx(20)];
        for id in 50..150 {
            graph.record_merge(node(tx(id), &[(tx(20), 60), (tx(3), 40)], 100));
            assert_eq!(
                graph.source_revisions[&tx(id)],
                vec![(0, TrstBasis::Remainder(revision))]
            );
        }
        assert_eq!(graph.revisions.len(), 1);
        graph.apply_unrevocation(&tx(1));
        assert_eq!(graph.revisions.len(), 2);
        let first = graph.apply_revocation(tx(1));
        assert_eq!(
            first.len(),
            1,
            "clean remerges never consumed this restored origin"
        );
        assert_eq!(graph.revisions.len(), 3);
        let second = graph.apply_revocation(tx(2));
        assert_eq!(second.len(), 101);
        assert!(second.iter().all(|cut| cut.tainted_amount == 60));
        let bytes = graph.to_bytes();
        for _ in 0..4 {
            graph = MergerGraph::from_bytes(&bytes).unwrap();
            assert_eq!(graph.to_bytes(), bytes);
        }
    }

    #[test]
    fn revision_snapshots_reject_legacy_trailing_and_dangling_history() {
        let mut graph = MergerGraph::new();
        graph.record_merge(node(tx(20), &[(tx(1), 40), (tx(2), 60)], 100));
        graph.apply_revocation(tx(1));
        graph.record_merge(node(tx(30), &[(tx(20), 60), (tx(3), 40)], 100));
        assert!(MergerGraph::from_bytes(&bincode::serialize(&graph).unwrap()).is_err());
        let mut trailing = graph.to_bytes();
        trailing.push(0);
        assert!(MergerGraph::from_bytes(&trailing).is_err());
        let revision = graph.current_revisions[&tx(20)];
        for mutation in 0..4 {
            let mut corrupted = graph.clone();
            match mutation {
                0 => {
                    corrupted.revisions.get_mut(&revision).unwrap().remaining += 1;
                }
                1 => {
                    corrupted.current_revisions.insert(tx(20), tx(99));
                }
                2 => {
                    corrupted.source_revisions.get_mut(&tx(30)).unwrap()[0].1 =
                        TrstBasis::Remainder(tx(99));
                }
                _ => {
                    let mut missing = corrupted.revisions.remove(&revision).unwrap();
                    missing.previous = tx(99);
                    let replacement = missing.hash();
                    corrupted.revisions.insert(replacement, missing);
                    corrupted.current_revisions.insert(tx(20), replacement);
                    corrupted.source_revisions.get_mut(&tx(30)).unwrap()[0].1 =
                        TrstBasis::Remainder(replacement);
                }
            }
            assert!(
                MergerGraph::from_bytes(&corrupted.to_bytes()).is_err(),
                "mutation {mutation}"
            );
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
