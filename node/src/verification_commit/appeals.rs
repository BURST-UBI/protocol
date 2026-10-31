use super::*;
use receipts::{ChallengeVerdict, Receipt, Verdict};

fn legitimate(receipt: &Receipt) -> Result<bool, String> {
    match receipt.challenge_judgment().map(|(_, _, result)| result) {
        Some(ChallengeVerdict::Rejected) => Ok(true),
        Some(ChallengeVerdict::Upheld) => Ok(false),
        _ => Err("appeal history contains no reviewable judgment".into()),
    }
}

fn history(batch: &impl Ledger, current: &Receipt) -> Result<Vec<Receipt>, String> {
    let Verdict::Appeal {
        reviewed, reason, ..
    } = &current.verdict
    else {
        return Err("appeal settlement lacks a reviewed outcome".into());
    };
    let mut next = *reviewed;
    let mut seen = HashSet::from([current.source]);
    let mut previous_time = current.completed_at;
    let mut total_bytes = 0usize;
    let mut values = Vec::new();
    loop {
        if !seen.insert(next) || values.len() >= crate::checkpoint_store::MAX_CHECKPOINTS as usize {
            return Err("appeal history is cyclic or exceeds replay bounds".into());
        }
        let bytes = batch
            .get_meta(&receipts::key(next))
            .map_err(|error| error.to_string())?
            .ok_or("appeal history receipt is missing")?;
        total_bytes = total_bytes
            .checked_add(bytes.len())
            .ok_or("appeal history length overflow")?;
        if total_bytes as u64 > crate::checkpoint_store::MAX_RECORD_BYTES {
            return Err("appeal history requires incremental replay".into());
        }
        let value = receipts::decode(&bytes)?;
        if value.source != next
            || value.target != current.target
            || value.completed_at > previous_time
            || value
                .challenge_judgment()
                .is_none_or(|(_, past_reason, _)| past_reason != *reason)
        {
            return Err("appeal history differs from the reviewed judgment".into());
        }
        legitimate(&value)?;
        previous_time = value.completed_at;
        let parent = match &value.verdict {
            Verdict::Appeal { reviewed, .. } => Some(*reviewed),
            Verdict::Challenge { .. } => None,
            _ => unreachable!("judgment checked above"),
        };
        values.push(value);
        if let Some(parent) = parent {
            next = parent;
        } else {
            return Ok(values);
        }
    }
}

/// Correct only the reward origins recorded for this judgment chain. The root
/// is a stable invalidation cause, so failed and repeated appeals cannot hide
/// an earlier wrong reward or mint a second copy of a restored reward.
pub(super) fn settle_review(
    batch: &mut impl Ledger,
    context: &Settlement,
    current: &Receipt,
    appeal: &burst_verification::AppealContext,
    brn: &mut BrnEngine,
    trst: &mut TrstEngine,
) -> Result<u128, String> {
    let records = history(batch, current)?;
    let prior = records.first().ok_or("appeal history is empty")?;
    if prior.source != appeal.reviewed_outcome || legitimate(prior)? != appeal.previously_legitimate
    {
        return Err("appeal panel reviewed another judgment".into());
    }
    let root = records.last().ok_or("appeal history has no root")?.source;
    let new_legitimate = legitimate(current)?;
    let first_fraud_verdict = records.iter().all(|record| legitimate(record) == Ok(true));
    let mut bounty_basis = 0;
    let mut affected_holders = HashSet::new();
    for record in &records {
        let origins: Vec<_> = record
            .rewards
            .iter()
            .map(|reward| (reward.recipient.clone(), reward.origin))
            .collect();
        if origins.is_empty() {
            continue;
        }
        if legitimate(record)? == new_legitimate {
            let restored = trst
                .restore_outcome_origins(root, &origins, context.timestamp)
                .map_err(|error| error.to_string())?;
            affected_holders.extend(restored.into_iter().map(|event| event.holder));
        } else {
            let (held, pending) = trst
                .invalidate_outcome_origins(root, &origins)
                .map_err(|error| error.to_string())?;
            affected_holders.extend(held.into_iter().map(|event| event.holder));
            revoke_pending(batch, &pending, trst)?;
        }
    }
    // No receipt or pending transition reads account counters. Publish each
    // affected holder's final cut once, after all reward-origin changes.
    for holder in affected_holders {
        let mut account = batch.get_account(&holder).map_err(|error| error.to_string())?;
        update_token_counters(&mut account, trst, context.timestamp)?;
        batch.put_account(&account).map_err(|error| error.to_string())?;
    }
    restore_pending(batch, trst)?;
    if new_legitimate != appeal.previously_legitimate {
        if new_legitimate {
            mark_wallet_verified(batch, &context.target, context.timestamp, brn, trst)?;
            if !matches!(
                context.authority,
                SettlementAuthority::CertifiedDeadline { .. }
            ) {
                dependencies::record_identity(batch, &context.target, context.hash)?;
            }
        } else {
            let (_, reason, _) = current
                .challenge_judgment()
                .ok_or("appeal reason is missing")?;
            match reason {
                burst_types::ChallengeReason::Fraud => {
                    let revoked =
                        revoke_wallet(batch, &context.target, context.timestamp, brn, trst)?;
                    if first_fraud_verdict {
                        bounty_basis = revoked;
                    }
                }
                burst_types::ChallengeReason::Inactivity => {
                    brn.deactivate_wallet(&context.target, context.timestamp)
                        .map_err(|error| error.to_string())?;
                    let mut account = batch
                        .get_account(&context.target)
                        .map_err(|error| error.to_string())?;
                    account.state = WalletState::Deactivated;
                    batch
                        .put_account(&account)
                        .map_err(|error| error.to_string())?;
                }
            }
        }
    }
    Ok(bounty_basis)
}

pub(super) fn revoke_wallet(
    batch: &mut impl Ledger,
    wallet: &WalletAddress,
    now: Timestamp,
    brn: &mut BrnEngine,
    trst: &mut TrstEngine,
) -> Result<u128, String> {
    let (held, pending) = trst.revoke_with_pending_effects(wallet);
    let mut revoked = 0u128;
    let mut refreshed = HashSet::new();
    for event in &held {
        revoked = revoked
            .checked_add(event.revoked_amount)
            .ok_or("revocation total overflow")?;
        if !refreshed.insert(&event.holder) {
            continue;
        }
        let mut holder = batch
            .get_account(&event.holder)
            .map_err(|error| error.to_string())?;
        update_token_counters(&mut holder, trst, now)?;
        batch
            .put_account(&holder)
            .map_err(|error| error.to_string())?;
    }
    revoked = revoked
        .checked_add(revoke_pending(batch, &pending, trst)?)
        .ok_or("revocation total overflow")?;
    brn.deactivate_wallet(wallet, now)
        .map_err(|error| error.to_string())?;
    let mut account = batch
        .get_account(wallet)
        .map_err(|error| error.to_string())?;
    account.state = WalletState::Revoked;
    batch
        .put_account(&account)
        .map_err(|error| error.to_string())?;
    Ok(revoked)
}
