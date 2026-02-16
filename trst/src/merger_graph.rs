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
pub struct MergerGraph {
    /// Maps an origin TxHash to all merge nodes that consumed it.
    origin_to_merges: HashMap<TxHash, Vec<TxHash>>,

    /// Maps a merge TxHash to its full merge node data.
    merge_nodes: HashMap<TxHash, MergeNode>,

    /// Maps a merge TxHash to downstream merges that consumed it.
    merge_to_downstream: HashMap<TxHash, Vec<TxHash>>,
}

impl MergerGraph {
    pub fn new() -> Self {
        Self {
            origin_to_merges: HashMap::new(),
            merge_nodes: HashMap::new(),
            merge_to_downstream: HashMap::new(),
        }
    }

    /// Record a new merge operation in the graph.
    pub fn record_merge(&mut self, node: MergeNode) {
        let merge_tx = node.merge_tx;
        for source in &node.source_origins {
            self.origin_to_merges
                .entry(source.origin)
                .or_default()
                .push(merge_tx);
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
    pub fn propagate_revocation(
        &self,
        revoked_origin: &TxHash,
    ) -> Vec<RevocationEvent> {
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
            // Calculate the proportion of this merge that came from the revoked origin.
            let revoked_amount: u128 = node
                .source_origins
                .iter()
                .filter(|s| s.origin == *revoked_origin)
                .map(|s| s.amount)
                .sum();

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
