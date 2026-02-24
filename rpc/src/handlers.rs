//! RPC request/response types and handler implementations.
//!
//! Each handler accepts `(params, state)` and returns
//! `Result<serde_json::Value, RpcError>`. Handlers query real storage
//! backends, compute BRN balances, process blocks, and generate work.

use crate::error::RpcError;
use crate::pagination::{self, PaginationParams};
use crate::server::RpcState;

use crate::server::ProcessResult;
use burst_brn::BrnWalletState;
use burst_governance::Proposal;
use burst_ledger::StateBlock;
use burst_store::account::AccountInfo;
use burst_store::StoreError;
use burst_types::{BlockHash, Signature, Timestamp, TxHash, WalletAddress};

use serde::{Deserialize, Serialize};
use tracing::debug;

// ── Helpers ─────────────────────────────────────────────────────────────

fn validate_account(account: &str) -> Result<(), RpcError> {
    if account.is_empty() {
        return Err(RpcError::InvalidRequest(
            "account address must not be empty".into(),
        ));
    }
    if !account.starts_with("brst_") || account.len() != 65 {
        return Err(RpcError::InvalidRequest(
            "invalid account address: must be 65 characters starting with brst_".into(),
        ));
    }
    Ok(())
}

fn validate_positive_amount(amount_str: &str) -> Result<u128, RpcError> {
    let amount: u128 = amount_str
        .parse()
        .map_err(|e| RpcError::InvalidRequest(format!("invalid amount: {e}")))?;
    if amount == 0 {
        return Err(RpcError::InvalidRequest(
            "amount must be greater than zero".into(),
        ));
    }
    Ok(amount)
}

fn validate_hash(hash: &str) -> Result<(), RpcError> {
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(RpcError::InvalidRequest(
            "invalid hash: expected 64-character hex string".into(),
        ));
    }
    Ok(())
}

fn to_value<T: Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).expect("serialization should not fail")
}

/// Parse a 64-char hex string into a 32-byte BlockHash.
fn parse_block_hash(hex_str: &str) -> Result<BlockHash, RpcError> {
    let bytes =
        hex::decode(hex_str).map_err(|e| RpcError::InvalidRequest(format!("invalid hex: {e}")))?;
    if bytes.len() != 32 {
        return Err(RpcError::InvalidRequest(
            "hash must decode to 32 bytes".into(),
        ));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(BlockHash::new(arr))
}

/// Parse a 64-char hex string into a 32-byte TxHash.
fn parse_tx_hash(hex_str: &str) -> Result<TxHash, RpcError> {
    let bytes =
        hex::decode(hex_str).map_err(|e| RpcError::InvalidRequest(format!("invalid hex: {e}")))?;
    if bytes.len() != 32 {
        return Err(RpcError::InvalidRequest(
            "hash must decode to 32 bytes".into(),
        ));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(TxHash::new(arr))
}

/// Parse an optional difficulty string (hex u64), defaulting to protocol params.
fn parse_difficulty(raw: &Option<String>, default: u64) -> Result<u64, RpcError> {
    match raw {
        Some(s) => u64::from_str_radix(s.trim_start_matches("0x"), 16)
            .map_err(|e| RpcError::InvalidRequest(format!("invalid difficulty: {e}"))),
        None => Ok(default),
    }
}

/// Build a `BrnWalletState` from an `AccountInfo`.
/// Rate is now stored globally in `RateHistory`, not per-wallet.
fn brn_state_from_account(account: &AccountInfo, _brn_rate: u128) -> BrnWalletState {
    let verified_at = account.verified_at.unwrap_or(Timestamp::new(0));
    let mut state = BrnWalletState::new(verified_at);
    state.total_burned = account.total_brn_burned;
    state
}

/// Map a `StoreError::NotFound` to an `RpcError::AccountNotFound`.
fn account_not_found(e: StoreError, address: &str) -> RpcError {
    match e {
        StoreError::NotFound(_) => RpcError::AccountNotFound(address.to_string()),
        other => RpcError::Store(other.to_string()),
    }
}

/// Check whether a specific block is confirmed by looking up its height
/// directly and comparing against the account's `confirmation_height`.
///
/// Uses `BlockStore::height_of_block` for O(1) lookup instead of loading
/// the entire account chain.
fn is_block_confirmed(block_hash: &BlockHash, account: &WalletAddress, state: &RpcState) -> bool {
    let acct = match state.account_store.get_account(account) {
        Ok(a) => a,
        Err(_) => return false,
    };
    if acct.confirmation_height == 0 {
        return false;
    }
    match state.block_store.height_of_block(block_hash) {
        Ok(Some(height)) => height <= acct.confirmation_height,
        _ => false,
    }
}

/// Deserialize stored block bytes to a `StateBlock`, trying JSON then bincode.
fn deserialize_block(bytes: &[u8]) -> Result<StateBlock, RpcError> {
    if let Ok(block) = serde_json::from_slice::<StateBlock>(bytes) {
        return Ok(block);
    }
    bincode::deserialize::<StateBlock>(bytes)
        .map_err(|e| RpcError::Node(format!("failed to deserialize block: {e}")))
}

/// Deserialize stored proposal bytes to a `Proposal`, trying JSON then bincode.
fn deserialize_proposal(bytes: &[u8]) -> Result<Proposal, RpcError> {
    if let Ok(proposal) = serde_json::from_slice::<Proposal>(bytes) {
        return Ok(proposal);
    }
    bincode::deserialize::<Proposal>(bytes)
        .map_err(|e| RpcError::Node(format!("failed to deserialize proposal: {e}")))
}

// ═══════════════════════════════════════════════════════════════════════
// Account handlers
// ═══════════════════════════════════════════════════════════════════════

// ── account_info ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AccountInfoRequest {
    pub account: String,
}

#[derive(Debug, Serialize)]
pub struct AccountInfoResponse {
    pub address: String,
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
}

pub async fn handle_account_info(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: AccountInfoRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&req.account)?;

    let address = WalletAddress::new(req.account.clone());
    let account = state
        .account_store
        .get_account(&address)
        .map_err(|e| account_not_found(e, &req.account))?;

    let now = Timestamp::now();
    let brn_state = brn_state_from_account(&account, state.params.brn_rate);
    let brn_balance = {
        let brn = state.brn_engine.lock().await;
        brn.compute_balance(&brn_state, now)
    };

    let verification_state = format!("{:?}", account.state).to_lowercase();

    let trst_expired = account.expired_trst.to_string();
    let trst_revoked = account.revoked_trst.to_string();

    Ok(to_value(&AccountInfoResponse {
        address: req.account,
        brn_balance: brn_balance.to_string(),
        trst_balance: account.trst_balance.to_string(),
        trst_expired,
        trst_revoked,
        total_brn_burned: account.total_brn_burned.to_string(),
        total_brn_staked: account.total_brn_staked.to_string(),
        verification_state,
        verified_at: account.verified_at.map(|t| t.as_secs()),
        block_count: account.block_count,
        confirmation_height: account.confirmation_height,
        representative: account.representative.to_string(),
    }))
}

// ── account_history ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AccountHistoryRequest {
    pub account: String,
    #[serde(flatten)]
    pub pagination: PaginationParams,
}

#[derive(Debug, Serialize)]
pub struct HistoryEntry {
    pub hash: String,
    pub block_type: String,
    pub account: String,
    pub amount: String,
    pub timestamp: u64,
}

#[derive(Debug, Serialize)]
pub struct AccountHistoryResponse {
    pub account: String,
    pub history: Vec<HistoryEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

pub async fn handle_account_history(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: AccountHistoryRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&req.account)?;

    let count = req.pagination.effective_count();
    let offset = req.pagination.decode_offset();

    let address = WalletAddress::new(req.account.clone());
    let block_hashes = state
        .block_store
        .get_account_blocks(&address)
        .map_err(|e| account_not_found(e, &req.account))?;

    let total = block_hashes.len() as u64;
    let start = (offset as usize).min(block_hashes.len());
    let end = (start + count as usize).min(block_hashes.len());
    let page_hashes = &block_hashes[start..end];

    let mut history = Vec::with_capacity(page_hashes.len());
    for bh in page_hashes {
        match state.block_store.get_block(bh) {
            Ok(block_bytes) => {
                if let Ok(block) = deserialize_block(&block_bytes) {
                    history.push(HistoryEntry {
                        hash: format!("{}", bh),
                        block_type: format!("{:?}", block.block_type),
                        account: block.account.to_string(),
                        amount: block.trst_balance.to_string(),
                        timestamp: block.timestamp.as_secs(),
                    });
                }
            }
            Err(_) => {
                history.push(HistoryEntry {
                    hash: format!("{}", bh),
                    block_type: "unknown".to_string(),
                    account: req.account.clone(),
                    amount: "0".to_string(),
                    timestamp: 0,
                });
            }
        }
    }

    let cursor = if end < total as usize {
        Some(pagination::encode_cursor(end as u64))
    } else {
        None
    };

    Ok(to_value(&AccountHistoryResponse {
        account: req.account,
        history,
        cursor,
    }))
}

// ── account_balance ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AccountBalanceRequest {
    pub account: String,
}

#[derive(Debug, Serialize)]
pub struct AccountBalanceResponse {
    pub brn_balance: String,
    pub trst_balance: String,
}

pub async fn handle_account_balance(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: AccountBalanceRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&req.account)?;

    let address = WalletAddress::new(req.account.clone());
    let account = state
        .account_store
        .get_account(&address)
        .map_err(|e| account_not_found(e, &req.account))?;

    let now = Timestamp::now();
    let brn_state = brn_state_from_account(&account, state.params.brn_rate);
    let brn_balance = {
        let brn = state.brn_engine.lock().await;
        brn.compute_balance(&brn_state, now)
    };

    Ok(to_value(&AccountBalanceResponse {
        brn_balance: brn_balance.to_string(),
        trst_balance: account.trst_balance.to_string(),
    }))
}

// ── account_pending ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AccountPendingRequest {
    pub account: String,
    /// Minimum amount threshold — only return pending entries with amount >= this value.
    pub threshold: Option<String>,
    #[serde(flatten)]
    pub pagination: PaginationParams,
}

#[derive(Debug, Serialize)]
pub struct PendingEntry {
    pub hash: String,
    pub source: String,
    pub amount: String,
    pub timestamp: u64,
}

#[derive(Debug, Serialize)]
pub struct AccountPendingResponse {
    pub account: String,
    pub pending: Vec<PendingEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

pub async fn handle_account_pending(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: AccountPendingRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&req.account)?;

    let count = req.pagination.effective_count();
    let offset = req.pagination.decode_offset();

    let threshold: u128 = match req.threshold.as_deref() {
        Some(s) => s
            .parse()
            .map_err(|e| RpcError::InvalidRequest(format!("invalid threshold: {e}")))?,
        None => 0,
    };

    let address = WalletAddress::new(req.account.clone());
    let mut all_pending = state
        .pending_store
        .get_pending_for_account(&address)
        .map_err(|e| RpcError::Store(format!("failed to query pending: {e}")))?;

    // Sort by amount descending (highest priority first)
    all_pending.sort_by_key(|p| std::cmp::Reverse(p.amount));

    // Apply threshold filter
    let filtered: Vec<_> = if threshold > 0 {
        all_pending
            .into_iter()
            .filter(|p| p.amount >= threshold)
            .collect()
    } else {
        all_pending
    };

    let start = (offset as usize).min(filtered.len());
    let end = (start + count as usize).min(filtered.len());
    let page = &filtered[start..end];

    let pending: Vec<PendingEntry> = page
        .iter()
        .map(|p| PendingEntry {
            hash: format!("{}:{}", p.source, p.timestamp.as_secs()),
            source: p.source.to_string(),
            amount: p.amount.to_string(),
            timestamp: p.timestamp.as_secs(),
        })
        .collect();

    let cursor = if end < filtered.len() {
        Some(pagination::encode_cursor(end as u64))
    } else {
        None
    };

    Ok(to_value(&AccountPendingResponse {
        account: req.account,
        pending,
        cursor,
    }))
}

// ── account_representative ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AccountRepresentativeRequest {
    pub account: String,
}

#[derive(Debug, Serialize)]
pub struct AccountRepresentativeResponse {
    pub account: String,
    pub representative: String,
}

pub async fn handle_account_representative(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: AccountRepresentativeRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&req.account)?;

    let address = WalletAddress::new(req.account.clone());
    let account = state
        .account_store
        .get_account(&address)
        .map_err(|e| account_not_found(e, &req.account))?;

    Ok(to_value(&AccountRepresentativeResponse {
        account: req.account,
        representative: account.representative.to_string(),
    }))
}

// ═══════════════════════════════════════════════════════════════════════
// Block / transaction handlers
// ═══════════════════════════════════════════════════════════════════════

// ── process (submit block) ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ProcessRequest {
    pub block: String,
}

#[derive(Debug, Serialize)]
pub struct ProcessResponse {
    pub hash: String,
    pub accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

pub async fn handle_process(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: ProcessRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;

    if req.block.is_empty() {
        return Err(RpcError::InvalidRequest("block field is empty".into()));
    }

    // Deserialize the block — accept JSON string or hex-encoded bincode
    let block: StateBlock = if let Ok(b) = serde_json::from_str::<StateBlock>(&req.block) {
        b
    } else if let Ok(bytes) = hex::decode(&req.block) {
        bincode::deserialize(&bytes)
            .map_err(|e| RpcError::InvalidRequest(format!("failed to deserialize block: {e}")))?
    } else {
        return Err(RpcError::InvalidRequest(
            "block must be a JSON object or hex-encoded bytes".into(),
        ));
    };

    let block_hash_str = format!("{}", block.hash);

    // Serialize block and process through the node's block processor callback.
    // The callback handles validation, persistence, and frontier updates.
    let block_bytes = bincode::serialize(&block)
        .map_err(|e| RpcError::Server(format!("failed to serialize block: {e}")))?;

    let result = state
        .block_processor
        .process_block(&block_bytes)
        .map_err(RpcError::Server)?;

    let accepted = matches!(result, ProcessResult::Accepted);
    let detail = match &result {
        ProcessResult::Accepted => None,
        ProcessResult::Duplicate => Some("duplicate block".to_string()),
        ProcessResult::Fork => Some("fork detected — election started".to_string()),
        ProcessResult::Gap => Some("gap — previous block unknown, queued for later".to_string()),
        ProcessResult::Queued => Some("queued for processing".to_string()),
        ProcessResult::Rejected(reason) => Some(reason.clone()),
    };

    if accepted {
        debug!(hash = %block_hash_str, "block accepted via RPC");
    }

    Ok(to_value(&ProcessResponse {
        hash: block_hash_str,
        accepted,
        detail,
    }))
}

// ── block_info ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct BlockInfoRequest {
    pub hash: String,
}

#[derive(Debug, Serialize)]
pub struct BlockInfoResponse {
    pub block_type: String,
    pub account: String,
    pub previous: String,
    pub representative: String,
    pub brn_balance: String,
    pub trst_balance: String,
    pub timestamp: u64,
    pub confirmed: bool,
}

pub async fn handle_block_info(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: BlockInfoRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_hash(&req.hash)?;

    let block_hash = parse_block_hash(&req.hash)?;
    let block_bytes = state
        .block_store
        .get_block(&block_hash)
        .map_err(|e| match e {
            StoreError::NotFound(_) => RpcError::BlockNotFound(req.hash.clone()),
            other => RpcError::Store(other.to_string()),
        })?;

    let block = deserialize_block(&block_bytes)?;

    let confirmed = is_block_confirmed(&block_hash, &block.account, state);

    Ok(to_value(&BlockInfoResponse {
        block_type: format!("{:?}", block.block_type),
        account: block.account.to_string(),
        previous: format!("{}", block.previous),
        representative: block.representative.to_string(),
        brn_balance: block.brn_balance.to_string(),
        trst_balance: block.trst_balance.to_string(),
        timestamp: block.timestamp.as_secs(),
        confirmed,
    }))
}

// ── blocks_info ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct BlocksInfoRequest {
    pub hashes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct BlocksInfoResponse {
    pub blocks: Vec<BlocksInfoEntry>,
}

#[derive(Debug, Serialize)]
pub struct BlocksInfoEntry {
    pub hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<BlockInfoResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

const MAX_BATCH_SIZE: usize = 1000;

pub async fn handle_blocks_info(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: BlocksInfoRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;

    if req.hashes.is_empty() {
        return Err(RpcError::InvalidRequest(
            "hashes array must not be empty".into(),
        ));
    }

    if req.hashes.len() > MAX_BATCH_SIZE {
        return Err(RpcError::InvalidRequest(format!(
            "batch size {} exceeds maximum of {}",
            req.hashes.len(),
            MAX_BATCH_SIZE
        )));
    }

    let mut blocks = Vec::with_capacity(req.hashes.len());
    for h in &req.hashes {
        if let Err(e) = validate_hash(h) {
            blocks.push(BlocksInfoEntry {
                hash: h.clone(),
                block: None,
                error: Some(e.to_string()),
            });
            continue;
        }

        let block_hash = match parse_block_hash(h) {
            Ok(bh) => bh,
            Err(e) => {
                blocks.push(BlocksInfoEntry {
                    hash: h.clone(),
                    block: None,
                    error: Some(e.to_string()),
                });
                continue;
            }
        };

        match state.block_store.get_block(&block_hash) {
            Ok(block_bytes) => match deserialize_block(&block_bytes) {
                Ok(block) => {
                    let confirmed = is_block_confirmed(&block_hash, &block.account, state);

                    blocks.push(BlocksInfoEntry {
                        hash: h.clone(),
                        block: Some(BlockInfoResponse {
                            block_type: format!("{:?}", block.block_type),
                            account: block.account.to_string(),
                            previous: format!("{}", block.previous),
                            representative: block.representative.to_string(),
                            brn_balance: block.brn_balance.to_string(),
                            trst_balance: block.trst_balance.to_string(),
                            timestamp: block.timestamp.as_secs(),
                            confirmed,
                        }),
                        error: None,
                    });
                }
                Err(e) => {
                    blocks.push(BlocksInfoEntry {
                        hash: h.clone(),
                        block: None,
                        error: Some(e.to_string()),
                    });
                }
            },
            Err(_) => {
                blocks.push(BlocksInfoEntry {
                    hash: h.clone(),
                    block: None,
                    error: Some("block not found".to_string()),
                });
            }
        }
    }

    Ok(to_value(&BlocksInfoResponse { blocks }))
}

// ── pending (alias for account_pending) ─────────────────────────────────

pub async fn handle_pending(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    handle_account_pending(params, state).await
}

// ═══════════════════════════════════════════════════════════════════════
// Work
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct WorkGenerateRequest {
    pub hash: String,
    pub difficulty: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WorkGenerateResponse {
    pub work: String,
    pub difficulty: String,
    pub multiplier: String,
    pub hash: String,
}

pub async fn handle_work_generate(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: WorkGenerateRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_hash(&req.hash)?;

    let block_hash = parse_block_hash(&req.hash)?;
    let difficulty = parse_difficulty(&req.difficulty, state.params.min_work_difficulty)?;
    let generator = state.work_generator.clone();

    // PoW generation is CPU-intensive; run it on a blocking thread.
    let result = tokio::task::spawn_blocking(move || generator.generate(&block_hash, difficulty))
        .await
        .map_err(|e| RpcError::Server(format!("work generation task failed: {e}")))?
        .map_err(|e| RpcError::WorkError(e.to_string()))?;

    let base_difficulty = state.params.min_work_difficulty;
    let multiplier = if base_difficulty > 0 {
        difficulty as f64 / base_difficulty as f64
    } else {
        1.0
    };

    Ok(to_value(&WorkGenerateResponse {
        work: format!("{:016x}", result.0),
        difficulty: format!("{:016x}", difficulty),
        multiplier: format!("{:.6}", multiplier),
        hash: req.hash,
    }))
}

// ═══════════════════════════════════════════════════════════════════════
// Governance
// ═══════════════════════════════════════════════════════════════════════

// ── governance_proposals ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct GovernanceProposalsRequest {
    #[serde(flatten)]
    pub pagination: PaginationParams,
}

#[derive(Debug, Serialize)]
pub struct ProposalSummary {
    pub hash: String,
    pub proposer: String,
    pub phase: String,
    pub description: String,
    pub votes_yea: u32,
    pub votes_nay: u32,
}

#[derive(Debug, Serialize)]
pub struct GovernanceProposalsResponse {
    pub proposals: Vec<ProposalSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

pub async fn handle_governance_proposals(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: GovernanceProposalsRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;

    let count = req.pagination.effective_count();
    let offset = req.pagination.decode_offset();

    let active_hashes = state
        .governance_store
        .list_active_proposals()
        .unwrap_or_default();

    let start = (offset as usize).min(active_hashes.len());
    let end = (start + count as usize).min(active_hashes.len());
    let page_hashes = &active_hashes[start..end];

    let mut proposals = Vec::with_capacity(page_hashes.len());
    for ph in page_hashes {
        if let Ok(data) = state.governance_store.get_proposal(ph) {
            if let Ok(proposal) = deserialize_proposal(&data) {
                let description = proposal_description(&proposal);
                let (votes_yea, votes_nay) = current_vote_counts(&proposal);

                proposals.push(ProposalSummary {
                    hash: format!("{}", ph),
                    proposer: proposal.proposer.to_string(),
                    phase: format!("{:?}", proposal.phase),
                    description,
                    votes_yea,
                    votes_nay,
                });
            }
        }
    }

    let cursor = if end < active_hashes.len() {
        Some(pagination::encode_cursor(end as u64))
    } else {
        None
    };

    Ok(to_value(&GovernanceProposalsResponse { proposals, cursor }))
}

/// Extract a human-readable description from proposal content.
fn proposal_description(proposal: &Proposal) -> String {
    match &proposal.content {
        burst_governance::ProposalContent::ParameterChange { param, new_value } => {
            format!("Change {:?} to {}", param, new_value)
        }
        burst_governance::ProposalContent::ConstitutionalAmendment { title, .. } => {
            format!("Constitutional amendment: {}", title)
        }
        burst_governance::ProposalContent::Emergency {
            description, param, ..
        } => {
            format!("EMERGENCY {:?}: {}", param, description)
        }
    }
}

/// Get the current active vote counts (from whichever phase is active).
fn current_vote_counts(proposal: &Proposal) -> (u32, u32) {
    match proposal.phase {
        burst_governance::GovernancePhase::Exploration => (
            proposal.exploration_votes_yea,
            proposal.exploration_votes_nay,
        ),
        burst_governance::GovernancePhase::Promotion => {
            (proposal.promotion_votes_yea, proposal.promotion_votes_nay)
        }
        _ => (
            proposal.exploration_votes_yea + proposal.promotion_votes_yea,
            proposal.exploration_votes_nay + proposal.promotion_votes_nay,
        ),
    }
}

// ── governance_vote ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct GovernanceVoteRequest {
    pub proposal_hash: String,
    pub voter: String,
    pub vote: String,
}

#[derive(Debug, Serialize)]
pub struct GovernanceVoteResponse {
    pub proposal_hash: String,
    pub vote: String,
    pub accepted: bool,
}

pub async fn handle_governance_vote(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: GovernanceVoteRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_hash(&req.proposal_hash)?;
    validate_account(&req.voter)?;

    let vote_lower = req.vote.to_lowercase();
    if !["yea", "nay", "abstain"].contains(&vote_lower.as_str()) {
        return Err(RpcError::InvalidRequest(
            "vote must be one of: yea, nay, abstain".into(),
        ));
    }

    let proposal_hash = parse_tx_hash(&req.proposal_hash)?;
    let voter = WalletAddress::new(req.voter.clone());

    // Check voter is verified
    match state.account_store.get_account(&voter) {
        Ok(acct) => {
            if acct.state != burst_types::WalletState::Verified {
                return Err(RpcError::InvalidRequest(
                    "voter must be a verified wallet".into(),
                ));
            }
        }
        Err(_) => {
            return Err(RpcError::InvalidRequest("voter account not found".into()));
        }
    }

    // Verify the proposal exists
    state
        .governance_store
        .get_proposal(&proposal_hash)
        .map_err(|e| match e {
            StoreError::NotFound(_) => RpcError::ProposalNotFound(req.proposal_hash.clone()),
            other => RpcError::Store(other.to_string()),
        })?;

    // Check for double-voting
    if let Ok(existing) = state.governance_store.get_vote(&proposal_hash, &voter) {
        if !existing.is_empty() {
            return Err(RpcError::InvalidRequest(
                "voter has already voted on this proposal".into(),
            ));
        }
    }

    // Serialize and store the vote
    let vote_data = serde_json::json!({
        "voter": req.voter,
        "vote": vote_lower,
        "timestamp": Timestamp::now().as_secs(),
    });
    let vote_bytes = serde_json::to_vec(&vote_data)
        .map_err(|e| RpcError::Server(format!("failed to serialize vote: {e}")))?;

    state
        .governance_store
        .put_vote(&proposal_hash, &voter, &vote_bytes)
        .map_err(|e| RpcError::Store(format!("failed to store vote: {e}")))?;

    Ok(to_value(&GovernanceVoteResponse {
        proposal_hash: req.proposal_hash,
        vote: vote_lower,
        accepted: true,
    }))
}

// ── governance_proposal_info ────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct GovernanceProposalInfoRequest {
    pub hash: String,
}

#[derive(Debug, Serialize)]
pub struct GovernanceProposalInfoResponse {
    pub hash: String,
    pub proposer: String,
    pub phase: String,
    pub description: String,
    pub param: String,
    pub proposed_value: String,
    pub votes_yea: u32,
    pub votes_nay: u32,
    pub votes_abstain: u32,
    pub created_at: u64,
    pub phase_deadline: u64,
}

pub async fn handle_governance_proposal_info(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: GovernanceProposalInfoRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_hash(&req.hash)?;

    let proposal_hash = parse_tx_hash(&req.hash)?;
    let data = state
        .governance_store
        .get_proposal(&proposal_hash)
        .map_err(|e| match e {
            StoreError::NotFound(_) => RpcError::ProposalNotFound(req.hash.clone()),
            other => RpcError::Store(other.to_string()),
        })?;

    let proposal = deserialize_proposal(&data)?;

    let (param, proposed_value) = match &proposal.content {
        burst_governance::ProposalContent::ParameterChange { param, new_value } => {
            (format!("{:?}", param), new_value.to_string())
        }
        burst_governance::ProposalContent::ConstitutionalAmendment { title, .. } => {
            ("constitution".to_string(), title.clone())
        }
        burst_governance::ProposalContent::Emergency {
            param, new_value, ..
        } => (format!("{:?}", param), new_value.to_string()),
    };

    let (votes_yea, votes_nay, votes_abstain) = match proposal.phase {
        burst_governance::GovernancePhase::Exploration => (
            proposal.exploration_votes_yea,
            proposal.exploration_votes_nay,
            proposal.exploration_votes_abstain,
        ),
        burst_governance::GovernancePhase::Promotion => (
            proposal.promotion_votes_yea,
            proposal.promotion_votes_nay,
            proposal.promotion_votes_abstain,
        ),
        _ => (
            proposal.exploration_votes_yea + proposal.promotion_votes_yea,
            proposal.exploration_votes_nay + proposal.promotion_votes_nay,
            proposal.exploration_votes_abstain + proposal.promotion_votes_abstain,
        ),
    };

    let phase_deadline = compute_phase_deadline(&proposal, &state.params);

    Ok(to_value(&GovernanceProposalInfoResponse {
        hash: req.hash,
        proposer: proposal.proposer.to_string(),
        phase: format!("{:?}", proposal.phase),
        description: proposal_description(&proposal),
        param,
        proposed_value,
        votes_yea,
        votes_nay,
        votes_abstain,
        created_at: proposal.created_at.as_secs(),
        phase_deadline,
    }))
}

/// Compute the deadline (Unix timestamp) for the current phase of a proposal.
fn compute_phase_deadline(proposal: &Proposal, params: &burst_types::ProtocolParams) -> u64 {
    match proposal.phase {
        burst_governance::GovernancePhase::Proposal => {
            proposal.created_at.as_secs() + params.governance_proposal_duration_secs
        }
        burst_governance::GovernancePhase::Exploration => proposal
            .exploration_started_at
            .map(|t| t.as_secs() + params.governance_exploration_duration_secs)
            .unwrap_or(0),
        burst_governance::GovernancePhase::Cooldown => proposal
            .cooldown_started_at
            .map(|t| t.as_secs() + params.governance_cooldown_duration_secs)
            .unwrap_or(0),
        burst_governance::GovernancePhase::Promotion => proposal
            .promotion_started_at
            .map(|t| t.as_secs() + params.governance_promotion_duration_secs)
            .unwrap_or(0),
        _ => 0,
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Telemetry
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct TelemetryRequest {}

#[derive(Debug, Serialize)]
pub struct TelemetryResponse {
    pub block_count: u64,
    pub account_count: u64,
    pub peer_count: u32,
    pub protocol_version: u16,
    pub uptime_secs: u64,
}

pub async fn handle_telemetry(
    _params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let block_count = state.block_store.block_count().unwrap_or(0);
    let account_count = state.account_store.account_count().unwrap_or(0);
    let peer_count = state.peer_manager.read().await.connected_count() as u32;

    Ok(to_value(&TelemetryResponse {
        block_count,
        account_count,
        peer_count,
        protocol_version: 1,
        uptime_secs: now.saturating_sub(state.started_at),
    }))
}

pub async fn handle_peers(
    _params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let pm = state.peer_manager.read().await;
    let peers: Vec<String> = pm.iter_connected().map(|(id, _)| id.clone()).collect();
    Ok(serde_json::json!({ "peers": peers, "count": peers.len() }))
}

// ═══════════════════════════════════════════════════════════════════════
// Verification
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct VerificationStatusRequest {
    pub account: String,
}

#[derive(Debug, Serialize)]
pub struct VerificationStatusResponse {
    pub account: String,
    pub status: String,
    pub verified_at: Option<u64>,
    pub endorser_count: u32,
    pub challenge_active: bool,
}

pub async fn handle_verification_status(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: VerificationStatusRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&req.account)?;

    let address = WalletAddress::new(req.account.clone());

    // Get endorsements count
    let endorsements = state
        .verification_store
        .get_endorsements(&address)
        .unwrap_or_default();
    let endorser_count = endorsements.len() as u32;

    // Check for active challenge
    let challenge_active = state
        .verification_store
        .get_challenge(&address)
        .ok()
        .flatten()
        .is_some();

    // Get verification state from account store
    let (status, verified_at) = match state.account_store.get_account(&address) {
        Ok(account) => {
            let status = format!("{:?}", account.state).to_lowercase();
            let verified_at = account.verified_at.map(|t| t.as_secs());
            (status, verified_at)
        }
        Err(_) => ("unverified".to_string(), None),
    };

    Ok(to_value(&VerificationStatusResponse {
        account: req.account,
        status,
        verified_at,
        endorser_count,
        challenge_active,
    }))
}

// ═══════════════════════════════════════════════════════════════════════
// Representatives
// ═══════════════════════════════════════════════════════════════════════

// ── representatives (list with weight) ──────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RepresentativesRequest {
    #[serde(flatten)]
    pub pagination: PaginationParams,
}

#[derive(Debug, Serialize)]
pub struct RepresentativeEntry {
    pub account: String,
    pub weight: String,
    pub delegator_count: u64,
}

#[derive(Debug, Serialize)]
pub struct RepresentativesResponse {
    pub representatives: Vec<RepresentativeEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

pub async fn handle_representatives(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: RepresentativesRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;

    let count = req.pagination.effective_count();
    let offset = req.pagination.decode_offset();

    let mut reps: Vec<(String, u128, u64)> = {
        let cache = state.rep_weight_cache.read().await;
        cache
            .all_weights()
            .iter()
            .map(|(addr, &weight)| (addr.to_string(), weight, 0u64))
            .collect()
    };
    reps.sort_by_key(|r| std::cmp::Reverse(r.1));

    let start = (offset as usize).min(reps.len());
    let end = (start + count as usize).min(reps.len());
    let page = &reps[start..end];

    let representatives: Vec<RepresentativeEntry> = page
        .iter()
        .map(|(account, weight, delegator_count)| RepresentativeEntry {
            account: account.clone(),
            weight: weight.to_string(),
            delegator_count: *delegator_count,
        })
        .collect();

    let cursor = if end < reps.len() {
        Some(pagination::encode_cursor(end as u64))
    } else {
        None
    };

    Ok(to_value(&RepresentativesResponse {
        representatives,
        cursor,
    }))
}

// ── representatives_online ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RepresentativesOnlineRequest {}

#[derive(Debug, Serialize)]
pub struct RepresentativeOnlineEntry {
    pub account: String,
    pub weight: String,
}

#[derive(Debug, Serialize)]
pub struct RepresentativesOnlineResponse {
    pub representatives: Vec<RepresentativeOnlineEntry>,
}

pub async fn handle_representatives_online(
    _params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let reps = state
        .online_reps
        .read()
        .map_err(|_| RpcError::Server("failed to read online representatives".into()))?;

    let representatives: Vec<RepresentativeOnlineEntry> = reps
        .iter()
        .map(|(addr, weight)| RepresentativeOnlineEntry {
            account: addr.to_string(),
            weight: weight.to_string(),
        })
        .collect();

    Ok(to_value(&RepresentativesOnlineResponse { representatives }))
}

// ═══════════════════════════════════════════════════════════════════════
// Convenience transaction endpoints
// ═══════════════════════════════════════════════════════════════════════

// ── send (TRST transfer) ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SendRequest {
    pub wallet: String,
    pub source: String,
    pub destination: String,
    pub amount: String,
}

#[derive(Debug, Serialize)]
pub struct SendResponse {
    pub block_hash: String,
    pub accepted: bool,
}

pub async fn handle_send(
    params: serde_json::Value,
    _state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: SendRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&req.source)?;
    validate_account(&req.destination)?;
    let _amount = validate_positive_amount(&req.amount)?;

    Err(RpcError::InvalidRequest(
        "send requires a signed block via 'process' — use wallet_core to build and sign, then submit via 'process'".into(),
    ))
}

// ── burn (BRN → TRST) ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct BurnRequest {
    pub source: String,
    pub destination: String,
    pub amount: String,
}

pub async fn handle_burn(
    params: serde_json::Value,
    _state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: BurnRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&req.source)?;
    validate_account(&req.destination)?;
    let _amount = validate_positive_amount(&req.amount)?;

    Err(RpcError::InvalidRequest(
        "burn requires a signed block via 'process' — use wallet_core to build and sign, then submit via 'process'".into(),
    ))
}

// ── receive ─────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ReceiveRequest {
    pub account: String,
    pub block: String,
}

pub async fn handle_receive(
    params: serde_json::Value,
    _state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: ReceiveRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&req.account)?;

    Err(RpcError::InvalidRequest(
        "receive requires a signed block via 'process' — use wallet_core to build and sign, then submit via 'process'".into(),
    ))
}

// ── wallet_create ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct WalletCreateRequest {
    #[serde(default)]
    pub seed: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WalletCreateResponse {
    pub address: String,
    pub public_key: String,
}

// Private key is NOT returned over RPC for security.
// Wallet creation with key custody should be done client-side.
pub async fn handle_wallet_create(
    params: serde_json::Value,
    _state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let _req: WalletCreateRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;

    let kp = burst_crypto::generate_keypair();
    let address = burst_crypto::derive_address(&kp.public);
    let public_key = hex::encode(kp.public.as_bytes());

    Ok(to_value(&WalletCreateResponse {
        address: address.to_string(),
        public_key,
    }))
}

// ── wallet_info ─────────────────────────────────────────────────────────

pub async fn handle_wallet_info(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    // Alias for account_info
    handle_account_info(params, state).await
}

// ── node_info ───────────────────────────────────────────────────────────

pub async fn handle_node_info(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    // Alias for telemetry
    handle_telemetry(params, state).await
}

// ═══════════════════════════════════════════════════════════════════════
// Testnet faucet
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct FaucetRequest {
    pub account: String,
}

#[derive(Debug, Serialize)]
pub struct FaucetResponse {
    pub account: String,
    pub status: String,
    pub message: String,
}

/// Testnet-only faucet: auto-verifies a wallet and credits test TRST.
pub async fn handle_faucet(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    if !state.enable_faucet {
        return Err(RpcError::InvalidRequest(
            "faucet is disabled on this node".into(),
        ));
    }

    let req: FaucetRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&req.account)?;

    let address = WalletAddress::new(req.account.clone());
    let now = Timestamp::now();

    let mut account_info = state
        .account_store
        .get_account(&address)
        .unwrap_or_else(|_| AccountInfo {
            address: address.clone(),
            state: burst_types::WalletState::Unverified,
            verified_at: None,
            head: BlockHash::ZERO,
            representative: address.clone(),
            block_count: 0,
            confirmation_height: 0,
            total_brn_burned: 0,
            trst_balance: 0,
            total_brn_staked: 0,
            expired_trst: 0,
            revoked_trst: 0,
            epoch: 0,
        });

    account_info.state = burst_types::WalletState::Verified;
    account_info.verified_at = Some(now);
    account_info.trst_balance = account_info
        .trst_balance
        .saturating_add(1_000_000_000_000_000_000);

    state
        .account_store
        .put_account(&account_info)
        .map_err(|e| RpcError::Store(format!("failed to update faucet account: {e}")))?;

    Ok(to_value(&FaucetResponse {
        account: req.account,
        status: "ok".to_string(),
        message: "Account verified and 1 TRST credited (testnet faucet)".to_string(),
    }))
}

// ═══════════════════════════════════════════════════════════════════════
// Testnet convenience RPCs (faucet-only)
//
// These endpoints construct, sign, and submit blocks server-side so that
// testnet validation can be performed via simple curl commands without a
// local wallet. They are gated behind `enable_faucet`.
// ═══════════════════════════════════════════════════════════════════════

fn require_faucet(state: &RpcState) -> Result<(), RpcError> {
    if !state.enable_faucet {
        return Err(RpcError::InvalidRequest(
            "this endpoint is only available on testnet nodes with faucet enabled".into(),
        ));
    }
    Ok(())
}

fn parse_private_key(hex_str: &str) -> Result<burst_types::PrivateKey, RpcError> {
    let bytes = hex::decode(hex_str)
        .map_err(|e| RpcError::InvalidRequest(format!("invalid private key hex: {e}")))?;
    if bytes.len() != 32 {
        return Err(RpcError::InvalidRequest(
            "private key must be 32 bytes (64 hex chars)".into(),
        ));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(burst_types::PrivateKey(arr))
}

/// Build a StateBlock, compute its hash, sign it, and generate PoW.
#[allow(clippy::too_many_arguments)]
fn build_and_sign_block(
    block_type: burst_ledger::BlockType,
    account: &WalletAddress,
    previous: BlockHash,
    representative: &WalletAddress,
    brn_balance: u128,
    trst_balance: u128,
    link: BlockHash,
    origin: TxHash,
    transaction: TxHash,
    private_key: &burst_types::PrivateKey,
    work_generator: &burst_work::WorkGenerator,
    min_work_difficulty: u64,
) -> Result<burst_ledger::StateBlock, RpcError> {
    use burst_ledger::CURRENT_BLOCK_VERSION;

    let now = Timestamp::now();

    let mut block = burst_ledger::StateBlock {
        version: CURRENT_BLOCK_VERSION,
        block_type,
        account: account.clone(),
        previous,
        representative: representative.clone(),
        brn_balance,
        trst_balance,
        link,
        origin,
        transaction,
        timestamp: now,
        work: 0,
        signature: Signature([0u8; 64]),
        hash: BlockHash::ZERO,
    };

    block.hash = block.compute_hash();
    block.signature = burst_crypto::sign_message(block.hash.as_bytes(), private_key);

    let work_kind = match block.block_type {
        burst_ledger::BlockType::Open | burst_ledger::BlockType::Receive => {
            burst_work::WorkBlockKind::ReceiveOrOpen
        }
        _ => burst_work::WorkBlockKind::Base,
    };
    let threshold =
        burst_work::WorkThresholds::with_base(min_work_difficulty).threshold_for(work_kind);
    let nonce = work_generator
        .generate(&block.hash, threshold)
        .map_err(|e| RpcError::Server(format!("work generation failed: {e}")))?;
    block.work = nonce.0;

    Ok(block)
}

/// Submit a block through the block processor and return whether it was accepted.
fn submit_block(block: &burst_ledger::StateBlock, state: &RpcState) -> Result<bool, RpcError> {
    let block_bytes = bincode::serialize(block)
        .map_err(|e| RpcError::Server(format!("block serialization failed: {e}")))?;
    let result = state
        .block_processor
        .process_block(&block_bytes)
        .map_err(RpcError::Server)?;
    let accepted = matches!(result, crate::server::ProcessResult::Accepted);
    if !accepted {
        let detail = match &result {
            crate::server::ProcessResult::Rejected(r) => r.clone(),
            crate::server::ProcessResult::Duplicate => "duplicate block".to_string(),
            crate::server::ProcessResult::Fork => "fork detected".to_string(),
            crate::server::ProcessResult::Gap => "gap — previous block unknown".to_string(),
            crate::server::ProcessResult::Queued => "queued".to_string(),
            _ => "unknown".to_string(),
        };
        return Err(RpcError::Server(format!("block not accepted: {detail}")));
    }
    Ok(true)
}

// ── wallet_create_full (testnet-only, returns private key) ──────────

#[derive(Debug, Deserialize)]
pub struct WalletCreateFullRequest {
    #[serde(default)]
    pub seed: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WalletCreateFullResponse {
    pub address: String,
    pub public_key: String,
    pub private_key: String,
}

pub async fn handle_wallet_create_full(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    require_faucet(state)?;

    let _req: WalletCreateFullRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;

    let kp = burst_crypto::generate_keypair();
    let address = burst_crypto::derive_address(&kp.public);

    Ok(to_value(&WalletCreateFullResponse {
        address: address.to_string(),
        public_key: hex::encode(kp.public.0),
        private_key: hex::encode(kp.private.0),
    }))
}

// ── burn_simple (BRN → TRST) ────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct BurnSimpleRequest {
    pub private_key: String,
    pub amount: String,
}

#[derive(Debug, Serialize)]
pub struct BurnSimpleResponse {
    pub block_hash: String,
    pub account: String,
    pub brn_before: String,
    pub brn_after: String,
    pub trst_before: String,
    pub trst_after: String,
    pub origin: String,
}

pub async fn handle_burn_simple(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    require_faucet(state)?;

    let req: BurnSimpleRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    let amount = validate_positive_amount(&req.amount)?;
    let private_key = parse_private_key(&req.private_key)?;
    let public_key = burst_crypto::public_from_private(&private_key);
    let address = burst_crypto::derive_address(&public_key);

    let account = state
        .account_store
        .get_account(&address)
        .map_err(|e| account_not_found(e, address.as_str()))?;

    if account.state != burst_types::WalletState::Verified {
        return Err(RpcError::InvalidRequest(
            "account must be verified to burn BRN (use faucet first)".into(),
        ));
    }

    let now = Timestamp::now();
    let brn_state = brn_state_from_account(&account, state.params.brn_rate);
    let brn_balance = {
        let brn = state.brn_engine.lock().await;
        brn.compute_balance(&brn_state, now)
    };

    if brn_balance < amount {
        return Err(RpcError::InvalidRequest(format!(
            "insufficient BRN: have {brn_balance}, need {amount}"
        )));
    }

    let brn_after = brn_balance - amount;
    let trst_before = account.trst_balance;
    let trst_after = trst_before + amount;

    let tx_hash = TxHash::new(burst_crypto::blake2b_256(
        &[
            address.as_str().as_bytes(),
            &now.as_secs().to_be_bytes(),
            b"burn",
        ]
        .concat(),
    ));

    let pk_bytes = private_key.0;
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let representative = account.representative.clone();
        let previous = account.head;
        let work_gen = state.work_generator.clone();
        let min_diff = state.params.min_work_difficulty;
        move || {
            let pk = burst_types::PrivateKey(pk_bytes);
            build_and_sign_block(
                burst_ledger::BlockType::Burn,
                &address,
                previous,
                &representative,
                brn_after,
                trst_after,
                BlockHash::ZERO,
                tx_hash,
                tx_hash,
                &pk,
                &work_gen,
                min_diff,
            )
        }
    })
    .await
    .map_err(|e| RpcError::Server(format!("block build task failed: {e}")))??;

    submit_block(&block, state)?;

    let mut updated = account.clone();
    updated.head = block.hash;
    updated.block_count += 1;
    updated.total_brn_burned += amount;
    updated.trst_balance = trst_after;

    state
        .account_store
        .put_account(&updated)
        .map_err(|e| RpcError::Store(format!("failed to update account: {e}")))?;

    let block_bytes = bincode::serialize(&block)
        .map_err(|e| RpcError::Server(format!("block serialization failed: {e}")))?;
    let _ = state.block_store.put_block(&block.hash, &block_bytes);

    Ok(to_value(&BurnSimpleResponse {
        block_hash: format!("{}", block.hash),
        account: address.to_string(),
        brn_before: brn_balance.to_string(),
        brn_after: brn_after.to_string(),
        trst_before: trst_before.to_string(),
        trst_after: trst_after.to_string(),
        origin: format!("{}", tx_hash),
    }))
}

// ── send_simple (TRST transfer) ─────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SendSimpleRequest {
    pub private_key: String,
    pub destination: String,
    pub amount: String,
}

#[derive(Debug, Serialize)]
pub struct SendSimpleResponse {
    pub block_hash: String,
    pub source: String,
    pub destination: String,
    pub amount: String,
    pub trst_before: String,
    pub trst_after: String,
}

pub async fn handle_send_simple(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    require_faucet(state)?;

    let req: SendSimpleRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&req.destination)?;
    let amount = validate_positive_amount(&req.amount)?;
    let private_key = parse_private_key(&req.private_key)?;
    let public_key = burst_crypto::public_from_private(&private_key);
    let address = burst_crypto::derive_address(&public_key);

    let account = state
        .account_store
        .get_account(&address)
        .map_err(|e| account_not_found(e, address.as_str()))?;

    if account.state != burst_types::WalletState::Verified {
        return Err(RpcError::InvalidRequest(
            "account must be verified to send TRST".into(),
        ));
    }

    if account.trst_balance < amount {
        return Err(RpcError::InvalidRequest(format!(
            "insufficient TRST: have {}, need {amount}",
            account.trst_balance
        )));
    }

    let trst_before = account.trst_balance;
    let trst_after = trst_before - amount;
    let destination = WalletAddress::new(req.destination.clone());

    let now = Timestamp::now();
    let brn_state = brn_state_from_account(&account, state.params.brn_rate);
    let brn_balance = {
        let brn = state.brn_engine.lock().await;
        brn.compute_balance(&brn_state, now)
    };

    let dest_hash = BlockHash::new(burst_crypto::blake2b_256(destination.as_str().as_bytes()));
    let tx_hash = TxHash::new(burst_crypto::blake2b_256(
        &[
            address.as_str().as_bytes(),
            &now.as_secs().to_be_bytes(),
            b"send",
        ]
        .concat(),
    ));

    let pk_bytes = private_key.0;
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let representative = account.representative.clone();
        let previous = account.head;
        let work_gen = state.work_generator.clone();
        let min_diff = state.params.min_work_difficulty;
        move || {
            let pk = burst_types::PrivateKey(pk_bytes);
            build_and_sign_block(
                burst_ledger::BlockType::Send,
                &address,
                previous,
                &representative,
                brn_balance,
                trst_after,
                dest_hash,
                TxHash::ZERO,
                tx_hash,
                &pk,
                &work_gen,
                min_diff,
            )
        }
    })
    .await
    .map_err(|e| RpcError::Server(format!("block build task failed: {e}")))??;

    submit_block(&block, state)?;

    let mut updated = account.clone();
    updated.head = block.hash;
    updated.block_count += 1;
    updated.trst_balance = trst_after;

    state
        .account_store
        .put_account(&updated)
        .map_err(|e| RpcError::Store(format!("failed to update account: {e}")))?;

    let block_bytes = bincode::serialize(&block)
        .map_err(|e| RpcError::Server(format!("block serialization failed: {e}")))?;
    let _ = state.block_store.put_block(&block.hash, &block_bytes);

    state
        .pending_store
        .put_pending(
            &destination,
            &tx_hash,
            &burst_store::pending::PendingInfo {
                source: address.clone(),
                amount,
                timestamp: now,
                provenance: vec![],
            },
        )
        .map_err(|e| RpcError::Store(format!("failed to create pending: {e}")))?;

    Ok(to_value(&SendSimpleResponse {
        block_hash: format!("{}", block.hash),
        source: address.to_string(),
        destination: req.destination,
        amount: req.amount,
        trst_before: trst_before.to_string(),
        trst_after: trst_after.to_string(),
    }))
}

// ── receive_simple (pocket a pending TRST transfer) ──────────────────

#[derive(Debug, Deserialize)]
pub struct ReceiveSimpleRequest {
    pub private_key: String,
    pub send_block_hash: String,
}

#[derive(Debug, Serialize)]
pub struct ReceiveSimpleResponse {
    pub block_hash: String,
    pub account: String,
    pub amount: String,
    pub trst_before: String,
    pub trst_after: String,
}

pub async fn handle_receive_simple(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    require_faucet(state)?;

    let req: ReceiveSimpleRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    let private_key = parse_private_key(&req.private_key)?;
    let public_key = burst_crypto::public_from_private(&private_key);
    let address = burst_crypto::derive_address(&public_key);

    let send_tx_hash = parse_tx_hash(&req.send_block_hash)?;

    let pending = state
        .pending_store
        .get_pending(&address, &send_tx_hash)
        .map_err(|e| {
            RpcError::InvalidRequest(format!(
                "no pending receive found for hash {}: {e}",
                req.send_block_hash
            ))
        })?;

    let account = state
        .account_store
        .get_account(&address)
        .unwrap_or_else(|_| AccountInfo {
            address: address.clone(),
            state: burst_types::WalletState::Unverified,
            verified_at: None,
            head: BlockHash::ZERO,
            representative: address.clone(),
            block_count: 0,
            confirmation_height: 0,
            total_brn_burned: 0,
            trst_balance: 0,
            total_brn_staked: 0,
            expired_trst: 0,
            revoked_trst: 0,
            epoch: 0,
        });

    let trst_before = account.trst_balance;
    let trst_after = trst_before + pending.amount;

    let now = Timestamp::now();
    let brn_state = brn_state_from_account(&account, state.params.brn_rate);
    let brn_balance = {
        let brn = state.brn_engine.lock().await;
        brn.compute_balance(&brn_state, now)
    };

    let is_open = account.head == BlockHash::ZERO;
    let block_type = if is_open {
        burst_ledger::BlockType::Open
    } else {
        burst_ledger::BlockType::Receive
    };

    let link = BlockHash::new(*send_tx_hash.as_bytes());
    let tx_hash = TxHash::new(burst_crypto::blake2b_256(
        &[
            address.as_str().as_bytes(),
            &now.as_secs().to_be_bytes(),
            b"receive",
        ]
        .concat(),
    ));

    let pk_bytes = private_key.0;
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let representative = account.representative.clone();
        let previous = account.head;
        let work_gen = state.work_generator.clone();
        let min_diff = state.params.min_work_difficulty;
        move || {
            let pk = burst_types::PrivateKey(pk_bytes);
            build_and_sign_block(
                block_type,
                &address,
                previous,
                &representative,
                brn_balance,
                trst_after,
                link,
                TxHash::ZERO,
                tx_hash,
                &pk,
                &work_gen,
                min_diff,
            )
        }
    })
    .await
    .map_err(|e| RpcError::Server(format!("block build task failed: {e}")))??;

    submit_block(&block, state)?;

    let mut updated = account.clone();
    updated.head = block.hash;
    updated.block_count += 1;
    updated.trst_balance = trst_after;

    state
        .account_store
        .put_account(&updated)
        .map_err(|e| RpcError::Store(format!("failed to update account: {e}")))?;

    let block_bytes = bincode::serialize(&block)
        .map_err(|e| RpcError::Server(format!("block serialization failed: {e}")))?;
    let _ = state.block_store.put_block(&block.hash, &block_bytes);

    state
        .pending_store
        .delete_pending(&address, &send_tx_hash)
        .map_err(|e| RpcError::Store(format!("failed to delete pending: {e}")))?;

    Ok(to_value(&ReceiveSimpleResponse {
        block_hash: format!("{}", block.hash),
        account: address.to_string(),
        amount: pending.amount.to_string(),
        trst_before: trst_before.to_string(),
        trst_after: trst_after.to_string(),
    }))
}
