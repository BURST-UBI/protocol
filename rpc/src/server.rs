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

/// Callback trait for processing a block through the node's pipeline.
///
/// The node provides a concrete implementation that calls its `BlockProcessor`.
/// This indirection breaks the `rpc → node → rpc` circular dependency.
pub trait BlockProcessorCallback: Send + Sync {
    fn process_block(&self, block_bytes: &[u8]) -> Result<ProcessResult, String>;
}

/// Top-level RPC server handle.
pub struct RpcServer {
    pub port: u16,
    pub state: Arc<RpcState>,
}

/// Shared state accessible by all handlers.
///
/// The RPC crate is deliberately decoupled from `burst-node` to avoid a
/// circular dependency. The node constructs an `RpcState` and passes it in
/// when starting the RPC server.
pub struct RpcState {
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
    /// BRN computation engine (shared with the node).
    pub brn_engine: Arc<tokio::sync::Mutex<BrnEngine>>,
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
    /// Create a server with custom shared state.
    pub fn new(port: u16, state: Arc<RpcState>) -> Self {
        Self { port, state }
    }

    /// Alias for `new` — create a server with custom shared state.
    pub fn with_state(port: u16, state: Arc<RpcState>) -> Self {
        Self::new(port, state)
    }

    /// Start listening. Blocks until the server is shut down.
    pub async fn start(&self) -> Result<(), RpcError> {
        let app = Router::new()
            .route("/health", get(|| async { "ok" }))
            .route("/metrics", get(metrics_handler))
            .route("/", post(handle_rpc))
            .with_state(Arc::clone(&self.state));

        let addr = format!("0.0.0.0:{}", self.port);
        info!("RPC server listening on {}", addr);

        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| RpcError::Server(e.to_string()))?;

        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .map_err(|e| RpcError::Server(e.to_string()))?;

        Ok(())
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
        "account_pending" => handlers::handle_account_pending(params, state).await,
        "account_representative" => handlers::handle_account_representative(params, state).await,
        "process" => handlers::handle_process(params, state).await,
        "block_info" => handlers::handle_block_info(params, state).await,
        "blocks_info" => handlers::handle_blocks_info(params, state).await,
        "pending" => handlers::handle_pending(params, state).await,
        "work_generate" => handlers::handle_work_generate(params, state).await,
        "governance_proposals" => handlers::handle_governance_proposals(params, state).await,
        "governance_vote" => handlers::handle_governance_vote(params, state).await,
        "governance_proposal_info" => {
            handlers::handle_governance_proposal_info(params, state).await
        }
        "telemetry" => handlers::handle_telemetry(params, state).await,
        "peers" => handlers::handle_peers(params, state).await,
        "verification_status" => handlers::handle_verification_status(params, state).await,
        "representatives" => handlers::handle_representatives(params, state).await,
        "representatives_online" => handlers::handle_representatives_online(params, state).await,
        "send" => handlers::handle_send(params, state).await,
        "burn" => handlers::handle_burn(params, state).await,
        "receive" => handlers::handle_receive(params, state).await,
        "wallet_create" => handlers::handle_wallet_create(params, state).await,
        "wallet_info" => handlers::handle_wallet_info(params, state).await,
        "node_info" => handlers::handle_node_info(params, state).await,
        "faucet" => handlers::handle_faucet(params, state).await,
        "wallet_create_full" => handlers::handle_wallet_create_full(params, state).await,
        "burn_simple" => handlers::handle_burn_simple(params, state).await,
        "send_simple" => handlers::handle_send_simple(params, state).await,
        "receive_simple" => handlers::handle_receive_simple(params, state).await,
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
