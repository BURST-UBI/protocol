//! Optional backward origin inspection. No group rules enter node validation.

use super::{is_block_confirmed_in, origin_history, parse_tx_hash};
use crate::{error::RpcError, RpcState};
use burst_ledger::BlockType;
use burst_trst::MergerGraph;
use burst_types::{BlockHash, TxHash};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};

const MAX_WALK_STEPS: usize = 8192;

pub(crate) async fn handle_trst_origins(
    params: Value,
    state: &RpcState,
) -> Result<Value, RpcError> {
    let origin = parse_tx_hash(
        params["origin"]
            .as_str()
            .ok_or_else(|| RpcError::InvalidRequest("origin hash required".into()))?,
    )?;
    let mut history = origin_history::Reader::new(state);
    let blocks = state.block_store.clone();
    let accounts = state.account_store.clone();
    let engine = state.trst_engine.clone();
    state.read_executor.run(move || {
        let root_hash = BlockHash::new(*origin.as_bytes());
        let root_entry = history.get(root_hash)?;
        let root_historical = root_entry.historical;
        let root = root_entry.block.clone();
        if root.hash != root_hash || root.compute_hash() != root_hash {
            return Err(RpcError::Server("origin block hash is inconsistent".into()));
        }
        if !matches!(root.block_type, BlockType::Burn | BlockType::Merge) {
            return Err(RpcError::InvalidRequest(
                "expected a burn or merge origin, not a transfer hash".into(),
            ));
        }
        let graph = engine.blocking_lock().merger_graph.clone();
        if (root.block_type == BlockType::Merge) != graph.contains_merge(&origin) {
            return Err(RpcError::Server(
                "origin block and merge index disagree".into(),
            ));
        }
        let leaves = origin_leaves(&graph, origin, MAX_WALK_STEPS)?;
        history.prefetch(leaves.iter().map(|leaf| BlockHash::new(*leaf.as_bytes())).collect())?;
        let mut origins = Vec::with_capacity(leaves.len());
        for leaf in leaves {
            let hash = BlockHash::new(*leaf.as_bytes());
            let entry = history.get(hash)?;
            let block = &entry.block;
            if block.block_type != BlockType::Burn || block.hash != hash || block.compute_hash() != hash
            {
                return Err(RpcError::Server(
                    "origin history contains an invalid burn".into(),
                ));
            }
            origins.push(json!({
                "hash":leaf.to_string(), "originator":block.account.as_str(),
                "timestamp":block.timestamp.as_secs(),
                "confirmed":entry.historical || is_block_confirmed_in(&hash, &block.account, accounts.as_ref(), blocks.as_ref()),
                "revoked":graph.is_origin_revoked(&leaf),
            }));
        }
        // Identity inspection is not a contribution-weight or spendability quote.
        Ok(json!({"origin":origin.to_string(), "complete":true,
            "confirmed":root_historical || is_block_confirmed_in(&root_hash, &root.account, accounts.as_ref(), blocks.as_ref()), "origins":origins}))
    }).await
}

// Walk backward to distinct burn IDs in hash order, including revoked origins.
// The budget counts scheduled entries, including merge exits and repeated edges,
// rather than unique leaves. Input sums and cycles are checked, not spendability.
fn origin_leaves(
    graph: &MergerGraph,
    origin: TxHash,
    max_steps: usize,
) -> Result<Vec<TxHash>, RpcError> {
    if max_steps == 0 {
        return Err(RpcError::Server(
            "origin inspection limit exceeded; no complete result".into(),
        ));
    }
    let mut stack = vec![(origin, false)];
    let mut scheduled: usize = 1;
    let mut active = HashSet::new();
    let mut done = HashSet::new();
    let mut leaves = BTreeSet::new();
    while let Some((hash, exiting)) = stack.pop() {
        if exiting {
            active.remove(&hash);
            done.insert(hash);
            continue;
        }
        if done.contains(&hash) {
            continue;
        }
        if !active.insert(hash) {
            return Err(RpcError::Server("cyclic TRST origin graph".into()));
        }
        if let Some(merge) = graph.get_merge(&hash) {
            let next = merge
                .source_origins
                .len()
                .checked_add(1)
                .and_then(|count| scheduled.checked_add(count))
                .filter(|count| *count <= max_steps)
                .ok_or_else(|| {
                    RpcError::Server("origin inspection limit exceeded; no complete result".into())
                })?;
            if merge.source_origins.is_empty()
                || merge.source_origins.iter().any(|source| source.amount == 0)
                || merge
                    .source_origins
                    .iter()
                    .try_fold(0u128, |sum, source| sum.checked_add(source.amount))
                    != Some(merge.total_amount)
            {
                return Err(RpcError::Server("inconsistent TRST merge inputs".into()));
            }
            scheduled = next;
            stack.push((hash, true));
            stack.extend(
                merge
                    .source_origins
                    .iter()
                    .map(|source| (source.origin, false)),
            );
        } else {
            active.remove(&hash);
            done.insert(hash);
            leaves.insert(*hash.as_bytes());
        }
    }
    Ok(leaves.into_iter().map(TxHash::new).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_trst::{MergeNode, MergeSource};

    fn hash(id: u8) -> TxHash {
        TxHash::new([id; 32])
    }

    fn merge(graph: &mut MergerGraph, id: u8, inputs: &[u8]) {
        graph.record_merge(MergeNode {
            merge_tx: hash(id),
            source_origins: inputs
                .iter()
                .map(|id| MergeSource {
                    origin: hash(*id),
                    amount: 1,
                })
                .collect(),
            total_amount: inputs.len() as u128,
            revoked_contribs: Default::default(),
        });
    }

    #[test]
    fn original_burn_and_nested_merges_return_sorted_distinct_leaves() {
        let mut graph = MergerGraph::new();
        assert_eq!(origin_leaves(&graph, hash(1), 1).unwrap(), vec![hash(1)]);
        merge(&mut graph, 10, &[1, 2]);
        merge(&mut graph, 11, &[2, 3]);
        merge(&mut graph, 12, &[10, 11]);
        assert_eq!(
            origin_leaves(&graph, hash(12), MAX_WALK_STEPS).unwrap(),
            vec![hash(1), hash(2), hash(3)]
        );
    }

    #[test]
    fn inspection_never_returns_a_partial_success() {
        let mut graph = MergerGraph::new();
        merge(&mut graph, 10, &[1, 2]);
        assert!(origin_leaves(&graph, hash(10), 3)
            .unwrap_err()
            .to_string()
            .contains("limit exceeded"));
        assert_eq!(origin_leaves(&graph, hash(10), 4).unwrap().len(), 2);
        assert!(origin_leaves(&graph, hash(1), 0).is_err());
    }

    #[test]
    fn cycles_and_empty_merges_are_errors_not_empty_trust_sets() {
        let mut graph = MergerGraph::new();
        merge(&mut graph, 10, &[11]);
        merge(&mut graph, 11, &[10]);
        assert!(origin_leaves(&graph, hash(10), MAX_WALK_STEPS)
            .unwrap_err()
            .to_string()
            .contains("cyclic"));
        merge(&mut graph, 12, &[]);
        assert!(origin_leaves(&graph, hash(12), MAX_WALK_STEPS).is_err());
    }

    #[test]
    fn shared_subgraphs_are_not_cycles_and_revocation_does_not_erase_origins() {
        let mut graph = MergerGraph::new();
        merge(&mut graph, 10, &[1, 2]);
        merge(&mut graph, 11, &[10, 3]);
        merge(&mut graph, 12, &[10, 11]);
        let snapshot = graph.clone();
        graph.apply_revocation(hash(1));
        assert!(!snapshot.is_origin_revoked(&hash(1)));
        assert!(graph.is_origin_revoked(&hash(1)));
        assert_eq!(
            origin_leaves(&snapshot, hash(12), MAX_WALK_STEPS).unwrap(),
            vec![hash(1), hash(2), hash(3)]
        );
        assert_eq!(
            origin_leaves(&graph, hash(12), MAX_WALK_STEPS).unwrap(),
            vec![hash(1), hash(2), hash(3)]
        );
    }
}
