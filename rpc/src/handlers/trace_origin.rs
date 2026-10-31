use super::origin_history::MAX_ROWS;
use crate::error::RpcError;
use burst_trst::{MergerGraph, WalletPortfolio};
use burst_utils::{Shared, SharedMap};
use burst_types::{TxHash, WalletAddress};
use std::collections::{HashMap, HashSet, VecDeque};

const MAX_HOLDING_WORK: usize = 131_072;

fn limit() -> RpcError {
    RpcError::Server("origin inspection limit exceeded; no complete result".into())
}

/// Collect distinct downstream merges, allowing at most 8,192 traversed edges.
/// Repeated edges still consume budget; result order is unspecified.
pub(super) fn descendants(origin: TxHash, graph: &MergerGraph) -> Result<Vec<TxHash>, RpcError> {
    descendants_with_limit(origin, graph, MAX_ROWS)
}

fn descendants_with_limit(
    origin: TxHash,
    graph: &MergerGraph,
    max_edges: usize,
) -> Result<Vec<TxHash>, RpcError> {
    let mut found = HashSet::new();
    let mut queue = VecDeque::from([origin]);
    let mut edges = 0usize;
    while let Some(parent) = queue.pop_front() {
        for &id in graph.direct_consumers(&parent) {
            edges += 1;
            if edges > max_edges {
                return Err(limit());
            }
            if found.insert(id) {
                graph
                    .get_merge(&id)
                    .ok_or_else(|| RpcError::Server("missing merge node".into()))?;
                queue.push_back(id);
            }
        }
    }
    Ok(found.into_iter().collect())
}

/// Report each descendant's held amounts, including expired and revoked tokens.
/// A merge with no held token emits a null holder and its original total amount.
/// Scan at most 131,072 wallets plus tokens and return at most 8,192 rows, sorted
/// by timestamp, merge hash and holder. Limits fail the request without partial output.
pub(super) fn forward_merges(
    origin: TxHash,
    graph: &MergerGraph,
    wallets: &SharedMap<WalletAddress, Shared<WalletPortfolio>>,
    mut timestamp: impl FnMut(TxHash) -> Result<u64, RpcError>,
) -> Result<Vec<serde_json::Value>, RpcError> {
    let mut merges = HashMap::new();
    for id in descendants(origin, graph)? {
        let node = graph
            .get_merge(&id)
            .ok_or_else(|| RpcError::Server("missing merge node".into()))?;
        merges.insert(id, (timestamp(id)?, node.total_amount, false));
    }
    if merges.is_empty() {
        return Ok(Vec::new());
    }
    let mut rows = Vec::new();
    // The live-holder index excludes revoked chunks. Scan holdings once so all
    // token states contribute to every selected descendant's totals.
    let mut work = 0usize;
    for (holder, portfolio) in wallets {
        work += 1;
        if work > MAX_HOLDING_WORK {
            return Err(limit());
        }
        let mut amounts = HashMap::<TxHash, u128>::new();
        for token in portfolio.iter_tokens() {
            work += 1;
            if work > MAX_HOLDING_WORK {
                return Err(limit());
            }
            if merges.contains_key(&token.origin) {
                let amount = amounts.entry(token.origin).or_default();
                *amount = amount
                    .checked_add(token.amount)
                    .ok_or_else(|| RpcError::Server("holding amount overflow".into()))?;
            }
        }
        for (id, amount) in amounts {
            let (at, _, found) = merges.get_mut(&id).expect("selected descendant");
            *found = true;
            if rows.len() == MAX_ROWS {
                return Err(limit());
            }
            rows.push((
                *at,
                id.to_string(),
                Some(holder.as_str().to_owned()),
                amount,
            ));
        }
    }
    for (id, (at, amount, found)) in merges {
        if !found {
            if rows.len() == MAX_ROWS {
                return Err(limit());
            }
            rows.push((at, id.to_string(), None, amount));
        }
    }
    rows.sort_unstable_by(|a, b| (&a.0, &a.1, &a.2).cmp(&(&b.0, &b.1, &b.2)));
    Ok(rows.into_iter().map(|(at, id, holder, amount)| {
        serde_json::json!({"merge_hash":id,"holder":holder,"amount":amount.to_string(),"timestamp":at})
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_trst::{MergeNode, MergeSource, TrstEngine};
    use burst_types::{Timestamp, TrstState};

    fn hash(id: u8) -> TxHash {
        TxHash::new([id; 32])
    }

    fn merge(graph: &mut MergerGraph, id: u8, sources: &[u8]) {
        graph.record_merge(MergeNode {
            merge_tx: hash(id),
            source_origins: sources
                .iter()
                .map(|id| MergeSource {
                    origin: hash(*id),
                    amount: 10,
                })
                .collect(),
            total_amount: sources.len() as u128 * 10,
            revoked_contribs: Default::default(),
        });
    }

    fn holding(
        engine: &mut TrstEngine,
        id: u8,
        origin: u8,
        holder: &str,
        amount: u128,
        revoked: bool,
    ) {
        let holder = WalletAddress::new(holder);
        let mut token = engine
            .mint(hash(id), holder.clone(), amount, holder, Timestamp::new(1))
            .unwrap();
        token.origin = hash(origin);
        if revoked {
            token.state = TrstState::Revoked;
            token.revoked_origin = Some(hash(1));
        }
        engine.track_token(token);
    }

    #[test]
    fn trace_deduplicates_diamonds_and_preserves_all_state_holders_and_absent_rows() {
        let mut engine = TrstEngine::new();
        merge(&mut engine.merger_graph, 10, &[1, 1, 2]);
        merge(&mut engine.merger_graph, 11, &[1, 3]);
        merge(&mut engine.merger_graph, 12, &[10, 11]);
        merge(&mut engine.merger_graph, 13, &[12]);
        merge(&mut engine.merger_graph, 14, &[9]); // unrelated
        holding(&mut engine, 20, 10, "brst_b", 4, false);
        holding(&mut engine, 21, 10, "brst_b", 3, true);
        holding(&mut engine, 22, 10, "brst_a", 2, true);
        holding(&mut engine, 23, 12, "brst_a", 5, false);
        holding(&mut engine, 24, 14, "brst_a", 100, false);
        let graph = engine.merger_graph.clone();
        let wallets = engine.wallets.clone();
        holding(&mut engine, 25, 10, "brst_b", 9, false);
        assert!(descendants_with_limit(hash(1), &graph, 1).is_err());
        let mut reads = Vec::new();
        let rows = forward_merges(hash(1), &graph, &wallets, |id| {
            reads.push(id);
            assert_ne!(id, hash(14));
            Ok(100)
        })
        .unwrap();
        reads.sort();
        assert_eq!(reads, vec![hash(10), hash(11), hash(12), hash(13)]);
        let row = |id, holder: Option<&str>, amount: &str| {
            serde_json::json!({
                "merge_hash":hash(id).to_string(), "holder":holder, "amount":amount, "timestamp":100
            })
        };
        assert_eq!(
            rows,
            vec![
                row(10, Some("brst_a"), "2"),
                row(10, Some("brst_b"), "7"),
                row(11, None, "20"),
                row(12, Some("brst_a"), "5"),
                row(13, None, "10")
            ]
        );
        assert!(forward_merges(hash(8), &graph, &wallets, |_| panic!(
            "unrelated block read"
        ))
        .unwrap()
        .is_empty());
        assert!(
            forward_merges(hash(1), &graph, &wallets, |_| Err(RpcError::Server(
                "missing block".into()
            )))
            .is_err()
        );
    }

    #[test]
    fn trace_rejects_overflow_instead_of_returning_partial_totals() {
        let mut engine = TrstEngine::new();
        merge(&mut engine.merger_graph, 10, &[1]);
        holding(&mut engine, 20, 10, "brst_a", 1, false);
        holding(&mut engine, 21, 10, "brst_a", 1, true);
        engine
            .wallets
            .get_mut(&WalletAddress::new("brst_a"))
            .unwrap()
            .tokens[0]
            .amount = u128::MAX;
        let error =
            forward_merges(hash(1), &engine.merger_graph, &engine.wallets, |_| Ok(1)).unwrap_err();
        assert!(error.to_string().contains("holding amount overflow"));
    }
}
