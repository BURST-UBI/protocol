//! Axum-based JSON-RPC server with action-based dispatch.

use crate::error::RpcError;
use crate::handlers;

use axum::{
    extract::{ConnectInfo, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use burst_brn::BrnEngine;
use burst_consensus::RepWeightCache;
use burst_store::account::AccountStore;
use burst_store::block::BlockStore;
use burst_store::governance::GovernanceStore;
use burst_store::verification::VerificationStore;
use burst_store::{FrontierStore, PendingStore};
use burst_types::{ProtocolParams, WalletAddress};

/// Trait for O(1) ledger counter lookups. Implemented by the node's
/// `LedgerCache` and injected into `RpcState` to break the circular
/// dependency between `burst-rpc` and `burst-node`.
pub trait LedgerCacheView {
    fn block_count(&self) -> u64;
    fn account_count(&self) -> u64;
    fn pending_count(&self) -> u64;
}
use burst_work::WorkGenerator;
use prometheus::{Encoder, Registry, TextEncoder};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::{info, warn};

/// Simple token-bucket rate limiter keyed by client IP.
pub struct RateLimiter {
    buckets: Mutex<HashMap<String, (u32, Instant)>>,
    max_requests_per_second: u32,
}

impl RateLimiter {
    pub fn new(max_rps: u32) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            max_requests_per_second: max_rps,
        }
    }

    /// Returns `true` if the request should be allowed.
    pub fn check(&self, client_ip: &str) -> bool {
        let mut buckets = match self.buckets.lock() {
            Ok(b) => b,
            Err(poisoned) => poisoned.into_inner(),
        };
        let now = Instant::now();
        let entry = buckets.entry(client_ip.to_string()).or_insert((0, now));

        if now.duration_since(entry.1).as_secs() >= 1 {
            entry.0 = 1;
            entry.1 = now;
            true
        } else if entry.0 < self.max_requests_per_second {
            entry.0 += 1;
            true
        } else {
            false
        }
    }

    /// Remove entries older than 60 seconds (call from a background task).
    pub fn cleanup(&self) {
        let mut buckets = match self.buckets.lock() {
            Ok(b) => b,
            Err(poisoned) => poisoned.into_inner(),
        };
        let now = Instant::now();
        buckets.retain(|_, (_, last)| now.duration_since(*last).as_secs() < 60);
    }
}

/// Result of processing a block, mirroring the node's `ProcessResult`.
///
/// Defined here to avoid a circular dependency on `burst-node`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProcessResult {
    Accepted,
    Gap,
    Fork,
    Rejected(String),
    Duplicate,
    Queued,
}

pub type HistoricalAccountPage = Vec<(u64, burst_ledger::StateBlock)>;
pub type HistoricalAccountPageFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<HistoricalAccountPage, String>> + Send + 'a>,
>;

/// Callback trait for processing a block through the node's pipeline.
///
/// The node provides a concrete implementation that calls its `BlockProcessor`.
/// This indirection breaks the `rpc → node → rpc` circular dependency.
pub trait BlockProcessorCallback: Send + Sync {
    /// Advisory synchronization observations, not authority to sign or confirm.
    fn checkpoint_sync_status(&self) -> Result<serde_json::Value, String> {
        Err("checkpoint synchronization status is unavailable on this node".into())
    }
    /// Authenticated account-prefix entries, keyed by their original heights.
    /// Missing entries are unavailable, not proof that the account has no history.
    fn historical_account_page(
        &self,
        _account: burst_types::WalletAddress,
        _first: u64,
        _count: u32,
    ) -> HistoricalAccountPageFuture<'_> {
        Box::pin(async { Ok(Vec::new()) })
    }
    /// Read-only membership proof. This must never admit or execute the block.
    fn historical_block(
        &self,
        _hash: burst_types::BlockHash,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<Option<burst_ledger::StateBlock>, String>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async { Ok(None) })
    }
    fn confirmed_account_slot(
        &self,
        _query: burst_types::payment_recovery::SlotQuery,
    ) -> Result<burst_types::payment_recovery::SlotReview, String> {
        Err("confirmed account slot recovery is unavailable on this node".into())
    }
    fn payment_exclusion(
        &self,
        _query: burst_types::payment_recovery::ExclusionQuery,
    ) -> Result<burst_types::payment_recovery::ExclusionReview, String> {
        Err("payment exclusion recovery is unavailable on this node".into())
    }
    fn process_block(&self, block_bytes: &[u8]) -> Result<ProcessResult, String>;
    fn supports_full_governance_content(&self) -> bool {
        false
    }
    fn supports_delegation_key_journal(&self) -> bool {
        false
    }
    fn appeal_review(
        &self,
        _account: &WalletAddress,
    ) -> Result<burst_types::appeal::AppealReview, String> {
        Err("appeal review is unavailable on this node".into())
    }
    fn delegation_keys(
        &self,
        _query: burst_governance::key_delegation::view::Query,
    ) -> Result<burst_governance::key_delegation::view::Page, String> {
        Err("scoped delegation discovery is unavailable".into())
    }

    fn process_block_with_content(
        &self,
        _block_bytes: &[u8],
        _content: &[u8],
    ) -> Result<ProcessResult, String> {
        Err("full governance content submission is unavailable".into())
    }

    /// Optional bounded wait for admission/persistence, not consensus finality.
    /// Missing or timed-out results remain queued, never become a rejection.
    fn wait_for_result<'a>(
        &'a self,
        _block_bytes: &'a [u8],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ProcessResult>> + Send + 'a>>
    {
        Box::pin(async { None })
    }
}

/// Top-level RPC server handle.
pub struct RpcServer {
    pub port: u16,
    /// Address to bind to (e.g. `127.0.0.1` or `0.0.0.0`).
    pub bind: String,
    pub state: Arc<RpcState>,
}

/// Shared state accessible by all handlers.
///
/// The RPC crate is deliberately decoupled from `burst-node` to avoid a
/// circular dependency. The node constructs an `RpcState` and passes it in
/// when starting the RPC server.
pub struct RpcState {
    /// Node clock for read-only time projections, not signing authority.
    pub display_now: Arc<dyn Fn() -> burst_types::Timestamp + Send + Sync>,
    pub meta_store: Arc<dyn burst_store::meta::MetaStore + Send + Sync>,
    /// Actual node network, independent of convenience-tool configuration.
    pub network: burst_types::NetworkId,
    /// Serialize genesis-chain transactions authored by these RPC helpers.
    pub genesis_authoring: tokio::sync::Mutex<()>,
    /// Unix timestamp (seconds) when the node started.
    pub started_at: u64,
    /// Prometheus metric registry (optional; when present the `/metrics`
    /// endpoint will serve the text exposition format).
    pub metrics_registry: Option<Registry>,
    /// Account storage backend.
    pub account_store: Arc<dyn AccountStore + Send + Sync>,
    /// Block storage backend (the DAG block-lattice).
    pub block_store: Arc<dyn BlockStore + Send + Sync>,
    /// Pending receive storage.
    pub pending_store: Arc<dyn PendingStore + Send + Sync>,
    /// Frontier storage (account chain heads).
    pub frontier_store: Arc<dyn FrontierStore + Send + Sync>,
    /// Verification data storage.
    pub verification_store: Arc<dyn VerificationStore + Send + Sync>,
    /// Governance data storage.
    pub governance_store: Arc<dyn GovernanceStore + Send + Sync>,
    /// Governance engine (shared with the node) for live proposal state.
    pub governance_engine: Option<Arc<tokio::sync::Mutex<burst_governance::GovernanceEngine>>>,
    /// BRN computation engine (shared with the node).
    pub brn_engine: Arc<tokio::sync::Mutex<BrnEngine>>,
    pub brn_store: Arc<dyn burst_store::brn::BrnStore + Send + Sync>,
    pub clock_sync: Arc<tokio::sync::Mutex<burst_network::ClockSync>>,
    pub verification_engine: Arc<tokio::sync::Mutex<burst_verification::VerificationOrchestrator>>,
    pub delegation_engine: Arc<tokio::sync::Mutex<burst_governance::DelegationEngine>>,
    pub trst_engine: Arc<tokio::sync::Mutex<burst_trst::TrstEngine>>,
    pub consti_engine: Arc<tokio::sync::Mutex<burst_consti::ConstiEngine>>,
    /// Cached representative weights (shared with the node).
    pub rep_weight_cache: Arc<tokio::sync::RwLock<RepWeightCache>>,
    /// Proof-of-work generator.
    pub work_generator: Arc<WorkGenerator>,
    /// Protocol parameters.
    pub params: Arc<ProtocolParams>,
    /// Block processor callback — the node injects a concrete implementation.
    pub block_processor: Arc<dyn BlockProcessorCallback>,
    /// Online representatives, updated by the peer manager.
    /// Each entry is (address, voting_weight).
    pub online_reps: Arc<std::sync::RwLock<Vec<(WalletAddress, u128)>>>,
    /// Peer manager for connected peer count.
    pub peer_manager: Arc<tokio::sync::RwLock<burst_network::PeerManager>>,
    /// Whether the testnet faucet endpoint is enabled. Default: `false`.
    /// Only set to `true` on dev/test nodes.
    pub enable_faucet: bool,
    /// Genesis signing seed — present ONLY on the creator's authority node.
    /// Gates `genesis_endorse` (bootstrap wallet verification). Must never be
    /// exposed: keep this node's RPC bound to localhost / firewalled.
    pub genesis_seed: Option<[u8; 32]>,
    /// Per-IP rate limiter for RPC requests.
    pub rate_limiter: Arc<RateLimiter>,
    /// Cached ledger counters (block/account/pending counts) — O(1) lookups.
    /// Optional to avoid breaking test callers that don't provide one.
    pub ledger_cache: Option<Arc<dyn LedgerCacheView + Send + Sync>>,
}

// ── JSON-RPC envelope types ─────────────────────────────────────────────

/// Incoming JSON-RPC request envelope.
#[derive(Debug, Deserialize)]
struct RpcRequest {
    action: String,
    #[serde(flatten)]
    params: serde_json::Value,
}

/// Outgoing JSON-RPC response envelope.
#[derive(Debug, Serialize)]
struct RpcResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl RpcResponse {
    fn ok(value: serde_json::Value) -> Self {
        Self {
            result: Some(value),
            error: None,
        }
    }

    fn err(msg: impl Into<String>) -> Self {
        Self {
            result: None,
            error: Some(msg.into()),
        }
    }
}

// ── Server impl ─────────────────────────────────────────────────────────

impl RpcServer {
    /// Create a server bound to localhost (`127.0.0.1`) with custom shared
    /// state. Use [`RpcServer::with_bind`] to bind a different address.
    pub fn new(port: u16, state: Arc<RpcState>) -> Self {
        Self::with_bind(port, "127.0.0.1".to_string(), state)
    }

    /// Alias for `new` — create a localhost-bound server with custom state.
    pub fn with_state(port: u16, state: Arc<RpcState>) -> Self {
        Self::new(port, state)
    }

    /// Create a server bound to an explicit address with custom shared state.
    pub fn with_bind(port: u16, bind: String, state: Arc<RpcState>) -> Self {
        Self { port, bind, state }
    }

    /// Stop accepting connections and drain existing HTTP sessions on shutdown.
    pub async fn start(
        &self,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), RpcError> {
        let app = Router::new()
            .route("/health", get(|| async { "ok" }))
            .route("/metrics", get(metrics_handler))
            .route("/", post(handle_rpc))
            .with_state(Arc::clone(&self.state));

        let addr = format!("{}:{}", self.bind, self.port);
        if self.bind == "0.0.0.0" {
            warn!(
                "RPC server binding to 0.0.0.0:{} — exposed on ALL interfaces. \
                 The RPC surface includes genesis_endorse; ensure a firewall \
                 restricts this port. Prefer rpc_bind = \"127.0.0.1\".",
                self.port
            );
        }
        info!("RPC server listening on {}", addr);

        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| RpcError::Server(e.to_string()))?;

        let cleanup = async {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                interval.tick().await;
                self.state.rate_limiter.cleanup();
            }
        };
        let server = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown);
        tokio::select! {
            result = server => result.map_err(|error| RpcError::Server(error.to_string())),
            () = cleanup => Ok(()),
        }
    }
}

// ── Prometheus metrics endpoint ──────────────────────────────────────────

async fn metrics_handler(State(state): State<Arc<RpcState>>) -> impl IntoResponse {
    match &state.metrics_registry {
        Some(registry) => {
            let encoder = TextEncoder::new();
            let metric_families = registry.gather();
            let mut buffer = Vec::new();
            if let Err(e) = encoder.encode(&metric_families, &mut buffer) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to encode metrics: {e}"),
                )
                    .into_response();
            }
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, encoder.format_type())],
                buffer,
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "metrics not configured").into_response(),
    }
}

// ── Main RPC handler (single + batch) ───────────────────────────────────

async fn handle_rpc(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<RpcState>>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let client_ip = addr.ip().to_string();
    if !state.rate_limiter.check(&client_ip) {
        warn!(ip = %client_ip, "RPC rate limit exceeded");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": "rate limited" })),
        );
    }

    let response = if body.is_array() {
        let items = match body.as_array() {
            Some(a) => a,
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "invalid batch request" })),
                );
            }
        };
        let mut responses = Vec::with_capacity(items.len());
        for item in items {
            let resp = dispatch_single(item.clone(), &state).await;
            responses.push(resp);
        }
        serde_json::to_value(responses).unwrap_or_else(|_| serde_json::json!([]))
    } else {
        let resp = dispatch_single(body, &state).await;
        serde_json::to_value(resp)
            .unwrap_or_else(|_| serde_json::json!({"error": "serialization failed"}))
    };

    (StatusCode::OK, Json(response))
}

/// Parse a single JSON-RPC request and route it to the correct handler.
async fn dispatch_single(body: serde_json::Value, state: &RpcState) -> RpcResponse {
    let req: RpcRequest = match serde_json::from_value(body) {
        Ok(r) => r,
        Err(e) => return RpcResponse::err(format!("invalid request: {e}")),
    };

    let result = dispatch_action(&req.action, req.params, state).await;
    match result {
        Ok(val) => RpcResponse::ok(val),
        Err(e) => RpcResponse::err(e.to_string()),
    }
}

/// Route an action string to the corresponding handler.
async fn dispatch_action(
    action: &str,
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    match action {
        "account_info" => handlers::handle_account_info(params, state).await,
        "account_history" => handlers::handle_account_history(params, state).await,
        "account_balance" => handlers::handle_account_balance(params, state).await,
        "brn_components" => handlers::handle_brn_components(params, state).await,
        "trst_portfolio" => handlers::handle_trst_portfolio(params, state).await,
        "account_pending" => handlers::handle_account_pending(params, state).await,
        "account_representative" => handlers::handle_account_representative(params, state).await,
        "process" => handlers::handle_process(params, state).await,
        "block_info" => handlers::handle_block_info(params, state).await,
        "params_hash" => handlers::handle_params_hash(params, state).await,
        "protocol_params" => handlers::handle_protocol_params(params, state).await,
        "blocks_info" => handlers::handle_blocks_info(params, state).await,
        "pending" => handlers::handle_pending(params, state).await,
        "work_generate" => handlers::handle_work_generate(params, state).await,
        "governance_proposals" => handlers::handle_governance_proposals(params, state).await,
        "verification_activity" => handlers::handle_verification_activity(params, state).await,
        "consti" => handlers::handle_consti(params, state).await,
        "merger_graph_stats" => handlers::handle_merger_graph_stats(params, state).await,
        "trace_origin" => handlers::handle_trace_origin(params, state).await,
        "trst_origins" => handlers::provenance::handle_trst_origins(params, state).await,
        "peers_detailed" => handlers::handle_peers_detailed(params, state).await,
        "governance_vote" => handlers::handle_governance_vote(params, state).await,
        "governance_proposal_info" => {
            handlers::handle_governance_proposal_info(params, state).await
        }
        "telemetry" => handlers::handle_telemetry(params, state).await,
        "peers" => handlers::handle_peers(params, state).await,
        "verification_status" => handlers::handle_verification_status(params, state).await,
        "verifier_assignments" => handlers::handle_verifier_assignments(params, state).await,
        "appeal_review" => {
            let request: handlers::VerificationStatusRequest = serde_json::from_value(params)
                .map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
            let account = WalletAddress::new(request.account);
            burst_crypto::decode_address(account.as_str())
                .ok_or_else(|| RpcError::InvalidRequest("invalid wallet address".into()))?;
            let review = state
                .block_processor
                .appeal_review(&account)
                .map_err(RpcError::Server)?;
            serde_json::to_value(review).map_err(|e| RpcError::Server(e.to_string()))
        }
        "confirmed_account_slot" => {
            let query = serde_json::from_value(params)
                .map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
            let review = state
                .block_processor
                .confirmed_account_slot(query)
                .map_err(RpcError::Server)?;
            serde_json::to_value(review).map_err(|e| RpcError::Server(e.to_string()))
        }
        "checkpoint_sync_status" => state
            .block_processor
            .checkpoint_sync_status()
            .map_err(RpcError::Server),
        "payment_exclusion" => {
            let query = serde_json::from_value(params)
                .map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
            let review = state
                .block_processor
                .payment_exclusion(query)
                .map_err(RpcError::Server)?;
            serde_json::to_value(review).map_err(|e| RpcError::Server(e.to_string()))
        }
        "delegation_info" => handlers::handle_delegation_info(params, state).await,
        "delegation_keys" => {
            let query = serde_json::from_value(params)
                .map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
            let page = state
                .block_processor
                .delegation_keys(query)
                .map_err(RpcError::Server)?;
            serde_json::to_value(page).map_err(|e| RpcError::Server(e.to_string()))
        }
        "representatives" => handlers::handle_representatives(params, state).await,
        "representatives_online" => handlers::handle_representatives_online(params, state).await,
        "send" => handlers::handle_send(params, state).await,
        "burn" => handlers::handle_burn(params, state).await,
        "receive" => handlers::handle_receive(params, state).await,
        "wallet_create" => handlers::handle_wallet_create(params, state).await,
        "wallet_info" => handlers::handle_wallet_info(params, state).await,
        "node_info" => handlers::handle_node_info(params, state).await,
        "faucet" => handlers::handle_faucet(params, state).await,
        "genesis_endorse" => handlers::handle_genesis_endorse(params, state).await,
        "wallet_create_full" => handlers::handle_wallet_create_full(params, state).await,
        "burn_simple" => handlers::handle_burn_simple(params, state).await,
        "send_simple" => handlers::handle_send_simple(params, state).await,
        "receive_simple" => handlers::handle_receive_simple(params, state).await,
        "change_rep_simple" => handlers::handle_change_rep_simple(params, state).await,
        "verifier_opt_in" => handlers::handle_verifier_opt_in(params, state).await,
        "verifier_opt_out" => handlers::handle_verifier_opt_out(params, state).await,
        "governance_propose_simple" => {
            handlers::handle_governance_propose_simple(params, state).await
        }
        "governance_propose_eviction" => {
            handlers::handle_governance_propose_eviction(params, state).await
        }
        "governance_vote_simple" => handlers::handle_governance_vote_simple(params, state).await,
        other => {
            warn!("unknown RPC action: {other}");
            Err(RpcError::InvalidRequest(format!("unknown action: {other}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_allows_first_request() {
        let limiter = RateLimiter::new(10);
        assert!(limiter.check("127.0.0.1"));
    }

    #[test]
    fn rate_limiter_allows_up_to_max_rps() {
        let limiter = RateLimiter::new(5);
        for _ in 0..5 {
            assert!(limiter.check("127.0.0.1"));
        }
        assert!(
            !limiter.check("127.0.0.1"),
            "6th request should be rejected"
        );
    }

    #[test]
    fn rate_limiter_separate_ips_independent() {
        let limiter = RateLimiter::new(2);
        assert!(limiter.check("10.0.0.1"));
        assert!(limiter.check("10.0.0.1"));
        assert!(!limiter.check("10.0.0.1"));

        assert!(limiter.check("10.0.0.2"), "different IP should be allowed");
        assert!(limiter.check("10.0.0.2"));
        assert!(!limiter.check("10.0.0.2"));
    }

    #[test]
    fn rate_limiter_zero_max_blocks_all() {
        let limiter = RateLimiter::new(0);
        assert!(!limiter.check("127.0.0.1"));
    }

    #[test]
    fn rate_limiter_cleanup_removes_old_entries() {
        let limiter = RateLimiter::new(100);
        limiter.check("10.0.0.1");
        limiter.check("10.0.0.2");
        {
            let buckets = limiter.buckets.lock().unwrap();
            assert_eq!(buckets.len(), 2);
        }
        limiter.cleanup();
        {
            let buckets = limiter.buckets.lock().unwrap();
            assert_eq!(buckets.len(), 2, "recent entries should survive cleanup");
        }
    }

    #[test]
    fn rate_limiter_single_request_exactly_at_limit() {
        let limiter = RateLimiter::new(1);
        assert!(limiter.check("x"), "first request allowed");
        assert!(!limiter.check("x"), "second request blocked");
    }
}
