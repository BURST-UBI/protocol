use crate::pagination::PaginationParams;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct AccountInfoRequest {
    pub account: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccountInfoResponse {
    /// Canonical first block, exposed only after the opening position is confirmed.
    pub confirmed_open: Option<String>,
    pub payment_epoch: String,
    pub release_sources: Vec<String>,
    pub has_more_releases: bool,
    pub address: String,
    pub head: String,
    pub brn_balance: String,
    pub trst_balance: String,
    pub trst_expired: String,
    pub trst_revoked: String,
    pub total_brn_burned: String,
    pub total_brn_staked: String,
    pub verification_state: String,
    pub verified_at: Option<u64>,
    pub block_count: u64,
    pub confirmation_height: u64,
    pub representative: String,
    /// Unix seconds when this wallet opted in to the verifier pool, if it has.
    pub verifier_opted_in_at: Option<u64>,
    /// Whether this representative has been evicted from the ORV consensus set.
    pub orv_evicted: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccountHistoryRequest {
    pub account: String,
    #[serde(flatten)]
    pub pagination: PaginationParams,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub hash: String,
    pub block_type: String,
    pub account: String,
    /// TRST balance recorded in this block, not the amount transferred.
    pub amount: String,
    pub timestamp: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccountHistoryResponse {
    pub account: String,
    pub history: Vec<HistoryEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccountBalanceRequest {
    pub account: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountBalanceResponse {
    pub brn_balance: String,
    pub trst_balance: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccountPendingRequest {
    pub account: String,
    /// Look up one pending source hash. Return at most one entry and no cursor;
    /// the amount threshold still applies.
    pub hash: Option<String>,
    /// Minimum amount as a decimal integer, inclusive. Defaults to zero.
    pub threshold: Option<String>,
    #[serde(flatten)]
    pub pagination: PaginationParams,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PendingEntry {
    pub hash: String,
    pub source: String,
    pub amount: String,
    pub timestamp: u64,
    pub return_credit: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccountPendingResponse {
    pub account: String,
    pub pending: Vec<PendingEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccountRepresentativeRequest {
    pub account: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccountRepresentativeResponse {
    pub account: String,
    pub representative: String,
}
