//! Shared economic transition for live admission and branch replacement.

use burst_brn::BrnEngine;
use burst_ledger::{BlockType, StateBlock};
use burst_store::{account::AccountInfo, pending::PendingInfo};
use burst_trst::TrstEngine;
use burst_types::ProtocolParams;

use crate::block_economics::EconomicResult;
use crate::ledger_updater::{PendingWrite, TrstIndexWrite};

pub(crate) mod inputs;
pub(crate) mod prepared;
mod staging;
pub(crate) use staging::{Engines, StagedEngines};

pub(crate) struct EconomicContext<'a> {
    pub account: Option<&'a AccountInfo>,
    pub previous_brn_spent: u128,
    pub pending: Option<&'a PendingInfo>,
    pub params: &'a ProtocolParams,
}

pub(crate) struct StagedBlockEffects {
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
) -> Result<StagedBlockEffects, String> {
    if block.block_type == BlockType::Epoch {
        let phase = crate::bootstrap_phase::read(batch)?;
        let count = batch
            .verified_account_count()
            .map_err(|error| error.to_string())?;
        if !phase.permits_creator(count, params.bootstrap_exit_threshold) {
            return Err("creator epoch authority has ended".into());
        }
    }
    let inputs = inputs::LedgerInputs::capture(batch, block, params)?;
    let economics = apply_with_engines(block, inputs.context(params), engines)?;
    crate::ledger_updater::stage_block(
        batch,
        block,
        inputs.account.as_ref(),
        inputs.previous_brn_spent,
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
) -> Result<StagedBlockEffects, String> {
    apply_with_engines(block, context, &mut (brn, trst))
}

fn apply_with_engines(
    block: &StateBlock,
    context: EconomicContext<'_>,
    engines: &mut impl Engines,
) -> Result<StagedBlockEffects, String> {
    if let Some(pending) = context.pending {
        crate::block_economics::validate_pending(pending)?;
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
                    .brn_spent
                    .checked_sub(context.previous_brn_spent)
                    .ok_or("BRN spend decreased")?
            };
            crate::limits::check_wallet_spending_limit(
                account,
                amount,
                block.timestamp,
                context.params,
            )?;
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

    let effect = crate::block_economics::process_with_engines(
        block,
        engines,
        block.timestamp,
        context.params.trst_expiry_secs,
        context.previous_brn_spent,
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
            let destination = crate::block_economics::wallet_from_link(&block.link)
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
                return Ok(StagedBlockEffects {
                    effect,
                    pending: None,
                    indices: None,
                });
            }
            crate::block_economics::deliver_pending(
                engines.trst_mut(),
                source,
                crate::block_economics::create_received_token(
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
                portfolio.select_expiry_merge_group(block.timestamp, trst.expiry_secs)
            } else {
                portfolio.select_merge_inputs(&block.merge_sources)?
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
    Ok(StagedBlockEffects {
        effect,
        pending,
        indices,
    })
}
