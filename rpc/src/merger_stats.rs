//! Derived dashboard data; persistent root identity is only a cache key.
use crate::error::RpcError;
use burst_trst::{MergerGraph, TrstEngine, WalletPortfolio};
use burst_types::{Timestamp, TxHash, WalletAddress};
use burst_utils::{Shared, SharedMap, SharedSet};

pub(crate) struct Snapshot {
    merger_graph: MergerGraph,
    wallets: SharedMap<WalletAddress, Shared<WalletPortfolio>>,
    wallet_origins: SharedMap<WalletAddress, SharedSet<TxHash>>,
    expiry_secs: u64,
}

impl Snapshot {
    pub(crate) fn new(engine: &TrstEngine) -> Self {
        Self {
            merger_graph: engine.merger_graph.clone(),
            wallets: engine.wallets.clone(),
            wallet_origins: engine.wallet_origins.clone(),
            expiry_secs: engine.expiry_secs,
        }
    }
    fn unchanged(&self, other: &Self) -> bool {
        self.expiry_secs == other.expiry_secs
            && self.merger_graph.shares_storage_with(&other.merger_graph)
            && self.wallets.shares_storage_with(&other.wallets)
            && self
                .wallet_origins
                .shares_storage_with(&other.wallet_origins)
    }
}

struct Entry {
    snapshot: Snapshot,
    computed_at: u64,
    next_expiry: Option<u64>,
    value: serde_json::Value,
}

/// Cache one display result and the shared state roots used to compute it.
/// Retaining one snapshot does not bound its memory footprint. Results are display
/// statistics, not consensus evidence. Root changes, expiry and clock reversal
/// invalidate reuse.
#[derive(Default)]
pub struct MergerStatsCache {
    entry: Option<Entry>,
    #[cfg(test)]
    recomputations: usize,
}

impl MergerStatsCache {
    pub(crate) fn read(
        &mut self,
        snapshot: Snapshot,
        now: Timestamp,
    ) -> Result<serde_json::Value, RpcError> {
        if let Some(entry) = &self.entry {
            if now.as_secs() >= entry.computed_at
                && entry
                    .next_expiry
                    .is_none_or(|deadline| now.as_secs() < deadline)
                && entry.snapshot.unchanged(&snapshot)
            {
                return Ok(entry.value.clone());
            }
        }
        let (value, next_expiry) = compute(&snapshot, now)?;
        #[cfg(test)]
        {
            self.recomputations += 1;
        }
        self.entry = Some(Entry {
            snapshot,
            computed_at: now.as_secs(),
            next_expiry,
            value: value.clone(),
        });
        Ok(value)
    }
}

// Scan all merge nodes, origin registrations and wallet holdings on a cache miss.
// Depth counts merge ancestry, with a 512-node recursion-path limit. Active chains
// count held token records that are neither marked revoked nor expired at `now`;
// pending transfers and token amounts are not counted.
fn compute(trst: &Snapshot, now: Timestamp) -> Result<(serde_json::Value, Option<u64>), RpcError> {
    fn depth(
        id: TxHash,
        graph: &burst_trst::MergerGraph,
        memo: &mut std::collections::HashMap<TxHash, u64>,
        path: &mut std::collections::HashSet<TxHash>,
    ) -> Result<u64, RpcError> {
        if let Some(d) = memo.get(&id) {
            return Ok(*d);
        }
        let Some(node) = graph.get_merge(&id) else {
            return Ok(0);
        };
        if path.len() >= 512 || !path.insert(id) {
            return Err(RpcError::Server(
                "merger ancestry too deep or cyclic for dashboard traversal".into(),
            ));
        }
        let mut max = 0;
        for source in &node.source_origins {
            max = max.max(depth(source.origin, graph, memo, path)?);
        }
        path.remove(&id);
        memo.insert(id, max + 1);
        Ok(max + 1)
    }
    let mut memo = std::collections::HashMap::new();
    let mut depths = Vec::new();
    let mut origins = std::collections::HashSet::new();
    for node in trst.merger_graph.iter_merges() {
        depths.push(depth(
            node.merge_tx,
            &trst.merger_graph,
            &mut memo,
            &mut std::collections::HashSet::new(),
        )?);
        for source in &node.source_origins {
            if !trst.merger_graph.contains_merge(&source.origin) {
                origins.insert(source.origin);
            }
        }
    }
    for ids in trst.wallet_origins.values() {
        origins.extend(ids);
    }
    let mut active = 0usize;
    let mut next_expiry = None;
    for token in trst
        .wallets
        .values()
        .flat_map(|portfolio| portfolio.iter_tokens())
    {
        if token.state == burst_types::TrstState::Revoked {
            continue;
        }
        if !token.is_expired(now, trst.expiry_secs) {
            active += 1;
        }
        let deadline = token.earliest_expiry(trst.expiry_secs).as_secs();
        if deadline > now.as_secs() {
            next_expiry = Some(next_expiry.map_or(deadline, |old: u64| old.min(deadline)));
        }
    }
    Ok((
        serde_json::json!({"total_origins":origins.len(),"total_merges":depths.len(),"total_active_chains":active,
        "total_revoked_origins":trst.merger_graph.revoked_origins().len(),"max_chain_depth":depths.iter().max().copied().unwrap_or(0),
        "avg_chain_depth":if depths.is_empty(){0.0}else{depths.iter().sum::<u64>() as f64 / depths.len() as f64}}),
        next_expiry,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_trst::{MergeNode, MergeSource};

    fn populated(at: u64, expiry: u64) -> TrstEngine {
        let mut engine = TrstEngine::new();
        engine.expiry_secs = expiry;
        let owner = WalletAddress::new("brst_stats");
        let token = engine
            .mint(
                TxHash::new([1; 32]),
                owner.clone(),
                10,
                owner,
                Timestamp::new(at),
            )
            .unwrap();
        engine.track_token(token);
        engine
    }

    fn read(cache: &mut MergerStatsCache, engine: &TrstEngine, now: u64) -> serde_json::Value {
        let value = cache
            .read(Snapshot::new(engine), Timestamp::new(now))
            .unwrap();
        assert_eq!(
            value,
            compute(&Snapshot::new(engine), Timestamp::new(now))
                .unwrap()
                .0
        );
        value
    }

    #[test]
    fn cache_hits_stop_at_expiry_and_revalidate_clock_and_rules() {
        let mut engine = populated(100, 10);
        let mut cache = MergerStatsCache::default();
        for now in [100, 105, 109] {
            assert_eq!(read(&mut cache, &engine, now)["total_active_chains"], 1);
        }
        assert_eq!(cache.recomputations, 1);
        assert_eq!(read(&mut cache, &engine, 110)["total_active_chains"], 0);
        read(&mut cache, &engine, 200);
        assert_eq!(cache.recomputations, 2);
        assert_eq!(read(&mut cache, &engine, 109)["total_active_chains"], 1);
        assert_eq!(cache.recomputations, 3);
        engine.expiry_secs = 100;
        assert_eq!(read(&mut cache, &engine, 110)["total_active_chains"], 1);
        assert_eq!(cache.recomputations, 4);
        let saturated = populated(u64::MAX - 2, 10);
        assert_eq!(
            read(&mut cache, &saturated, u64::MAX - 1)["total_active_chains"],
            1
        );
        assert_eq!(
            read(&mut cache, &saturated, u64::MAX)["total_active_chains"],
            0
        );
    }

    #[test]
    fn cache_invalidates_mutation_and_rollback_without_changing_old_snapshot() {
        let mut engine = populated(100, 100);
        let original = engine.clone();
        let mut cache = MergerStatsCache::default();
        let baseline = read(&mut cache, &engine, 101);
        let owner = WalletAddress::new("brst_stats");
        engine.wallets.get_mut(&owner).unwrap().tokens[0].state = burst_types::TrstState::Revoked;
        assert_eq!(read(&mut cache, &engine, 101)["total_active_chains"], 0);
        assert_eq!(read(&mut cache, &original, 101), baseline);
        engine = original.clone();
        engine
            .wallet_origins
            .get_mut(&owner)
            .unwrap()
            .insert(TxHash::new([2; 32]));
        assert_eq!(read(&mut cache, &engine, 101)["total_origins"], 2);
        engine.merger_graph.record_merge(MergeNode {
            merge_tx: TxHash::new([3; 32]),
            source_origins: vec![MergeSource {
                origin: TxHash::new([1; 32]),
                amount: 10,
            }],
            total_amount: 10,
            revoked_contribs: Default::default(),
        });
        let merged = read(&mut cache, &engine, 101);
        assert_eq!(merged["total_merges"], 1);
        assert_eq!(merged["max_chain_depth"], 1);
        assert_eq!(cache.recomputations, 5);
        read(&mut cache, &engine.clone(), 102);
        assert_eq!(cache.recomputations, 5);
    }
}
