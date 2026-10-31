//! HTTP server for single or batched action requests, plus health and metrics.

use crate::error::RpcError;
use crate::handlers;

use axum::{
    extract::{ConnectInfo, State, DefaultBodyLimit},
    Extension,
    http::{header, StatusCode, HeaderMap},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use burst_types::WalletAddress;

use prometheus::{Encoder, TextEncoder};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{info, warn};
pub use crate::state::*;

/// Top-level RPC server handle.
pub struct RpcServer {
    pub port: u16,
    /// Address to bind to (e.g. `127.0.0.1` or `0.0.0.0`).
    pub bind: String,
    pub state: Arc<RpcState>,
}

/// Action name and parameters in a single JSON object.
#[derive(Debug, Deserialize)]
struct RpcRequest {
    action: String,
    #[serde(flatten)]
    params: serde_json::Value,
}

/// Result or error for one action; no JSON-RPC 2.0 request ID.
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

impl RpcServer {
    /// Create a server bound to localhost (`127.0.0.1`) with custom shared
    /// state. Use [`RpcServer::with_bind`] to bind a different address.
    pub fn new(port: u16, state: Arc<RpcState>) -> Self {
        Self::with_bind(port, "127.0.0.1".to_string(), state)
    }

    /// Create a server bound to an explicit address with custom shared state.
    pub fn with_bind(port: u16, bind: String, state: Arc<RpcState>) -> Self {
        Self { port, bind, state }
    }

    /// Serve requests until shutdown, then drain existing HTTP sessions.
    /// HTTP handling allows 32 concurrent requests and a 30-second deadline.
    /// Dropping a timed-out handler does not necessarily stop its blocking work;
    /// individual workers own their cancellation and capacity limits.
    pub async fn start(
        &self,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), RpcError> {
        if !crate::security::authority_bind_allowed(&self.bind, self.state.genesis_seed.is_some()) {
            return Err(RpcError::Server("genesis authority RPC must bind to a numeric loopback address".into()));
        }
        let token = match std::env::var("BURST_RPC_ADMIN_TOKEN") {
            Ok(token) => Some(token),
            Err(std::env::VarError::NotPresent) => None,
            Err(_) => return Err(RpcError::Server("invalid BURST_RPC_ADMIN_TOKEN encoding".into())),
        };
        let security = crate::security::Security::new(token.as_deref()).map_err(RpcError::Server)?;
        let requests = security.requests.clone();
        let app = Router::new()
            .route("/health", get(|| async { "ok" }))
            .route("/metrics", get(metrics_handler))
            .route("/", post(handle_rpc))
            .with_state(Arc::clone(&self.state))
            .layer(DefaultBodyLimit::max(crate::security::MAX_REQUEST_BYTES))
            .layer(Extension(security))
            .layer(axum::middleware::from_fn(move |request: axum::extract::Request, next: axum::middleware::Next| {
                let requests = requests.clone();
                async move {
                    let Ok(_permit) = requests.try_acquire_owned() else {
                        return (StatusCode::TOO_MANY_REQUESTS, "RPC busy").into_response();
                    };
                    match tokio::time::timeout(std::time::Duration::from_secs(30), next.run(request)).await {
                        Ok(response) => response,
                        Err(_) => (StatusCode::REQUEST_TIMEOUT, "RPC deadline exceeded").into_response(),
                    }
                }
            }));

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

        let listener = tokio::net::TcpListener::bind((self.bind.as_str(), self.port))
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

/// Authorize and rate-limit the envelope before executing batch actions in order.
/// A batch is not atomic. Action errors use HTTP 200 with per-action error bodies.
async fn handle_rpc(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<RpcState>>,
    Extension(security): Extension<crate::security::Security>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let cost = match security.authorize(&headers, &body) {
        Ok(cost) => cost,
        Err(status) => return (status, Json(serde_json::json!({"error": if status == StatusCode::UNAUTHORIZED {
            "admin authorization required"
        } else { "invalid batch size" }}))),
    };
    let client_ip = addr.ip().to_string();
    if !state.rate_limiter.check_cost(&client_ip, cost as u32) {
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

/// Parse one action request and route it to its handler.
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
        "bootstrap_status" => handlers::handle_bootstrap_status(params, state).await,
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
                .confirmed_account_slot_async(query)
                .await
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
            let processor = state.block_processor.clone();
            let review = state.read_executor.run(move || {
                processor.payment_exclusion(query).map_err(RpcError::Server)
            }).await?;
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
