use crate::error::RpcError;
use burst_brn::BrnWalletState;
use burst_store::account::AccountInfo;
use burst_types::{BlockHash, Timestamp, TxHash};
use serde::Serialize;

/// Check address length and prefix only, without decoding its alphabet or checksum.
pub(super) fn validate_account_format(account: &str) -> Result<(), RpcError> {
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

pub(super) fn validate_positive_amount(amount_str: &str) -> Result<u128, RpcError> {
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

pub(super) fn validate_hash(hash: &str) -> Result<(), RpcError> {
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(RpcError::InvalidRequest(
            "invalid hash: expected 64-character hex string".into(),
        ));
    }
    Ok(())
}

pub(super) fn to_value<T: Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).expect("serialization should not fail")
}

/// Parse a 64-char hex string into a 32-byte BlockHash.
pub(super) fn parse_block_hash(hex_str: &str) -> Result<BlockHash, RpcError> {
    hex_str
        .parse()
        .map_err(|error: burst_types::transaction_hash::ParseHashError| {
            RpcError::InvalidRequest(error.to_string())
        })
}

/// Parse a 64-char hex string into a 32-byte TxHash.
pub(super) fn parse_tx_hash(hex_str: &str) -> Result<TxHash, RpcError> {
    hex_str
        .parse()
        .map_err(|error: burst_types::transaction_hash::ParseHashError| {
            RpcError::InvalidRequest(error.to_string())
        })
}

/// Parse a hexadecimal u64 threshold, accepting a lowercase `0x` prefix.
/// Missing input uses the caller's default; this does not enforce work limits.
pub(super) fn parse_difficulty(raw: &Option<String>, default: u64) -> Result<u64, RpcError> {
    match raw {
        Some(s) => u64::from_str_radix(s.trim_start_matches("0x"), 16)
            .map_err(|e| RpcError::InvalidRequest(format!("invalid difficulty: {e}"))),
        None => Ok(default),
    }
}

/// Build fallback BRN state from account verification time and burned/staked totals.
/// This cannot reconstruct pause history or pending releases. The rate argument is
/// unused; balance calculation uses the engine's rate history.
pub(super) fn brn_state_from_account(account: &AccountInfo, _brn_rate: u128) -> BrnWalletState {
    let verified_at = account.verified_at.unwrap_or(Timestamp::new(0));
    let mut state = BrnWalletState::new(verified_at);
    state.total_burned = account.total_brn_burned;
    state.total_staked = account.total_brn_staked;
    state.accrual_active = account.verified_at.is_some();
    if !state.accrual_active {
        state.accrual_stopped_at = Some(Timestamp::new(0));
    }
    state
}

pub(super) fn checked_brn_balance(
    engine: &burst_brn::BrnEngine,
    wallet: &BrnWalletState,
    now: Timestamp,
) -> Result<u128, RpcError> {
    engine
        .compute_balance_checked(wallet, now)
        .map_err(|error| RpcError::Server(error.to_string()))
}
