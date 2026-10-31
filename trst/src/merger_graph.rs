//! Forward index from burn origins to the merges that consumed them.
//!
//! Each merge records its immediate inputs. A source can be a burn or an earlier
//! merge; ancestor lists are not copied into every descendant. Forward edges
//! let revocation find the affected merges, and the engine's holder index locates
//! their current tokens.
//!
//! ```text
//! burn -> consuming merges -> downstream merges
//! ```
//!
//! Revocation traverses this affected graph and records contributions and
//! composition revisions. Later token operations use that state. Basis checks
//! may traverse revision history, so they are not universally constant-time.

use crate::{SharedMap, SharedSet};
use bincode::Options;
use burst_types::{TrstBasis, TxHash};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

const SNAPSHOT_MAGIC: &[u8; 8] = b"BRSTMG03";

mod edges;
use edges::EdgeList;
mod appended_validation;
mod comparison;
mod records;
mod revision_index;
mod snapshot_validation;
mod taint_lookup;
pub use records::{GraphRecord, GraphRecordReader};
pub(crate) use revision_index::RevisionIndex;

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

#[cfg(test)]
fn sorted_set<S: serde::Serializer>(
    set: &HashSet<TxHash>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    set.iter()
        .map(TxHash::as_bytes)
        .collect::<std::collections::BTreeSet<_>>()
        .serialize(serializer)
}

/// A merge and the immediate sources it consumed.
///
/// `source_origins` lists the merge's immediate inputs only. Current holders of
/// tokens descended from this merge are tracked by the engine's origin-holders
/// index. Recording holders here would leave stale entries after transfers.
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
    /// These contributions bound later revocations by the remaining amount and
    /// identify the graph contribution to remove when an origin is restored.
    #[serde(default, serialize_with = "sorted_map")]
    pub revoked_contribs: HashMap<TxHash, u128>,
}

#[cfg(test)]
thread_local! {
    pub(crate) static REVISION_VISITS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static REVOCATION_TOTAL_PROBE: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

impl MergeNode {
    /// Sum of all amounts already revoked from this merge.
    pub fn revoked_total(&self) -> u128 {
        #[cfg(test)]
        REVOCATION_TOTAL_PROBE.with(|probe| {
            if let Some(count) = probe.get() {
                probe.set(Some(count + self.revoked_contribs.len()));
            }
        });
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

/// Maps origins to their immediate consumers and downstream merges.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MergerGraph {
    /// Maps an input origin (burn tx or merge tx) to all merges that consumed it.
    origin_to_merges: SharedMap<TxHash, EdgeList>,

    /// Maps a merge TxHash to its full merge node data.
    merge_nodes: SharedMap<TxHash, MergeNode>,

    /// Maps a merge TxHash to downstream merges that consumed it.
    merge_to_downstream: SharedMap<TxHash, EdgeList>,

    /// Set of burn origins that are currently revoked.
    revoked_origins: SharedSet<TxHash>,

    revisions: SharedMap<TxHash, RevocationRevision>,
    current_revisions: SharedMap<TxHash, TxHash>,
    /// Child merge -> immediate source/revision pairs, only for changed sources.
    source_revisions: SharedMap<TxHash, Vec<(usize, TrstBasis)>>,
}

/// Exact ceil-divide, capped at `amount`: `ceil(amount * num / den)`.
/// Fractional revocations round up to the next raw unit.
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
pub struct MergeRevocation {
    /// The merge whose descendants contain tainted TRST.
    pub merge_tx: TxHash,
    /// Amount of this merge attributable to the revoked origin.
    pub tainted_amount: u128,
    /// Sum of contributions revoked before this event. Token cuts also depend
    /// on each token's composition basis; this is not a universal denominator.
    pub prior_revoked: u128,
    /// Total amount of the merged token at merge time.
    pub total_amount: u128,
}

/// Amount restored when a burn origin becomes valid again.
#[derive(Clone, Debug)]
pub struct MergeRestoration {
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

    /// Record a merge using each source's current composition revision.
    ///
    /// The caller must validate the merge and supply a new transaction hash.
    /// Parent merges must already be present for their downstream edges to be
    /// recorded. This method does not check ownership, amounts, or cycles.
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

    /// Append one downstream edge occurrence without validating either endpoint.
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

    /// Immediate consumers of a burn or merge origin, from the retained forward index.
    /// Duplicate input edges are preserved; callers walking a closure must deduplicate.
    pub fn direct_consumers<'a>(&'a self, origin: &TxHash) -> impl Iterator<Item = &'a TxHash> {
        self.origin_to_merges
            .get(origin)
            .into_iter()
            .flat_map(|edges| edges.iter())
    }

    /// Collect all merges affected by an origin: its direct consumers plus the
    /// full downstream closure, in a topological order (parents before children).
    ///
    /// Requires a validated acyclic graph with matching source and downstream
    /// edge counts. This traversal does not detect malformed graphs. Independent
    /// branches have no specified order.
    fn affected_merges_topo(&self, origin: &TxHash) -> Vec<TxHash> {
        let mut affected: HashSet<TxHash> = HashSet::new();
        let mut queue: VecDeque<TxHash> = VecDeque::new();

        if let Some(direct) = self.origin_to_merges.get(origin) {
            for &merge in direct.iter() {
                if affected.insert(merge) {
                    queue.push_back(merge);
                }
            }
        }
        while let Some(merge) = queue.pop_front() {
            if let Some(children) = self.merge_to_downstream.get(&merge) {
                for &child in children.iter() {
                    if affected.insert(child) {
                        queue.push_back(child);
                    }
                }
            }
        }

        // Kahn's algorithm restricted to the affected set.
        let mut in_degree: HashMap<TxHash, usize> = HashMap::new();
        for &merge in &affected {
            let node = match self.merge_nodes.get(&merge) {
                Some(node) => node,
                None => continue,
            };
            let degree = node
                .source_origins
                .iter()
                .filter(|source| affected.contains(&source.origin))
                .count();
            in_degree.insert(merge, degree);
        }

        let mut ready: VecDeque<TxHash> = in_degree
            .iter()
            .filter(|(_, &degree)| degree == 0)
            .map(|(&merge, _)| merge)
            .collect();
        let mut order = Vec::with_capacity(affected.len());
        while let Some(merge) = ready.pop_front() {
            order.push(merge);
            if let Some(children) = self.merge_to_downstream.get(&merge) {
                for child in children.iter() {
                    if let Some(degree) = in_degree.get_mut(child) {
                        *degree = degree.saturating_sub(1);
                        if *degree == 0 {
                            ready.push_back(*child);
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
    /// A matching single-origin source contributes its full amount. Other merged
    /// sources contribute a rounded-up fraction using their recorded composition
    /// basis. A remainder excludes origins already revoked at that revision and
    /// uses the revision's remaining amount as its denominator. Each node's taint
    /// is capped at its total amount.
    fn compute_taints(&self, origin: &TxHash) -> Vec<(TxHash, u128)> {
        let order = self.affected_merges_topo(origin);
        let mut taint: HashMap<TxHash, u128> = HashMap::new();
        let mut taints = Vec::new();
        let mut revision_taints = HashMap::new();

        for merge in order {
            let node = match self.merge_nodes.get(&merge) {
                Some(node) => node,
                None => continue,
            };
            let bases = taint_lookup::source_bases(
                node.source_origins.len(),
                self.source_revisions
                    .get(&merge)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            );
            let mut tainted_amount: u128 = 0;
            for (index, source) in node.source_origins.iter().enumerate() {
                let basis = bases[index].unwrap_or_default();
                if let TrstBasis::Single(single) = basis {
                    if single == *origin {
                        tainted_amount = tainted_amount.saturating_add(source.amount);
                    }
                } else if source.origin == *origin {
                    tainted_amount = tainted_amount.saturating_add(source.amount);
                } else if let (Some(&parent_taint), Some(parent)) = (
                    taint.get(&source.origin),
                    self.merge_nodes.get(&source.origin),
                ) {
                    let denominator = match basis {
                        TrstBasis::Remainder(revision) => {
                            if self.revoked_at_cached(revision, origin, &mut revision_taints) {
                                continue;
                            }
                            self.revisions[&revision].remaining
                        }
                        _ => parent.total_amount,
                    };
                    tainted_amount = tainted_amount.saturating_add(ceil_proportion(
                        source.amount,
                        parent_taint,
                        denominator,
                    ));
                }
            }
            let tainted_amount = tainted_amount.min(node.total_amount);
            if tainted_amount > 0 {
                taint.insert(merge, tainted_amount);
                taints.push((merge, tainted_amount));
            }
        }
        taints
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

    #[cfg(test)]
    fn record_revision(&mut self, merge_tx: TxHash, origin: TxHash, revoked: bool) {
        let node = &self.merge_nodes[&merge_tx];
        self.record_revision_remaining(
            merge_tx,
            origin,
            revoked,
            node.total_amount.saturating_sub(node.revoked_total()),
        );
    }

    fn record_revision_remaining(
        &mut self,
        merge_tx: TxHash,
        origin: TxHash,
        revoked: bool,
        remaining: u128,
    ) {
        let revision = RevocationRevision {
            merge_tx,
            origin,
            revoked,
            previous: self
                .current_revisions
                .get(&merge_tx)
                .copied()
                .unwrap_or(TxHash::ZERO),
            remaining,
        };
        let hash = revision.hash();
        self.revisions.insert(hash, revision);
        self.current_revisions.insert(merge_tx, hash);
    }

    /// A validated graph's reverse edges cover every direct burn source.
    pub(crate) fn references_burn_origin(&self, origin: &TxHash) -> bool {
        self.is_origin_revoked(origin)
            || self
                .origin_to_merges
                .get(origin)
                .is_some_and(|edges| !edges.is_empty())
    }

    /// Changed domains of `valid_basis`; contribution amounts and revision ancestry
    /// do not affect that predicate. Callers must also check newly supplied bases.
    pub fn basis_validation_changes(&self, before: &Self) -> (Vec<TxHash>, Vec<TxHash>) {
        let merges = self
            .merge_nodes
            .changed_keys(&before.merge_nodes)
            .into_iter()
            .filter(|id| self.merge_nodes.contains_key(id) != before.merge_nodes.contains_key(id))
            .copied()
            .collect();
        let revisions = self
            .revisions
            .changed_keys(&before.revisions)
            .into_iter()
            .filter(|id| {
                let domain = |graph: &Self| {
                    graph
                        .revisions
                        .get(id)
                        .map(|r| (r.merge_tx, r.remaining > 0))
                };
                domain(self) != domain(before)
            })
            .copied()
            .collect();
        (merges, revisions)
    }

    /// Check the basis form and referenced revision domain. A single-origin
    /// basis on a merge is accepted without proving that origin is an ancestor.
    /// Ownership and provenance require the surrounding ledger validation.
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

    #[cfg(test)]
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
        self.basis_cut_using(origin, basis, amount, revoked, tainted, |revision| {
            self.revoked_at(revision, revoked)
        })
    }

    fn basis_cut_using(
        &self,
        origin: &TxHash,
        basis: TrstBasis,
        amount: u128,
        revoked: &TxHash,
        tainted: u128,
        mut excludes: impl FnMut(TxHash) -> bool,
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
                if excludes(revision) {
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

    /// Retain a reduced token composition without changing the merge's current
    /// revision. The supplied basis must belong to a validated graph.
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
    /// Requires a validated graph. Records each positive contribution and returns
    /// an event for the engine to apply to holdings and pending transfers. This
    /// method changes graph state only. An already revoked origin produces no events.
    pub fn apply_revocation(&mut self, origin: TxHash) -> Vec<MergeRevocation> {
        self.apply_revocation_with_totals(origin, &mut HashMap::new())
    }

    /// Operation-local totals for sequential revocations only. Start empty and
    /// discard before any merge insertion/replacement or unrevocation. Portfolio
    /// splitting may add basis revisions but cannot change node contributions.
    pub(crate) fn apply_revocation_with_totals(
        &mut self,
        origin: TxHash,
        totals: &mut HashMap<TxHash, u128>,
    ) -> Vec<MergeRevocation> {
        if !self.revoked_origins.insert(origin) {
            return Vec::new();
        }
        let taints = self.compute_taints(&origin);
        let mut events = Vec::with_capacity(taints.len());
        for (merge_tx, tainted) in taints {
            if let Some(node) = self.merge_nodes.get_mut(&merge_tx) {
                let prior = *totals
                    .entry(merge_tx)
                    .or_insert_with(|| node.revoked_total());
                let tainted = tainted.min(node.total_amount.saturating_sub(prior));
                if tainted == 0 {
                    continue;
                }
                node.revoked_contribs.insert(origin, tainted);
                events.push(MergeRevocation {
                    merge_tx,
                    tainted_amount: tainted,
                    prior_revoked: prior,
                    total_amount: node.total_amount,
                });
                let revoked = prior.saturating_add(tainted);
                let remaining = node.total_amount.saturating_sub(revoked);
                totals.insert(merge_tx, revoked);
                self.record_revision_remaining(merge_tx, origin, true, remaining);
            }
        }
        events
    }

    /// Clear a burn origin's revocation flag and its revoked-contribution entries
    /// in affected merges, recording new composition revisions. Returns removed
    /// contributions for the engine to restore holdings; this method changes
    /// graph state only. An origin that is not revoked produces no events.
    pub fn apply_restoration(&mut self, origin: &TxHash) -> Vec<MergeRestoration> {
        self.apply_restoration_with_totals(origin, &mut HashMap::new())
    }

    /// Reuse totals across sequential restorations of validated nodes. Start empty
    /// and discard before any merge insertion, replacement, or revocation.
    pub(crate) fn apply_restoration_with_totals(
        &mut self,
        origin: &TxHash,
        totals: &mut HashMap<TxHash, u128>,
    ) -> Vec<MergeRestoration> {
        if !self.revoked_origins.remove(origin) {
            return Vec::new();
        }
        let order = self.affected_merges_topo(origin);
        let mut events = Vec::new();
        for merge in order {
            if let Some(node) = self.merge_nodes.get_mut(&merge) {
                if let Some(restored) = node.revoked_contribs.remove(origin) {
                    events.push(MergeRestoration {
                        merge_tx: merge,
                        restored_amount: restored,
                    });
                    let total = totals
                        .entry(merge)
                        .and_modify(|total| *total = total.saturating_sub(restored))
                        .or_insert_with(|| node.revoked_total());
                    let remaining = node.total_amount.saturating_sub(*total);
                    self.record_revision_remaining(merge, *origin, false, remaining);
                }
            }
        }
        events
    }

    /// Look up current revocation status without traversing merge ancestry.
    pub fn is_origin_revoked(&self, origin: &TxHash) -> bool {
        self.revoked_origins.contains(origin)
    }

    /// Get the set of all currently revoked burn origins.
    pub fn revoked_origins(&self) -> &SharedSet<TxHash> {
        &self.revoked_origins
    }

    /// Whether a merge has any recorded revoked contributions.
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

    /// Write a complete snapshot. The caller must bound the sink if needed;
    /// this method imposes no output-size limit.
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

    /// Decode a canonical snapshot of at most 64 MiB and validate graph structure
    /// and revision references. Holdings and ledger ownership are checked elsewhere.
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

    fn validate_revision(
        &self,
        hash: &TxHash,
        revision: &RevocationRevision,
    ) -> Result<(), String> {
        if *hash != revision.hash()
            || revision.origin == TxHash::ZERO
            || !self
                .merge_nodes
                .get(&revision.merge_tx)
                .is_some_and(|node| revision.remaining <= node.total_amount)
            || (revision.previous != TxHash::ZERO
                && !self
                    .revisions
                    .get(&revision.previous)
                    .is_some_and(|parent| parent.merge_tx == revision.merge_tx))
        {
            return Err("invalid merger graph revision".into());
        }
        Ok(())
    }

    fn validate_current_revision(&self, merge: &TxHash, revision: &TxHash) -> Result<(), String> {
        if !self
            .revisions
            .get(revision)
            .is_some_and(|r| r.merge_tx == *merge)
        {
            return Err("invalid current merger graph revision".into());
        }
        Ok(())
    }

    pub(crate) fn validate_persisted(&self) -> Result<(), String> {
        self.validate_persisted_with_poll(&|_| Ok(()))
    }

    pub(crate) fn validate_persisted_with_poll(
        &self,
        poll: &dyn Fn(&'static str) -> Result<(), String>,
    ) -> Result<(), String> {
        let graph = self;
        graph.validate_structure_with_poll(poll)?;
        for (hash, revision) in &graph.revisions {
            poll("trst:graph-revision")?;
            graph.validate_revision(hash, revision)?;
        }
        for (merge_tx, revision) in &graph.current_revisions {
            poll("trst:graph-current-revision")?;
            graph.validate_current_revision(merge_tx, revision)?;
        }
        for (merge_tx, sources) in &graph.source_revisions {
            poll("trst:graph-source-revisions")?;
            let node = graph
                .merge_nodes
                .get(merge_tx)
                .ok_or("missing revised merge")?;
            for pair in sources.windows(2) {
                poll("trst:graph-source-order")?;
                if pair[0].0 >= pair[1].0 {
                    return Err("noncanonical merge source revisions".into());
                }
            }
            for (index, basis) in sources {
                poll("trst:graph-source-basis")?;
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
    fn batched_revocation_totals_match_sequential_rounding_and_revisions() {
        // Compare cached totals with repeated summation. The reference shares
        // taint propagation, so this checks the cache, not an independent model
        // of provenance or ledger settlement.
        fn reference(graph: &mut MergerGraph, origin: TxHash) -> Vec<MergeRevocation> {
            if !graph.revoked_origins.insert(origin) {
                return Vec::new();
            }
            let taints = graph.compute_taints(&origin);
            let mut events = Vec::new();
            for (merge_tx, tainted) in taints {
                let node = graph.merge_nodes.get_mut(&merge_tx).unwrap();
                let prior = node.revoked_total();
                let tainted = tainted.min(node.total_amount.saturating_sub(prior));
                if tainted == 0 {
                    continue;
                }
                node.revoked_contribs.insert(origin, tainted);
                events.push(MergeRevocation {
                    merge_tx,
                    tainted_amount: tainted,
                    prior_revoked: prior,
                    total_amount: node.total_amount,
                });
                graph.record_revision(merge_tx, origin, true);
            }
            events
        }
        let mut graph = MergerGraph::new();
        let sources: Vec<_> = (1..=64).map(|id| (tx(id), 7)).collect();
        graph.record_merge(node(tx(200), &sources, 448));
        graph.record_merge(node(tx(201), &[(tx(200), 223), (tx(90), 37)], 260));
        graph.record_merge(node(tx(202), &[(tx(201), 101), (tx(91), 17)], 118));
        graph.apply_revocation(tx(1));
        let baseline = graph.clone();
        let mut expected = baseline.clone();
        let mut totals = HashMap::new();
        let normalize = |events: Vec<MergeRevocation>| {
            let mut events: Vec<_> = events
                .into_iter()
                .map(|e| {
                    (
                        e.merge_tx,
                        e.tainted_amount,
                        e.prior_revoked,
                        e.total_amount,
                    )
                })
                .collect();
            events.sort();
            events
        };
        let mut scanned = 0;
        let mut reference_scanned = 0;
        for id in 2..=64 {
            REVOCATION_TOTAL_PROBE.with(|p| p.set(Some(0)));
            let actual = graph.apply_revocation_with_totals(tx(id), &mut totals);
            scanned += REVOCATION_TOTAL_PROBE.with(|p| p.replace(None).unwrap());
            REVOCATION_TOTAL_PROBE.with(|p| p.set(Some(0)));
            let wanted = reference(&mut expected, tx(id));
            reference_scanned += REVOCATION_TOTAL_PROBE.with(|p| p.replace(None).unwrap());
            assert_eq!(normalize(actual), normalize(wanted));
            assert_eq!(graph.to_bytes(), expected.to_bytes());
        }
        assert_eq!(
            scanned, 3,
            "each affected merge sums its initial contribution once"
        );
        assert!(
            reference_scanned > 1_000,
            "fixture must expose repeated summation"
        );
        assert_eq!(totals.len(), 3);
        assert!(graph
            .apply_revocation_with_totals(tx(64), &mut totals)
            .is_empty());
        assert_eq!(baseline.revoked_origins.len(), 1);
        fn restore_reference(graph: &mut MergerGraph, origin: TxHash) -> Vec<(TxHash, u128)> {
            if !graph.revoked_origins.remove(&origin) {
                return Vec::new();
            }
            let mut events = Vec::new();
            for merge_tx in graph.affected_merges_topo(&origin) {
                if let Some(restored) = graph
                    .merge_nodes
                    .get_mut(&merge_tx)
                    .unwrap()
                    .revoked_contribs
                    .remove(&origin)
                {
                    events.push((merge_tx, restored));
                    graph.record_revision(merge_tx, origin, false);
                }
            }
            events.sort();
            events
        }
        let expected_scanned: usize = graph
            .merge_nodes
            .values()
            .map(|node| node.revoked_contribs.len() - 1)
            .sum();
        let mut restoration_totals = HashMap::new();
        let mut scanned = 0;
        let mut reference_scanned = 0;
        for id in (2..=64).rev() {
            REVOCATION_TOTAL_PROBE.with(|p| p.set(Some(0)));
            let mut actual: Vec<_> = graph
                .apply_restoration_with_totals(&tx(id), &mut restoration_totals)
                .into_iter()
                .map(|e| (e.merge_tx, e.restored_amount))
                .collect();
            scanned += REVOCATION_TOTAL_PROBE.with(|p| p.replace(None).unwrap());
            actual.sort();
            REVOCATION_TOTAL_PROBE.with(|p| p.set(Some(0)));
            let wanted = restore_reference(&mut expected, tx(id));
            reference_scanned += REVOCATION_TOTAL_PROBE.with(|p| p.replace(None).unwrap());
            assert_eq!(actual, wanted);
            assert_eq!(graph.to_bytes(), expected.to_bytes());
        }
        assert_eq!(scanned, expected_scanned);
        assert!(scanned <= 192);
        assert!(reference_scanned > 1_000);
        assert!(graph
            .apply_restoration_with_totals(&tx(2), &mut restoration_totals)
            .is_empty());
        totals.clear();
        assert_eq!(
            normalize(graph.apply_revocation_with_totals(tx(2), &mut totals)),
            normalize(reference(&mut expected, tx(2)))
        );
        assert_eq!(graph.to_bytes(), expected.to_bytes());
    }

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
        graph.apply_restoration(&tx(1));
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
        // The merged token's origin is m1, so a second-level merge
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

        // Half of the 60 units consumed from m1 are tainted.
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
    fn proportional_revocation_rounds_up() {
        let mut graph = MergerGraph::new();
        let (o1, o2, m1, m2) = (tx(1), tx(2), tx(10), tx(20));

        graph.record_merge(node(m1, &[(o1, 1), (o2, 2)], 3));
        graph.record_merge(node(m2, &[(m1, 2), (o2, 8)], 10));

        let events = graph.apply_revocation(o1);
        let m2_ev = events.iter().find(|e| e.merge_tx == m2).unwrap();
        // Two thirds of a raw unit rounds up to one.
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
    fn restoration_removes_revoked_contributions() {
        let mut graph = MergerGraph::new();
        let (o1, o2, m1) = (tx(1), tx(2), tx(10));
        graph.record_merge(node(m1, &[(o1, 50), (o2, 50)], 100));

        graph.apply_revocation(o1);
        assert!(graph.is_origin_revoked(&o1));
        assert!(graph.merge_has_revoked_origins(&m1));

        let restored = graph.apply_restoration(&o1);
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
