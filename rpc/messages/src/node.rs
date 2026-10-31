use crate::pagination::PaginationParams;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct ProcessRequest {
    /// Signed StateBlock encoded as a JSON string or hex-encoded bincode bytes.
    pub block: String,
    /// Canonical full proposal bytes, hex encoded. Never an unsigned proposal.
    pub proposal_content: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProcessResponse {
    pub hash: String,
    /// Accepted or queued for processing; this does not establish confirmation.
    pub accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlockInfoRequest {
    pub hash: String,
    #[serde(default)]
    pub include_signed_block: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlockInfoResponse {
    pub active_signing_grant: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Full signed block as a JSON string when requested, preserving u128 integers
    /// without converting them through JavaScript numbers.
    pub signed_block: Option<String>,
    pub payment_epoch: String,
    pub block_type: String,
    pub account: String,
    pub previous: String,
    pub representative: String,
    /// Cumulative BRN spend recorded in this block, not the wallet's available BRN.
    pub brn_balance: String,
    pub trst_balance: String,
    pub link: String,
    pub origin: String,
    pub transaction: String,
    pub target_head: String,
    pub timestamp: u64,
    pub confirmed: bool,
    pub confirmed_successor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlocksInfoRequest {
    pub hashes: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlocksInfoResponse {
    pub blocks: Vec<BlocksInfoEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BlocksInfoEntry {
    pub hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<BlockInfoResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkGenerateRequest {
    /// Work root: predecessor hash, or the account-bound opening root.
    pub hash: String,
    pub difficulty: Option<String>,
    pub block_type: Option<burst_ledger::BlockType>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkGenerateResponse {
    pub work: String,
    pub difficulty: String,
    /// Requested threshold divided by the current base threshold, formatted to
    /// six decimals. This is not the expected computational-work multiplier.
    pub multiplier: String,
    pub hash: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ParamsHashResponse {
    pub online_signing: String,
    pub full_governance_content: bool,
    pub delegation_key_journal: bool,
    pub payment_epoch: String,
    pub params_hash: String,
    pub block_version: u8,
    pub min_work_difficulty: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TelemetryRequest {}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SigningClockStatus {
    Disabled,
    Ready { earliest: u64, latest: u64 },
    Unavailable { reason: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TelemetryResponse {
    pub network: String,
    pub faucet_enabled: bool,
    pub block_count: u64,
    pub account_count: u64,
    pub peer_count: u32,
    pub protocol_version: u16,
    pub uptime_secs: u64,
    pub verified_count: u64,
    pub unverified_count: u64,
    /// Active tokens currently held; excludes unaccepted pending transfers.
    pub trst_circulating: String,
    pub trst_expired_total: String,
    pub trst_revoked_total: String,
    /// Unauthenticated diagnostic offset; not signing-time readiness.
    pub ntp_offset_ms: Option<i64>,
    /// Absent on older responses; never interpret absence as ready.
    #[serde(default)]
    pub signing_clock: Option<SigningClockStatus>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VerificationStatusRequest {
    pub account: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VerificationStatusResponse {
    pub account: String,
    pub status: String,
    pub phase: Option<String>,
    pub verified_at: Option<u64>,
    pub endorser_count: u32,
    pub endorsement_threshold: u32,
    pub endorsers: Vec<String>,
    pub verifier_votes: Vec<serde_json::Value>,
    pub challenge_active: bool,
    pub selection_available: bool,
    pub verifier_opted_in: bool,
    pub selected_verifiers: Vec<String>,
    pub selection_context: Option<String>,
    pub selection_unavailable_reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RepresentativesRequest {
    #[serde(flatten)]
    pub pagination: PaginationParams,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RepresentativeEntry {
    pub account: String,
    pub weight: String,
    pub delegator_count: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RepresentativesResponse {
    pub representatives: Vec<RepresentativeEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RepresentativesOnlineRequest {}

#[derive(Debug, Serialize, Deserialize)]
pub struct RepresentativeOnlineEntry {
    pub account: String,
    pub weight: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RepresentativesOnlineResponse {
    pub representatives: Vec<RepresentativeOnlineEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SendRequest {
    pub wallet: String,
    pub source: String,
    pub destination: String,
    pub amount: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SendResponse {
    pub block_hash: String,
    pub accepted: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BurnRequest {
    pub source: String,
    pub destination: String,
    pub amount: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReceiveRequest {
    pub account: String,
    pub block: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WalletCreateRequest {
    /// Accepted but ignored; this endpoint generates a fresh keypair.
    #[serde(default)]
    pub seed: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WalletCreateResponse {
    pub address: String,
    pub public_key: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FaucetRequest {
    pub account: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FaucetResponse {
    pub account: String,
    pub status: String,
    pub message: String,
    pub send_block_hash: String,
    pub amount: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GenesisEndorseRequest {
    /// Wallet to endorse. Confirmation of the endorsement does not always verify it.
    pub account: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GenesisEndorseResponse {
    pub target: String,
    pub block_hash: String,
    pub genesis: String,
    /// Target's observed verification state after the endorsement confirms.
    pub verified: bool,
    pub message: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WalletCreateFullRequest {
    /// Accepted but ignored; this endpoint generates a fresh keypair.
    #[serde(default)]
    pub seed: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WalletCreateFullResponse {
    pub address: String,
    pub public_key: String,
    pub private_key: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BurnSimpleRequest {
    pub private_key: String,
    pub amount: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BurnSimpleResponse {
    pub block_hash: String,
    pub account: String,
    pub brn_before: String,
    pub brn_after: String,
    pub trst_before: String,
    pub trst_after: String,
    pub origin: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SendSimpleRequest {
    pub private_key: String,
    pub destination: String,
    /// Burn or merge origin selected from `trst_portfolio`.
    pub origin: String,
    pub amount: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SendSimpleResponse {
    pub block_hash: String,
    pub source: String,
    pub destination: String,
    pub amount: String,
    pub trst_before: String,
    pub trst_after: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReceiveSimpleRequest {
    pub private_key: String,
    pub send_block_hash: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReceiveSimpleResponse {
    pub block_hash: String,
    pub account: String,
    pub amount: String,
    pub trst_before: String,
    pub trst_after: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChangeRepSimpleRequest {
    pub private_key: String,
    pub new_representative: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ChangeRepSimpleResponse {
    pub block_hash: String,
    pub account: String,
    pub old_representative: String,
    pub new_representative: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VerifierOptRequest {
    pub private_key: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VerifierOptResponse {
    pub block_hash: String,
    pub account: String,
    pub opted_in: bool,
}
