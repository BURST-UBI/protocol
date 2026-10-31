//! Shared read-only RPC state for node integration fixtures.
use crate::BurstNode;
use burst_rpc::{BlockProcessorCallback, ProcessResult, RpcState};
use burst_types::{NetworkId, Timestamp};
use std::sync::Arc;

struct ReadOnly;
impl BlockProcessorCallback for ReadOnly {
    fn process_block(&self, _: &[u8]) -> Result<ProcessResult, String> {
        Err("read-only pagination fixture".into())
    }
}

pub(crate) fn state(node: &BurstNode, live: bool) -> RpcState {
    RpcState {
        governance_reader: live.then(|| {
            Arc::new(crate::governance_read::GovernanceReader {
                store: node.store.clone(),
                fallback_params: node.config.params.clone(),
            }) as Arc<dyn burst_rpc::governance_snapshot::Reader>
        }),
        read_executor: Default::default(),
        governance_projection_cache: Default::default(),
        merger_stats_cache: Default::default(),
        display_now: Arc::new(|| Timestamp::new(101)),
        meta_store: Arc::new(node.store.meta_store()),
        network: NetworkId::Dev,
        genesis_authoring: Default::default(),
        started_at: 100,
        metrics_registry: None,
        account_store: Arc::new(node.store.account_store()),
        block_store: Arc::new(node.store.block_store()),
        pending_store: Arc::new(node.store.pending_store()),
        frontier_store: Arc::new(node.store.frontier_store()),
        verification_store: Arc::new(node.store.verification_store()),
        governance_store: Arc::new(node.store.governance_store()),
        governance_engine: live.then(|| node.governance.clone()),
        brn_engine: node.brn_engine.clone(),
        brn_store: Arc::new(node.store.brn_store()),
        clock_sync: node.clock_sync.clone(),
        signing_clock: node.signing_clock.clone(),
        verification_engine: node.verification_orchestrator.clone(),
        delegation_engine: node.delegation_engine.clone(),
        trst_engine: node.trst_engine.clone(),
        consti_engine: node.consti_engine.clone(),
        rep_weight_cache: node.rep_weights.clone(),
        work_generator: Arc::new(burst_work::WorkGenerator),
        params: Arc::new(node.config.params.clone()),
        block_processor: Arc::new(ReadOnly),
        online_reps: Default::default(),
        peer_manager: node.peer_manager.clone(),
        enable_faucet: false,
        genesis_seed: None,
        rate_limiter: Arc::new(burst_rpc::RateLimiter::new(100)),
        ledger_cache: None,
    }
}
