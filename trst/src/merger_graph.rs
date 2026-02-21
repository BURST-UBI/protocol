//! The Merger Graph — forward index for proactive revocation.
//!
//! Normal transaction chains are backward-linked (holder → link → origin).
//! The merger graph is the **inverse**: origin → [merges containing it] → current balances.
//!
//! Without the merger graph, every transaction requires O(n) backward traversal to check
//! for revoked origins. With it, revocation is a one-time O(k) forward traversal at catch time,
//! and every subsequent transaction is O(1).

use burst_types::{TxHash, WalletAddress};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// A node in the merger graph representing a merge operation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeNode {
    /// Hash of the merge transaction.
    pub merge_tx: TxHash,
    /// Origins that were consumed by this merge, with their amounts.
    pub source_origins: Vec<MergeSource>,
    /// Total amount of the merged token.
    pub total_amount: u128,
    /// Current holder of this merged token.
    pub holder: WalletAddress,
}

/// One source in a merge operation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MergeSource {
    pub origin: TxHash,
    pub amount: u128,
}

/// The merger graph — maps origins forward to all current live balances.
///
/// ```text
/// origin (burn) → [merges containing it] → [merges of merges] → current balances
/// ```
#[derive(Serialize, Deserialize)]
pub struct MergerGraph {
    /// Maps an origin TxHash to all merge nodes that consumed it.
    origin_to_merges: HashMap<TxHash, Vec<TxHash>>,

    /// Maps a merge TxHash to its full merge node data.
    merge_nodes: HashMap<TxHash, MergeNode>,

    /// Maps a merge TxHash to downstream merges that consumed it.
    merge_to_downstream: HashMap<TxHash, Vec<TxHash>>,

    /// Set of origin TxHashes that are currently revoked.
    /// Used to determine whether a merged token can be un-revoked
    /// (all its constituent origins must be non-revoked).
    revoked_origins: HashSet<TxHash>,
}

impl MergerGraph {
    pub fn new() -> Self {
        Self {
            origin_to_merges: HashMap::new(),
            merge_nodes: HashMap::new(),
            merge_to_downstream: HashMap::new(),
            revoked_origins: HashSet::new(),
        }
    }

    /// Record a new merge operation in the graph.
    ///
    /// Automatically links downstream when a source origin is itself a merge
    /// (i.e., when a merged token is consumed by a subsequent merge). This
    /// removes the need for callers to manually call `record_downstream()` for
    /// multi-level merge chains.
    pub fn record_merge(&mut self, node: MergeNode) {
        let merge_tx = node.merge_tx;
        for source in &node.source_origins {
            self.origin_to_merges
                .entry(source.origin)
                .or_default()
                .push(merge_tx);

            // Auto-downstream: if this source origin is itself a merge tx,
            // link the parent merge to this new merge. This happens when a
            // merged token (whose origin = its merge_tx) is consumed again.
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

    /// Propagate a revocation from a single origin forward through the graph.
    ///
    /// Returns all affected (wallet, amount_to_revoke) pairs for proportional splitting.
    pub fn propagate_revocation(&self, revoked_origin: &TxHash) -> Vec<RevocationEvent> {
        let mut affected = Vec::new();
        let mut visited = HashSet::new();

        if let Some(merges) = self.origin_to_merges.get(revoked_origin) {
            for &merge_tx in merges {
                self.traverse_forward(merge_tx, revoked_origin, &mut affected, &mut visited);
            }
        }

        affected
    }

    /// Recursively traverse the merger graph forward to find all affected balances.
    ///
    /// A node is tainted if it directly references the revoked origin, or if one
    /// of its sources is a previously-visited (tainted) merge. This handles the
    /// auto-downstream case where a merged token (whose origin = its merge_tx)
    /// is consumed by a subsequent merge.
    fn traverse_forward(
        &self,
        merge_tx: TxHash,
        revoked_origin: &TxHash,
        affected: &mut Vec<RevocationEvent>,
        visited: &mut HashSet<TxHash>,
    ) {
        if !visited.insert(merge_tx) {
            return;
        }

        if let Some(node) = self.merge_nodes.get(&merge_tx) {
            // Direct contribution: amount sourced from the revoked origin itself.
            let direct_revoked: u128 = node
                .source_origins
                .iter()
                .filter(|s| s.origin == *revoked_origin)
                .map(|s| s.amount)
                .sum();

            // Indirect contribution: amount sourced from a tainted parent merge
            // (reached via auto-downstream linking). A source whose origin is
            // already in the visited set is a tainted upstream merge.
            let indirect_revoked: u128 = if direct_revoked == 0 {
                node.source_origins
                    .iter()
                    .filter(|s| s.origin != *revoked_origin && visited.contains(&s.origin))
                    .map(|s| s.amount)
                    .sum()
            } else {
                0
            };

            let revoked_amount = direct_revoked + indirect_revoked;

            if revoked_amount > 0 {
                // Check if there are downstream merges.
                if let Some(downstream) = self.merge_to_downstream.get(&merge_tx) {
                    for &child in downstream {
                        self.traverse_forward(child, revoked_origin, affected, visited);
                    }
                } else {
                    // This is a leaf — a current live balance.
                    affected.push(RevocationEvent {
                        holder: node.holder.clone(),
                        merge_tx,
                        revoked_amount,
                        total_amount: node.total_amount,
                    });
                }
            }
        }
    }

    /// Get all current holders affected by a specific origin.
    pub fn get_affected_holders(&self, origin: &TxHash) -> Vec<WalletAddress> {
        self.propagate_revocation(origin)
            .into_iter()
            .map(|e| e.holder)
            .collect()
    }

    /// Mark an origin as revoked in the graph.
    pub fn mark_origin_revoked(&mut self, origin: TxHash) {
        self.revoked_origins.insert(origin);
    }

    /// Remove revocation mark for an origin (wallet re-verified).
    pub fn mark_origin_unrevoked(&mut self, origin: &TxHash) {
        self.revoked_origins.remove(origin);
    }

    /// Check if a specific origin is currently revoked.
    pub fn is_origin_revoked(&self, origin: &TxHash) -> bool {
        self.revoked_origins.contains(origin)
    }

    /// Get the set of all currently revoked origin hashes.
    pub fn revoked_origins(&self) -> &HashSet<TxHash> {
        &self.revoked_origins
    }

    /// Serialize the entire graph to bytes for LMDB persistence.
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).expect("merger graph serialization should not fail")
    }

    /// Deserialize a graph from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        bincode::deserialize(bytes).map_err(|e| e.to_string())
    }

    /// Check whether a merge node has ANY remaining revoked origins.
    ///
    /// A merged token can only be restored to Active if none of its
    /// constituent origins are still revoked.
    pub fn merge_has_revoked_origins(&self, merge_tx: &TxHash) -> bool {
        if let Some(node) = self.merge_nodes.get(merge_tx) {
            node.source_origins
                .iter()
                .any(|s| self.revoked_origins.contains(&s.origin))
        } else {
            false
        }
    }

    /// Propagate un-revocation from an origin forward through the graph.
    ///
    /// Returns all merge nodes that are now fully clean (no remaining
    /// revoked origins) and can be restored to Active.
    pub fn propagate_unrevocation(&self, unrevoked_origin: &TxHash) -> Vec<UnRevocationEvent> {
        let mut restored = Vec::new();
        let mut visited = HashSet::new();

        if let Some(merges) = self.origin_to_merges.get(unrevoked_origin) {
            for &merge_tx in merges {
                self.traverse_forward_unrevoke(merge_tx, &mut restored, &mut visited);
            }
        }

        restored
    }

    /// Recursively traverse forward to find merge nodes that can be restored.
    fn traverse_forward_unrevoke(
        &self,
        merge_tx: TxHash,
        restored: &mut Vec<UnRevocationEvent>,
        visited: &mut HashSet<TxHash>,
    ) {
        if !visited.insert(merge_tx) {
            return;
        }

        if let Some(node) = self.merge_nodes.get(&merge_tx) {
            let still_has_revoked = node
                .source_origins
                .iter()
                .any(|s| self.revoked_origins.contains(&s.origin));

            if !still_has_revoked {
                if let Some(downstream) = self.merge_to_downstream.get(&merge_tx) {
                    for &child in downstream {
                        self.traverse_forward_unrevoke(child, restored, visited);
                    }
                } else {
                    restored.push(UnRevocationEvent {
                        holder: node.holder.clone(),
                        merge_tx,
                        total_amount: node.total_amount,
                    });
                }
            }
        }
    }
}

impl Default for MergerGraph {
    fn default() -> Self {
        Self::new()
    }
}

/// A single revocation event — tells a holder how much TRST to invalidate.
#[derive(Clone, Debug)]
pub struct RevocationEvent {
    /// The wallet holding the affected merged token.
    pub holder: WalletAddress,
    /// The merge transaction containing the tainted TRST.
    pub merge_tx: TxHash,
    /// Amount of TRST to revoke (proportional to the revoked origin's share).
    pub revoked_amount: u128,
    /// Total amount of the merged token (for computing proportions).
    pub total_amount: u128,
}

/// An un-revocation event — a merged token that can be restored to Active.
#[derive(Clone, Debug)]
pub struct UnRevocationEvent {
    /// The wallet holding the restored merged token.
    pub holder: WalletAddress,
    /// The merge transaction that is now clean.
    pub merge_tx: TxHash,
    /// Total amount of the restored merged token.
    pub total_amount: u128,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tx(id: u8) -> TxHash {
        TxHash::new([id; 32])
    }

    fn wallet(name: &str) -> WalletAddress {
        WalletAddress::new(format!("brst_{name}"))
    }

    #[test]
    fn record_merge_indexes_by_origin() {
        let mut graph = MergerGraph::new();
        let origin1 = tx(1);
        let origin2 = tx(2);
        let merge1 = tx(10);

        graph.record_merge(MergeNode {
            merge_tx: merge1,
            source_origins: vec![
                MergeSource {
                    origin: origin1,
                    amount: 50,
                },
                MergeSource {
                    origin: origin2,
                    amount: 50,
                },
            ],
            total_amount: 100,
            holder: wallet("alice"),
        });

        assert!(graph.origin_to_merges.contains_key(&origin1));
        assert!(graph.origin_to_merges.contains_key(&origin2));
        assert_eq!(graph.origin_to_merges[&origin1], vec![merge1]);
        assert_eq!(graph.origin_to_merges[&origin2], vec![merge1]);
    }

    #[test]
    fn auto_downstream_two_level_merge() {
        let mut graph = MergerGraph::new();

        let origin1 = tx(1);
        let origin2 = tx(2);
        let merge1 = tx(10);

        // Level 1: merge origin1 + origin2 → merge1
        graph.record_merge(MergeNode {
            merge_tx: merge1,
            source_origins: vec![
                MergeSource {
                    origin: origin1,
                    amount: 50,
                },
                MergeSource {
                    origin: origin2,
                    amount: 50,
                },
            ],
            total_amount: 100,
            holder: wallet("alice"),
        });

        // Level 2: merge merge1 (as origin) + origin3 → merge2
        // When a merged token is consumed, its origin = its merge_tx.
        let origin3 = tx(3);
        let merge2 = tx(20);

        graph.record_merge(MergeNode {
            merge_tx: merge2,
            source_origins: vec![
                MergeSource {
                    origin: merge1,
                    amount: 60,
                },
                MergeSource {
                    origin: origin3,
                    amount: 40,
                },
            ],
            total_amount: 100,
            holder: wallet("bob"),
        });

        // Auto-downstream should have linked merge1 → merge2
        assert!(graph.merge_to_downstream.contains_key(&merge1));
        assert_eq!(graph.merge_to_downstream[&merge1], vec![merge2]);
    }

    #[test]
    fn auto_downstream_three_level_merge() {
        let mut graph = MergerGraph::new();

        let origin1 = tx(1);
        let origin2 = tx(2);
        let merge1 = tx(10);
        let merge2 = tx(20);

        // Level 1: merge origin1 + origin2 → merge1
        graph.record_merge(MergeNode {
            merge_tx: merge1,
            source_origins: vec![
                MergeSource {
                    origin: origin1,
                    amount: 50,
                },
                MergeSource {
                    origin: origin2,
                    amount: 50,
                },
            ],
            total_amount: 100,
            holder: wallet("alice"),
        });

        // Level 2: merge merge1 + origin3 → merge2
        let origin3 = tx(3);
        graph.record_merge(MergeNode {
            merge_tx: merge2,
            source_origins: vec![
                MergeSource {
                    origin: merge1,
                    amount: 60,
                },
                MergeSource {
                    origin: origin3,
                    amount: 40,
                },
            ],
            total_amount: 100,
            holder: wallet("bob"),
        });

        // Level 3: merge merge2 + origin4 → merge3
        let origin4 = tx(4);
        let merge3 = tx(30);
        graph.record_merge(MergeNode {
            merge_tx: merge3,
            source_origins: vec![
                MergeSource {
                    origin: merge2,
                    amount: 70,
                },
                MergeSource {
                    origin: origin4,
                    amount: 30,
                },
            ],
            total_amount: 100,
            holder: wallet("carol"),
        });

        // Verify downstream chain: merge1 → merge2 → merge3
        assert_eq!(graph.merge_to_downstream[&merge1], vec![merge2]);
        assert_eq!(graph.merge_to_downstream[&merge2], vec![merge3]);

        // Now revoke origin1 — should propagate through merge1 → merge2 → merge3
        graph.mark_origin_revoked(origin1);
        let events = graph.propagate_revocation(&origin1);

        // merge3 is the leaf — should get a revocation event
        // merge2 has downstream (merge3) so it's not a leaf
        // merge1 has downstream (merge2) so it's not a leaf
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].merge_tx, merge3);
        assert_eq!(events[0].holder, wallet("carol"));
    }

    #[test]
    fn no_auto_downstream_for_plain_origins() {
        let mut graph = MergerGraph::new();

        let origin1 = tx(1);
        let origin2 = tx(2);
        let merge1 = tx(10);

        graph.record_merge(MergeNode {
            merge_tx: merge1,
            source_origins: vec![
                MergeSource {
                    origin: origin1,
                    amount: 50,
                },
                MergeSource {
                    origin: origin2,
                    amount: 50,
                },
            ],
            total_amount: 100,
            holder: wallet("alice"),
        });

        // No downstream entries should exist — neither origin1 nor origin2 are merges
        assert!(graph.merge_to_downstream.is_empty());
    }

    #[test]
    fn revocation_propagates_through_auto_downstream() {
        let mut graph = MergerGraph::new();

        let origin1 = tx(1);
        let origin2 = tx(2);
        let merge1 = tx(10);

        // Level 1
        graph.record_merge(MergeNode {
            merge_tx: merge1,
            source_origins: vec![
                MergeSource {
                    origin: origin1,
                    amount: 50,
                },
                MergeSource {
                    origin: origin2,
                    amount: 50,
                },
            ],
            total_amount: 100,
            holder: wallet("alice"),
        });

        // Level 2: consume merge1
        let origin3 = tx(3);
        let merge2 = tx(20);
        graph.record_merge(MergeNode {
            merge_tx: merge2,
            source_origins: vec![
                MergeSource {
                    origin: merge1,
                    amount: 60,
                },
                MergeSource {
                    origin: origin3,
                    amount: 40,
                },
            ],
            total_amount: 100,
            holder: wallet("bob"),
        });

        // Revoke origin1 — affects merge1 → merge2
        graph.mark_origin_revoked(origin1);
        let events = graph.propagate_revocation(&origin1);

        // merge2 is the leaf (merge1 has downstream to merge2)
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].merge_tx, merge2);
        // merge2 sourced 60 from the tainted merge1
        assert_eq!(events[0].revoked_amount, 60);
    }

    #[test]
    fn unrevocation_propagates_through_auto_downstream() {
        let mut graph = MergerGraph::new();

        let origin1 = tx(1);
        let origin2 = tx(2);
        let merge1 = tx(10);

        // Level 1
        graph.record_merge(MergeNode {
            merge_tx: merge1,
            source_origins: vec![
                MergeSource {
                    origin: origin1,
                    amount: 50,
                },
                MergeSource {
                    origin: origin2,
                    amount: 50,
                },
            ],
            total_amount: 100,
            holder: wallet("alice"),
        });

        // Level 2: consume merge1
        let origin3 = tx(3);
        let merge2 = tx(20);
        graph.record_merge(MergeNode {
            merge_tx: merge2,
            source_origins: vec![
                MergeSource {
                    origin: merge1,
                    amount: 60,
                },
                MergeSource {
                    origin: origin3,
                    amount: 40,
                },
            ],
            total_amount: 100,
            holder: wallet("bob"),
        });

        // Revoke then unrevoke origin1
        graph.mark_origin_revoked(origin1);
        graph.mark_origin_unrevoked(&origin1);

        let restored = graph.propagate_unrevocation(&origin1);

        // merge2 should be restorable since origin1 is no longer revoked
        assert!(!restored.is_empty());
        assert_eq!(restored[0].merge_tx, merge2);
    }
}
