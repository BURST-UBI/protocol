use super::{account_not_found, deserialize_block};
use super::helpers::{validate_account_format, parse_tx_hash, to_value, brn_state_from_account, checked_brn_balance};
use crate::{error::RpcError, pagination, state::RpcState};
use burst_types::{Timestamp, WalletAddress};
use burst_store::StoreError;

pub async fn handle_account_info(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let request_started = std::time::Instant::now();
    let payment_epoch = state.meta_store.payment_epoch()?;
    let req: AccountInfoRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account_format(&req.account)?;

    let address = WalletAddress::new(req.account.clone());
    let account = state
        .account_store
        .get_account(&address)
        .map_err(|e| account_not_found(e, &req.account))?;

    let now = Timestamp::now();
    let brn_state = brn_state_from_account(&account, state.current_params()?.brn_rate);
    let lock_started = std::time::Instant::now();
    let (brn_balance, release_sources, has_more_releases, lock_acquired) = {
        let brn = state.brn_engine.lock().await;
        let lock_acquired = std::time::Instant::now();
        let wallet = brn.get_wallet(&account.address).unwrap_or(&brn_state);
        let sources = wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES);
        (
            checked_brn_balance(&brn, wallet, now)?,
            sources.iter().map(ToString::to_string).collect(),
            wallet.has_more_releases(&sources),
            lock_acquired,
        )
    };

    let lock_released = std::time::Instant::now();
    tracing::debug!(account = %account.address, head = %account.head,
        before_lock_us = lock_started.duration_since(request_started).as_micros(),
        lock_wait_us = lock_acquired.duration_since(lock_started).as_micros(),
        locked_work_us = lock_released.duration_since(lock_acquired).as_micros(),
        "account signing view stages");

    let verification_state = format!("{:?}", account.state).to_lowercase();

    let trst_expired = account.expired_trst.to_string();
    let trst_revoked = account.revoked_trst.to_string();
    if state.meta_store.payment_epoch()? != payment_epoch {
        return Err(RpcError::Server(
            "payment epoch changed while reading account; retry".into(),
        ));
    }

    Ok(to_value(&AccountInfoResponse {
        confirmed_open: if account.confirmation_height > 0 {
            state
                .block_store
                .block_at_height(&address, 1)?
                .map(|hash| hash.to_string())
        } else {
            None
        },
        payment_epoch: payment_epoch.to_string(),
        release_sources,
        has_more_releases,
        address: req.account,
        head: format!("{}", account.head),
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
        verifier_opted_in_at: account.verifier_opted_in_at.map(|t| t.as_secs()),
        orv_evicted: account.orv_evicted,
    }))
}

/// Return ascending account heights, recovering missing bodies through the node.
/// Reject an incomplete page or a changed account head/count. History `amount`
/// is the block's TRST balance, not a transfer amount.
pub async fn handle_account_history(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: AccountHistoryRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account_format(&req.account)?;

    let count = req.pagination.effective_count();
    let offset = req.pagination.decode_offset();

    let address = WalletAddress::new(req.account.clone());
    let account = match state.account_store.get_account(&address) {
        Ok(account) => Some(account),
        Err(StoreError::NotFound(_)) => None,
        Err(error) => return Err(RpcError::Store(error.to_string())),
    };
    let total = account.as_ref().map_or(0, |account| account.block_count);
    let start = offset.min(total);
    let end = start.saturating_add(u64::from(count)).min(total);
    let mut entries = std::collections::BTreeMap::new();
    let mut indexed = std::collections::BTreeMap::new();
    for offset in start..end {
        let height = offset + 1;
        if let Some(hash) = state.block_store.block_at_height(&address, height)? {
            indexed.insert(height, hash);
            match state.block_store.get_block(&hash) {
                Ok(bytes) => {
                    let block = deserialize_block(&bytes)?;
                    if block.hash != hash || block.account != address {
                        return Err(RpcError::Store(
                            "account history index/body mismatch".into(),
                        ));
                    }
                    entries.insert(height, block);
                }
                Err(StoreError::NotFound(_)) => {}
                Err(error) => return Err(RpcError::Store(error.to_string())),
            }
        }
    }
    if entries.len() as u64 != end - start {
        let recovered = state
            .block_processor
            .historical_account_page(address.clone(), start + 1, (end - start) as u32)
            .await
            .map_err(RpcError::Node)?;
        for (height, block) in recovered {
            if height <= start
                || height > end
                || block.account != address
                || indexed.get(&height).is_some_and(|hash| *hash != block.hash)
                || entries
                    .get(&height)
                    .is_some_and(|local| local.hash != block.hash)
            {
                return Err(RpcError::Node(
                    "historical account page disagrees with requested chain".into(),
                ));
            }
            entries.insert(height, block);
        }
        if entries.len() as u64 != end - start {
            return Err(RpcError::Node(
                "account history is unavailable; reconnect to an archival peer and retry".into(),
            ));
        }
    }
    if let Some(before) = account {
        let after = state.account_store.get_account(&address)?;
        if before.head != after.head || before.block_count != after.block_count {
            return Err(RpcError::Node(
                "account changed during history lookup; retry".into(),
            ));
        }
    }
    let history = entries
        .into_values()
        .map(|block| HistoryEntry {
            hash: block.hash.to_string(),
            block_type: format!("{:?}", block.block_type),
            account: block.account.to_string(),
            amount: block.trst_balance.to_string(),
            timestamp: block.timestamp.as_secs(),
        })
        .collect();

    let cursor = if end < total {
        Some(pagination::encode_cursor(end))
    } else {
        None
    };

    Ok(to_value(&AccountHistoryResponse {
        account: req.account,
        history,
        cursor,
    }))
}

pub async fn handle_account_balance(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: AccountBalanceRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account_format(&req.account)?;

    let address = WalletAddress::new(req.account.clone());
    let account = state
        .account_store
        .get_account(&address)
        .map_err(|e| account_not_found(e, &req.account))?;

    let now = Timestamp::now();
    let brn_state = brn_state_from_account(&account, state.current_params()?.brn_rate);
    let brn_balance = {
        let brn = state.brn_engine.lock().await;
        checked_brn_balance(
            &brn,
            brn.get_wallet(&account.address).unwrap_or(&brn_state),
            now,
        )?
    };

    Ok(to_value(&AccountBalanceResponse {
        brn_balance: brn_balance.to_string(),
        trst_balance: account.trst_balance.to_string(),
    }))
}

pub async fn handle_account_pending(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: AccountPendingRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account_format(&req.account)?;

    let count = req.pagination.effective_count();
    let (after, offset) = pagination::decode_pending_cursor(req.pagination.cursor.as_deref())
        .map_err(|error| RpcError::InvalidRequest(error.into()))?;

    let threshold: u128 = match req.threshold.as_deref() {
        Some(s) => s
            .parse()
            .map_err(|e| RpcError::InvalidRequest(format!("invalid threshold: {e}")))?,
        None => 0,
    };

    let address = WalletAddress::new(req.account.clone());
    let hash = req.hash.as_deref().map(parse_tx_hash).transpose()?;
    let store = state.pending_store.clone();
    state
        .read_executor
        .run(move || {
            if let Some(hash) = hash {
                let pending = match store.get_pending(&address, &hash) {
                    Ok(info) if info.amount >= threshold => vec![PendingEntry {
                        return_credit: info.return_credit,
                        hash: hash.to_string(),
                        source: info.source.to_string(),
                        amount: info.amount.to_string(),
                        timestamp: info.timestamp.as_secs(),
                    }],
                    Ok(_) | Err(StoreError::NotFound(_)) => Vec::new(),
                    Err(error) => return Err(RpcError::Store(error.to_string())),
                };
                return Ok(to_value(&AccountPendingResponse {
                    account: req.account,
                    pending,
                    cursor: None,
                }));
            }
            let page = if offset > 0 {
                store.get_pending_page(&address, threshold, offset, count)
            } else {
                store.get_pending_page_after(&address, threshold, after, count)
            }
            .map_err(|e| RpcError::Store(format!("failed to query pending: {e}")))?;

            let pending: Vec<PendingEntry> = page
                .entries
                .iter()
                .map(|(source_hash, p)| PendingEntry {
                    return_credit: p.return_credit,
                    hash: format!("{}", source_hash),
                    source: p.source.to_string(),
                    amount: p.amount.to_string(),
                    timestamp: p.timestamp.as_secs(),
                })
                .collect();

            let cursor = if page.has_more {
                page.entries
                    .last()
                    .map(|(hash, info)| pagination::encode_pending_cursor(info.amount, hash))
            } else {
                None
            };

            Ok(to_value(&AccountPendingResponse {
                account: req.account,
                pending,
                cursor,
            }))
        })
        .await
}

pub async fn handle_account_representative(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: AccountRepresentativeRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account_format(&req.account)?;

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

pub use crate::contracts::accounts::*;
