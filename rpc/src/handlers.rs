//! RPC handlers for ledger queries, block submission and test-wallet helpers.

use crate::error::RpcError;
use crate::pagination;
use crate::state::RpcState;

use crate::state::ProcessResult;
#[cfg(test)]
use burst_brn::BrnWalletState;
use burst_governance::Proposal;
use burst_ledger::StateBlock;
use burst_store::account::AccountInfo;
use burst_store::StoreError;
use burst_types::{BlockHash, Signature, Timestamp, TxHash, WalletAddress};

use serde::Deserialize;
use tracing::debug;

mod confirmed_successor;
mod governance_projection;
mod origin_history;
pub(crate) mod provenance;
mod representatives;
mod telemetry;
mod trace_origin;
mod verification_activity;

impl RpcState {
    /// Read persisted parameters, falling back to startup configuration if absent.
    fn current_params(&self) -> Result<burst_types::ProtocolParams, RpcError> {
        match self.brn_store.get_meta(b"protocol_params")? {
            Some(bytes) => bincode::deserialize(&bytes).map_err(|e| {
                RpcError::Server(format!("invalid persisted protocol parameters: {e}"))
            }),
            None => Ok((*self.params).clone()),
        }
    }
}

mod helpers;
use helpers::*;

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

/// Return wallet accrual state and rate history for local BRN calculations.
pub async fn handle_brn_components(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: AccountInfoRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account_format(&req.account)?;
    let address = WalletAddress::new(req.account.clone());
    let account_store = state.account_store.clone();
    let engine = state.brn_engine.clone();
    state.read_executor.run(move || {
        let account = account_store
            .get_account(&address)
            .map_err(|e| account_not_found(e, &req.account))?;
        let (wallet, rate_history) = {
            let brn = engine.blocking_lock();
            let wallet = brn
            .get_wallet(&address)
            .cloned()
            .unwrap_or_else(|| brn_state_from_account(&account, 0));
            (wallet, brn.rate_history.clone())
        };
        Ok(serde_json::json!({
            "account": req.account, "verified_at": wallet.verified_at.as_secs(),
            "total_burned": wallet.total_burned.to_string(), "total_staked": wallet.total_staked.to_string(),
            "release_sources": wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES).iter().map(ToString::to_string).collect::<Vec<_>>(),
            "accrual_active": wallet.accrual_active,
            "accrual_stopped_at": wallet.accrual_stopped_at.map(|t| t.as_secs()),
            "paused_intervals": wallet.paused_intervals.iter().map(|(start,end)| serde_json::json!({"start":start.as_secs(),"end":end.as_secs()})).collect::<Vec<_>>(),
            "as_of": Timestamp::now().as_secs(),
            "rate_history": rate_history.segments().iter().map(|s| serde_json::json!({
                "rate": s.rate.to_string(), "start": s.start.as_secs(), "end": s.end.map(|t|t.as_secs())
            })).collect::<Vec<_>>()
        }))
    }).await
}

pub async fn handle_trst_portfolio(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: AccountInfoRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account_format(&req.account)?;
    let address = WalletAddress::new(req.account);
    let engine = state.trst_engine.clone();
    let pending = state.pending_store.clone();
    state
        .read_executor
        .run(move || {
            let (snapshot, outgoing) =
                PortfolioSnapshot::capture_with_pending(&engine, &address, || {
                    pending.capture_pending_from_account(&address)
                })?;
            let totals = outgoing.totals(&address)?;
            portfolio_snapshot_totals_response(&snapshot, Timestamp::now(), totals)
        })
        .await
}

struct PortfolioSnapshot {
    portfolio: Option<burst_utils::Shared<burst_trst::WalletPortfolio>>,
    merger_graph: burst_trst::MergerGraph,
    wallet_origins: burst_utils::SharedMap<WalletAddress, burst_utils::SharedSet<TxHash>>,
    expiry_secs: u64,
}

impl PortfolioSnapshot {
    fn capture_with_pending<T>(
        engine: &tokio::sync::Mutex<burst_trst::TrstEngine>,
        address: &WalletAddress,
        pending: impl FnOnce() -> Result<T, StoreError>,
    ) -> Result<(Self, T), RpcError> {
        let engine = engine.blocking_lock();
        let outgoing = pending()?;
        Ok((Self::capture(&engine, address), outgoing))
    }

    fn capture(engine: &burst_trst::TrstEngine, address: &WalletAddress) -> Self {
        Self {
            portfolio: engine.wallets.get(address).cloned(),
            merger_graph: engine.merger_graph.clone(),
            wallet_origins: engine.wallet_origins.clone(),
            expiry_secs: engine.expiry_secs,
        }
    }
}

#[cfg(test)]
fn portfolio_response(
    trst: &burst_trst::TrstEngine,
    address: &WalletAddress,
    now: Timestamp,
    outgoing: &[(WalletAddress, TxHash, burst_store::pending::PendingInfo)],
) -> Result<serde_json::Value, RpcError> {
    portfolio_snapshot_response(
        &PortfolioSnapshot::capture(trst, address),
        address,
        now,
        outgoing,
    )
}

#[cfg(test)]
fn portfolio_snapshot_response(
    trst: &PortfolioSnapshot,
    address: &WalletAddress,
    now: Timestamp,
    outgoing: &[(WalletAddress, TxHash, burst_store::pending::PendingInfo)],
) -> Result<serde_json::Value, RpcError> {
    let mut totals = burst_store::pending::PendingSourceTotals::default();
    for (_, _, pending) in outgoing {
        totals.include(address, pending)?;
    }
    portfolio_snapshot_totals_response(trst, now, totals)
}

fn portfolio_snapshot_totals_response(
    trst: &PortfolioSnapshot,
    now: Timestamp,
    totals: burst_store::pending::PendingSourceTotals,
) -> Result<serde_json::Value, RpcError> {
    let mut active = 0u128;
    let mut expired = 0u128;
    let mut revoked = 0u128;
    let mut batches = Vec::new();
    if let Some(portfolio) = trst.portfolio.as_ref() {
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
        "total_pending":totals.amount.to_string(), "pending_count":totals.count}),
    )
}

#[cfg(test)]
mod portfolio_tests {
    use super::*;

    #[test]
    fn portfolio_pending_capture_is_locked_but_enumeration_is_unlocked() {
        let owner = WalletAddress::new("brst_cut");
        let engine = tokio::sync::Mutex::new(burst_trst::TrstEngine::new());
        let (snapshot, outgoing) = PortfolioSnapshot::capture_with_pending(&engine, &owner, || {
            assert!(
                engine.try_lock().is_err(),
                "pending and live holdings must share the lock cut"
            );
            Ok(|| {
                assert!(
                    engine.try_lock().is_ok(),
                    "pending enumeration must not retain the live lock"
                );
                Vec::new()
            })
        })
        .unwrap();
        let outgoing = outgoing();
        assert!(
            engine.try_lock().is_ok(),
            "formatting must not retain the live lock"
        );
        assert_eq!(
            portfolio_snapshot_response(&snapshot, &owner, Timestamp::new(1), &outgoing).unwrap()
                ["total_pending"],
            "0"
        );
    }

    #[test]
    fn captured_portfolio_preserves_order_and_totals_after_live_mutation() {
        let owner = WalletAddress::new("brst_snapshot");
        let mut engine = burst_trst::TrstEngine::with_expiry(100);
        for id in [3u8, 1, 2] {
            let token = engine
                .mint(
                    TxHash::new([id; 32]),
                    owner.clone(),
                    id as u128,
                    owner.clone(),
                    Timestamp::new(100),
                )
                .unwrap();
            engine.track_token(token);
        }
        let expected = portfolio_response(&engine, &owner, Timestamp::new(150), &[]).unwrap();
        let snapshot = PortfolioSnapshot::capture(&engine, &owner);
        let token = engine
            .mint(
                TxHash::new([4; 32]),
                owner.clone(),
                10,
                owner.clone(),
                Timestamp::new(150),
            )
            .unwrap();
        engine.track_token(token);
        engine.set_expiry_period(1, Timestamp::new(150)).unwrap();
        assert_eq!(
            portfolio_snapshot_response(&snapshot, &owner, Timestamp::new(150), &[]).unwrap(),
            expected
        );
        assert_ne!(
            portfolio_response(&engine, &owner, Timestamp::new(150), &[]).unwrap(),
            expected
        );
    }

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
        engine
            .set_expiry_period(u64::MAX, Timestamp::new(200))
            .unwrap();
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
    let engine = state.consti_engine.clone();
    state.read_executor.run(move || {
        let engine = engine.blocking_lock().clone();
        let document = engine.get_constitution();
        Ok(serde_json::json!({"version":document.version,
            "amendment_threshold":format!("{}%",params.consti_supermajority_bps as f64 / 100.0),
            "articles":document.articles.iter().filter(|a|!a.repealed).map(|a|serde_json::json!({
                "id":a.number.to_string(), "title":a.title, "text":a.text,
                "adopted_at":null, "amended_at":null, "introduced_by_amendment":a.introduced_by_amendment
            })).collect::<Vec<_>>()
        }))
    }).await
}

pub async fn handle_verification_activity(
    _: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    verification_activity::read(state).await
}

pub async fn handle_merger_graph_stats(
    _: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let cache = state.merger_stats_cache.clone();
    let trst = state.trst_engine.clone();
    let now = (state.display_now)();
    state
        .read_executor
        .run(move || {
            let mut cache = cache.blocking_lock();
            let snapshot = crate::merger_stats::Snapshot::new(&trst.blocking_lock());
            cache.read(snapshot, now)
        })
        .await
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
    let mut history = origin_history::Reader::new(state);
    let engine = state.trst_engine.clone();
    state.read_executor.run(move || {
        let block = history.get(BlockHash::new(*origin.as_bytes()))?.block.clone();
        if block.block_type != burst_ledger::BlockType::Burn {
            return Err(RpcError::InvalidRequest(
                "trace_origin expects the original BRN burn hash, not a send or merge hash".into(),
            ));
        }
        let previous = &history.get(block.previous)?.block;
        if previous.account != block.account {
            return Err(RpcError::Server("burn predecessor belongs to another account".into()));
        }
        let amount = block.brn_spent.checked_sub(previous.brn_spent)
            .ok_or_else(|| RpcError::Server("invalid burn odometer".into()))?;
        let (graph, wallets, expiry_secs) = {
            let trst = engine.blocking_lock();
            (trst.merger_graph.clone(), trst.wallets.clone(), trst.expiry_secs)
        };
        let status = if graph.is_origin_revoked(&origin) {
            "revoked"
        } else if expiry_secs != u64::MAX
            && Timestamp::now().as_secs() >= block.timestamp.as_secs().saturating_add(expiry_secs)
        {
            "expired"
        } else {
            "active"
        };
        let descendants = trace_origin::descendants(origin, &graph)?;
        history.prefetch(descendants.iter().map(|id| BlockHash::new(*id.as_bytes())).collect())?;
        let merges = trace_origin::forward_merges(origin, &graph, &wallets, |id| {
            let merged = &history.get(BlockHash::new(*id.as_bytes()))?.block;
            if merged.block_type != burst_ledger::BlockType::Merge {
                return Err(RpcError::Server("merge history contains a non-merge block".into()));
            }
            Ok(merged.timestamp.as_secs())
        })?;
        Ok(serde_json::json!({"origin":origin.to_string(),"originator":block.account.as_str(),"timestamp":block.timestamp.as_secs(),"amount":amount.to_string(),"status":status,"forward_merges":merges}))
    }).await
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
/// Uses indexed height lookups instead of loading the entire account chain.
fn is_block_confirmed(block_hash: &BlockHash, account: &WalletAddress, state: &RpcState) -> bool {
    is_block_confirmed_in(
        block_hash,
        account,
        state.account_store.as_ref(),
        state.block_store.as_ref(),
    )
}

fn is_block_confirmed_in(
    block_hash: &BlockHash,
    account: &WalletAddress,
    accounts: &dyn burst_store::account::AccountStore,
    blocks: &dyn burst_store::block::BlockStore,
) -> bool {
    let account_info = match accounts.get_account(account) {
        Ok(a) => a,
        Err(_) => return false,
    };
    if account_info.confirmation_height == 0 {
        return false;
    }
    confirmed_successor::confirmed(
        blocks,
        block_hash,
        account,
        account_info.confirmation_height,
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
        state
            .block_processor
            .process_block_async(&block_bytes)
            .await
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

pub async fn handle_block_info(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let request_started = std::time::Instant::now();
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

    let block_loaded = std::time::Instant::now();
    let confirmed = historical || is_block_confirmed(&block_hash, &block.account, state);
    let confirmed_successor = confirmed_successor::lookup(&block_hash, &block.account, state)?;
    tracing::debug!(hash = %block_hash, historical,
        load_us = block_loaded.duration_since(request_started).as_micros(),
        confirmation_lookup_us = block_loaded.elapsed().as_micros(),
        "block signing view stages");

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
        brn_balance: block.brn_spent.to_string(),
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

const MAX_BLOCKS_PER_REQUEST: usize = 1000;

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

    if req.hashes.len() > MAX_BLOCKS_PER_REQUEST {
        return Err(RpcError::InvalidRequest(format!(
            "batch size {} exceeds maximum of {}",
            req.hashes.len(),
            MAX_BLOCKS_PER_REQUEST
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
                            brn_balance: block.brn_spent.to_string(),
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

pub async fn handle_pending(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    handle_account_pending(params, state).await
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
    crate::work_budget::check_difficulty(difficulty, required)?;
    let result =
        crate::work_budget::generate(state.work_generator.clone(), block_hash, difficulty).await?;

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

pub async fn handle_telemetry(
    _params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let account_store = state.account_store.clone();
    let block_store = state.block_store.clone();
    let ledger_cache = state.ledger_cache.clone();
    let trst_engine = state.trst_engine.clone();
    let (account_count, verified_count, unverified_count, block_count, totals) = state
        .read_executor
        .run(move || {
            let summary = telemetry::read(&trst_engine, Timestamp::new(now), |visit| {
                account_store.visit_accounts(visit)
            })?;
            let block_count = if let Some(cache) = ledger_cache {
                cache.block_count()
            } else {
                block_store.block_count()?
            };
            Ok((
                summary.accounts,
                summary.verified,
                summary.unverified,
                block_count,
                summary.holdings,
            ))
        })
        .await?;
    let peer_count = state.peer_manager.read().await.connected_count() as u32;
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
        signing_clock: Some(telemetry::signing_clock(state.signing_clock.as_deref())),
    }))
}

pub async fn handle_peers(
    _params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let manager = state.peer_manager.read().await;
    let peers: Vec<String> = manager.iter_connected().map(|(id, _)| id.clone()).collect();
    Ok(serde_json::json!({ "peers": peers, "count": peers.len() }))
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
    validate_account_format(&request.account)?;
    if let Some(cursor) = &request.cursor {
        validate_account_format(cursor)?;
    }
    let verifier = WalletAddress::new(request.account);
    let engine = state.verification_engine.clone();
    state
        .read_executor
        .run(move || {
            let snapshot = engine.blocking_lock().activity_snapshot();
            let (targets, cursor) =
                verifier_assignment_page(snapshot.states(), &verifier, request.cursor.as_deref());
            Ok(serde_json::json!({"targets": targets, "cursor": cursor}))
        })
        .await
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
                endorsements: Vec::new().into(),
                endorser_set: Default::default(),
                selected_verifiers: vec![voter.clone()].into(),
                votes: Default::default(),
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

pub async fn handle_verification_status(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: VerificationStatusRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account_format(&req.account)?;

    let address = WalletAddress::new(req.account.clone());

    let account_store = state.account_store.clone();
    let verification_store = state.verification_store.clone();
    let engine = state.verification_engine.clone();
    let network = state.network;
    let endorsement_threshold = state.current_params()?.endorsement_threshold;
    state.read_executor.run(move || {
        let endorsements = verification_store.get_endorsements(&address)?;
        let mut endorser_count = endorsements.len() as u32;

        let mut challenge_active = verification_store
            .get_challenge(&address)?
            .is_some();

        let (mut status, verified_at, verifier_opted_in) =
            match account_store.get_account(&address) {
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
        let (process, live_challenge, selection_context) = {
            let verification = engine.blocking_lock();
            (verification.get_state(&address).cloned(), verification.has_active_challenge(&address),
                verification.selection_context(&address))
        };
        if let Some(process) = process {
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
            challenge_active = live_challenge;
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
            endorsement_threshold: endorsement_threshold,
            endorsers,
            verifier_votes,
            challenge_active,
            selection_available: burst_verification::checkpoint_selection_enabled(network),
            verifier_opted_in,
            selected_verifiers,
            selection_context: selection_context.map(|hash| hash.to_string()),
            selection_unavailable_reason: (!burst_verification::checkpoint_selection_enabled(
                network,
            ))
            .then(|| "certified_checkpoint_selection_release_gated".to_owned()),
        }))
    }).await
}

pub async fn handle_delegation_info(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: AccountInfoRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account_format(&req.account)?;
    let address = WalletAddress::new(req.account);
    let engine = state.delegation_engine.clone();
    state.read_executor.run(move || {
        let delegation = engine.blocking_lock().clone();
        Ok(serde_json::json!({
            "delegated_to": delegation.get_delegate(&address).map(|a| a.to_string()),
            "delegated_from": delegation.get_delegators(&address).iter().map(|a| a.to_string()).collect::<Vec<_>>()
        }))
    }).await
}

pub async fn handle_representatives(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: RepresentativesRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;

    let count = req.pagination.effective_count();
    let offset = req.pagination.decode_offset();

    let cache = state.rep_weight_cache.clone();
    state
        .read_executor
        .run(move || {
            let snapshot = cache.blocking_read().snapshot();
            let (page, next) = representatives::page(snapshot.weights(), offset, count);
            let representatives = page
                .into_iter()
                .map(|(account, weight)| RepresentativeEntry {
                    account: account.to_string(),
                    weight: weight.to_string(),
                    delegator_count: u64::try_from(
                        weight / burst_consensus::rep_weights::WEIGHT_PER_HUMAN,
                    )
                    .unwrap_or(u64::MAX),
                })
                .collect();
            Ok(to_value(&RepresentativesResponse {
                representatives,
                cursor: next.map(pagination::encode_cursor),
            }))
        })
        .await
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

pub async fn handle_send(
    params: serde_json::Value,
    _state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: SendRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account_format(&req.source)?;
    validate_account_format(&req.destination)?;
    let _amount = validate_positive_amount(&req.amount)?;

    Err(RpcError::InvalidRequest(
        "send requires a signed block via 'process' — use wallet_core to build and sign, then submit via 'process'".into(),
    ))
}

pub async fn handle_burn(
    params: serde_json::Value,
    _state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: BurnRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account_format(&req.source)?;
    validate_account_format(&req.destination)?;
    let _amount = validate_positive_amount(&req.amount)?;

    Err(RpcError::InvalidRequest(
        "burn requires a signed block via 'process' — use wallet_core to build and sign, then submit via 'process'".into(),
    ))
}

pub async fn handle_receive(
    params: serde_json::Value,
    _state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let req: ReceiveRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account_format(&req.account)?;

    Err(RpcError::InvalidRequest(
        "receive requires a signed block via 'process' — use wallet_core to build and sign, then submit via 'process'".into(),
    ))
}

/// Generate an address and public key, discarding the private key.
/// The optional seed is ignored. Create spendable wallets client-side.
pub async fn handle_wallet_create(
    params: serde_json::Value,
    _state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    let _req: WalletCreateRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;

    let keys = burst_crypto::generate_keypair();
    let address = burst_crypto::derive_address(&keys.public);
    let public_key = hex::encode(keys.public.as_bytes());

    Ok(to_value(&WalletCreateResponse {
        address: address.to_string(),
        public_key,
    }))
}

pub async fn handle_wallet_info(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    handle_account_info(params, state).await
}

pub async fn handle_node_info(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    handle_telemetry(params, state).await
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
    validate_account_format(&req.account)?;

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
    let (previous_spent, signing_grant) = signing_predecessor(state, &account.head)?;
    let spent = previous_spent
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

/// Author and confirm a genesis Endorse block during bootstrap. Requires the
/// genesis seed. Ledger rules determine whether this verifies the target;
/// the response reports its verification state after confirmation.
/// Genesis authoring is serialized because each block extends the same account.
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
    validate_account_format(&req.account)?;
    let target = WalletAddress::new(req.account.clone());

    let keys = burst_crypto::keypair_from_seed(&seed);
    let genesis_address = burst_crypto::derive_address(&keys.public);
    if target == genesis_address {
        return Err(RpcError::InvalidRequest(
            "cannot endorse the genesis account itself".into(),
        ));
    }
    let account = match state.account_store.get_account(&target) {
        Ok(account) => Some(account),
        Err(StoreError::NotFound(_)) => None,
        Err(error) => return Err(error.into()),
    };
    let target_head = account
        .as_ref()
        .map_or(BlockHash::ZERO, |account| account.head);
    if account
        .as_ref()
        .is_some_and(|account| account.state.is_verified())
    {
        return Err(RpcError::InvalidRequest(
            "target is already verified".into(),
        ));
    }
    let verified_count = state.account_store.verified_account_count()?;
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

    // Endorse encodes the target public key in link.
    let target_pubkey = burst_crypto::decode_address(target.as_str()).ok_or_else(|| {
        RpcError::InvalidRequest(format!("invalid target address: {}", target.as_str()))
    })?;
    let link = BlockHash::new(target_pubkey);

    let genesis_account = state
        .account_store
        .get_account(&genesis_address)
        .map_err(|e| {
            RpcError::Server(format!(
                "genesis account not found (is this the authority node?): {e}"
            ))
        })?;
    let previous = state
        .frontier_store
        .get_frontier(&genesis_address)
        .unwrap_or(genesis_account.head);
    let (previous_brn_spent, signing_grant) = signing_predecessor(state, &previous)?;
    let burn_amount = state.current_params()?.endorsement_burn_amount;

    // Endorse block: odometer += burn, TRST unchanged; signed by genesis key.
    let genesis_rep = genesis_account.representative.clone();
    let genesis_seed = seed;
    let work_generator = state.work_generator.clone();
    let min_work_difficulty = state.current_params()?.min_work_difficulty;
    let (payment_epoch, params_hash) = (
        state.meta_store.payment_epoch()?,
        state.current_params()?.params_hash(),
    );
    let genesis_for_build = genesis_address.clone();
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&genesis_address)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let block = tokio::task::spawn_blocking(move || {
        let private_key = burst_crypto::keypair_from_seed(&genesis_seed).private;
        let mut block = build_and_sign_block(
            signing_grant,
            payment_epoch,
            release_sources,
            burst_ledger::BlockType::Endorse,
            &genesis_for_build,
            previous,
            &genesis_rep,
            previous_brn_spent
                .checked_add(burn_amount)
                .ok_or_else(|| RpcError::Server("BRN spend overflow".into()))?,
            genesis_account.trst_balance,
            link,
            TxHash::ZERO,
            TxHash::ZERO,
            &private_key,
            &work_generator,
            min_work_difficulty,
            params_hash,
            Vec::new(),
        )?;
        block.target_head = target_head;
        block.hash = block.compute_hash();
        block.signature = burst_crypto::sign_message(block.hash.as_bytes(), &private_key);
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
        genesis: genesis_address.to_string(),
        verified,
        message: if verified {
            "Bootstrap verification confirmed."
        } else {
            "Endorsement confirmed; normal verification is still required."
        }
        .to_string(),
    }))
}

// Server-side signing helpers require Dev or Test with enable_faucet.

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
    if !phase.permits_creator(verified_count, threshold) {
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
    brn_spent: u128,
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
        brn_spent,
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
    let nonce =
        crate::work_budget::generate_blocking(work_generator, &block.work_root(), threshold)?;
    block.work = nonce.0;

    Ok(block)
}

// These root-key authoring helpers are test-tools-only, not device wallets.
// A missing or corrupt predecessor must not silently reset their grant.
fn signing_predecessor(state: &RpcState, head: &BlockHash) -> Result<(u128, BlockHash), RpcError> {
    if head.is_zero() {
        return Ok((0, BlockHash::ZERO));
    }
    let bytes = state.block_store.get_block(head)?;
    let block: burst_ledger::StateBlock = bincode::deserialize(&bytes)
        .map_err(|e| RpcError::Server(format!("invalid signing predecessor: {e}")))?;
    if block.hash != *head || block.compute_hash() != *head {
        return Err(RpcError::Server("signing predecessor hash mismatch".into()));
    }
    Ok((block.brn_spent, block.active_signing_grant()))
}

/// Submit a block and return true for accepted or queued results. Other outcomes
/// are errors; this does not wait for persistence or consensus confirmation.
async fn submit_block(
    block: &burst_ledger::StateBlock,
    state: &RpcState,
) -> Result<bool, RpcError> {
    let block_bytes = bincode::serialize(block)
        .map_err(|e| RpcError::Server(format!("block serialization failed: {e}")))?;
    let result = state
        .block_processor
        .process_block_async(&block_bytes)
        .await
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

/// Submit, then poll the exact block's canonical confirmation position for up to
/// 15 seconds. A timeout does not cancel the submission; callers must query its
/// status before retrying.
async fn submit_and_confirm(block: &StateBlock, state: &RpcState) -> Result<(), RpcError> {
    submit_block(block, state).await?;
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

pub async fn handle_wallet_create_full(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    require_faucet(state)?;

    let _req: WalletCreateFullRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;

    let keys = burst_crypto::generate_keypair();
    let address = burst_crypto::derive_address(&keys.public);

    Ok(to_value(&WalletCreateFullResponse {
        address: address.to_string(),
        public_key: hex::encode(keys.public.0),
        private_key: hex::encode(keys.private.0),
    }))
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
            "account must be verified to burn BRN".into(),
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
    // A burn creates pending TRST: the minted TRST goes to pending and must be
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

    let (spent_brn, signing_grant) = signing_predecessor(state, &account.head)?;
    let private_key_bytes = private_key.0;
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&address)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let representative = account.representative.clone();
        let previous = account.head;
        let work_generator = state.work_generator.clone();
        let min_work_difficulty = state.current_params()?.min_work_difficulty;
        let (payment_epoch, params_hash) = (
            state.meta_store.payment_epoch()?,
            state.current_params()?.params_hash(),
        );
        move || {
            let private_key = burst_types::PrivateKey(private_key_bytes);
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
                &private_key,
                &work_generator,
                min_work_difficulty,
                params_hash,
                Vec::new(),
            )
        }
    })
    .await
    .map_err(|e| RpcError::Server(format!("block build task failed: {e}")))??;

    // The processor owns ledger writes, including balances and account indexes.
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

pub async fn handle_send_simple(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    require_faucet(state)?;

    let req: SendSimpleRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account_format(&req.destination)?;
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

    let (spent_brn, signing_grant) = signing_predecessor(state, &account.head)?;
    let private_key_bytes = private_key.0;
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&address)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let representative = account.representative.clone();
        let previous = account.head;
        let work_generator = state.work_generator.clone();
        let min_work_difficulty = state.current_params()?.min_work_difficulty;
        let (payment_epoch, params_hash) = (
            state.meta_store.payment_epoch()?,
            state.current_params()?.params_hash(),
        );
        move || {
            let private_key = burst_types::PrivateKey(private_key_bytes);
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
                &private_key,
                &work_generator,
                min_work_difficulty,
                params_hash,
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

    let (spent_brn, signing_grant) = signing_predecessor(state, &account.head)?;
    let private_key_bytes = private_key.0;
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&address)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let representative = account.representative.clone();
        let previous = account.head;
        let work_generator = state.work_generator.clone();
        let min_work_difficulty = state.current_params()?.min_work_difficulty;
        let (payment_epoch, params_hash) = (
            state.meta_store.payment_epoch()?,
            state.current_params()?.params_hash(),
        );
        move || {
            let private_key = burst_types::PrivateKey(private_key_bytes);
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
                &private_key,
                &work_generator,
                min_work_difficulty,
                params_hash,
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

pub async fn handle_change_rep_simple(
    params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    require_faucet(state)?;

    let req: ChangeRepSimpleRequest =
        serde_json::from_value(params).map_err(|e| RpcError::InvalidRequest(e.to_string()))?;
    validate_account_format(&req.new_representative)?;
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

    let (spent_brn, signing_grant) = signing_predecessor(state, &account.head)?;
    let private_key_bytes = private_key.0;
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&address)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let new_rep = new_representative.clone();
        let previous = account.head;
        let trst_balance = account.trst_balance;
        let work_generator = state.work_generator.clone();
        let min_work_difficulty = state.current_params()?.min_work_difficulty;
        let (payment_epoch, params_hash) = (
            state.meta_store.payment_epoch()?,
            state.current_params()?.params_hash(),
        );
        move || {
            let private_key = burst_types::PrivateKey(private_key_bytes);
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
                &private_key,
                &work_generator,
                min_work_difficulty,
                params_hash,
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

// Server-side signing is a test tool. Production wallets sign locally through
// set_verifier_opt_in_signed and submit the resulting block via process.

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
    require_faucet(state)?;
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

    let (spent_brn, signing_grant) = signing_predecessor(state, &account.head)?;
    let private_key_bytes = private_key.0;
    let release_sources = state
        .brn_engine
        .lock()
        .await
        .get_wallet(&address)
        .map_or_else(Vec::new, |wallet| {
            wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
        });
    let block = tokio::task::spawn_blocking({
        let address = address.clone();
        let representative = account.representative.clone();
        let previous = account.head;
        let trst_balance = account.trst_balance;
        let work_generator = state.work_generator.clone();
        let min_work_difficulty = state.current_params()?.min_work_difficulty;
        let (payment_epoch, params_hash) = (
            state.meta_store.payment_epoch()?,
            state.current_params()?.params_hash(),
        );
        move || {
            let private_key = burst_types::PrivateKey(private_key_bytes);
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
                &private_key,
                &work_generator,
                min_work_difficulty,
                params_hash,
                Vec::new(),
            )
        }
    })
    .await
    .map_err(|e| RpcError::Server(format!("block build task failed: {e}")))??;

    // The processor validates and persists the choice. Submission may still be
    // queued, so opted_in in this response is the requested state, not confirmation.
    submit_block(&block, state).await?;

    Ok(to_value(&VerifierOptResponse {
        block_hash: format!("{}", block.hash),
        account: address.to_string(),
        opted_in: opt_in,
    }))
}

/// Advisory review data; block admission checks authority in the ledger writer.
pub async fn handle_bootstrap_status(
    _params: serde_json::Value,
    state: &RpcState,
) -> Result<serde_json::Value, RpcError> {
    use burst_verification::bootstrap::{BootstrapPhase, MAX_BOOTSTRAP_HUMANS};
    let creator = state
        .block_processor
        .bootstrap_creator()
        .map_err(RpcError::Server)?;
    let bytes = state.brn_store.get_meta(BootstrapPhase::KEY)?;
    let phase = BootstrapPhase::decode(bytes.as_deref())
        .map_err(|error| RpcError::Server(error.to_string()))?;
    let count = state.account_store.verified_account_count()?;
    let threshold = state
        .current_params()?
        .bootstrap_exit_threshold
        .min(MAX_BOOTSTRAP_HUMANS);
    Ok(serde_json::json!({
        "creator": creator.as_str(), "verified_count": count, "threshold": threshold,
        "active": phase.permits_creator(count, threshold),
        "veto_available": phase.permits_creator(count, threshold)
            && burst_verification::checkpoint_selection_enabled(state.network),
    }))
}

mod accounts;
pub use accounts::*;

mod governance;
pub use governance::*;

pub use crate::contracts::node::*;
