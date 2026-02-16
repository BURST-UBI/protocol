//! RPC request handlers.

use serde::{Deserialize, Serialize};

// ── Account ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AccountInfoRequest {
    pub account: String,
}

#[derive(Serialize)]
pub struct AccountInfoResponse {
    pub address: String,
    pub brn_balance: String,
    pub trst_balance: String,
    pub trst_expired: String,
    pub verification_state: String,
    pub block_count: u64,
    pub representative: String,
}

// ── Transaction ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SubmitTransactionRequest {
    pub transaction_json: String,
}

#[derive(Serialize)]
pub struct SubmitTransactionResponse {
    pub hash: String,
    pub accepted: bool,
}

// ── Block ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct BlockInfoRequest {
    pub hash: String,
}

#[derive(Serialize)]
pub struct BlockInfoResponse {
    pub block_type: String,
    pub account: String,
    pub previous: String,
    pub timestamp: u64,
    pub confirmed: bool,
}

// ── Governance ───────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ActiveProposalsResponse {
    pub proposals: Vec<ProposalSummary>,
}

#[derive(Serialize)]
pub struct ProposalSummary {
    pub hash: String,
    pub proposer: String,
    pub phase: String,
    pub description: String,
    pub votes_yea: u32,
    pub votes_nay: u32,
}

// ── Telemetry ────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct TelemetryResponse {
    pub block_count: u64,
    pub account_count: u64,
    pub peer_count: u32,
    pub protocol_version: u16,
    pub uptime_secs: u64,
}
