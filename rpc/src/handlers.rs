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

mod confirmed_successor;
mod governance_projection;
pub(crate) mod provenance;

// ── Helpers ─────────────────────────────────────────────────────────────

impl RpcState {
    /// Governance persists activated parameters; startup configuration is only
    /// the fallback before the first activation.
    fn current_params(&self) -> Result<burst_types::ProtocolParams, RpcError> {
        match self.brn_store.get_meta(b"protocol_params")? {
            Some(bytes) => bincode::deserialize(&bytes).map_err(|e| {
                RpcError::Server(format!("invalid persisted protocol parameters: {e}"))
            }),
            None => Ok((*self.params).clone()),
        }
    }
}

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
    state.total_staked = account.total_brn_staked;
    state.accrual_active = account.verified_at.is_some();
    if !state.accrual_active {
        state.accrual_stopped_at = Some(Timestamp::new(0));
    }
    state
}

fn checked_brn_balance(
    engine: &burst_brn::BrnEngine,
    wallet: &BrnWalletState,
    now: Timestamp,
) -> Result<u128, RpcError> {
    engine
        .compute_balance_checked(wallet, now)
        .map_err(|error| RpcError::Server(error.to_string()))
}

#[cfg(test)]
mod brn_balance_tests {
    use super::*;

    #[test]
    fn checked_rpc_balance_distinguishes_zero_funds_from_overflow() {
        let wallet = BrnWalletState::new(Timestamp::EPOCH);
        let now = Timestamp::new(2);
        let zero = burst_brn::BrnEngine::with_rate(0, Timestamp::EPOCH);
        assert_eq!(checked_brn_balance(&zero, &wallet, now).unwrap(), 0);
        let valid = burst_brn::BrnEngine::with_rate(7, Timestamp::EPOCH);
        assert_eq!(checked_brn_balance(&valid, &wallet, now).unwrap(), 14);
        let overflowing = burst_brn::BrnEngine::with_rate(u128::MAX, Timestamp::EPOCH);
        assert!(matches!(checked_brn_balance(&overflowing, &wallet, now),
            Err(RpcError::Server(message)) if message.contains("overflow")));
    }
}

/// Decision 5.11(b): expose inputs, so wallets compute BRN locally. The old
/// account balance field remains for compatibility; it is not a ledger field.
pub async fn handle_brn_components(
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
    let brn = state.brn_engine.lock().await;
    let wallet = brn
        .get_wallet(&address)
        .cloned()
        .unwrap_or_else(|| brn_state_from_account(&account, 0));
    Ok(serde_json::json!({
        "account": req.account, "verified_at": wallet.verified_at.as_secs(),
        "total_burned": wallet.total_burned.to_string(), "total_staked": wallet.total_staked.to_string(),
        "release_sources": wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES).iter().map(ToString::to_string).collect::<Vec<_>>(),
        "accrual_active": wallet.accrual_active,
        "accrual_stopped_at": wallet.accrual_stopped_at.map(|t| t.as_secs()),
        "paused_intervals": wallet.paused_intervals.iter().map(|(start,end)| serde_json::json!({"start":start.as_secs(),"end":end.as_secs()})).collect::<Vec<_>>(),
        "as_of": Timestamp::now().as_secs(),
        "rate_history": brn.rate_history.segments.iter().map(|s| serde_json::json!({
            "rate": s.rate.to_string(), "start": s.start.as_secs(), "end": s.end.map(|t|t.as_secs())
        })).collect::<Vec<_>>()
    }))
}

pub async fn handle_trst_portfolio(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: AccountInfoRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&req.account)?;
    let address = WalletAddress::new(req.account);
    let trst = state.trst_engine.lock().await;
    let outgoing = state.pending_store.get_pending_from_account(&address)?;
    portfolio_response(&trst, &address, Timestamp::now(), &outgoing)
}

fn portfolio_response(
    trst: &burst_trst::TrstEngine,
    address: &WalletAddress,
    now: Timestamp,
    outgoing: &[(WalletAddress, TxHash, burst_store::pending::PendingInfo)],
) -> Result<serde_json::Value, RpcError> {
    let outgoing: Vec<_> = outgoing
        .iter()
        .filter(|(_, _, pending)| !pending.return_credit)
        .collect();
    let pending = outgoing.iter().try_fold(0u128, |total, (_, _, pending)| {
        if &pending.source != address {
            return Err(RpcError::Server(
                "pending transfer belongs to another sender".into(),
            ));
        }
        total
            .checked_add(pending.amount)
            .ok_or_else(|| RpcError::Server("pending amount overflow".into()))
    })?;
    let mut active = 0u128;
    let mut expired = 0u128;
    let mut revoked = 0u128;
    let mut batches = Vec::new();
    if let Some(portfolio) = trst.get_portfolio(address) {
        for token in &portfolio.tokens {
            let (status, sum) = if token.state == burst_types::TrstState::Revoked {
                ("revoked", &mut revoked)
            } else if token.is_expired(now, trst.expiry_secs) {
                ("expired", &mut expired)
            } else {
                ("active", &mut active)
            };
            *sum = sum
                .checked_add(token.amount)
                .ok_or_else(|| RpcError::Server("portfolio amount overflow".into()))?;
            let origin_kind = if trst.merger_graph.contains_merge(&token.origin) {
                "merge"
            } else if trst
                .wallet_origins
                .get(&token.origin_wallet)
                .is_some_and(|origins| origins.contains(&token.origin))
            {
                "burn"
            } else {
                "unknown"
            };
            batches.push(serde_json::json!({ "id":token.id.to_string(), "origin": token.origin.to_string(),
                "origin_kind":origin_kind,
                "link": token.link.to_string(), "amount":token.amount.to_string(), "originator":token.origin_wallet.as_str(),
                "timestamp":token.effective_origin_timestamp.as_secs(),
                "expiry_at": if trst.expiry_secs == u64::MAX { None } else { Some(token.earliest_expiry(trst.expiry_secs).as_secs()) },
                "reference_value_bps": token.current_value_bps(now, trst.expiry_secs),
                "status":status, "chain_length":null }));
        }
    }
    Ok(
        serde_json::json!({"batches":batches,"total_active":active.to_string(),"total_expired":expired.to_string(),"total_revoked":revoked.to_string(),
        "total_pending":pending.to_string(), "pending_count":outgoing.len()}),
    )
}

#[cfg(test)]
mod portfolio_tests {
    use super::*;

    #[test]
    fn linear_reference_does_not_change_face_amount_or_expiry_state() {
        let owner = WalletAddress::new("brst_reference");
        let mut engine = burst_trst::TrstEngine::with_expiry(100);
        let token = engine
            .mint(
                TxHash::new([1; 32]),
                owner.clone(),
                1_000,
                owner.clone(),
                Timestamp::new(100),
            )
            .unwrap();
        engine.track_token(token);
        for (now, bps, active, expired) in [
            (100, 10_000, "1000", "0"),
            (150, 5_000, "1000", "0"),
            (200, 0, "0", "1000"),
        ] {
            let result = portfolio_response(&engine, &owner, Timestamp::new(now), &[]).unwrap();
            assert_eq!(result["batches"][0]["reference_value_bps"], bps);
            assert_eq!(result["batches"][0]["amount"], "1000");
            assert_eq!(result["total_active"], active);
            assert_eq!(result["total_expired"], expired);
        }
        engine.set_expiry_period(u64::MAX, Timestamp::new(200));
        let result = portfolio_response(&engine, &owner, Timestamp::new(200), &[]).unwrap();
        assert_eq!(result["batches"][0]["reference_value_bps"], 10_000);
        assert_eq!(result["total_active"], "1000");
        assert!(result["batches"][0]["expiry_at"].is_null());
    }

    #[test]
    fn burn_and_split_merged_holdings_keep_distinct_identity_and_provenance() {
        let mut engine = burst_trst::TrstEngine::with_expiry(u64::MAX);
        let owner = WalletAddress::new("brst_merger");
        let recipient = WalletAddress::new("brst_recipient");
        let creator = WalletAddress::new("brst_creator");
        let first = engine
            .mint(
                TxHash::new([1; 32]),
                owner.clone(),
                10,
                creator.clone(),
                Timestamp::new(100),
            )
            .unwrap();
        let second = engine
            .mint(
                TxHash::new([2; 32]),
                owner.clone(),
                20,
                WalletAddress::new("brst_other_creator"),
                Timestamp::new(200),
            )
            .unwrap();
        let merge = engine
            .merge(
                &[first, second],
                owner.clone(),
                TxHash::new([3; 32]),
                Timestamp::new(300),
            )
            .unwrap();
        let (sent, change) = engine
            .transfer(
                &merge,
                &owner,
                recipient.clone(),
                10,
                TxHash::new([4; 32]),
                TxHash::new([5; 32]),
                Timestamp::new(400),
            )
            .unwrap();
        let change = change.unwrap();
        let (sent_again, _) = engine
            .transfer(
                &change,
                &owner,
                recipient.clone(),
                20,
                TxHash::new([6; 32]),
                TxHash::new([7; 32]),
                Timestamp::new(500),
            )
            .unwrap();
        engine.track_token(sent);
        engine.track_token(sent_again);
        let burn = engine
            .mint(
                TxHash::new([8; 32]),
                recipient.clone(),
                5,
                creator.clone(),
                Timestamp::new(600),
            )
            .unwrap();
        engine.track_token(burn);
        let result = portfolio_response(&engine, &recipient, Timestamp::new(700), &[]).unwrap();
        assert_eq!(result["total_active"], "35");
        let batches = result["batches"].as_array().unwrap();
        assert_eq!(batches.len(), 3);
        for (id, amount) in [(4, "10"), (6, "20")] {
            let batch = batches
                .iter()
                .find(|b| b["id"] == TxHash::new([id; 32]).to_string())
                .unwrap();
            assert_eq!(batch["amount"], amount);
            assert_eq!(batch["origin_kind"], "merge");
            assert_eq!(batch["origin"], TxHash::new([3; 32]).to_string());
            assert_eq!(batch["originator"], owner.as_str());
            assert_eq!(batch["timestamp"], 100);
        }
        let original = batches
            .iter()
            .find(|b| b["id"] == TxHash::new([8; 32]).to_string())
            .unwrap();
        assert_eq!(original["origin_kind"], "burn");
        assert_eq!(original["originator"], creator.as_str());
        assert_eq!(original["timestamp"], 600);

        engine.wallet_origins.clear();
        let incomplete = portfolio_response(&engine, &recipient, Timestamp::new(700), &[]).unwrap();
        assert_eq!(
            incomplete["batches"]
                .as_array()
                .unwrap()
                .iter()
                .find(|b| b["id"] == original["id"])
                .unwrap()["origin_kind"],
            "unknown"
        );
    }

    #[test]
    fn outgoing_pending_is_attributed_but_never_added_to_spendable_holdings() {
        let mut engine = burst_trst::TrstEngine::with_expiry(100);
        let owner = WalletAddress::new("brst_sender");
        let receiver = WalletAddress::new("brst_receiver");
        let token = engine
            .mint(
                TxHash::new([21; 32]),
                owner.clone(),
                30,
                owner.clone(),
                Timestamp::new(10),
            )
            .unwrap();
        engine.track_token(token);
        let outgoing = vec![(
            receiver.clone(),
            TxHash::new([22; 32]),
            burst_store::pending::PendingInfo {
                return_credit: false,
                source: owner.clone(),
                amount: 20,
                timestamp: Timestamp::new(15),
                provenance: Vec::new(),
            },
        )];
        for (now, active, expired) in [(50, "30", "0"), (110, "0", "30")] {
            let result =
                portfolio_response(&engine, &owner, Timestamp::new(now), &outgoing).unwrap();
            assert_eq!(result["total_active"], active);
            assert_eq!(result["total_expired"], expired);
            assert_eq!(result["total_pending"], "20");
            assert_eq!(result["pending_count"], 1);
            assert_eq!(result["batches"].as_array().unwrap().len(), 1);
        }
        let received = portfolio_response(&engine, &receiver, Timestamp::new(50), &[]).unwrap();
        assert_eq!(received["total_active"], "0");
        assert_eq!(received["total_pending"], "0");
        assert!(portfolio_response(&engine, &receiver, Timestamp::new(50), &outgoing).is_err());
        let mut returned = outgoing.clone();
        returned[0].2.return_credit = true;
        let credited = portfolio_response(&engine, &owner, Timestamp::new(50), &returned).unwrap();
        assert_eq!(credited["total_pending"], "0");
        assert_eq!(credited["pending_count"], 0);
        assert_eq!(credited["total_active"], "30");
        let mut overflow = outgoing.clone();
        overflow[0].2.amount = u128::MAX;
        overflow.extend(outgoing);
        assert!(portfolio_response(&engine, &owner, Timestamp::new(50), &overflow).is_err());
    }
}

pub async fn handle_peers_detailed(
    _: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let manager = state.peer_manager.read().await;
    let peers: Vec<_> = manager
        .iter_connected()
        .map(|(id, p)| {
            serde_json::json!({
                "address":format!("{}:{}",p.address.ip,p.address.port), "node_id":id,
                "protocol_version":p.telemetry.as_ref().map(|t|t.protocol_version),
                "clock_offset_ms":null, "last_seen":p.last_seen_secs, "connection_type":"connected"
            })
        })
        .collect();
    Ok(serde_json::json!({"count":peers.len(),"peers":peers}))
}

pub async fn handle_consti(
    _: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let params = state.current_params()?;
    let engine = state.consti_engine.lock().await;
    let document = engine.get_constitution();
    Ok(serde_json::json!({"version":document.version,
        "amendment_threshold":format!("{}%",params.consti_supermajority_bps as f64 / 100.0),
        "articles":document.articles.iter().filter(|a|!a.repealed).map(|a|serde_json::json!({
            "id":a.number.to_string(), "title":a.title, "text":a.text,
            "adopted_at":null, "amended_at":null, "introduced_by_amendment":a.introduced_by_amendment
        })).collect::<Vec<_>>()
    }))
}

pub async fn handle_verification_activity(
    _: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    use burst_verification::state::VerificationPhase;
    let accounts = state.account_store.iter_accounts()?;
    let snapshot = state.verification_engine.lock().await.snapshot();
    let mut endorsements: Vec<_> = snapshot.states.values().flat_map(|s|s.endorsements.iter().map(|e|serde_json::json!({
        "endorser":e.endorser.as_str(), "target":s.target.as_str(), "amount":e.burn_amount.to_string(), "timestamp":e.timestamp.as_secs()
    }))).collect();
    endorsements.sort_by_key(|e| std::cmp::Reverse(e["timestamp"].as_u64().unwrap_or(0)));
    endorsements.truncate(50);
    let mut verified: Vec<_> = accounts.iter().filter_map(|a|a.verified_at.map(|at|serde_json::json!({
        "account":a.address.as_str(),"result":format!("{:?}",a.state).to_lowercase(),"timestamp":at.as_secs(),
        "verifier_count":snapshot.states.get(&a.address).map_or(0, |s|s.votes.len())
    }))).collect();
    verified.sort_by_key(|e| std::cmp::Reverse(e["timestamp"].as_u64().unwrap_or(0)));
    verified.truncate(50);
    let challenges: Vec<_> = snapshot.active_challenges.values().take(50).map(|c|serde_json::json!({
        "challenger":c.challenger.as_str(),"target":c.target.as_str(),"stake":c.stake_amount.to_string(),"status":"active","timestamp":c.initiated_at.as_secs()
    })).collect();
    Ok(serde_json::json!({
        "active_verifications":snapshot.states.values().filter(|s|matches!(s.phase,VerificationPhase::Endorsing|VerificationPhase::Voting|VerificationPhase::Challenged)).count(),
        "pending_endorsements":snapshot.states.values().filter(|s|s.phase==VerificationPhase::Endorsing).map(|s|s.endorsements.len()).sum::<usize>(),
        "active_challenges":snapshot.active_challenges.len(),
        "verifier_pool_size":accounts.iter().filter(|a|a.state==burst_types::WalletState::Verified && a.verifier_opted_in_at.is_some()).count(),
        "total_verified":accounts.iter().filter(|a|a.state==burst_types::WalletState::Verified).count(),
        "total_revoked":accounts.iter().filter(|a|a.state==burst_types::WalletState::Revoked).count(),
        "recent_verifications":verified,"recent_endorsements":endorsements,"recent_challenges":challenges
    }))
}

pub async fn handle_merger_graph_stats(
    _: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    fn depth(
        id: TxHash,
        graph: &burst_trst::MergerGraph,
        memo: &mut std::collections::HashMap<TxHash, u64>,
        path: &mut std::collections::HashSet<TxHash>,
    ) -> Result<u64, RpcError> {
        if let Some(d) = memo.get(&id) {
            return Ok(*d);
        }
        let Some(node) = graph.get_merge(&id) else {
            return Ok(0);
        };
        if path.len() >= 512 || !path.insert(id) {
            return Err(RpcError::Server(
                "merger ancestry too deep or cyclic for dashboard traversal".into(),
            ));
        }
        let mut max = 0;
        for source in &node.source_origins {
            max = max.max(depth(source.origin, graph, memo, path)?);
        }
        path.remove(&id);
        memo.insert(id, max + 1);
        Ok(max + 1)
    }
    let trst = state.trst_engine.lock().await;
    let mut memo = std::collections::HashMap::new();
    let mut depths = Vec::new();
    let mut origins = std::collections::HashSet::new();
    for node in trst.merger_graph.iter_merges() {
        depths.push(depth(
            node.merge_tx,
            &trst.merger_graph,
            &mut memo,
            &mut std::collections::HashSet::new(),
        )?);
        for source in &node.source_origins {
            if !trst.merger_graph.contains_merge(&source.origin) {
                origins.insert(source.origin);
            }
        }
    }
    for ids in trst.wallet_origins.values() {
        origins.extend(ids);
    }
    let now = Timestamp::now();
    let active = trst
        .wallets
        .values()
        .flat_map(|p| p.tokens.iter())
        .filter(|t| {
            t.state != burst_types::TrstState::Revoked && !t.is_expired(now, trst.expiry_secs)
        })
        .count();
    Ok(
        serde_json::json!({"total_origins":origins.len(),"total_merges":depths.len(),"total_active_chains":active,
        "total_revoked_origins":trst.merger_graph.revoked_origins().len(),"max_chain_depth":depths.iter().max().copied().unwrap_or(0),
        "avg_chain_depth":if depths.is_empty(){0.0}else{depths.iter().sum::<u64>() as f64 / depths.len() as f64}}),
    )
}

pub async fn handle_trace_origin(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let origin = parse_tx_hash(
        params["origin"]
            .as_str()
            .ok_or_else(|| RpcError::InvalidRequest("origin hash required".into()))?,
    )?;
    let bytes = state
        .block_store
        .get_block(&BlockHash::new(*origin.as_bytes()))?;
    let block: burst_ledger::StateBlock =
        bincode::deserialize(&bytes).map_err(|e| RpcError::Server(e.to_string()))?;
    if block.block_type != burst_ledger::BlockType::Burn {
        return Err(RpcError::InvalidRequest(
            "trace_origin expects the original BRN burn hash, not a send or merge hash".into(),
        ));
    }
    let previous: burst_ledger::StateBlock =
        bincode::deserialize(&state.block_store.get_block(&block.previous)?)
            .map_err(|e| RpcError::Server(e.to_string()))?;
    let amount = block
        .brn_balance
        .checked_sub(previous.brn_balance)
        .ok_or_else(|| RpcError::Server("invalid burn odometer".into()))?;
    let trst = state.trst_engine.lock().await;
    let status = if trst.merger_graph.is_origin_revoked(&origin) {
        "revoked"
    } else if trst.expiry_secs != u64::MAX
        && Timestamp::now().as_secs() >= block.timestamp.as_secs().saturating_add(trst.expiry_secs)
    {
        "expired"
    } else {
        "active"
    };
    let mut forward = std::collections::HashMap::<TxHash, Vec<TxHash>>::new();
    for node in trst.merger_graph.iter_merges() {
        for source in &node.source_origins {
            forward
                .entry(source.origin)
                .or_default()
                .push(node.merge_tx);
        }
    }
    let mut queue = std::collections::VecDeque::from([origin]);
    let mut visited = std::collections::HashSet::new();
    let mut merges = Vec::new();
    while let Some(parent) = queue.pop_front() {
        for &id in forward.get(&parent).into_iter().flatten() {
            if !visited.insert(id) {
                continue;
            }
            queue.push_back(id);
            let node = trst
                .merger_graph
                .get_merge(&id)
                .ok_or_else(|| RpcError::Server("missing merge node".into()))?;
            let merged_bytes = state
                .block_store
                .get_block(&BlockHash::new(*id.as_bytes()))?;
            let merged: burst_ledger::StateBlock =
                bincode::deserialize(&merged_bytes).map_err(|e| RpcError::Server(e.to_string()))?;
            let mut found = false;
            for (holder, portfolio) in &trst.wallets {
                let tokens: Vec<_> = portfolio.tokens.iter().filter(|t| t.origin == id).collect();
                if tokens.is_empty() {
                    continue;
                }
                let amount = tokens
                    .iter()
                    .try_fold(0u128, |sum, t| sum.checked_add(t.amount))
                    .ok_or_else(|| RpcError::Server("holding amount overflow".into()))?;
                found = true;
                merges.push(serde_json::json!({"merge_hash":id.to_string(),"holder":holder.as_str(),"amount":amount.to_string(),"timestamp":merged.timestamp.as_secs()}));
            }
            if !found {
                merges.push(serde_json::json!({"merge_hash":id.to_string(),"holder":null,"amount":node.total_amount.to_string(),"timestamp":merged.timestamp.as_secs()}));
            }
        }
    }
    merges.sort_by_key(|m| {
        (
            m["timestamp"].as_u64().unwrap_or(0),
            m["merge_hash"].as_str().unwrap_or("").to_string(),
            m["holder"].as_str().unwrap_or("").to_string(),
        )
    });
    Ok(
        serde_json::json!({"origin":origin.to_string(),"originator":block.account.as_str(),"timestamp":block.timestamp.as_secs(),"amount":amount.to_string(),"status":status,"forward_merges":merges}),
    )
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
    confirmed_successor::confirmed(
        state.block_store.as_ref(),
        block_hash,
        account,
        acct.confirmation_height,
    )
    .unwrap_or(false)
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

pub async fn handle_account_info(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let payment_epoch = state.meta_store.payment_epoch()?;
    let req: AccountInfoRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&req.account)?;

    let address = WalletAddress::new(req.account.clone());
    let account = state
        .account_store
        .get_account(&address)
        .map_err(|e| account_not_found(e, &req.account))?;

    let now = Timestamp::now();
    let brn_state = brn_state_from_account(&account, state.current_params()?.brn_rate);
    let (brn_balance, release_sources, has_more_releases) = {
        let brn = state.brn_engine.lock().await;
        let wallet = brn.get_wallet(&account.address).unwrap_or(&brn_state);
        let sources = wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES);
        (
            checked_brn_balance(&brn, wallet, now)?,
            sources.iter().map(ToString::to_string).collect(),
            wallet.has_more_releases(&sources),
        )
    };

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

// ── account_pending ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AccountPendingRequest {
    pub account: String,
    /// Inspect one pending source without scanning a truncated first page.
    pub hash: Option<String>,
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
    pub return_credit: bool,
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
    if let Some(hash) = req.hash.as_deref() {
        let hash = parse_tx_hash(hash)?;
        let pending = match state.pending_store.get_pending(&address, &hash) {
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
    let mut all_pending = state
        .pending_store
        .get_pending_for_account_with_hashes(&address)
        .map_err(|e| RpcError::Store(format!("failed to query pending: {e}")))?;

    // Sort by amount descending (highest priority first)
    all_pending.sort_by_key(|(_, p)| std::cmp::Reverse(p.amount));

    // Apply threshold filter
    let filtered: Vec<_> = if threshold > 0 {
        all_pending
            .into_iter()
            .filter(|(_, p)| p.amount >= threshold)
            .collect()
    } else {
        all_pending
    };

    let start = (offset as usize).min(filtered.len());
    let end = (start + count as usize).min(filtered.len());
    let page = &filtered[start..end];

    let pending: Vec<PendingEntry> = page
        .iter()
        .map(|(source_hash, p)| PendingEntry {
            return_credit: p.return_credit,
            hash: format!("{}", source_hash),
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
    /// Canonical full proposal bytes, hex encoded. Never an unsigned proposal.
    pub proposal_content: Option<String>,
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

    let result = if let Some(content) = req.proposal_content {
        if block.block_type != burst_ledger::BlockType::GovernanceProposal
            || !state
                .account_store
                .get_account(&block.account)
                .is_ok_and(|account| account.state.is_verified())
            || !block.verify_work(state.current_params()?.min_work_difficulty)
        {
            return Err(RpcError::InvalidRequest(
                "proposal content requires a verified proposer and valid work".into(),
            ));
        }
        if content.len() > burst_governance::content::MAX_CONTENT_BYTES * 2 {
            return Err(RpcError::InvalidRequest(
                "proposal content is too large".into(),
            ));
        }
        let bytes =
            hex::decode(content).map_err(|error| RpcError::InvalidRequest(error.to_string()))?;
        state
            .block_processor
            .process_block_with_content(&block_bytes, &bytes)
    } else {
        state.block_processor.process_block(&block_bytes)
    }
    .map_err(RpcError::Server)?;

    let result = resolve_queued_result(
        state.block_processor.as_ref(),
        &block_bytes,
        result,
        std::time::Duration::from_secs(2),
    )
    .await;

    let accepted = matches!(result, ProcessResult::Accepted | ProcessResult::Queued);
    let detail = match &result {
        ProcessResult::Accepted => None,
        ProcessResult::Duplicate => Some("duplicate block".to_string()),
        ProcessResult::Fork => Some("fork detected — election started".to_string()),
        ProcessResult::Gap => Some("gap — previous block unknown, queued for later".to_string()),
        ProcessResult::Queued => None,
        ProcessResult::Rejected(reason) => Some(reason.clone()),
    };

    if accepted {
        debug!(hash = %block_hash_str, "block queued for processing via RPC");
    }

    Ok(to_value(&ProcessResponse {
        hash: block_hash_str,
        accepted,
        detail,
    }))
}

async fn resolve_queued_result(
    processor: &dyn crate::server::BlockProcessorCallback,
    payload: &[u8],
    initial: ProcessResult,
    max_wait: std::time::Duration,
) -> ProcessResult {
    if initial == ProcessResult::Queued {
        if let Ok(Some(outcome)) =
            tokio::time::timeout(max_wait, processor.wait_for_result(payload)).await
        {
            // A known fork must retain its conflict detail, not look like an
            // ordinary queued payment. It may still win the election later.
            if matches!(
                outcome,
                ProcessResult::Accepted | ProcessResult::Fork | ProcessResult::Rejected(_)
            ) {
                return outcome;
            }
        }
    }
    initial
}

#[cfg(test)]
mod submission_response_tests {
    use super::*;
    use crate::server::BlockProcessorCallback;
    use std::time::Duration;

    struct Processor {
        outcome: Option<ProcessResult>,
        stalled: bool,
    }
    impl BlockProcessorCallback for Processor {
        fn process_block(&self, _: &[u8]) -> Result<ProcessResult, String> {
            Ok(ProcessResult::Queued)
        }
        fn wait_for_result<'a>(
            &'a self,
            _: &'a [u8],
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<ProcessResult>> + Send + 'a>>
        {
            Box::pin(async move {
                if self.stalled {
                    std::future::pending::<()>().await;
                }
                self.outcome.clone()
            })
        }
    }

    #[tokio::test]
    async fn reports_known_admission_conflict_rejection_or_persistence() {
        for outcome in [
            ProcessResult::Accepted,
            ProcessResult::Fork,
            ProcessResult::Rejected("not eligible".into()),
        ] {
            let processor = Processor {
                outcome: Some(outcome.clone()),
                stalled: false,
            };
            assert_eq!(
                resolve_queued_result(
                    &processor,
                    b"block",
                    ProcessResult::Queued,
                    Duration::from_millis(10)
                )
                .await,
                outcome
            );
        }
    }

    #[tokio::test]
    async fn absent_or_nonterminal_outcome_is_not_a_rejection() {
        for outcome in [
            None,
            Some(ProcessResult::Queued),
            Some(ProcessResult::Gap),
            Some(ProcessResult::Duplicate),
        ] {
            let processor = Processor {
                outcome,
                stalled: false,
            };
            assert_eq!(
                resolve_queued_result(
                    &processor,
                    b"block",
                    ProcessResult::Queued,
                    Duration::from_millis(10)
                )
                .await,
                ProcessResult::Queued
            );
        }
        let stalled = Processor {
            outcome: None,
            stalled: true,
        };
        assert_eq!(
            resolve_queued_result(
                &stalled,
                b"block",
                ProcessResult::Queued,
                Duration::from_millis(5)
            )
            .await,
            ProcessResult::Queued
        );
        assert_eq!(
            resolve_queued_result(
                &stalled,
                b"block",
                ProcessResult::Rejected("known".into()),
                Duration::from_millis(5)
            )
            .await,
            ProcessResult::Rejected("known".into())
        );
    }
}

// ── block_info ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct BlockInfoRequest {
    pub hash: String,
    #[serde(default)]
    pub include_signed_block: bool,
}

#[derive(Debug, Serialize)]
pub struct BlockInfoResponse {
    pub active_signing_grant: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    // Keep u128 fields out of serde_json::Value and JavaScript number conversion.
    pub signed_block: Option<String>,
    pub payment_epoch: String,
    pub block_type: String,
    pub account: String,
    pub previous: String,
    pub representative: String,
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

pub async fn handle_block_info(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: BlockInfoRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_hash(&req.hash)?;

    let block_hash = parse_block_hash(&req.hash)?;
    let (block, historical) = match state.block_store.get_block(&block_hash) {
        Ok(bytes) => (deserialize_block(&bytes)?, false),
        Err(StoreError::NotFound(_)) => {
            let block = state
                .block_processor
                .historical_block(block_hash)
                .await
                .map_err(RpcError::Node)?
                .ok_or_else(|| RpcError::BlockNotFound(req.hash.clone()))?;
            if block.hash != block_hash {
                return Err(RpcError::Node(
                    "historical lookup returned a different block".into(),
                ));
            }
            (block, true)
        }
        Err(error) => return Err(RpcError::Store(error.to_string())),
    };

    let confirmed = historical || is_block_confirmed(&block_hash, &block.account, state);
    let confirmed_successor = confirmed_successor::lookup(&block_hash, &block.account, state)?;

    Ok(to_value(&BlockInfoResponse {
        active_signing_grant: block.active_signing_grant().to_string(),
        signed_block: req
            .include_signed_block
            .then(|| serde_json::to_string(&block))
            .transpose()
            .map_err(|error| RpcError::Server(format!("cannot encode signed block: {error}")))?,
        payment_epoch: block.payment_epoch.to_string(),
        block_type: format!("{:?}", block.block_type),
        account: block.account.to_string(),
        previous: format!("{}", block.previous),
        representative: block.representative.to_string(),
        brn_balance: block.brn_balance.to_string(),
        trst_balance: block.trst_balance.to_string(),
        link: format!("{}", block.link),
        origin: format!("{}", block.origin),
        transaction: format!("{}", block.transaction),
        target_head: block.target_head.to_string(),
        timestamp: block.timestamp.as_secs(),
        confirmed,
        confirmed_successor,
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
                    let confirmed_successor =
                        confirmed_successor::lookup(&block_hash, &block.account, state)?;

                    blocks.push(BlocksInfoEntry {
                        hash: h.clone(),
                        block: Some(BlockInfoResponse {
                            active_signing_grant: block.active_signing_grant().to_string(),
                            signed_block: None,
                            payment_epoch: block.payment_epoch.to_string(),
                            block_type: format!("{:?}", block.block_type),
                            account: block.account.to_string(),
                            previous: format!("{}", block.previous),
                            representative: block.representative.to_string(),
                            brn_balance: block.brn_balance.to_string(),
                            trst_balance: block.trst_balance.to_string(),
                            link: format!("{}", block.link),
                            origin: format!("{}", block.origin),
                            transaction: format!("{}", block.transaction),
                            target_head: block.target_head.to_string(),
                            timestamp: block.timestamp.as_secs(),
                            confirmed,
                            confirmed_successor,
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
    /// Work root: predecessor hash, or the account-bound opening root.
    pub hash: String,
    pub difficulty: Option<String>,
    pub block_type: Option<burst_ledger::BlockType>,
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
    let kind = match req.block_type {
        Some(burst_ledger::BlockType::Open | burst_ledger::BlockType::Receive) => {
            burst_work::WorkBlockKind::ReceiveOrOpen
        }
        Some(burst_ledger::BlockType::Epoch) => burst_work::WorkBlockKind::Epoch,
        _ => burst_work::WorkBlockKind::Base,
    };
    let required =
        burst_work::WorkThresholds::with_base(state.current_params()?.min_work_difficulty)
            .threshold_for(kind);
    let difficulty = parse_difficulty(&req.difficulty, required)?.max(required);
    let generator = state.work_generator.clone();

    // PoW generation is CPU-intensive; run it on a blocking thread.
    let result = tokio::task::spawn_blocking(move || generator.generate(&block_hash, difficulty))
        .await
        .map_err(|e| RpcError::Server(format!("work generation task failed: {e}")))?
        .map_err(|e| RpcError::WorkError(e.to_string()))?;

    let base_difficulty = state.current_params()?.min_work_difficulty;
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

// ── params_hash ────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ParamsHashResponse {
    pub online_signing: String,
    pub full_governance_content: bool,
    pub delegation_key_journal: bool,
    pub payment_epoch: String,
    pub params_hash: String,
    pub block_version: u8,
    pub min_work_difficulty: String,
}

fn online_signing_phase(
    network: burst_types::NetworkId,
    epoch: BlockHash,
    observed: &serde_json::Value,
) -> &'static str {
    if network == burst_types::NetworkId::Dev {
        return "local";
    }
    if observed["payment_epoch"] != epoch.to_string() {
        return "unknown";
    }
    match observed["phase"].as_str() {
        Some("peer_matches")
            if observed["observed_epoch"] == epoch.to_string()
                && observed["observed_height"].as_u64().is_some() =>
        {
            "peer_matches"
        }
        Some("catching_up") => "catching_up",
        Some("peer_differs") => "peer_differs",
        _ => "unknown",
    }
}

pub async fn handle_params_hash(
    _params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let current = state.current_params()?;
    let epoch = state.meta_store.payment_epoch()?;
    let observed = if state.network == burst_types::NetworkId::Dev {
        serde_json::Value::Null
    } else {
        state
            .block_processor
            .checkpoint_sync_status()
            .map_err(RpcError::Server)?
    };
    let online_signing = online_signing_phase(state.network, epoch, &observed).to_string();
    Ok(to_value(&ParamsHashResponse {
        online_signing,
        full_governance_content: state.block_processor.supports_full_governance_content(),
        delegation_key_journal: state.block_processor.supports_delegation_key_journal(),
        payment_epoch: epoch.to_string(),
        params_hash: current.params_hash().to_string(),
        block_version: burst_ledger::CURRENT_BLOCK_VERSION,
        min_work_difficulty: format!("{:016x}", current.min_work_difficulty),
    }))
}

#[cfg(test)]
mod signing_readiness_tests {
    use super::*;

    #[test]
    fn only_dev_can_use_local_mode_and_matching_observations_bind_to_epoch() {
        let epoch = BlockHash::new([1; 32]);
        let mut view = serde_json::json!({"payment_epoch":epoch.to_string(), "phase":"local"});
        assert_eq!(
            online_signing_phase(burst_types::NetworkId::Dev, epoch, &view),
            "local"
        );
        for network in [burst_types::NetworkId::Test, burst_types::NetworkId::Live] {
            assert_eq!(online_signing_phase(network, epoch, &view), "unknown");
        }
        view["phase"] = serde_json::json!("peer_matches");
        assert_eq!(
            online_signing_phase(burst_types::NetworkId::Test, epoch, &view),
            "unknown"
        );
        view["observed_epoch"] = serde_json::json!(epoch.to_string());
        view["observed_height"] = serde_json::json!(0);
        assert_eq!(
            online_signing_phase(burst_types::NetworkId::Test, epoch, &view),
            "peer_matches"
        );
        assert_eq!(
            online_signing_phase(burst_types::NetworkId::Test, BlockHash::new([2; 32]), &view),
            "unknown"
        );
        view["phase"] = serde_json::json!("catching_up");
        assert_eq!(
            online_signing_phase(burst_types::NetworkId::Test, epoch, &view),
            "catching_up"
        );
    }
}

pub async fn handle_protocol_params(
    _params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let p = state.current_params()?;
    Ok(serde_json::json!({
        "governable_parameters": burst_governance::params::view::snapshot(&p),
        "params_hash":p.params_hash().to_string(), "brn_rate":p.brn_rate.to_string(),
        "trst_expiry_secs":p.trst_expiry_secs, "endorsement_threshold":p.endorsement_threshold,
        "verification_threshold":p.verification_threshold_bps as f64 / 10_000.0,
        "endorsement_burn_amount":p.endorsement_burn_amount.to_string(),
        "verifier_stake_amount":p.verifier_stake_amount.to_string(), "challenge_stake_amount":p.challenge_stake_amount.to_string(),
        "max_revotes":p.max_revotes, "governance_quorum":p.governance_quorum_bps as f64 / 10_000.0,
        "governance_supermajority":p.governance_supermajority_bps as f64 / 10_000.0,
        "governance_proposal_window_secs":p.governance_proposal_duration_secs,
        "governance_voting_window_secs":p.governance_exploration_duration_secs,
        "governance_endorsement_cost":p.governance_proposal_cost.to_string(),
        "governance_endorsements_required":p.governance_proposal_endorsements,
        "governance_cooldown_secs":p.governance_cooldown_duration_secs,
        "spending_limit_new_wallets":p.new_wallet_spending_limit.to_string()
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
    pub phase_projected: bool,
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

    if let Some((gov, projected)) = governance_projection::read(state).await? {
        let all: Vec<_> = gov.all_proposals().collect();
        let start = (offset as usize).min(all.len());
        let end = (start + count as usize).min(all.len());
        let page = &all[start..end];

        let proposals: Vec<ProposalSummary> = page
            .iter()
            .map(|proposal| {
                let description = proposal_description(proposal);
                let (votes_yea, votes_nay) = current_vote_counts(proposal);
                ProposalSummary {
                    hash: format!("{}", proposal.hash),
                    proposer: proposal.proposer.to_string(),
                    phase: format!("{:?}", proposal.phase),
                    phase_projected: projected.contains(&proposal.hash),
                    description,
                    votes_yea,
                    votes_nay,
                }
            })
            .collect();

        let cursor = if end < all.len() {
            Some(pagination::encode_cursor(end as u64))
        } else {
            None
        };

        return Ok(to_value(&GovernanceProposalsResponse { proposals, cursor }));
    }

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
                    phase_projected: false,
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
        burst_governance::ProposalContent::ConstitutionalAmendment { title, .. }
        | burst_governance::ProposalContent::ConstitutionalDiff { title, .. } => {
            format!("Constitutional amendment: {}", title)
        }
        burst_governance::ProposalContent::Emergency {
            description, param, ..
        } => {
            format!("EMERGENCY {:?}: {}", param, description)
        }
        burst_governance::ProposalContent::RepresentativeEviction {
            target,
            evict,
            reason,
        } => {
            let verb = if *evict { "Evict" } else { "Reinstate" };
            format!(
                "{} representative {} from ORV set: {}",
                verb, target, reason
            )
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
    _params: serde_json::Value,
    _state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    // A public address is not authorization. Real governance stays available
    // through the signed DAG transaction path used by the desktop wallet.
    Err(RpcError::InvalidRequest(
        "governance votes require a signed GovernanceVote block submitted through process".into(),
    ))
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
    pub phase_projected: bool,
    pub description: String,
    pub param: String,
    pub proposed_value: String,
    pub votes_yea: u32,
    pub votes_nay: u32,
    pub votes_abstain: u32,
    pub created_at: u64,
    pub phase_deadline: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amendment_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amendment_operations: Option<Vec<burst_governance::proposal::AmendmentOp>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activation_conflict: Option<burst_consti::AmendmentConflict>,
}

#[cfg(test)]
mod governance_response_tests {
    use super::GovernanceProposalInfoResponse;

    #[test]
    fn amendment_text_is_complete_and_absent_for_other_proposals() {
        let text = "Article 1\nEvery verified person has an equal vote.";
        let mut response = GovernanceProposalInfoResponse {
            hash: "a".repeat(64),
            proposer: "brst_fixture".into(),
            phase: "Exploration".into(),
            phase_projected: true,
            description: "Amend article 1".into(),
            param: "constitution".into(),
            proposed_value: "Article 1".into(),
            votes_yea: 0,
            votes_nay: 0,
            votes_abstain: 0,
            created_at: 1,
            phase_deadline: 2,
            amendment_text: Some(text.into()),
            amendment_operations: None,
            activation_conflict: None,
        };
        assert_eq!(
            serde_json::to_value(&response).unwrap()["amendment_text"],
            text
        );
        response.amendment_text = None;
        assert!(serde_json::to_value(&response)
            .unwrap()
            .get("amendment_text")
            .is_none());
    }
}

pub async fn handle_governance_proposal_info(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: GovernanceProposalInfoRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_hash(&req.hash)?;

    let proposal_hash = parse_tx_hash(&req.hash)?;
    // Use the same read-only phase projection as governance_proposals. The legacy
    // proposal table is not the live node's governance state.
    let current_params = state.current_params()?;
    let (proposal, phase_projected, timing) = if let Some((gov, projected)) =
        governance_projection::read(state).await?
    {
        let proposal = gov
            .get_proposal(&proposal_hash)
            .cloned()
            .ok_or_else(|| RpcError::ProposalNotFound(req.hash.clone()))?;
        let timing = gov
            .timing(&proposal, &current_params)
            .map_err(|error| RpcError::Store(error.to_string()))?;
        (proposal, projected.contains(&proposal_hash), timing)
    } else {
        let data = state
            .governance_store
            .get_proposal(&proposal_hash)
            .map_err(|e| match e {
                StoreError::NotFound(_) => RpcError::ProposalNotFound(req.hash.clone()),
                other => RpcError::Store(other.to_string()),
            })?;
        let proposal = deserialize_proposal(&data)?;
        let timing = burst_governance::GovernanceTiming::new(&current_params, &proposal.content);
        (proposal, false, timing)
    };

    let (param, proposed_value) = match &proposal.content {
        burst_governance::ProposalContent::ParameterChange { param, new_value } => {
            (format!("{:?}", param), new_value.to_string())
        }
        burst_governance::ProposalContent::ConstitutionalAmendment { title, .. }
        | burst_governance::ProposalContent::ConstitutionalDiff { title, .. } => {
            ("constitution".to_string(), title.clone())
        }
        burst_governance::ProposalContent::Emergency {
            param, new_value, ..
        } => (format!("{:?}", param), new_value.to_string()),
        burst_governance::ProposalContent::RepresentativeEviction { target, evict, .. } => (
            if *evict { "evict_rep" } else { "reinstate_rep" }.to_string(),
            target.to_string(),
        ),
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

    let phase_deadline = compute_phase_deadline(&proposal, &current_params, timing)?;
    let amendment_text = match &proposal.content {
        burst_governance::ProposalContent::ConstitutionalAmendment { text, .. } => {
            Some(text.clone())
        }
        _ => None,
    };
    let amendment_operations = match &proposal.content {
        burst_governance::ProposalContent::ConstitutionalDiff { operations, .. } => {
            Some(operations.clone())
        }
        _ => None,
    };

    let activation_conflict = state
        .consti_engine
        .lock()
        .await
        .activation_conflict(&proposal_hash);
    Ok(to_value(&GovernanceProposalInfoResponse {
        hash: req.hash,
        proposer: proposal.proposer.to_string(),
        phase: format!("{:?}", proposal.phase),
        phase_projected,
        description: proposal_description(&proposal),
        param,
        proposed_value,
        votes_yea,
        votes_nay,
        votes_abstain,
        created_at: proposal.created_at.as_secs(),
        phase_deadline,
        amendment_text,
        amendment_operations,
        activation_conflict,
    }))
}

/// Compute the deadline (Unix timestamp) for the current phase of a proposal.
fn compute_phase_deadline(
    proposal: &Proposal,
    params: &burst_types::ProtocolParams,
    timing: burst_governance::GovernanceTiming,
) -> Result<u64, RpcError> {
    let deadline = match proposal.phase {
        burst_governance::GovernancePhase::Proposal => timing
            .proposal_end(proposal.created_at)
            .map_err(|error| RpcError::Store(error.to_string()))?
            .as_secs(),
        burst_governance::GovernancePhase::Exploration => proposal
            .exploration_started_at
            .map(|t| timing.voting_end(t).map(|at| at.as_secs()))
            .transpose()
            .map_err(|error| RpcError::Store(error.to_string()))?
            .unwrap_or(0),
        burst_governance::GovernancePhase::Cooldown => proposal
            .activation_at
            .map(|at| at.as_secs())
            .unwrap_or_else(|| {
                proposal
                    .cooldown_started_at
                    .map(|t| {
                        t.as_secs()
                            .saturating_add(params.governance_cooldown_duration_secs)
                    })
                    .unwrap_or(0)
            }),
        burst_governance::GovernancePhase::Activation => {
            proposal.activation_at.map(|t| t.as_secs()).unwrap_or(0)
        }
        _ => 0,
    };
    Ok(deadline)
}

// ═══════════════════════════════════════════════════════════════════════
// Telemetry
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct TelemetryRequest {}

#[derive(Debug, Serialize)]
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
    pub ntp_offset_ms: Option<i64>,
}

pub async fn handle_telemetry(
    _params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let accounts = state.account_store.iter_accounts()?;
    let account_count = accounts.len() as u64;
    let verified_count = accounts.iter().filter(|a| a.state.is_verified()).count() as u64;
    let unverified_count = accounts
        .iter()
        .filter(|a| a.state == burst_types::WalletState::Unverified)
        .count() as u64;
    let block_count = if let Some(ref cache) = state.ledger_cache {
        cache.block_count()
    } else {
        state.block_store.block_count()?
    };
    let peer_count = state.peer_manager.read().await.connected_count() as u32;
    let mut totals = [0u128; 3];
    {
        let trst = state.trst_engine.lock().await;
        for account in &accounts {
            if let Some(portfolio) = trst.get_portfolio(&account.address) {
                for token in &portfolio.tokens {
                    let index = if token.state == burst_types::TrstState::Revoked {
                        2
                    } else if token.is_expired(Timestamp::new(now), trst.expiry_secs) {
                        1
                    } else {
                        0
                    };
                    totals[index] = totals[index]
                        .checked_add(token.amount)
                        .ok_or_else(|| RpcError::Server("TRST total overflow".into()))?;
                }
            }
        }
    }
    let clock = state.clock_sync.lock().await;
    let ntp_offset_ms =
        if clock.last_sync_secs > 0 && now.saturating_sub(clock.last_sync_secs) < 600 {
            Some(clock.offset_ms)
        } else {
            None
        };

    Ok(to_value(&TelemetryResponse {
        network: state.network.as_str().to_owned(),
        faucet_enabled: test_tools_allowed(state.network, state.enable_faucet),
        block_count,
        account_count,
        peer_count,
        protocol_version: burst_types::PROTOCOL_VERSION,
        uptime_secs: now.saturating_sub(state.started_at),
        verified_count,
        unverified_count,
        trst_circulating: totals[0].to_string(),
        trst_expired_total: totals[1].to_string(),
        trst_revoked_total: totals[2].to_string(),
        ntp_offset_ms,
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

pub async fn handle_verifier_assignments(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    #[derive(Deserialize)]
    struct Request {
        account: String,
        cursor: Option<String>,
    }
    let request: Request =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&request.account)?;
    if let Some(cursor) = &request.cursor {
        validate_account(cursor)?;
    }
    let verifier = WalletAddress::new(request.account);
    let engine = state.verification_engine.lock().await;
    let (targets, cursor) =
        verifier_assignment_page(engine.states(), &verifier, request.cursor.as_deref());
    Ok(serde_json::json!({"targets": targets, "cursor": cursor}))
}

fn verifier_assignment_page<'a>(
    processes: impl Iterator<Item = &'a burst_verification::state::VerificationState>,
    verifier: &WalletAddress,
    after: Option<&str>,
) -> (Vec<&'a str>, Option<String>) {
    let mut targets = Vec::with_capacity(21);
    for process in processes {
        use burst_verification::state::VerificationPhase;
        if !matches!(
            process.phase,
            VerificationPhase::Voting | VerificationPhase::Challenged
        ) || !process.selected_verifiers.contains(verifier)
            || process.votes.iter().any(|vote| &vote.verifier == verifier)
            || after.is_some_and(|cursor| process.target.as_str() <= cursor)
        {
            continue;
        }
        let target = process.target.as_str();
        let index = targets.binary_search(&target).unwrap_or_else(|index| index);
        if index < 21 {
            targets.insert(index, target);
            targets.truncate(21);
        }
    }
    let cursor = (targets.len() > 20).then(|| targets[19].to_owned());
    targets.truncate(20);
    (targets, cursor)
}

#[cfg(test)]
mod verifier_assignment_tests {
    use super::*;
    use burst_verification::state::{VerificationPhase, VerificationState, VerifierVote};

    #[test]
    fn assignment_pages_are_ordered_bounded_and_only_include_pending_panel_votes() {
        let voter = WalletAddress::new("brst_voter");
        let mut processes: Vec<_> = (0..25)
            .rev()
            .map(|index| VerificationState {
                target: WalletAddress::new(format!("brst_target{index:02}")),
                phase: VerificationPhase::Voting,
                endorsements: Vec::new(),
                endorser_set: Default::default(),
                selected_verifiers: vec![voter.clone()],
                votes: Vec::new(),
                revote_count: 0,
                excluded_verifiers: Default::default(),
                started_at: Timestamp::new(1),
            })
            .collect();
        let (first, cursor) = verifier_assignment_page(processes.iter(), &voter, None);
        assert_eq!(first.len(), 20);
        assert_eq!(first.first(), Some(&"brst_target00"));
        assert_eq!(cursor.as_deref(), Some("brst_target19"));
        let (second, end) = verifier_assignment_page(processes.iter(), &voter, cursor.as_deref());
        assert_eq!(
            second,
            vec![
                "brst_target20",
                "brst_target21",
                "brst_target22",
                "brst_target23",
                "brst_target24"
            ]
        );
        assert!(end.is_none());
        processes[0].phase = VerificationPhase::Endorsing;
        processes[1].selected_verifiers.clear();
        processes[2].votes.push(VerifierVote {
            verifier: voter.clone(),
            vote: burst_verification::voting::Vote::Neither,
            stake_amount: 0,
            timestamp: Timestamp::new(2),
        });
        processes[3].phase = VerificationPhase::Challenged;
        let (remaining, _) =
            verifier_assignment_page(processes.iter(), &voter, Some("brst_target19"));
        assert_eq!(remaining, vec!["brst_target20", "brst_target21"]);
        assert!(
            verifier_assignment_page(processes.iter(), &voter, Some("brst_target24"))
                .0
                .is_empty()
        );
    }
}

#[derive(Debug, Serialize)]
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
    pub selection_unavailable_reason: Option<&'static str>,
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
    let mut endorser_count = endorsements.len() as u32;

    // Check for active challenge
    let mut challenge_active = state
        .verification_store
        .get_challenge(&address)
        .ok()
        .flatten()
        .is_some();

    // Get verification state from account store
    let (mut status, verified_at, verifier_opted_in) =
        match state.account_store.get_account(&address) {
            Ok(account) => {
                let status = format!("{:?}", account.state).to_lowercase();
                let verified_at = account.verified_at.map(|t| t.as_secs());
                (status, verified_at, account.verifier_opted_in_at.is_some())
            }
            Err(StoreError::NotFound(_)) => ("unverified".to_string(), None, false),
            Err(error) => return Err(error.into()),
        };

    let mut endorsers = Vec::new();
    let mut verifier_votes = Vec::new();
    let mut phase = None;
    let mut selected_verifiers = Vec::new();
    let verification = state.verification_engine.lock().await;
    if let Some(process) = verification.get_state(&address) {
        selected_verifiers = process
            .selected_verifiers
            .iter()
            .map(ToString::to_string)
            .collect();
        endorsers = process
            .endorsements
            .iter()
            .map(|e| e.endorser.to_string())
            .collect();
        endorser_count = endorsers.len() as u32;
        challenge_active = verification.has_active_challenge(&address);
        let process_phase = format!("{:?}", process.phase).to_lowercase();
        phase = Some(process_phase.clone());
        if !matches!(status.as_str(), "revoked" | "deactivated")
            && !challenge_active
            && !matches!(
                process.phase,
                burst_verification::state::VerificationPhase::Verified
            )
        {
            status = process_phase;
        }
        verifier_votes = process.votes.iter().map(|v| serde_json::json!({"verifier":v.verifier.to_string(), "vote":format!("{:?}",v.vote), "timestamp":v.timestamp.as_secs()})).collect();
    }

    Ok(to_value(&VerificationStatusResponse {
        account: req.account,
        status,
        phase,
        verified_at,
        endorser_count,
        endorsement_threshold: state.current_params()?.endorsement_threshold,
        endorsers,
        verifier_votes,
        challenge_active,
        selection_available: burst_verification::checkpoint_selection_enabled(state.network),
        verifier_opted_in,
        selected_verifiers,
        selection_context: verification
            .selection_context(&address)
            .map(|hash| hash.to_string()),
        selection_unavailable_reason: (!burst_verification::checkpoint_selection_enabled(
            state.network,
        ))
        .then_some("certified_checkpoint_selection_release_gated"),
    }))
}

pub async fn handle_delegation_info(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: AccountInfoRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&req.account)?;
    let address = WalletAddress::new(req.account);
    let delegation = state.delegation_engine.lock().await;
    Ok(serde_json::json!({
        "delegated_to": delegation.get_delegate(&address).map(|a| a.to_string()),
        "delegated_from": delegation.get_delegators(&address).iter().map(|a| a.to_string()).collect::<Vec<_>>()
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
            .into_iter()
            .map(|(addr, weight)| {
                let people = weight / burst_consensus::rep_weights::WEIGHT_PER_HUMAN;
                (
                    addr.to_string(),
                    weight,
                    u64::try_from(people).unwrap_or(u64::MAX),
                )
            })
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
    pub send_block_hash: String,
    pub amount: String,
}

/// Test-only funding through a real burn. Verification is a separate action:
/// adding an unopened, offline voter before funding can strand the quorum.
/// The recipient signs its Receive and representative choice before bootstrap
/// verification; this endpoint never fabricates identity or ledger balances.
pub async fn handle_faucet(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    require_faucet(state)?;

    let req: FaucetRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&req.account)?;

    let address = WalletAddress::new(req.account.clone());
    let seed = state.genesis_seed.ok_or_else(|| {
        RpcError::InvalidRequest("test faucet requires this network's genesis authority".into())
    })?;
    let _authoring = state.genesis_authoring.lock().await;
    // Funding uses the genesis wallet's accrued BRN, never an unbacked credit.
    let key = burst_crypto::keypair_from_seed(&seed);
    let genesis = burst_crypto::derive_address(&key.public);
    let account = state.account_store.get_account(&genesis)?;
    let params = state.current_params()?;
    let amount = 1_000_000_000_000_000_000u128;
    let spent = prev_spent_brn(state, &account.head)
        .checked_add(amount)
        .ok_or_else(|| RpcError::InvalidRequest("BRN odometer overflow".into()))?;
    let link = BlockHash::new(
        burst_crypto::decode_address(address.as_str())
            .ok_or_else(|| RpcError::InvalidRequest("invalid faucet recipient".into()))?,
    );
    let work = state.work_generator.clone();
    let payment_epoch = state.meta_store.payment_epoch()?;
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&genesis)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let signing_grant = prev_signing_grant(state, &account.head)?;
    let block = tokio::task::spawn_blocking(move || {
        build_and_sign_block(
            signing_grant,
            payment_epoch,
            release_sources,
            burst_ledger::BlockType::Burn,
            &genesis,
            account.head,
            &account.representative,
            spent,
            account.trst_balance,
            link,
            TxHash::ZERO,
            TxHash::ZERO,
            &key.private,
            &work,
            params.min_work_difficulty,
            params.params_hash(),
            Vec::new(),
        )
    })
    .await
    .map_err(|e| RpcError::Server(format!("faucet build failed: {e}")))??;
    submit_and_confirm(&block, state).await?;

    Ok(to_value(&FaucetResponse {
        account: req.account,
        status: "pending_receive".to_string(),
        message: "1 TRST is pending. Accept it with a signed receive; verification is separate. Test funds have no real value.".to_string(),
        send_block_hash: block.hash.to_string(),
        amount: amount.to_string(),
    }))
}

// ═══════════════════════════════════════════════════════════════════════
// Genesis bootstrap endorsement (authority node only)
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct GenesisEndorseRequest {
    /// The wallet address to verify during the bootstrap phase.
    pub account: String,
}

#[derive(Debug, Serialize)]
pub struct GenesisEndorseResponse {
    pub target: String,
    pub block_hash: String,
    pub genesis: String,
    pub verified: bool,
    pub message: String,
}

/// Author a genesis Endorse block that directly verifies `account` during the
/// bootstrap phase (whitepaper §Genesis: the creator endorses the first
/// wallets). Only works on the node holding the genesis seed; the resulting
/// block, signed by the genesis key, triggers `genesis_verify` when processed.
///
/// Call once per wallet, sequentially — each block builds on the genesis
/// account's previous one, so let each confirm before the next.
pub async fn handle_genesis_endorse(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let _authoring = state.genesis_authoring.lock().await;
    genesis_endorse_locked(params, state).await
}

async fn genesis_endorse_locked(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let seed = state.genesis_seed.ok_or_else(|| {
        RpcError::InvalidRequest(
            "this node is not the genesis authority (no genesis seed) — run genesis_endorse on the creator's node".into(),
        )
    })?;

    let req: GenesisEndorseRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&req.account)?;
    let target = WalletAddress::new(req.account.clone());

    // Genesis identity from the seed.
    let kp = burst_crypto::keypair_from_seed(&seed);
    let genesis_addr = burst_crypto::derive_address(&kp.public);
    if target == genesis_addr {
        return Err(RpcError::InvalidRequest(
            "cannot endorse the genesis account itself".into(),
        ));
    }
    let accounts = state.account_store.iter_accounts()?;
    let target_head = accounts
        .iter()
        .find(|a| a.address == target)
        .map_or(BlockHash::ZERO, |a| a.head);
    if accounts
        .iter()
        .any(|a| a.address == target && a.state.is_verified())
    {
        return Err(RpcError::InvalidRequest(
            "target is already verified".into(),
        ));
    }
    let verified_count = accounts.iter().filter(|a| a.state.is_verified()).count() as u64;
    let phase_bytes = state
        .brn_store
        .get_meta(burst_verification::bootstrap::BootstrapPhase::KEY)?;
    let phase = burst_verification::bootstrap::BootstrapPhase::decode(phase_bytes.as_deref())
        .map_err(|error| RpcError::Server(error.to_string()))?;
    require_bootstrap_window(
        phase,
        verified_count,
        state.current_params()?.bootstrap_exit_threshold,
    )?;

    // Target → link (its public key bytes), same encoding the econ layer decodes.
    let target_pubkey = burst_crypto::decode_address(target.as_str()).ok_or_else(|| {
        RpcError::InvalidRequest(format!("invalid target address: {}", target.as_str()))
    })?;
    let link = BlockHash::new(target_pubkey);

    // Genesis account current state: head (previous) + spent-BRN odometer.
    let genesis_acct = state
        .account_store
        .get_account(&genesis_addr)
        .map_err(|e| {
            RpcError::Server(format!(
                "genesis account not found (is this the authority node?): {e}"
            ))
        })?;
    let previous = state
        .frontier_store
        .get_frontier(&genesis_addr)
        .unwrap_or(genesis_acct.head);
    let prev_spent = prev_spent_brn(state, &previous);
    let burn_amount = state.current_params()?.endorsement_burn_amount;

    // Endorse block: odometer += burn, TRST unchanged; signed by genesis key.
    let genesis_rep = genesis_acct.representative.clone();
    let pk_bytes = seed;
    let work_gen = state.work_generator.clone();
    let min_diff = state.current_params()?.min_work_difficulty;
    let (payment_epoch, ph) = (
        state.meta_store.payment_epoch()?,
        state.current_params()?.params_hash(),
    );
    let genesis_for_build = genesis_addr.clone();
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&genesis_addr)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let signing_grant = prev_signing_grant(state, &previous)?;
    let block = tokio::task::spawn_blocking(move || {
        let pk = burst_crypto::keypair_from_seed(&pk_bytes).private;
        let mut block = build_and_sign_block(
            signing_grant,
            payment_epoch,
            release_sources,
            burst_ledger::BlockType::Endorse,
            &genesis_for_build,
            previous,
            &genesis_rep,
            prev_spent
                .checked_add(burn_amount)
                .ok_or_else(|| RpcError::Server("BRN spend overflow".into()))?,
            genesis_acct.trst_balance,
            link,
            TxHash::ZERO,
            TxHash::ZERO,
            &pk,
            &work_gen,
            min_diff,
            ph,
            Vec::new(),
        )?;
        block.target_head = target_head;
        block.hash = block.compute_hash();
        block.signature = burst_crypto::sign_message(block.hash.as_bytes(), &pk);
        Ok::<_, RpcError>(block)
    })
    .await
    .map_err(|e| RpcError::Server(format!("block build task failed: {e}")))??;

    submit_and_confirm(&block, state).await?;

    let verified = match state.account_store.get_account(&target) {
        Ok(account) => account.state.is_verified(),
        Err(burst_store::StoreError::NotFound(_)) => false,
        Err(error) => return Err(error.into()),
    };
    Ok(to_value(&GenesisEndorseResponse {
        target: req.account,
        block_hash: format!("{}", block.hash),
        genesis: genesis_addr.to_string(),
        verified,
        message: if verified {
            "Bootstrap verification confirmed."
        } else {
            "Endorsement confirmed; normal verification is still required."
        }
        .to_string(),
    }))
}

// ═══════════════════════════════════════════════════════════════════════
// Testnet convenience RPCs (faucet-only)
//
// These endpoints construct, sign, and submit blocks server-side so that
// testnet validation can be performed via simple curl commands without a
// local wallet. They are gated behind `enable_faucet`.
// ═══════════════════════════════════════════════════════════════════════

fn test_tools_allowed(network: burst_types::NetworkId, enabled: bool) -> bool {
    enabled
        && matches!(
            network,
            burst_types::NetworkId::Dev | burst_types::NetworkId::Test
        )
}

fn require_bootstrap_window(
    phase: burst_verification::bootstrap::BootstrapPhase,
    verified_count: u64,
    threshold: u32,
) -> Result<(), RpcError> {
    if phase == burst_verification::bootstrap::BootstrapPhase::Complete
        || verified_count >= u64::from(threshold)
    {
        return Err(RpcError::InvalidRequest(
            "genesis bootstrap has ended; use normal endorsements and verifier votes".into(),
        ));
    }
    Ok(())
}

fn require_faucet(state: &RpcState) -> Result<(), RpcError> {
    if !test_tools_allowed(state.network, state.enable_faucet) {
        return Err(RpcError::InvalidRequest(
            "test tools require a dev/test network AND an enabled faucet; never available on live"
                .into(),
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
    signing_grant: BlockHash,
    payment_epoch: BlockHash,
    release_sources: Vec<BlockHash>,
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
    params_hash: BlockHash,
    merge_sources: Vec<burst_types::TxHash>,
) -> Result<burst_ledger::StateBlock, RpcError> {
    use burst_ledger::CURRENT_BLOCK_VERSION;

    let now = Timestamp::now();

    let mut block = burst_ledger::StateBlock {
        signing_grant,
        delegation_payload: Vec::new(),
        version: CURRENT_BLOCK_VERSION,
        payment_epoch,
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
        params_hash,
        target_head: BlockHash::ZERO,
        merge_sources,
        release_sources,
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
        .generate(&block.work_root(), threshold)
        .map_err(|e| RpcError::Server(format!("work generation failed: {e}")))?;
    block.work = nonce.0;

    Ok(block)
}

// These root-key authoring helpers are test-tools-only, not device wallets.
// A missing or corrupt predecessor must not silently reset their grant.
fn prev_signing_grant(state: &RpcState, head: &BlockHash) -> Result<BlockHash, RpcError> {
    if head.is_zero() {
        return Ok(BlockHash::ZERO);
    }
    let bytes = state.block_store.get_block(head)?;
    let block: burst_ledger::StateBlock = bincode::deserialize(&bytes)
        .map_err(|e| RpcError::Server(format!("invalid signing predecessor: {e}")))?;
    if block.hash != *head || block.compute_hash() != *head {
        return Err(RpcError::Server("signing predecessor hash mismatch".into()));
    }
    Ok(block.active_signing_grant())
}

/// Cumulative-spent BRN (odometer) recorded on the account's head block.
/// Zero for new accounts — open blocks must not claim BRN (birthright is
/// computed, never self-reported).
fn prev_spent_brn(state: &RpcState, head: &BlockHash) -> u128 {
    if head.is_zero() {
        return 0;
    }
    state
        .block_store
        .get_block(head)
        .ok()
        .and_then(|b| bincode::deserialize::<burst_ledger::StateBlock>(&b).ok())
        .map_or(0, |b| b.brn_balance)
}

/// Submit a block through the block processor and return whether it was accepted.
fn submit_block(block: &burst_ledger::StateBlock, state: &RpcState) -> Result<bool, RpcError> {
    let block_bytes = bincode::serialize(block)
        .map_err(|e| RpcError::Server(format!("block serialization failed: {e}")))?;
    let result = state
        .block_processor
        .process_block(&block_bytes)
        .map_err(RpcError::Server)?;
    let accepted = matches!(
        result,
        crate::server::ProcessResult::Accepted | crate::server::ProcessResult::Queued
    );
    if !accepted {
        let detail = match &result {
            crate::server::ProcessResult::Rejected(r) => r.clone(),
            crate::server::ProcessResult::Duplicate => "duplicate block".to_string(),
            crate::server::ProcessResult::Fork => "fork detected".to_string(),
            crate::server::ProcessResult::Gap => "gap — previous block unknown".to_string(),
            _ => "unknown".to_string(),
        };
        return Err(RpcError::Server(format!("block not accepted: {detail}")));
    }
    Ok(true)
}

/// Queued is not confirmed. Do not advertise test funding as successful until
/// the same consensus path used by real wallets confirms the exact block.
async fn submit_and_confirm(block: &StateBlock, state: &RpcState) -> Result<(), RpcError> {
    submit_block(block, state)?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if is_block_confirmed(&block.hash, &block.account, state) {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    Err(RpcError::Server(format!(
        "confirmation pending for {}; check block_info and account_pending before retrying",
        block.hash,
    )))
}

#[cfg(test)]
mod test_tool_policy_tests {
    use super::{require_bootstrap_window, test_tools_allowed};
    use burst_types::NetworkId;

    #[test]
    fn live_never_allows_test_tools_even_when_misconfigured() {
        assert!(!test_tools_allowed(NetworkId::Live, true));
        assert!(!test_tools_allowed(NetworkId::Live, false));
    }

    #[test]
    fn dev_and_test_require_explicit_opt_in() {
        for network in [NetworkId::Dev, NetworkId::Test] {
            assert!(!test_tools_allowed(network, false));
            assert!(test_tools_allowed(network, true));
        }
    }

    #[test]
    fn genesis_shortcut_closes_at_the_bootstrap_threshold() {
        use burst_verification::bootstrap::BootstrapPhase::{Active, Complete};
        assert!(require_bootstrap_window(Active, 49, 50).is_ok());
        assert!(require_bootstrap_window(Active, 50, 50).is_err());
        assert!(require_bootstrap_window(Active, 51, 50).is_err());
        assert!(require_bootstrap_window(Active, 0, 0).is_err());
        assert!(require_bootstrap_window(Complete, 1, 1000).is_err());
    }
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

    if !account.state.is_verified() {
        return Err(RpcError::InvalidRequest(
            "account must be verified to burn BRN (use faucet first)".into(),
        ));
    }

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

    if brn_balance < amount {
        return Err(RpcError::InvalidRequest(format!(
            "insufficient BRN: have {brn_balance}, need {amount}"
        )));
    }

    let brn_after = brn_balance - amount; // computed remaining (for the response)
    let trst_before = account.trst_balance;
    // Two-phase burn (8.4a): the minted TRST goes to pending and must be
    // claimed with a Receive block — the burn block itself never changes
    // the burner's TRST balance.
    let trst_after = trst_before;

    let tx_hash = TxHash::new(burst_crypto::blake2b_256(
        &[
            address.as_str().as_bytes(),
            &now.as_secs().to_be_bytes(),
            b"burn",
        ]
        .concat(),
    ));

    if account.head.is_zero() {
        return Err(RpcError::InvalidRequest(
            "receive an initial payment before burning BRN".into(),
        ));
    }
    let block_type = burst_ledger::BlockType::Burn;
    let destination = BlockHash::new(
        burst_crypto::decode_address(address.as_str())
            .ok_or_else(|| RpcError::InvalidRequest("invalid burn recipient".into()))?,
    );

    let spent_brn = prev_spent_brn(state, &account.head);
    let pk_bytes = private_key.0;
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&address)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let signing_grant = prev_signing_grant(state, &account.head)?;
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let representative = account.representative.clone();
        let previous = account.head;
        let work_gen = state.work_generator.clone();
        let min_diff = state.current_params()?.min_work_difficulty;
        let (payment_epoch, ph) = (
            state.meta_store.payment_epoch()?,
            state.current_params()?.params_hash(),
        );
        move || {
            let pk = burst_types::PrivateKey(pk_bytes);
            build_and_sign_block(
                signing_grant,
                payment_epoch,
                release_sources,
                block_type,
                &address,
                previous,
                &representative,
                spent_brn
                    .checked_add(amount)
                    .ok_or_else(|| RpcError::Server("BRN spend overflow".into()))?,
                trst_after,
                destination,
                TxHash::ZERO,
                tx_hash,
                &pk,
                &work_gen,
                min_diff,
                ph,
                Vec::new(),
            )
        }
    })
    .await
    .map_err(|e| RpcError::Server(format!("block build task failed: {e}")))??;

    // The block processor is the single authoritative writer: it persists the
    // block (with a consistent per-account height index), updates the account
    // record, applies the BRN burn, and updates rep weights. The handler must
    // NOT also write the block/account directly — doing both double-counted
    // block_count and corrupted the height index, which made the account chain
    // unservable to the ascending-bootstrap responder.
    submit_and_confirm(&block, state).await?;

    Ok(to_value(&BurnSimpleResponse {
        block_hash: format!("{}", block.hash),
        account: address.to_string(),
        brn_before: brn_balance.to_string(),
        brn_after: brn_after.to_string(),
        trst_before: trst_before.to_string(),
        trst_after: trst_after.to_string(),
        origin: block.hash.to_string(),
    }))
}

// ── send_simple (TRST transfer) ─────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SendSimpleRequest {
    pub private_key: String,
    pub destination: String,
    /// Burn or merge origin selected from trst_portfolio, just like a wallet.
    pub origin: String,
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

    if !account.state.is_verified() {
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

    let dest_hash = BlockHash::new(
        burst_crypto::decode_address(destination.as_str())
            .ok_or_else(|| RpcError::InvalidRequest("invalid destination".into()))?,
    );
    let origin = parse_tx_hash(&req.origin)?;
    let tx_hash = TxHash::new(burst_crypto::blake2b_256(
        &[
            address.as_str().as_bytes(),
            &now.as_secs().to_be_bytes(),
            b"send",
        ]
        .concat(),
    ));

    if account.head.is_zero() {
        return Err(RpcError::InvalidRequest(
            "receive an initial payment before sending TRST".into(),
        ));
    }
    let block_type = burst_ledger::BlockType::Send;

    let spent_brn = prev_spent_brn(state, &account.head);
    let pk_bytes = private_key.0;
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&address)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let signing_grant = prev_signing_grant(state, &account.head)?;
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let representative = account.representative.clone();
        let previous = account.head;
        let work_gen = state.work_generator.clone();
        let min_diff = state.current_params()?.min_work_difficulty;
        let (payment_epoch, ph) = (
            state.meta_store.payment_epoch()?,
            state.current_params()?.params_hash(),
        );
        move || {
            let pk = burst_types::PrivateKey(pk_bytes);
            build_and_sign_block(
                signing_grant,
                payment_epoch,
                release_sources,
                block_type,
                &address,
                previous,
                &representative,
                spent_brn,
                trst_after,
                dest_hash,
                origin,
                tx_hash,
                &pk,
                &work_gen,
                min_diff,
                ph,
                Vec::new(),
            )
        }
    })
    .await
    .map_err(|e| RpcError::Server(format!("block build task failed: {e}")))??;

    // The processor alone validates provenance and writes account/pending state.
    submit_and_confirm(&block, state).await?;

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
            verifier_opted_in_at: None,
            orv_evicted: false,
        });

    let trst_before = account.trst_balance;
    let trst_after = trst_before
        .checked_add(pending.amount)
        .ok_or_else(|| RpcError::InvalidRequest("TRST balance overflow".into()))?;

    let now = Timestamp::now();

    let is_open = account.head == BlockHash::ZERO;
    let block_type = if is_open {
        burst_ledger::BlockType::Open
    } else {
        burst_ledger::BlockType::Receive
    };

    let link = BlockHash::new(*send_tx_hash.as_bytes());
    let source_block = deserialize_block(&state.block_store.get_block(&link)?)?;
    let origin = if source_block.block_type == burst_ledger::BlockType::Burn {
        send_tx_hash
    } else {
        source_block.origin
    };
    let tx_hash = TxHash::new(burst_crypto::blake2b_256(
        &[
            address.as_str().as_bytes(),
            &now.as_secs().to_be_bytes(),
            b"receive",
        ]
        .concat(),
    ));

    let spent_brn = prev_spent_brn(state, &account.head);
    let pk_bytes = private_key.0;
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&address)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let signing_grant = prev_signing_grant(state, &account.head)?;
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let representative = account.representative.clone();
        let previous = account.head;
        let work_gen = state.work_generator.clone();
        let min_diff = state.current_params()?.min_work_difficulty;
        let (payment_epoch, ph) = (
            state.meta_store.payment_epoch()?,
            state.current_params()?.params_hash(),
        );
        move || {
            let pk = burst_types::PrivateKey(pk_bytes);
            build_and_sign_block(
                signing_grant,
                payment_epoch,
                release_sources,
                block_type,
                &address,
                previous,
                &representative,
                spent_brn,
                trst_after,
                link,
                origin,
                tx_hash,
                &pk,
                &work_gen,
                min_diff,
                ph,
                Vec::new(),
            )
        }
    })
    .await
    .map_err(|e| RpcError::Server(format!("block build task failed: {e}")))??;

    submit_and_confirm(&block, state).await?;

    Ok(to_value(&ReceiveSimpleResponse {
        block_hash: format!("{}", block.hash),
        account: address.to_string(),
        amount: pending.amount.to_string(),
        trst_before: trst_before.to_string(),
        trst_after: trst_after.to_string(),
    }))
}

// ── change_rep_simple (change consensus representative) ──────────────

#[derive(Debug, Deserialize)]
pub struct ChangeRepSimpleRequest {
    pub private_key: String,
    pub new_representative: String,
}

#[derive(Debug, Serialize)]
pub struct ChangeRepSimpleResponse {
    pub block_hash: String,
    pub account: String,
    pub old_representative: String,
    pub new_representative: String,
}

pub async fn handle_change_rep_simple(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    require_faucet(state)?;

    let req: ChangeRepSimpleRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account(&req.new_representative)?;
    let private_key = parse_private_key(&req.private_key)?;
    let public_key = burst_crypto::public_from_private(&private_key);
    let address = burst_crypto::derive_address(&public_key);

    let account = state
        .account_store
        .get_account(&address)
        .map_err(|e| account_not_found(e, address.as_str()))?;

    if account.head == BlockHash::ZERO {
        return Err(RpcError::InvalidRequest(
            "account has no blocks yet — send or receive first".into(),
        ));
    }

    let old_representative = account.representative.clone();
    let new_representative = WalletAddress::new(req.new_representative.clone());

    if old_representative == new_representative {
        return Err(RpcError::InvalidRequest(
            "new representative is the same as current".into(),
        ));
    }

    let now = Timestamp::now();

    let tx_hash = TxHash::new(burst_crypto::blake2b_256(
        &[
            address.as_str().as_bytes(),
            &now.as_secs().to_be_bytes(),
            b"change_rep",
        ]
        .concat(),
    ));

    let spent_brn = prev_spent_brn(state, &account.head);
    let pk_bytes = private_key.0;
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&address)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let signing_grant = prev_signing_grant(state, &account.head)?;
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let new_rep = new_representative.clone();
        let previous = account.head;
        let trst_balance = account.trst_balance;
        let work_gen = state.work_generator.clone();
        let min_diff = state.current_params()?.min_work_difficulty;
        let (payment_epoch, ph) = (
            state.meta_store.payment_epoch()?,
            state.current_params()?.params_hash(),
        );
        move || {
            let pk = burst_types::PrivateKey(pk_bytes);
            build_and_sign_block(
                signing_grant,
                payment_epoch,
                release_sources,
                burst_ledger::BlockType::ChangeRepresentative,
                &address,
                previous,
                &new_rep,
                spent_brn,
                trst_balance,
                BlockHash::ZERO,
                TxHash::ZERO,
                tx_hash,
                &pk,
                &work_gen,
                min_diff,
                ph,
                Vec::new(),
            )
        }
    })
    .await
    .map_err(|e| RpcError::Server(format!("block build task failed: {e}")))??;

    // Human voting weight follows the confirmed transaction, never a local
    // account rewrite or the amount of TRST held.
    submit_and_confirm(&block, state).await?;

    Ok(to_value(&ChangeRepSimpleResponse {
        block_hash: format!("{}", block.hash),
        account: address.to_string(),
        old_representative: old_representative.to_string(),
        new_representative: req.new_representative,
    }))
}

// ── verifier_opt_in / verifier_opt_out ───────────────────────────────
// A verified wallet volunteers for (or leaves) the verifier pool. Published
// on-chain so every node derives the same eligible set (deterministic VRF
// selection). Not faucet-gated: opting in is a real protocol action needed for
// decentralized verification after the genesis window. (Server-side signing
// with the caller's key follows the same convenience pattern as the other
// build-and-submit RPCs; a client-signed submission path is the mainnet ideal.)

#[derive(Debug, Deserialize)]
pub struct VerifierOptRequest {
    pub private_key: String,
}

#[derive(Debug, Serialize)]
pub struct VerifierOptResponse {
    pub block_hash: String,
    pub account: String,
    pub opted_in: bool,
}

pub async fn handle_verifier_opt_in(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    verifier_opt(params, state, true).await
}

pub async fn handle_verifier_opt_out(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    verifier_opt(params, state, false).await
}

async fn verifier_opt(
    params: serde_json::Value,
    state: &RpcState,
    opt_in: bool,
) -> Result<serde_json::Value, RpcError> {
    let req: VerifierOptRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    let private_key = parse_private_key(&req.private_key)?;
    let public_key = burst_crypto::public_from_private(&private_key);
    let address = burst_crypto::derive_address(&public_key);

    let account = state
        .account_store
        .get_account(&address)
        .map_err(|e| account_not_found(e, address.as_str()))?;

    if account.head == BlockHash::ZERO {
        return Err(RpcError::InvalidRequest(
            "account has no blocks yet — receive or transact first".into(),
        ));
    }

    let block_type = if opt_in {
        burst_ledger::BlockType::VerifierOptIn
    } else {
        burst_ledger::BlockType::VerifierOptOut
    };
    let now = Timestamp::now();
    let tx_hash = TxHash::new(burst_crypto::blake2b_256(
        &[
            address.as_str().as_bytes(),
            &now.as_secs().to_be_bytes(),
            if opt_in {
                b"verifier_opt_in"
            } else {
                b"verifier_opt_out"
            },
        ]
        .concat(),
    ));

    let spent_brn = prev_spent_brn(state, &account.head);
    let pk_bytes = private_key.0;
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&address)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let signing_grant = prev_signing_grant(state, &account.head)?;
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let representative = account.representative.clone();
        let previous = account.head;
        let trst_balance = account.trst_balance;
        let work_gen = state.work_generator.clone();
        let min_diff = state.current_params()?.min_work_difficulty;
        let (payment_epoch, ph) = (
            state.meta_store.payment_epoch()?,
            state.current_params()?.params_hash(),
        );
        move || {
            let pk = burst_types::PrivateKey(pk_bytes);
            build_and_sign_block(
                signing_grant,
                payment_epoch,
                release_sources,
                block_type,
                &address,
                previous,
                &representative,
                spent_brn,
                trst_balance,
                BlockHash::ZERO,
                TxHash::ZERO,
                tx_hash,
                &pk,
                &work_gen,
                min_diff,
                ph,
                Vec::new(),
            )
        }
    })
    .await
    .map_err(|e| RpcError::Server(format!("block build task failed: {e}")))??;

    // Single-writer: the block processor validates (requires Verified for
    // opt-in), persists, and sets AccountInfo.verifier_opted_in_at.
    submit_block(&block, state)?;

    Ok(to_value(&VerifierOptResponse {
        block_hash: format!("{}", block.hash),
        account: address.to_string(),
        opted_in: opt_in,
    }))
}

// ── governance_propose_simple (create a parameter change proposal) ────

#[derive(Debug, Deserialize)]
pub struct GovernanceProposeSimpleRequest {
    pub private_key: String,
    pub param: String,
    pub new_value: String,
}

#[derive(Debug, Serialize)]
pub struct GovernanceProposeSimpleResponse {
    pub block_hash: String,
    pub proposal_hash: String,
    pub account: String,
    pub param: String,
    pub new_value: String,
}

pub async fn handle_governance_propose_simple(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    require_faucet(state)?;

    let req: GovernanceProposeSimpleRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    let private_key = parse_private_key(&req.private_key)?;
    let public_key = burst_crypto::public_from_private(&private_key);
    let address = burst_crypto::derive_address(&public_key);

    let account = state
        .account_store
        .get_account(&address)
        .map_err(|e| account_not_found(e, address.as_str()))?;

    if account.head == BlockHash::ZERO {
        return Err(RpcError::InvalidRequest(
            "account has no blocks yet — burn BRN first".into(),
        ));
    }

    let param = parse_governable_param(&req.param)?;
    let new_value: u128 = req
        .new_value
        .parse()
        .map_err(|e| RpcError::InvalidRequest(format!("invalid new_value: {e}")))?;

    let content = burst_governance::ProposalContent::ParameterChange {
        param: param.clone(),
        new_value,
    };
    let link = BlockHash::new(
        content
            .encode_compact()
            .map_err(|e| RpcError::InvalidRequest(e.to_string()))?,
    );

    let now = Timestamp::now();

    let tx_hash = TxHash::new(burst_crypto::blake2b_256(
        &[
            address.as_str().as_bytes(),
            &now.as_secs().to_be_bytes(),
            b"governance_propose",
        ]
        .concat(),
    ));

    let spent_brn = prev_spent_brn(state, &account.head);
    let pk_bytes = private_key.0;
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&address)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let signing_grant = prev_signing_grant(state, &account.head)?;
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let representative = account.representative.clone();
        let previous = account.head;
        let trst_balance = account.trst_balance;
        let work_gen = state.work_generator.clone();
        let min_diff = state.current_params()?.min_work_difficulty;
        let (payment_epoch, ph) = (
            state.meta_store.payment_epoch()?,
            state.current_params()?.params_hash(),
        );
        move || {
            let pk = burst_types::PrivateKey(pk_bytes);
            build_and_sign_block(
                signing_grant,
                payment_epoch,
                release_sources,
                burst_ledger::BlockType::GovernanceProposal,
                &address,
                previous,
                &representative,
                spent_brn,
                trst_balance,
                link,
                TxHash::ZERO,
                tx_hash,
                &pk,
                &work_gen,
                min_diff,
                ph,
                Vec::new(),
            )
        }
    })
    .await
    .map_err(|e| RpcError::Server(format!("block build task failed: {e}")))??;

    submit_and_confirm(&block, state).await?;

    Ok(to_value(&GovernanceProposeSimpleResponse {
        block_hash: format!("{}", block.hash),
        proposal_hash: format!("{}", block.hash),
        account: address.to_string(),
        param: req.param,
        new_value: req.new_value,
    }))
}

// ── governance_propose_eviction (evict/reinstate an ORV representative) ──
//
// Opens a governance proposal to EVICT a representative from the ORV consensus
// set (or REINSTATE one). Decided by the same one-verified-human-one-vote
// process as every proposal — NOT by ORV weight — so a rep that amassed weight
// cannot vote down its own eviction. On activation the target's `orv_evicted`
// flag is set/cleared and it contributes zero consensus weight.
//
// Encoding: the target's 32-byte pubkey rides in the block `link`; the `origin`
// field carries the evict/reinstate sentinel (the 32-byte link has no room for
// both a target and a discriminant). `reason` is a UX/audit field only.

#[derive(Debug, Deserialize)]
pub struct GovernanceProposeEvictionRequest {
    pub private_key: String,
    /// The representative address to evict or reinstate.
    pub target: String,
    /// `true` to evict, `false` to reinstate.
    pub evict: bool,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct GovernanceProposeEvictionResponse {
    pub block_hash: String,
    pub proposal_hash: String,
    pub account: String,
    pub target: String,
    pub evict: bool,
}

pub async fn handle_governance_propose_eviction(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    require_faucet(state)?;

    let req: GovernanceProposeEvictionRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    let private_key = parse_private_key(&req.private_key)?;
    let public_key = burst_crypto::public_from_private(&private_key);
    let address = burst_crypto::derive_address(&public_key);

    let account = state
        .account_store
        .get_account(&address)
        .map_err(|e| account_not_found(e, address.as_str()))?;
    if account.head == BlockHash::ZERO {
        return Err(RpcError::InvalidRequest(
            "account has no blocks yet — burn BRN first".into(),
        ));
    }

    // Target address → 32-byte pubkey carried in the block `link`.
    let target_pubkey = burst_crypto::decode_address(&req.target).ok_or_else(|| {
        RpcError::InvalidRequest(format!("invalid target address: {}", req.target))
    })?;
    let link = BlockHash::new(target_pubkey);
    let origin = TxHash::new(if req.evict {
        burst_governance::ORV_EVICT_MARKER
    } else {
        burst_governance::ORV_REINSTATE_MARKER
    });

    let now = Timestamp::now();

    let tx_hash = TxHash::new(burst_crypto::blake2b_256(
        &[
            address.as_str().as_bytes(),
            &now.as_secs().to_be_bytes(),
            b"governance_propose_eviction",
            req.target.as_bytes(),
        ]
        .concat(),
    ));

    let spent_brn = prev_spent_brn(state, &account.head);
    let pk_bytes = private_key.0;
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&address)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let signing_grant = prev_signing_grant(state, &account.head)?;
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let representative = account.representative.clone();
        let previous = account.head;
        let trst_balance = account.trst_balance;
        let work_gen = state.work_generator.clone();
        let min_diff = state.current_params()?.min_work_difficulty;
        let (payment_epoch, ph) = (
            state.meta_store.payment_epoch()?,
            state.current_params()?.params_hash(),
        );
        move || {
            let pk = burst_types::PrivateKey(pk_bytes);
            build_and_sign_block(
                signing_grant,
                payment_epoch,
                release_sources,
                burst_ledger::BlockType::GovernanceProposal,
                &address,
                previous,
                &representative,
                spent_brn,
                trst_balance,
                link,
                origin,
                tx_hash,
                &pk,
                &work_gen,
                min_diff,
                ph,
                Vec::new(),
            )
        }
    })
    .await
    .map_err(|e| RpcError::Server(format!("block build task failed: {e}")))??;

    submit_and_confirm(&block, state).await?;

    Ok(to_value(&GovernanceProposeEvictionResponse {
        block_hash: format!("{}", block.hash),
        proposal_hash: format!("{}", block.hash),
        account: address.to_string(),
        target: req.target,
        evict: req.evict,
    }))
}

// ── governance_vote_simple (vote on a governance proposal) ───────────

#[derive(Debug, Deserialize)]
pub struct GovernanceVoteSimpleRequest {
    pub private_key: String,
    pub proposal_hash: String,
    pub vote: String,
}

#[derive(Debug, Serialize)]
pub struct GovernanceVoteSimpleResponse {
    pub block_hash: String,
    pub account: String,
    pub proposal_hash: String,
    pub vote: String,
}

pub async fn handle_governance_vote_simple(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    require_faucet(state)?;

    let req: GovernanceVoteSimpleRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    let private_key = parse_private_key(&req.private_key)?;
    let public_key = burst_crypto::public_from_private(&private_key);
    let address = burst_crypto::derive_address(&public_key);

    let account = state
        .account_store
        .get_account(&address)
        .map_err(|e| account_not_found(e, address.as_str()))?;

    if account.head == BlockHash::ZERO {
        return Err(RpcError::InvalidRequest(
            "account has no blocks yet — burn BRN first".into(),
        ));
    }

    let vote_byte: u8 = match req.vote.to_lowercase().as_str() {
        "yea" | "yes" | "y" => 0,
        "nay" | "no" | "n" => 1,
        "abstain" | "a" => 2,
        _ => {
            return Err(RpcError::InvalidRequest(
                "vote must be 'yea', 'nay', or 'abstain'".into(),
            ))
        }
    };

    let proposal_hash_bytes = hex::decode(&req.proposal_hash)
        .map_err(|e| RpcError::InvalidRequest(format!("invalid proposal_hash hex: {e}")))?;
    if proposal_hash_bytes.len() != 32 {
        return Err(RpcError::InvalidRequest(
            "proposal_hash must be 32 bytes (64 hex chars)".into(),
        ));
    }
    let mut ph = [0u8; 32];
    ph.copy_from_slice(&proposal_hash_bytes);
    let link = BlockHash::new(ph);

    let mut tx_bytes = [0u8; 32];
    tx_bytes[0] = vote_byte;
    let transaction = TxHash::new(tx_bytes);

    let spent_brn = prev_spent_brn(state, &account.head);
    let pk_bytes = private_key.0;
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&address)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let signing_grant = prev_signing_grant(state, &account.head)?;
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let representative = account.representative.clone();
        let previous = account.head;
        let trst_balance = account.trst_balance;
        let work_gen = state.work_generator.clone();
        let min_diff = state.current_params()?.min_work_difficulty;
        let (payment_epoch, ph) = (
            state.meta_store.payment_epoch()?,
            state.current_params()?.params_hash(),
        );
        move || {
            let pk = burst_types::PrivateKey(pk_bytes);
            build_and_sign_block(
                signing_grant,
                payment_epoch,
                release_sources,
                burst_ledger::BlockType::GovernanceVote,
                &address,
                previous,
                &representative,
                spent_brn,
                trst_balance,
                link,
                TxHash::ZERO,
                transaction,
                &pk,
                &work_gen,
                min_diff,
                ph,
                Vec::new(),
            )
        }
    })
    .await
    .map_err(|e| RpcError::Server(format!("block build task failed: {e}")))??;

    submit_and_confirm(&block, state).await?;

    let vote_name = match vote_byte {
        0 => "yea",
        1 => "nay",
        _ => "abstain",
    };

    Ok(to_value(&GovernanceVoteSimpleResponse {
        block_hash: format!("{}", block.hash),
        account: address.to_string(),
        proposal_hash: req.proposal_hash,
        vote: vote_name.to_string(),
    }))
}

fn parse_governable_param(name: &str) -> Result<burst_governance::GovernableParam, RpcError> {
    burst_governance::GovernableParam::from_name(name)
        .map_err(|error| RpcError::InvalidRequest(error.to_string()))
}
