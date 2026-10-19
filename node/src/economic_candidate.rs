//! Shared economic transition for live admission and branch replacement.

use burst_brn::BrnEngine;
use burst_ledger::{BlockType, StateBlock};
use burst_store::{account::AccountInfo, pending::PendingInfo};
use burst_trst::{TrstEngine, TrstToken, WalletPortfolio};
use burst_types::{ProtocolParams, Timestamp, TrstState};

use crate::ledger_bridge::EconomicResult;
use crate::ledger_updater::{PendingWrite, TrstIndexWrite};

mod staging;
pub(crate) use staging::{Engines, StagedEngines};

pub(crate) struct EconomicContext<'a> {
    pub account: Option<&'a AccountInfo>,
    pub previous_brn: u128,
    pub pending: Option<&'a PendingInfo>,
    pub params: &'a ProtocolParams,
}

pub(crate) struct CandidateEconomics {
    pub effect: EconomicResult,
    pub pending: Option<PendingWrite>,
    pub indices: Option<TrstIndexWrite>,
}

/// Stage one authenticated block against the writer's predecessor and pending
/// state. The caller owns lifecycle effects, engine persistence and commit.
pub(crate) fn stage(
    batch: &mut burst_store_lmdb::LmdbWriteTransaction<'_>,
    block: &StateBlock,
    params: &ProtocolParams,
    engines: &mut impl Engines,
) -> Result<CandidateEconomics, String> {
    use crate::{block_processor::BlockProcessor, ledger_updater};
    use bincode::Options;

    let account = ledger_updater::predecessor_account(batch, block)?;
    let previous: Option<StateBlock> = if block.previous.is_zero() {
        None
    } else {
        let bytes = batch
            .get_block(&block.previous)
            .map_err(|e| e.to_string())?;
        Some(
            bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .with_limit(burst_types::MAX_P2P_MESSAGE_BYTES as u64)
                .reject_trailing_bytes()
                .deserialize(&bytes)
                .map_err(|e| e.to_string())?,
        )
    };
    if previous.as_ref().is_some_and(|p| {
        p.account != block.account || p.hash != block.previous || p.hash != p.compute_hash()
    }) {
        return Err("candidate predecessor account mismatch".into());
    }
    if (block.block_type == BlockType::Open) != block.previous.is_zero() {
        return Err("invalid opening position".into());
    }
    block
        .validate_signing_grant(previous.as_ref())
        .map_err(str::to_owned)?;
    let previous_brn = previous.as_ref().map_or(0, |p| p.brn_balance);
    if let Some(previous) = &previous {
        BlockProcessor::validate_balance_transition(block, previous_brn, previous.trst_balance)?;
    }
    let claims = matches!(
        block.block_type,
        BlockType::Receive | BlockType::RejectReceive
    ) || (block.block_type == BlockType::Open && !block.link.is_zero());
    let pending = if claims {
        Some(
            batch
                .get_pending(&block.account, &block.link.into_tx_hash())
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    BlockProcessor::validate_open_block(block, pending.as_ref().map(|p| p.amount))?;
    let economics = apply_with_engines(
        block,
        EconomicContext {
            account: account.as_ref(),
            previous_brn,
            pending: pending.as_ref(),
            params,
        },
        engines,
    )?;
    ledger_updater::stage_block(
        batch,
        block,
        account.as_ref(),
        previous_brn,
        economics.pending.as_ref(),
        economics.indices.as_ref(),
    )?;
    Ok(economics)
}

pub(crate) fn requires_verified(kind: &BlockType) -> bool {
    matches!(
        kind,
        BlockType::Send
            | BlockType::Burn
            | BlockType::Merge
            | BlockType::Endorse
            | BlockType::Challenge
            | BlockType::VerifierOptIn
    )
}

/// Engines belong to the candidate. Discard them if this or any later write fails.
pub(crate) fn apply(
    block: &StateBlock,
    context: EconomicContext<'_>,
    brn: &mut BrnEngine,
    trst: &mut TrstEngine,
) -> Result<CandidateEconomics, String> {
    apply_with_engines(block, context, &mut (brn, trst))
}

fn apply_with_engines(
    block: &StateBlock,
    context: EconomicContext<'_>,
    engines: &mut impl Engines,
) -> Result<CandidateEconomics, String> {
    if let Some(pending) = context.pending {
        crate::ledger_bridge::validate_pending(pending)?;
        if pending.return_credit {
            if block.block_type != BlockType::Receive || pending.source != block.account {
                return Err("expiry return credit must be received by its owner".into());
            }
        } else if pending.provenance.iter().any(|part| {
            part.effective_origin_timestamp
                .has_expired(context.params.trst_expiry_secs, block.timestamp)
        }) {
            return Err("pending transfer has expired".into());
        }
    }
    if !block.release_sources.is_empty() {
        engines
            .brn_mut()
            .get_wallet_mut(&block.account)
            .ok_or("stake release has no BRN wallet")?
            .acknowledge_releases(&block.release_sources, block.timestamp)
            .map_err(|error| error.to_string())?;
    }
    let mut send_amount = None;
    if requires_verified(&block.block_type) {
        let account = context.account.ok_or("missing spending account")?;
        if !account.state.can_transact() {
            return Err("spending requires a verified wallet".into());
        }
        if matches!(block.block_type, BlockType::Send | BlockType::Burn) {
            let amount = if block.block_type == BlockType::Send {
                let amount = account
                    .trst_balance
                    .checked_sub(block.trst_balance)
                    .ok_or("send balance increased")?;
                send_amount = Some(amount);
                amount
            } else {
                block
                    .brn_balance
                    .checked_sub(context.previous_brn)
                    .ok_or("BRN spend decreased")?
            };
            crate::limits::check_wallet_limits(account, amount, block.timestamp, context.params)?;
        }
    }
    if let Some(amount) = send_amount {
        let (transferable, origin) = engines
            .trst()
            .candidate_balances(&block.account, &block.origin, block.timestamp)
            .ok_or("missing sender provenance")?;
        if amount == 0 || amount > transferable || amount > origin {
            return Err("insufficient transferable origin balance".into());
        }
        engines
            .trst_mut()
            .transferable_balance(&block.account, block.timestamp);
    }

    let effect = crate::ledger_bridge::process_with_engines(
        block,
        engines,
        block.timestamp,
        context.params.trst_expiry_secs,
        context.previous_brn,
    );
    let mut pending = None;
    let mut indices = None;
    match &effect {
        EconomicResult::Rejected { reason } => return Err(reason.clone()),
        EconomicResult::BurnAndMint {
            mint_token: Some(token),
            ..
        } => {
            indices = Some((
                token.origin,
                token.id,
                token.earliest_expiry(context.params.trst_expiry_secs),
            ));
            pending = Some((
                token.amount,
                token.holder.clone(),
                vec![burst_trst::ConsumedProvenance {
                    basis: token.basis,
                    amount: token.amount,
                    origin: token.origin,
                    origin_wallet: token.origin_wallet.clone(),
                    origin_timestamp: token.origin_timestamp,
                    effective_origin_timestamp: token.effective_origin_timestamp,
                }],
            ));
        }
        EconomicResult::Send { .. } => {
            let amount = send_amount.ok_or("missing send amount")?;
            let destination = crate::ledger_bridge::extract_receiver_from_link(&block.link)
                .ok_or("invalid send destination")?;
            let consumed = engines.trst_mut().debit_wallet_with_provenance(
                &block.account,
                &block.origin,
                amount,
            );
            if consumed
                .iter()
                .try_fold(0u128, |sum, p| sum.checked_add(p.amount))
                != Some(amount)
            {
                return Err("incomplete send provenance".into());
            }
            pending = Some((amount, destination, consumed));
        }
        EconomicResult::Receive { .. } => {
            let source = context.pending.ok_or("missing pending transfer")?;
            if source.return_credit {
                // The certified return already delivered these tokens. Only
                // the signed balance and receipt consumption change here.
                return Ok(CandidateEconomics {
                    effect,
                    pending: None,
                    indices: None,
                });
            }
            crate::ledger_bridge::deliver_pending(
                engines.trst_mut(),
                source,
                crate::ledger_bridge::create_received_token(
                    block,
                    source,
                    context.params.trst_expiry_secs,
                ),
                block.timestamp,
            )?;
        }
        EconomicResult::Merge { account } => {
            let trst = engines.trst();
            let portfolio = trst
                .get_portfolio(account)
                .ok_or("missing merge portfolio")?;
            let selected = if block.merge_sources.is_empty() {
                select_expiry_merge_group(portfolio, block.timestamp, trst.expiry_secs)
            } else {
                block
                    .merge_sources
                    .iter()
                    .map(|id| {
                        portfolio
                            .tokens
                            .iter()
                            .find(|token| token.id == *id)
                            .cloned()
                            .ok_or_else(|| format!("merge input {id} is not held by this wallet"))
                    })
                    .collect::<Result<Vec<_>, _>>()?
            };
            let trst = engines.trst_mut();
            let merged = trst
                .merge(
                    &selected,
                    account.clone(),
                    block.hash.into_tx_hash(),
                    block.timestamp,
                )
                .map_err(|e| e.to_string())?;
            let consumed = selected.iter().map(|token| token.id).collect();
            trst.bulk_untrack(account, &consumed);
            trst.track_token(merged);
        }
        _ => {}
    }
    Ok(CandidateEconomics {
        effect,
        pending,
        indices,
    })
}

// Preserve the existing automatic grouping for blocks without explicit inputs.
// Explicit inputs also permit expired-only consolidation (decision 6.8b).
fn select_expiry_merge_group(
    portfolio: &WalletPortfolio,
    now: Timestamp,
    expiry_secs: u64,
) -> Vec<TrstToken> {
    let mut live: Vec<_> = portfolio
        .tokens
        .iter()
        .filter(|token| {
            token.state == TrstState::Active
                && token.revoked_origin.is_none()
                && !token.is_expired(now, expiry_secs)
        })
        .collect();
    live.sort_by_key(|token| (token.effective_origin_timestamp, *token.id.as_bytes()));
    let Some(oldest) = live.first() else {
        return Vec::new();
    };
    let cutoff = oldest
        .effective_origin_timestamp
        .as_secs()
        .saturating_add(expiry_secs / 10);
    live.into_iter()
        .take_while(|token| token.effective_origin_timestamp.as_secs() <= cutoff)
        .take(burst_trst::MAX_MERGE_SOURCES)
        .cloned()
        .collect()
}
