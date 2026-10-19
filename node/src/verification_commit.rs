//! Candidate-only lifecycle effects; the caller commits them with their block.

use std::collections::{BTreeMap, HashSet};

use bincode::Options;
use burst_brn::BrnEngine;
use burst_ledger::{BlockType, StateBlock};
use burst_store::{
    account::AccountInfo,
    pending::{PendingInfo, PendingProvenance},
    StoreError,
};
#[cfg(test)]
use burst_store_lmdb::LmdbStore;
use burst_trst::TrstEngine;
use burst_types::{BlockHash, ProtocolParams, Timestamp, TxHash, WalletAddress, WalletState};
use burst_verification::{
    ChallengeResult, VerificationEvent, VerificationOrchestrator, VerificationResult,
    VerifierOutcome,
};
use serde::{Deserialize, Serialize};

use crate::ledger_bridge::EconomicResult;
use crate::ledger_updater::update_token_counters;

mod appeals;
mod deadline;
pub(crate) mod ledger;
mod outcome_context;
pub(crate) mod receipts;
pub(crate) use deadline::settle_panel_deadline_candidate;
pub(crate) mod dependencies;
use ledger::Ledger;

#[derive(Serialize, Deserialize)]
struct StakeReceipt {
    block: BlockHash,
    amount: u128,
    settled_by: Option<BlockHash>,
}

struct Settlement {
    network: burst_types::NetworkId,
    hash: BlockHash,
    target: WalletAddress,
    timestamp: Timestamp,
    sources: Vec<BlockHash>,
    authority: SettlementAuthority,
}

enum SettlementAuthority {
    SignedVotes,
    SignedVotesWithDefaults {
        absent: HashSet<WalletAddress>,
    },
    #[cfg(test)]
    UnprovenExpiry,
    CertifiedDeadline {
        absent: HashSet<WalletAddress>,
    },
}

impl SettlementAuthority {
    fn is_unproven_expiry(&self) -> bool {
        #[cfg(test)]
        {
            matches!(self, Self::UnprovenExpiry)
        }
        #[cfg(not(test))]
        {
            false
        }
    }
}

fn stake_key(
    target: &WalletAddress,
    staker: &WalletAddress,
    challenge: bool,
    attempt: BlockHash,
) -> String {
    format!(
        "verification_stake/v2/{}/{target}/{staker}/{attempt}",
        if challenge { "challenge" } else { "vote" }
    )
}

fn read_stake(batch: &impl Ledger, key: &str) -> Result<Option<StakeReceipt>, String> {
    batch
        .get_meta(key)
        .map_err(|e| e.to_string())?
        .map(|bytes| {
            bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .with_limit(128)
                .reject_trailing_bytes()
                .deserialize(&bytes)
                .map_err(|e| e.to_string())
        })
        .transpose()
}

fn record_stake(
    batch: &mut impl Ledger,
    block: &StateBlock,
    target: &WalletAddress,
    amount: u128,
) -> Result<(), String> {
    let attempt = match block.block_type {
        BlockType::Challenge => block.hash,
        BlockType::VerificationVote => BlockHash::new(*block.origin.as_bytes()),
        _ => return Err("unsupported verification stake source".into()),
    };
    if attempt.is_zero() {
        return Err("verification stake attempt is missing".into());
    }
    let key = stake_key(
        target,
        &block.account,
        block.block_type == BlockType::Challenge,
        attempt,
    );
    if read_stake(batch, &key)?.is_some() {
        return Err("verification stake attempt already exists".into());
    }
    let receipt = StakeReceipt {
        block: block.hash,
        amount,
        settled_by: None,
    };
    batch
        .put_meta(
            &key,
            &bincode::serialize(&receipt).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
}

pub(crate) fn collect(
    batch: &mut impl Ledger,
    block: &StateBlock,
    economic: &EconomicResult,
    params: &ProtocolParams,
    orchestrator: &mut VerificationOrchestrator,
    engines: (&mut BrnEngine, &mut TrstEngine),
    network: burst_types::NetworkId,
) -> Result<(), String> {
    apply_policy(
        batch,
        block,
        economic,
        params,
        orchestrator,
        engines,
        SettlementPolicy::Collect,
        network,
    )
}

enum SettlementPolicy {
    Collect,
    #[cfg(test)]
    UnorderedFixture,
}

/// Component fixtures exercise settlement arithmetic independently of agreement.
/// This entry point is absent from production; the live path only collects votes.
#[cfg(test)]
fn apply(
    batch: &mut impl Ledger,
    block: &StateBlock,
    economic: &EconomicResult,
    params: &ProtocolParams,
    orchestrator: &mut VerificationOrchestrator,
    engines: (&mut BrnEngine, &mut TrstEngine),
    network: burst_types::NetworkId,
) -> Result<(), String> {
    apply_policy(
        batch,
        block,
        economic,
        params,
        orchestrator,
        engines,
        SettlementPolicy::UnorderedFixture,
        network,
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_policy(
    batch: &mut impl Ledger,
    block: &StateBlock,
    economic: &EconomicResult,
    params: &ProtocolParams,
    orchestrator: &mut VerificationOrchestrator,
    engines: (&mut BrnEngine, &mut TrstEngine),
    policy: SettlementPolicy,
    network: burst_types::NetworkId,
) -> Result<(), String> {
    let target = crate::ledger_bridge::extract_receiver_from_link(&block.link)
        .ok_or("verification target is invalid")?;
    match economic {
        EconomicResult::Challenge { stake_amount, .. }
            if block.block_type == BlockType::Challenge =>
        {
            let appeal = if !block.target_head.is_zero() {
                let context = receipts::appeal_context(batch, block)?;
                if !cfg!(test) && !crate::release_policy::appeals(network) {
                    return Err("meta-challenge admission requires certified appeal support".into());
                }
                Some(context)
            } else {
                None
            };
            if appeal.is_none()
                && !batch
                    .get_account(&target)
                    .map_err(|e| e.to_string())?
                    .state
                    .is_verified()
            {
                return Err("challenge target is not verified".into());
            }
            let verified = batch
                .get_account(&block.account)
                .map_err(|e| e.to_string())?
                .state
                .is_verified();
            if let Some(appeal) = appeal {
                orchestrator
                    .initiate_signed_appeal_at(
                        &target,
                        &block.account,
                        verified,
                        *stake_amount,
                        (
                            burst_verification::ChallengeReason::from_origin(
                                block.origin.as_bytes(),
                            ),
                            block.timestamp,
                            block.hash,
                            appeal,
                        ),
                        params,
                    )
                    .map_err(|e| e.to_string())?;
            } else {
                orchestrator
                    .initiate_signed_challenge_at(
                        &target,
                        &block.account,
                        verified,
                        *stake_amount,
                        (
                            burst_verification::ChallengeReason::from_origin(
                                block.origin.as_bytes(),
                            ),
                            block.timestamp,
                            block.hash,
                        ),
                        params,
                    )
                    .map_err(|e| e.to_string())?;
            }
            record_stake(batch, block, &target, *stake_amount)?;
        }
        EconomicResult::VerificationVoteResult { vote, stake, .. }
            if block.block_type == BlockType::VerificationVote =>
        {
            let previous_brn = block
                .brn_balance
                .checked_sub(*stake)
                .ok_or("invalid vote stake delta")?;
            crate::verification_vote::validate(block, previous_brn, orchestrator, params)?;
            crate::verification_activation::validate_vote(batch, block)?;
            record_stake(batch, block, &target, *stake)?;
            let vote = match vote {
                1 => burst_verification::Vote::Legitimate,
                2 => burst_verification::Vote::Illegitimate,
                3 => burst_verification::Vote::Neither,
                _ => return Err("invalid verification vote".into()),
            };
            orchestrator
                .process_certified_vote_at(
                    &target,
                    &block.account,
                    vote,
                    BlockHash::new(*block.origin.as_bytes()),
                    params,
                    block.timestamp,
                )
                .map_err(|e| e.to_string())?;
            match policy {
                SettlementPolicy::Collect => {
                    orchestrator.collect_challenge_outcome(&target, params)
                }
                #[cfg(test)]
                SettlementPolicy::UnorderedFixture => {
                    orchestrator.try_resolve_challenge(&target, params)
                }
            }
            .map_err(|e| e.to_string())?;
        }
        _ => return Err("verification economic result missing".into()),
    }
    let mut account = batch
        .get_account(&block.account)
        .map_err(|e| e.to_string())?;
    account.total_brn_staked = engines
        .0
        .get_wallet(&block.account)
        .ok_or("verification BRN wallet missing")?
        .total_staked;
    batch.put_account(&account).map_err(|e| e.to_string())?;
    let events = orchestrator.drain_events();
    if let Some(context) = Settlement::from_events(batch, &target, &events, network)? {
        settle(batch, &context, events, params, engines)?;
        dependencies::persist(batch, block.hash, context.hash, &context.sources)?;
    } else if events
        .iter()
        .any(|event| !matches!(event, VerificationEvent::VerifierPenalized { .. }))
    {
        return Err("verification transition has no signed outcome".into());
    }
    persist(batch, orchestrator)
}

fn persist(batch: &mut impl Ledger, orchestrator: &VerificationOrchestrator) -> Result<(), String> {
    let bytes = orchestrator
        .snapshot()
        .encode()
        .map_err(|e| e.to_string())?;
    batch
        .put_meta(crate::endorsement_commit::SNAPSHOT_KEY, &bytes)
        .map_err(|e| e.to_string())
}

/// Evaluate an upheld outcome on an isolated, replay-derived cut. This does not
/// authorize installing the candidate in the live ledger or spending its refunds.
pub(crate) fn settle_upheld_candidate(
    batch: &mut impl Ledger,
    target: &WalletAddress,
    cutoff: Timestamp,
    params: &ProtocolParams,
    orchestrator: &mut VerificationOrchestrator,
    engines: (&mut BrnEngine, &mut TrstEngine),
    network: burst_types::NetworkId,
) -> Result<BlockHash, String> {
    let verdict = orchestrator.pending_challenge_verdict(target, params);
    if verdict.is_none()
        || (orchestrator.active_appeal(target).is_none()
            && verdict != Some(ChallengeResult::FraudConfirmed))
    {
        return Err("challenge has no complete upheld verdict".into());
    }
    orchestrator
        .resolve_challenge(target, params)
        .map_err(|error| error.to_string())?;
    let events = orchestrator.drain_events();
    let mut context = Settlement::from_events(batch, target, &events, network)?
        .ok_or("upheld challenge has no signed outcome")?;
    if context.timestamp > cutoff {
        return Err("outcome contains votes after its ledger cut".into());
    }
    context.sources.sort_unstable();
    let sources = bincode::serialize(&context.sources).map_err(|error| error.to_string())?;
    let commitment = BlockHash::new(burst_crypto::blake2b_256_multi(&[
        b"BURST/challenge-outcome/v1\0",
        target.as_str().as_bytes(),
        &cutoff.as_secs().to_be_bytes(),
        &sources,
    ]));
    // Rights and accrual stop at the agreed boundary, not at an earlier vote
    // timestamp that would retroactively invalidate included payments.
    context.timestamp = cutoff;
    settle(batch, &context, events, params, engines)?;
    dependencies::persist(batch, context.hash, context.hash, &context.sources)?;
    persist(batch, orchestrator)?;
    Ok(commitment)
}

/// Legacy half-stake expiry regression fixture, absent from ordinary builds.
/// Live deadlines are installed through authenticated checkpoint replay.
#[cfg(test)]
pub(crate) fn expire(
    store: &LmdbStore,
    now: Timestamp,
    params: &ProtocolParams,
    live: (
        &mut BrnEngine,
        &mut TrstEngine,
        &mut VerificationOrchestrator,
    ),
) -> Result<(), String> {
    let (live_brn, live_trst, live_orchestrator) = live;
    if !live_orchestrator.has_expired_challenges(now, params.challenge_duration_secs) {
        return Ok(());
    }
    let mut brn = live_brn.clone();
    let mut trst = live_trst.clone();
    let mut orchestrator = live_orchestrator.clone();
    let mut batch = store.tx_begin_write().map_err(|e| e.to_string())?;
    let settlements = expire_candidate(
        &mut batch,
        now,
        params,
        &mut orchestrator,
        (&mut brn, &mut trst),
    )?;
    if settlements.is_empty() {
        return Ok(());
    }
    crate::ledger_updater::persist_economics(&mut batch, &brn, &trst)?;
    batch.commit().map_err(|e| e.to_string())?;
    *live_brn = brn;
    *live_trst = trst;
    *live_orchestrator = orchestrator;
    Ok(())
}

/// Stage expiry on an isolated ledger/engine candidate. The returned identifiers
/// bind signed challenge attempts to their deadlines, not a caller's tick time.
/// They are not certificates: refunded credit remains unavailable to spending.
/// Discard all candidate state if any settlement fails.
#[cfg(test)]
pub(crate) fn expire_candidate(
    batch: &mut impl Ledger,
    now: Timestamp,
    params: &ProtocolParams,
    orchestrator: &mut VerificationOrchestrator,
    engines: (&mut BrnEngine, &mut TrstEngine),
) -> Result<Vec<BlockHash>, String> {
    let (brn, trst) = engines;
    let active = orchestrator.snapshot().active_challenges;
    let events = orchestrator
        .cleanup_expired_challenges(now, params.challenge_duration_secs)
        .map_err(|e| e.to_string())?;
    if events.is_empty() {
        return Ok(Vec::new());
    }
    let mut settlements = Vec::with_capacity(events.len());
    for event in events {
        let VerificationEvent::ChallengeResolved {
            ref wallet,
            ref outcome,
        } = event
        else {
            return Err("unexpected challenge expiry event".into());
        };
        if outcome.outcome != ChallengeResult::Expired
            || outcome.verifier_outcomes.iter().any(|v| v.trst_reward != 0)
        {
            return Err("challenge expiry cannot mint rewards".into());
        }
        let challenge = active
            .get(wallet)
            .ok_or("expired challenge history missing")?;
        let deadline = challenge
            .initiated_at
            .as_secs()
            .checked_add(params.challenge_duration_secs)
            .ok_or("challenge deadline overflow")?;
        let attempt = outcome
            .challenge_context
            .ok_or("expired challenge attempt is missing")?;
        let receipt = read_stake(
            batch,
            &stake_key(wallet, &outcome.challenger, true, attempt),
        )?
        .ok_or("expired challenge stake missing")?;
        if receipt.block != attempt {
            return Err("expired challenge stake source mismatch".into());
        }
        let hash = BlockHash::new(burst_crypto::blake2b_256_multi(&[
            b"BURST/challenge-expiry/v1\0",
            receipt.block.as_bytes(),
            &deadline.to_be_bytes(),
        ]));
        let context = Settlement {
            hash,
            target: wallet.clone(),
            network: burst_types::NetworkId::Dev,
            timestamp: Timestamp::new(deadline),
            sources: Vec::new(),
            authority: SettlementAuthority::UnprovenExpiry,
        };
        settle(batch, &context, vec![event], params, (brn, trst))?;
        settlements.push(hash);
    }
    persist(batch, orchestrator)?;
    settlements.sort_unstable();
    Ok(settlements)
}

fn settle_stake(
    batch: &mut impl Ledger,
    brn: &mut BrnEngine,
    context: &Settlement,
    staker: &WalletAddress,
    amounts: (u128, u128),
    challenge: bool,
    attempt: Option<BlockHash>,
) -> Result<(), String> {
    let (amount, penalty) = amounts;
    let attempt = attempt
        .filter(|hash| !hash.is_zero())
        .ok_or("settlement attempt is missing; signed history replay is required")?;
    let key = stake_key(&context.target, staker, challenge, attempt);
    let mut receipt = read_stake(batch, &key)?.ok_or("settlement has no matching stake receipt")?;
    if receipt.amount != amount
        || receipt.settled_by.is_some()
        || penalty > amount
        || (challenge && receipt.block != attempt)
    {
        return Err("settlement stake mismatch or duplicate".into());
    }
    if amount > 0 {
        let state = brn
            .get_wallet_mut(staker)
            .ok_or("settlement BRN wallet missing")?;
        let locked = state
            .total_staked
            .checked_sub(amount)
            .ok_or("settlement exceeds locked BRN")?;
        let burned = state
            .total_burned
            .checked_add(penalty)
            .ok_or("settlement BRN overflow")?;
        state.total_staked = locked;
        state.total_burned = burned;
        if amount > penalty {
            if context.sources.len() > burst_ledger::state_block::MAX_RELEASE_SOURCES {
                return Err("stake release exceeds signed proof capacity".into());
            }
            if context.authority.is_unproven_expiry() {
                // A local timer is not a signed deadline certificate. Keep its
                // returned credit unavailable to subsequent signed spending.
                state
                    .record_unproven_release(context.hash, amount - penalty, context.timestamp)
                    .map_err(|error| error.to_string())?;
            } else if matches!(
                context.authority,
                SettlementAuthority::SignedVotes
                    | SettlementAuthority::SignedVotesWithDefaults { .. }
            ) {
                state
                    .record_release(
                        context.hash,
                        amount - penalty,
                        &context.sources,
                        context.timestamp,
                    )
                    .map_err(|error| error.to_string())?;
            }
            // A certified deadline publishes this credit only with its new
            // payment epoch. It cannot create a release before installation or
            // make an unrelated legacy unproven release spendable.
        }
        let mut account = batch.get_account(staker).map_err(|e| e.to_string())?;
        account.total_brn_staked = locked;
        account.total_brn_burned = account
            .total_brn_burned
            .checked_add(penalty)
            .ok_or("account burn counter overflow")?;
        batch.put_account(&account).map_err(|e| e.to_string())?;
    }
    receipt.settled_by = Some(context.hash);
    batch
        .put_meta(
            &key,
            &bincode::serialize(&receipt).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
}

fn settle_verifiers(
    batch: &mut impl Ledger,
    brn: &mut BrnEngine,
    context: &Settlement,
    outcomes: &[VerifierOutcome],
    rewards: &mut BTreeMap<String, u128>,
) -> Result<(), String> {
    let mut seen = HashSet::new();
    let mut burned = 0u128;
    let mut minted = 0u128;
    for outcome in outcomes {
        if !seen.insert(&outcome.address)
            || outcome.penalty
                != if outcome.voted_correctly {
                    0
                } else {
                    outcome.staked
                }
            || (outcome.trst_reward > 0 && (!outcome.voted_correctly || outcome.staked == 0))
        {
            return Err("invalid verifier settlement".into());
        }
        burned = burned
            .checked_add(outcome.penalty)
            .ok_or("verifier penalty overflow")?;
        minted = minted
            .checked_add(outcome.trst_reward)
            .ok_or("verifier reward overflow")?;
    }
    if minted > burned {
        return Err("verifier rewards exceed burned stakes".into());
    }
    for outcome in outcomes {
        if let SettlementAuthority::CertifiedDeadline { absent }
        | SettlementAuthority::SignedVotesWithDefaults { absent } = &context.authority
        {
            if absent.contains(&outcome.address) {
                if outcome.selection_context.is_some()
                    || outcome.staked != 0
                    || outcome.penalty != 0
                    || outcome.trst_reward != 0
                {
                    return Err("defaulted verifier cannot have a signed stake or reward".into());
                }
                continue;
            }
        }
        settle_stake(
            batch,
            brn,
            context,
            &outcome.address,
            (outcome.staked, outcome.penalty),
            false,
            outcome.selection_context,
        )?;
        add_reward(rewards, &outcome.address, outcome.trst_reward)?;
    }
    Ok(())
}

fn add_reward(
    rewards: &mut BTreeMap<String, u128>,
    recipient: &WalletAddress,
    amount: u128,
) -> Result<(), String> {
    if amount > 0 {
        let total = rewards.entry(recipient.to_string()).or_default();
        *total = total.checked_add(amount).ok_or("reward amount overflow")?;
    }
    Ok(())
}

pub(crate) fn verify_wallet(
    batch: &mut impl Ledger,
    wallet: &WalletAddress,
    now: Timestamp,
    brn: &mut BrnEngine,
    trst: &mut TrstEngine,
) -> Result<(), String> {
    let mut account = match batch.get_account(wallet) {
        Ok(account) => account,
        Err(StoreError::NotFound(_)) => AccountInfo::unopened(wallet.clone()),
        Err(error) => return Err(error.to_string()),
    };
    if account.state == WalletState::Revoked {
        for restored in trst.un_revoke_by_origin(wallet, now) {
            let mut holder = if restored.holder == *wallet {
                account.clone()
            } else {
                batch
                    .get_account(&restored.holder)
                    .map_err(|e| e.to_string())?
            };
            update_token_counters(&mut holder, trst, now)?;
            batch.put_account(&holder).map_err(|e| e.to_string())?;
            if restored.holder == *wallet {
                account = holder;
            }
        }
        restore_pending(batch, trst)?;
    }
    verify_account(&mut account, now, brn)?;
    batch.put_account(&account).map_err(|e| e.to_string())
}

pub(crate) fn restore_pending(batch: &mut impl Ledger, trst: &TrstEngine) -> Result<(), String> {
    for (destination, hash, mut entry) in batch.all_pending().map_err(|e| e.to_string())? {
        if entry.return_credit {
            continue;
        }
        let mut changed = false;
        for provenance in &mut entry.provenance {
            for (origin, chunk) in &mut provenance.revoked_amounts {
                if !chunk.restored && !trst.merger_graph.is_origin_revoked(&TxHash::new(*origin)) {
                    chunk.restored = true;
                    changed = true;
                }
            }
        }
        if changed {
            batch
                .put_pending(&destination, &hash, &entry)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Revoked-origin restoration is performed by the caller before this transition.
pub(crate) fn verify_account(
    account: &mut AccountInfo,
    now: Timestamp,
    brn: &mut BrnEngine,
) -> Result<(), String> {
    let wallet = &account.address;
    if matches!(
        account.state,
        WalletState::Revoked | WalletState::Deactivated
    ) {
        brn.get_wallet_mut(wallet)
            .ok_or("frozen BRN history missing")?
            .resume_accrual(now);
    } else {
        // Reverification cannot erase unrelated outstanding stakes.
        let locked = brn.get_wallet(wallet).map_or(0, |state| state.total_staked);
        let mut state = burst_brn::BrnWalletState::new(now);
        state.total_staked = locked;
        brn.track_wallet(wallet.clone(), state);
    }
    account.state = WalletState::Verified;
    account.verified_at = Some(now);
    Ok(())
}

pub(crate) fn revoke_pending(
    batch: &mut impl Ledger,
    effects: &[burst_trst::engine::PendingRevocation],
    trst: &mut TrstEngine,
) -> Result<u128, String> {
    let mut total = 0u128;
    for (destination, hash, mut entry) in batch.all_pending().map_err(|e| e.to_string())? {
        if entry.return_credit {
            continue;
        }
        crate::ledger_bridge::validate_pending(&entry)?;
        let mut changed = false;
        for provenance in &mut entry.provenance {
            let split = provenance
                .revoked_amounts
                .values()
                .try_fold(0u128, |sum, chunk| sum.checked_add(chunk.amount))
                .ok_or("pending revocation overflow")?;
            let mut clean = provenance
                .amount
                .checked_sub(split)
                .ok_or("invalid pending split total")?;
            for effect in effects {
                if let Some(chunk) = provenance.revoked_amounts.get_mut(effect.origin.as_bytes()) {
                    if chunk.restored {
                        chunk.restored = false;
                        total = total
                            .checked_add(chunk.amount)
                            .ok_or("pending revocation total overflow")?;
                        changed = true;
                    }
                }
                let cut = effect.cut_basis(
                    &mut trst.merger_graph,
                    &provenance.origin,
                    &mut provenance.basis,
                    clean,
                )?;
                if cut == 0 {
                    continue;
                }
                if provenance
                    .revoked_amounts
                    .insert(
                        *effect.origin.as_bytes(),
                        burst_types::TrstRevocationChunk {
                            amount: cut,
                            restored: false,
                        },
                    )
                    .is_some()
                {
                    return Err("pending origin already split".into());
                }
                clean -= cut;
                total = total
                    .checked_add(cut)
                    .ok_or("pending revocation total overflow")?;
                changed = true;
            }
        }
        if changed {
            batch
                .put_pending(&destination, &hash, &entry)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(total)
}

fn settle(
    batch: &mut impl Ledger,
    context: &Settlement,
    events: Vec<VerificationEvent>,
    params: &ProtocolParams,
    engines: (&mut BrnEngine, &mut TrstEngine),
) -> Result<(), String> {
    let (brn, trst) = engines;
    let target = &context.target;
    if !cfg!(test)
        && !crate::release_policy::appeals(context.network)
        && events.iter().any(|event| {
            matches!(event,
        VerificationEvent::ChallengeResolved { outcome, .. } if outcome.appeal.is_some())
        })
    {
        return Err("meta-challenge settlement requires certified appeal support".into());
    }
    let mut receipt = receipts::prepare(batch, context, &events)?;
    let mut rewards = BTreeMap::new();
    let mut revoked = 0u128;
    for event in events {
        match event {
            VerificationEvent::VerificationComplete {
                wallet,
                result,
                outcomes,
            } => {
                if &wallet != target || outcomes.wallet != wallet || outcomes.result != result {
                    return Err("verification settlement target mismatch".into());
                }
                if result == VerificationResult::Verified {
                    verify_wallet(batch, &wallet, context.timestamp, brn, trst)?;
                    if !matches!(
                        context.authority,
                        SettlementAuthority::CertifiedDeadline { .. }
                    ) {
                        dependencies::record_identity(batch, &wallet, context.hash)?;
                    }
                }
                settle_verifiers(batch, brn, context, &outcomes.verifiers, &mut rewards)?;
            }
            VerificationEvent::WalletUnverified { wallet } => {
                if &wallet != target {
                    return Err("revocation target mismatch".into());
                }
                revoked = revoked
                    .checked_add(appeals::revoke_wallet(
                        batch,
                        &wallet,
                        context.timestamp,
                        brn,
                        trst,
                    )?)
                    .ok_or("revocation total overflow")?;
            }
            VerificationEvent::WalletDeactivated { wallet } => {
                if &wallet != target {
                    return Err("deactivation target mismatch".into());
                }
                brn.deactivate_wallet(&wallet, context.timestamp)
                    .map_err(|e| e.to_string())?;
                let mut account = batch.get_account(&wallet).map_err(|e| e.to_string())?;
                account.state = WalletState::Deactivated;
                batch.put_account(&account).map_err(|e| e.to_string())?;
            }
            VerificationEvent::ChallengeResolved { wallet, outcome } => {
                if &wallet != target || outcome.challenged_wallet != wallet {
                    return Err("challenge settlement target mismatch".into());
                }
                let mut reward_basis = revoked;
                let penalty = if let Some(appeal) = &outcome.appeal {
                    reward_basis =
                        appeals::settle_review(batch, context, &receipt, appeal, brn, trst)?;
                    let legitimate = outcome.outcome == ChallengeResult::ChallengeRejected;
                    if legitimate != appeal.previously_legitimate {
                        0
                    } else {
                        outcome.challenger_stake
                    }
                } else {
                    match outcome.outcome {
                        ChallengeResult::FraudConfirmed => 0,
                        ChallengeResult::ChallengeRejected => outcome.challenger_stake,
                        ChallengeResult::Expired => outcome.challenger_stake / 2,
                    }
                };
                settle_stake(
                    batch,
                    brn,
                    context,
                    &outcome.challenger,
                    (outcome.challenger_stake, penalty),
                    true,
                    outcome.challenge_context,
                )?;
                settle_verifiers(
                    batch,
                    brn,
                    context,
                    &outcome.verifier_outcomes,
                    &mut rewards,
                )?;
                if outcome.outcome == ChallengeResult::FraudConfirmed && reward_basis > 0 {
                    if params.challenge_reward_bps > 10_000 {
                        return Err("challenge reward exceeds revocation backing".into());
                    }
                    let bps = params.challenge_reward_bps as u128;
                    let reward = (reward_basis / 10_000 * bps
                        + reward_basis % 10_000 * bps / 10_000)
                        .min(params.challenge_reward_cap);
                    add_reward(&mut rewards, &outcome.challenger, reward)?;
                }
            }
            VerificationEvent::VerifierPenalized { .. } => {}
            _ => return Err("unexpected verification settlement event".into()),
        }
    }
    for (recipient, amount) in rewards {
        let recipient = WalletAddress::new(recipient);
        // The canonical signed stake source gives every node the same claim ID.
        // Cementation must still include the complete outcome's dependencies.
        match batch.get_pending(&recipient, &context.hash.into_tx_hash()) {
            Err(StoreError::NotFound(_)) => {}
            Err(error) => return Err(error.to_string()),
            Ok(_) => return Err("reward pending already exists".into()),
        }
        let origin = receipts::reward_origin(context.hash, &recipient);
        receipt.rewards.push(receipts::Reward {
            recipient: recipient.clone(),
            origin,
            amount,
        });
        trst.mint(
            origin,
            recipient.clone(),
            amount,
            recipient.clone(),
            context.timestamp,
        )
        .map_err(|e| e.to_string())?;
        let pending = PendingInfo {
            return_credit: false,
            source: recipient.clone(),
            amount,
            timestamp: context.timestamp,
            provenance: vec![PendingProvenance {
                basis: Default::default(),
                revoked_amounts: Default::default(),
                amount,
                origin,
                origin_wallet: recipient.clone(),
                origin_timestamp: context.timestamp,
                effective_origin_timestamp: context.timestamp,
            }],
        };
        batch
            .put_pending(&recipient, &context.hash.into_tx_hash(), &pending)
            .map_err(|e| e.to_string())?;
    }
    receipts::persist(batch, &receipt)
}

#[cfg(test)]
mod tests {
    mod appeals;
    mod composition;
    mod dependencies;
    mod order_tests;
    mod receipts;
    mod replay_backend;
    mod reverification;
    use super::*;
    use crate::verification_test_support::time as test_time;
    use burst_store::{
        account::AccountStore, block::BlockStore, meta::MetaStore, pending::PendingStore,
    };
    use burst_types::Signature;

    struct Fixture {
        directory: tempfile::TempDir,
        store: LmdbStore,
        brn: BrnEngine,
        trst: TrstEngine,
        orchestrator: VerificationOrchestrator,
        params: ProtocolParams,
        target: WalletAddress,
    }

    fn address(seed: u8) -> WalletAddress {
        burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[seed; 32]).public)
    }

    fn sign(block: &mut StateBlock, seed: u8) {
        block.hash = block.compute_hash();
        block.signature = burst_crypto::sign_message(
            block.hash.as_bytes(),
            &burst_crypto::keypair_from_seed(&[seed; 32]).private,
        );
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(directory.path(), 32, 32 * 1024 * 1024).unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            crate::payment_epoch::initialize(&mut batch, BlockHash::new([1; 32])).unwrap();
            batch.commit().unwrap();
            let params = ProtocolParams {
                min_work_difficulty: 0,
                endorsement_threshold: 1,
                endorsement_burn_amount: 10,
                num_verifiers: 3,
                verification_threshold_bps: 6000,
                verifier_stake_amount: 20,
                max_revotes: 1,
                challenge_stake_amount: 100,
                challenge_duration_secs: 30 * 100,
                trst_expiry_secs: 30 * 10_000,
                ..ProtocolParams::default()
            };
            let mut fixture = Self {
                directory,
                store,
                brn: BrnEngine::with_rate(1, test_time(0)),
                trst: TrstEngine::new(),
                orchestrator: VerificationOrchestrator::new(),
                params,
                target: address(10),
            };
            fixture.trst.expiry_secs = fixture.params.trst_expiry_secs;
            for seed in 0..=6 {
                fixture.add_account(seed);
            }
            fixture
                .orchestrator
                .process_endorsement_at(
                    &fixture.target,
                    &address(0),
                    10,
                    &fixture.params,
                    test_time(100),
                )
                .unwrap();
            fixture.select(&[1, 2, 3]);
            fixture
        }

        fn add_account(&mut self, seed: u8) {
            let mut block = StateBlock {
                signing_grant: burst_types::BlockHash::ZERO,
                delegation_payload: Vec::new(),
                version: burst_ledger::CURRENT_BLOCK_VERSION,
                payment_epoch: burst_types::BlockHash::new([1; 32]),
                block_type: BlockType::Open,
                account: address(seed),
                representative: address(seed),
                previous: BlockHash::ZERO,
                brn_balance: 0,
                trst_balance: 0,
                link: BlockHash::ZERO,
                origin: TxHash::ZERO,
                transaction: TxHash::ZERO,
                target_head: burst_types::BlockHash::ZERO,
                merge_sources: vec![],
                release_sources: Vec::new(),
                timestamp: test_time(0),
                params_hash: self.params.params_hash(),
                work: 0,
                signature: Signature([0; 64]),
                hash: BlockHash::ZERO,
            };
            sign(&mut block, seed);
            let account =
                crate::genesis_state::account_to_write(None, address(seed), block.hash).unwrap();
            let mut batch = self.store.tx_begin_write().unwrap();
            batch.put_account_info(&account, false).unwrap();
            batch
                .put_block_with_account(
                    &block.hash,
                    &bincode::serialize(&block).unwrap(),
                    &block.account,
                    1,
                )
                .unwrap();
            batch.put_frontier(&block.account, &block.hash).unwrap();
            self.brn
                .track_wallet(address(seed), burst_brn::BrnWalletState::new(test_time(0)));
            crate::ledger_updater::persist_economics(&mut batch, &self.brn, &self.trst).unwrap();
            batch.commit().unwrap();
        }

        fn select(&mut self, seeds: &[u8]) {
            let pool: Vec<_> = seeds.iter().map(|seed| address(*seed)).collect();
            crate::verification_test_support::select(
                &mut self.orchestrator,
                &self.target,
                &pool,
                &self.params,
            );
            self.orchestrator.drain_events();
        }

        fn vote(&self, seed: u8, vote: u8, time: u64) -> StateBlock {
            let account = self
                .store
                .account_store()
                .get_account(&address(seed))
                .unwrap();
            let previous: StateBlock =
                bincode::deserialize(&self.store.block_store().get_block(&account.head).unwrap())
                    .unwrap();
            let mut encoded = [0; 32];
            encoded[0] = vote;
            let mut block = StateBlock {
                block_type: BlockType::VerificationVote,
                previous: previous.hash,
                link: BlockHash::new(burst_crypto::decode_address(self.target.as_str()).unwrap()),
                brn_balance: previous.brn_balance
                    + if vote == 3 {
                        0
                    } else {
                        self.params.verifier_stake_amount
                    },
                transaction: TxHash::new(encoded),
                origin: self
                    .orchestrator
                    .selection_context(&self.target)
                    .unwrap()
                    .into_tx_hash(),
                timestamp: test_time(time),
                release_sources: self
                    .brn
                    .get_wallet(&address(seed))
                    .unwrap()
                    .release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES),
                ..previous
            };
            sign(&mut block, seed);
            block
        }

        fn apply(&mut self, block: &StateBlock, commit: bool) -> Result<(), String> {
            self.apply_with_policy(block, commit, false)
        }

        fn apply_with_policy(
            &mut self,
            block: &StateBlock,
            commit: bool,
            collect_only: bool,
        ) -> Result<(), String> {
            let mut brn = self.brn.clone();
            let mut trst = self.trst.clone();
            let mut orchestrator = self.orchestrator.clone();
            let mut batch = self.store.tx_begin_write().map_err(|e| e.to_string())?;
            let account = crate::ledger_updater::predecessor_account(&batch, block)?;
            let previous: StateBlock = bincode::deserialize(
                &batch
                    .get_block(&block.previous)
                    .map_err(|e| e.to_string())?,
            )
            .unwrap();
            let economics = crate::economic_candidate::apply(
                block,
                crate::economic_candidate::EconomicContext {
                    account: account.as_ref(),
                    previous_brn: previous.brn_balance,
                    pending: None,
                    params: &self.params,
                },
                &mut brn,
                &mut trst,
            )?;
            crate::ledger_updater::stage_block(
                &mut batch,
                block,
                account.as_ref(),
                previous.brn_balance,
                None,
                None,
            )?;
            let transition = if collect_only { collect } else { apply };
            transition(
                &mut batch,
                block,
                &economics.effect,
                &self.params,
                &mut orchestrator,
                (&mut brn, &mut trst),
                burst_types::NetworkId::Dev,
            )?;
            crate::ledger_updater::persist_economics(&mut batch, &brn, &trst)?;
            if commit {
                batch.commit().map_err(|e| e.to_string())?;
                self.brn = brn;
                self.trst = trst;
                self.orchestrator = orchestrator;
            }
            Ok(())
        }

        fn reopen(self) -> Self {
            let Self {
                directory,
                store,
                params,
                target,
                ..
            } = self;
            drop(store);
            let store = LmdbStore::open(directory.path(), 32, 32 * 1024 * 1024).unwrap();
            let batch = store.tx_begin_write().unwrap();
            let state = crate::payment_transition::restore(&batch, &params).unwrap();
            let orchestrator = VerificationOrchestrator::restore(
                burst_verification::OrchestratorSnapshot::decode(
                    &batch
                        .get_meta(crate::endorsement_commit::SNAPSHOT_KEY)
                        .unwrap()
                        .unwrap(),
                )
                .unwrap(),
            );
            drop(batch);
            Self {
                directory,
                store,
                brn: state.brn,
                trst: state.trst,
                orchestrator,
                params,
                target,
            }
        }

        fn challenge(&self, reason: burst_verification::ChallengeReason) -> StateBlock {
            let mut block = self.vote(0, 1, 1100);
            block.block_type = BlockType::Challenge;
            block.brn_balance = block.brn_balance - self.params.verifier_stake_amount
                + self.params.challenge_stake_amount;
            block.transaction = TxHash::ZERO;
            block.origin = TxHash::new(reason.to_origin());
            sign(&mut block, 0);
            block
        }

        fn payment(
            &mut self,
            seed: u8,
            kind: BlockType,
            amount: u128,
            source: (BlockHash, TxHash),
            time: u64,
        ) -> Result<BlockHash, String> {
            let account = self
                .store
                .account_store()
                .get_account(&address(seed))
                .unwrap();
            let previous = if account.head.is_zero() {
                None
            } else {
                Some(
                    bincode::deserialize::<StateBlock>(
                        &self.store.block_store().get_block(&account.head).unwrap(),
                    )
                    .unwrap(),
                )
            };
            let mut block = StateBlock {
                signing_grant: burst_types::BlockHash::ZERO,
                delegation_payload: Vec::new(),
                version: burst_ledger::CURRENT_BLOCK_VERSION,
                payment_epoch: burst_types::BlockHash::new([1; 32]),
                block_type: kind,
                account: address(seed),
                representative: address(seed),
                previous: account.head,
                brn_balance: previous.as_ref().map_or(0, |b| b.brn_balance),
                trst_balance: account.trst_balance,
                link: source.0,
                origin: source.1,
                transaction: TxHash::ZERO,
                target_head: burst_types::BlockHash::ZERO,
                merge_sources: vec![],
                release_sources: self
                    .brn
                    .get_wallet(&address(seed))
                    .map_or_else(Vec::new, |wallet| {
                        wallet.release_sources(burst_ledger::state_block::MAX_RELEASE_SOURCES)
                    }),
                timestamp: test_time(time),
                params_hash: self.params.params_hash(),
                work: 0,
                signature: Signature([0; 64]),
                hash: BlockHash::ZERO,
            };
            match block.block_type {
                BlockType::Burn => block.brn_balance += amount,
                BlockType::Receive => block.trst_balance += amount,
                BlockType::Send => block.trst_balance -= amount,
                _ => {}
            }
            sign(&mut block, seed);
            let mut batch = self.store.tx_begin_write().unwrap();
            let candidate =
                crate::payment_transition::apply(&mut batch, &block, &self.params, block.timestamp)
                    .map_err(|e| e.to_string())?;
            batch.commit().unwrap();
            self.brn = candidate.brn;
            self.trst = candidate.trst;
            Ok(block.hash)
        }

        fn verified_target(&mut self) {
            for seed in [1, 2, 3] {
                self.apply(&self.vote(seed, 1, 1000 + seed as u64), true)
                    .unwrap();
            }
        }

        fn seed_target_origin_at_holder(&mut self) {
            let token = self
                .trst
                .mint(
                    TxHash::new([91; 32]),
                    address(6),
                    10_000,
                    self.target.clone(),
                    test_time(1050),
                )
                .unwrap();
            self.trst.track_token(token);
            let mut holder = self.store.account_store().get_account(&address(6)).unwrap();
            holder.trst_balance = 10_000;
            self.store.account_store().put_account(&holder).unwrap();
        }
    }

    #[test]
    fn final_vote_settles_atomically_and_reward_has_a_claimable_causal_source() {
        let mut f = Fixture::new();
        f.apply(&f.vote(1, 1, 1000), true).unwrap();
        f.apply(&f.vote(2, 1, 1001), true).unwrap();
        let last = f.vote(3, 2, 1002);
        f.apply(&last, false).unwrap();
        assert!(f.store.block_store().get_block(&last.hash).is_err());
        assert!(f.store.account_store().get_account(&f.target).is_err());
        assert_eq!(f.store.pending_store().pending_count().unwrap(), 0);
        assert_eq!(f.brn.get_wallet(&address(1)).unwrap().total_staked, 20);
        f.apply(&last, true).unwrap();
        f = f.reopen();
        let account = f.store.account_store().get_account(&f.target).unwrap();
        assert_eq!(account.state, WalletState::Verified);
        assert_eq!(account.verified_at, Some(last.timestamp));
        for seed in [1, 2, 3] {
            assert_eq!(f.brn.get_wallet(&address(seed)).unwrap().total_staked, 0);
        }
        assert_eq!(f.brn.get_wallet(&address(3)).unwrap().total_burned, 20);
        for seed in [1, 2] {
            let pending = f
                .store
                .pending_store()
                .get_pending(&address(seed), &last.hash.into_tx_hash())
                .unwrap();
            assert_eq!(pending.amount, 10);
            assert_eq!(pending.provenance[0].origin_wallet, address(seed));
        }
        let previous: StateBlock = bincode::deserialize(
            &f.store
                .block_store()
                .get_block(
                    &f.store
                        .account_store()
                        .get_account(&address(1))
                        .unwrap()
                        .head,
                )
                .unwrap(),
        )
        .unwrap();
        let mut receive = StateBlock {
            block_type: BlockType::Receive,
            previous: previous.hash,
            link: last.hash,
            trst_balance: 10,
            timestamp: test_time(1003),
            transaction: TxHash::ZERO,
            origin: TxHash::ZERO,
            ..previous
        };
        sign(&mut receive, 1);
        let mut batch = f.store.tx_begin_write().unwrap();
        let state =
            crate::payment_transition::apply(&mut batch, &receive, &f.params, receive.timestamp)
                .unwrap();
        assert_eq!(
            state.trst.get_portfolio(&address(1)).unwrap().tokens[0].amount,
            10
        );
        batch.commit().unwrap();
        assert!(f
            .store
            .pending_store()
            .get_pending(&address(1), &last.hash.into_tx_hash())
            .is_err());
        assert!(f.apply(&last, true).is_err());
        assert_eq!(f.brn.get_wallet(&address(3)).unwrap().total_burned, 20);
    }

    #[test]
    fn stale_outcomes_cannot_settle_new_attempts_or_overwrite_spent_receipts() {
        let mut f = Fixture::new();
        let old_vote = f.vote(1, 1, 1000);
        let old_attempt = BlockHash::new(*old_vote.origin.as_bytes());
        f.apply(&old_vote, true).unwrap();
        f.apply(&f.vote(2, 1, 1001), true).unwrap();
        f.apply(&f.vote(3, 1, 1002), true).unwrap();
        f = f.reopen();
        f.apply(
            &f.challenge(burst_verification::ChallengeReason::Fraud),
            true,
        )
        .unwrap();
        f.select(&[1, 2, 3]);
        let new_vote = f.vote(1, 1, 1101);
        let new_attempt = BlockHash::new(*new_vote.origin.as_bytes());
        assert_ne!(old_attempt, new_attempt);
        f.apply(&new_vote, true).unwrap();
        f = f.reopen();
        let mut brn = f.brn.clone();
        let mut batch = f.store.tx_begin_write().unwrap();
        let context = Settlement {
            hash: new_vote.hash,
            network: burst_types::NetworkId::Dev,
            target: f.target.clone(),
            timestamp: test_time(1102),
            sources: vec![new_vote.hash],
            authority: SettlementAuthority::SignedVotes,
        };
        for attempt in [Some(old_attempt), None, Some(BlockHash::ZERO)] {
            assert!(settle_stake(
                &mut batch,
                &mut brn,
                &context,
                &address(1),
                (20, 0),
                false,
                attempt
            )
            .is_err());
            assert_eq!(brn.get_wallet(&address(1)).unwrap().total_staked, 20);
            let receipt = read_stake(
                &batch,
                &stake_key(&f.target, &address(1), false, new_attempt),
            )
            .unwrap()
            .unwrap();
            assert_eq!(receipt.settled_by, None);
        }
        assert!(record_stake(&mut batch, &old_vote, &f.target, 20)
            .unwrap_err()
            .contains("already exists"));
        let previous = read_stake(
            &batch,
            &stake_key(&f.target, &address(1), false, old_attempt),
        )
        .unwrap()
        .unwrap();
        assert!(previous.settled_by.is_some());
        settle_stake(
            &mut batch,
            &mut brn,
            &context,
            &address(1),
            (20, 0),
            false,
            Some(new_attempt),
        )
        .unwrap();
        assert_eq!(brn.get_wallet(&address(1)).unwrap().total_staked, 0);
        assert!(settle_stake(
            &mut batch,
            &mut brn,
            &context,
            &address(1),
            (20, 0),
            false,
            Some(new_attempt)
        )
        .is_err());
    }

    #[test]
    fn missing_receipt_cannot_partially_verify_burn_or_mint() {
        let mut f = Fixture::new();
        f.apply(&f.vote(1, 1, 1000), true).unwrap();
        f.apply(&f.vote(2, 1, 1001), true).unwrap();
        f.store
            .meta_store()
            .put_meta(
                &stake_key(
                    &f.target,
                    &address(2),
                    false,
                    f.orchestrator.selection_context(&f.target).unwrap(),
                ),
                &[255],
            )
            .unwrap();
        let last = f.vote(3, 2, 1002);
        assert!(f.apply(&last, true).is_err());
        assert!(f.store.block_store().get_block(&last.hash).is_err());
        assert!(f.store.account_store().get_account(&f.target).is_err());
        assert_eq!(f.store.pending_store().pending_count().unwrap(), 0);
        assert_eq!(f.brn.get_wallet(&address(1)).unwrap().total_staked, 20);
        assert_eq!(f.brn.get_wallet(&address(3)).unwrap().total_burned, 0);
        assert_eq!(f.orchestrator.get_state(&f.target).unwrap().votes.len(), 2);
    }

    #[test]
    fn revote_keeps_prior_stakes_across_restart_and_settles_them_once() {
        let mut f = Fixture::new();
        for seed in [1, 2, 3] {
            f.apply(&f.vote(seed, 2, 1000 + seed as u64), true).unwrap();
        }
        assert_eq!(f.orchestrator.get_state(&f.target).unwrap().revote_count, 1);
        f = f.reopen();
        assert_eq!(f.brn.get_wallet(&address(1)).unwrap().total_staked, 20);
        f.select(&[4, 5, 6]);
        for seed in [4, 5, 6] {
            f.apply(&f.vote(seed, 1, 1010 + seed as u64), true).unwrap();
        }
        for seed in [1, 2, 3] {
            assert_eq!(f.brn.get_wallet(&address(seed)).unwrap().total_staked, 0);
            assert_eq!(f.brn.get_wallet(&address(seed)).unwrap().total_burned, 20);
        }
        let pending = f.store.pending_store().get_all_pending().unwrap();
        assert_eq!(pending.len(), 3);
        assert_eq!(pending.iter().map(|(_, _, p)| p.amount).sum::<u128>(), 60);
    }

    #[test]
    fn challenge_panel_rejects_old_context_before_locking_a_new_stake() {
        let mut f = Fixture::new();
        let old_context = f.orchestrator.selection_context(&f.target).unwrap();
        f.verified_target();
        f.apply(
            &f.challenge(burst_verification::ChallengeReason::Fraud),
            true,
        )
        .unwrap();
        f.select(&[3, 4, 5]);
        assert_ne!(
            f.orchestrator.selection_context(&f.target),
            Some(old_context)
        );
        let mut replayed = f.vote(3, 1, 1101);
        replayed.origin = old_context.into_tx_hash();
        sign(&mut replayed, 3);
        let before = f.store.account_store().get_account(&address(3)).unwrap();
        assert!(f
            .apply(&replayed, true)
            .unwrap_err()
            .contains("wrong selection context"));
        assert_eq!(f.brn.get_wallet(&address(3)).unwrap().total_staked, 0);
        assert_eq!(
            f.store
                .account_store()
                .get_account(&address(3))
                .unwrap()
                .head,
            before.head
        );
        assert!(f
            .orchestrator
            .get_state(&f.target)
            .unwrap()
            .votes
            .is_empty());
        f.apply(&f.vote(3, 1, 1101), true).unwrap();
        f = f.reopen();
        assert_eq!(f.brn.get_wallet(&address(3)).unwrap().total_staked, 20);
        assert_ne!(
            f.orchestrator.selection_context(&f.target),
            Some(old_context)
        );
    }

    #[test]
    fn fraud_bounty_uses_downstream_revocation_and_inactivity_never_revokes() {
        for reason in [
            burst_verification::ChallengeReason::Fraud,
            burst_verification::ChallengeReason::Inactivity,
        ] {
            let mut f = Fixture::new();
            f.verified_target();
            f.seed_target_origin_at_holder();
            f.apply(&f.challenge(reason), true).unwrap();
            f.select(&[3, 4, 5]);
            f.apply(&f.vote(3, 2, 1101), true).unwrap();
            f.apply(&f.vote(4, 2, 1102), true).unwrap();
            let last = f.vote(5, 2, 1103);
            f.apply(&last, false).unwrap();
            assert_eq!(
                f.store
                    .account_store()
                    .get_account(&f.target)
                    .unwrap()
                    .state,
                WalletState::Verified
            );
            assert_eq!(
                f.store
                    .account_store()
                    .get_account(&address(6))
                    .unwrap()
                    .trst_balance,
                10_000
            );
            f.apply(&last, true).unwrap();
            f = f.reopen();
            let account = f.store.account_store().get_account(&f.target).unwrap();
            assert_eq!(account.trst_balance, 0);
            assert_eq!(f.brn.get_wallet(&address(0)).unwrap().total_staked, 0);
            assert_eq!(f.brn.get_wallet(&address(0)).unwrap().total_burned, 0);
            let holder = f.store.account_store().get_account(&address(6)).unwrap();
            if reason == burst_verification::ChallengeReason::Fraud {
                assert_eq!(account.state, WalletState::Revoked);
                assert_eq!(holder.revoked_trst, 10_000);
                assert_eq!(holder.trst_balance, 10_000);
                assert_eq!(
                    f.trst.transferable_balance(&address(6), test_time(1103)),
                    Some(0)
                );
                let reward = f
                    .store
                    .pending_store()
                    .get_pending(&address(0), &last.hash.into_tx_hash())
                    .unwrap();
                assert_eq!(reward.amount, 100);
                let mut brn = f.brn.clone();
                let mut trst = f.trst.clone();
                let mut batch = f.store.tx_begin_write().unwrap();
                verify_wallet(&mut batch, &f.target, test_time(1200), &mut brn, &mut trst).unwrap();
                assert_eq!(batch.get_account(&address(6)).unwrap().trst_balance, 10_000);
                assert_eq!(batch.get_account(&address(6)).unwrap().revoked_trst, 0);
                assert_eq!(
                    trst.transferable_balance(&address(6), test_time(1200)),
                    Some(10_000)
                );
                drop(batch);
                assert_eq!(
                    f.store
                        .account_store()
                        .get_account(&address(6))
                        .unwrap()
                        .revoked_trst,
                    10_000
                );
            } else {
                assert_eq!(account.state, WalletState::Deactivated);
                assert_eq!(holder.revoked_trst, 0);
                assert_eq!(holder.trst_balance, 10_000);
                assert_eq!(f.store.pending_store().pending_count().unwrap(), 0);
            }
        }
    }

    #[test]
    fn rejected_challenge_burns_stake_without_revoking_and_expiry_is_atomic() {
        let mut f = Fixture::new();
        f.verified_target();
        f.apply(
            &f.challenge(burst_verification::ChallengeReason::Fraud),
            true,
        )
        .unwrap();
        f.select(&[3, 4, 5]);
        for seed in [3, 4, 5] {
            f.apply(&f.vote(seed, 1, 1100 + seed as u64), true).unwrap();
        }
        assert_eq!(
            f.store
                .account_store()
                .get_account(&f.target)
                .unwrap()
                .state,
            WalletState::Verified
        );
        assert_eq!(f.brn.get_wallet(&address(0)).unwrap().total_burned, 100);
        assert_eq!(f.brn.get_wallet(&address(0)).unwrap().total_staked, 0);
        assert_eq!(f.store.pending_store().pending_count().unwrap(), 0);

        let mut f = Fixture::new();
        f.verified_target();
        f.apply(
            &f.challenge(burst_verification::ChallengeReason::Fraud),
            true,
        )
        .unwrap();
        f.select(&[3, 4, 5]);
        f.apply(&f.vote(3, 2, 1101), true).unwrap();
        expire(
            &f.store,
            test_time(1199),
            &f.params,
            (&mut f.brn, &mut f.trst, &mut f.orchestrator),
        )
        .unwrap();
        assert_eq!(f.brn.get_wallet(&address(0)).unwrap().total_staked, 100);
        let key = stake_key(
            &f.target,
            &address(3),
            false,
            f.orchestrator.selection_context(&f.target).unwrap(),
        );
        let saved = f.store.meta_store().get_meta(&key).unwrap();
        f.store.meta_store().put_meta(&key, &[255]).unwrap();
        assert!(expire(
            &f.store,
            test_time(1200),
            &f.params,
            (&mut f.brn, &mut f.trst, &mut f.orchestrator)
        )
        .is_err());
        assert_eq!(f.brn.get_wallet(&address(0)).unwrap().total_staked, 100);
        assert_eq!(f.brn.get_wallet(&address(0)).unwrap().total_burned, 0);
        assert_eq!(f.orchestrator.snapshot().active_challenges.len(), 1);
        f.store.meta_store().put_meta(&key, &saved).unwrap();
        expire(
            &f.store,
            test_time(1200),
            &f.params,
            (&mut f.brn, &mut f.trst, &mut f.orchestrator),
        )
        .unwrap();
        f = f.reopen();
        assert_eq!(f.brn.get_wallet(&address(0)).unwrap().total_staked, 0);
        assert_eq!(f.brn.get_wallet(&address(0)).unwrap().total_burned, 50);
        assert_eq!(f.brn.get_wallet(&address(3)).unwrap().total_staked, 0);
        expire(
            &f.store,
            test_time(1300),
            &f.params,
            (&mut f.brn, &mut f.trst, &mut f.orchestrator),
        )
        .unwrap();
        assert_eq!(f.brn.get_wallet(&address(0)).unwrap().total_burned, 50);
    }

    #[test]
    fn partial_revocation_preserves_signed_balances_and_only_clean_funds_can_move() {
        let mut f = Fixture::new();
        f.verified_target();
        let destination =
            |seed| BlockHash::new(burst_crypto::decode_address(address(seed).as_str()).unwrap());
        f.payment(
            10,
            BlockType::Open,
            0,
            (BlockHash::ZERO, TxHash::ZERO),
            1010,
        )
        .unwrap();
        let tainted = f
            .payment(
                10,
                BlockType::Burn,
                500,
                (destination(6), TxHash::ZERO),
                2000,
            )
            .unwrap();
        f.payment(6, BlockType::Receive, 500, (tainted, TxHash::ZERO), 2001)
            .unwrap();
        let clean = f
            .payment(
                0,
                BlockType::Burn,
                300,
                (destination(6), TxHash::ZERO),
                2002,
            )
            .unwrap();
        let head = f
            .payment(6, BlockType::Receive, 300, (clean, TxHash::ZERO), 2003)
            .unwrap();
        let mut challenge = f.challenge(burst_verification::ChallengeReason::Fraud);
        challenge.timestamp = test_time(2100);
        sign(&mut challenge, 0);
        f.apply(&challenge, true).unwrap();
        f.select(&[3, 4, 5]);
        for seed in [3, 4, 5] {
            f.apply(&f.vote(seed, 2, 2100 + seed as u64), true).unwrap();
        }
        let holder = f.store.account_store().get_account(&address(6)).unwrap();
        assert_eq!(holder.head, head);
        assert_eq!(holder.trst_balance, 800);
        assert_eq!(holder.revoked_trst, 500);
        assert_eq!(
            f.trst.transferable_balance(&address(6), test_time(2110)),
            Some(300)
        );
        f.payment(
            6,
            BlockType::Send,
            100,
            (destination(1), clean.into_tx_hash()),
            2110,
        )
        .unwrap();
        let before = f
            .store
            .account_store()
            .get_account(&address(6))
            .unwrap()
            .head;
        assert!(f
            .payment(
                6,
                BlockType::Send,
                500,
                (destination(1), tainted.into_tx_hash()),
                2111
            )
            .is_err());
        assert_eq!(
            f.store
                .account_store()
                .get_account(&address(6))
                .unwrap()
                .head,
            before
        );
        assert_eq!(
            f.trst.transferable_balance(&address(6), test_time(2111)),
            Some(200)
        );
    }

    #[test]
    fn expired_return_revocation_cannot_consume_an_unrelated_signed_balance() {
        let mut f = Fixture::new();
        f.verified_target();
        let token = f
            .trst
            .mint(
                TxHash::new([91; 32]),
                address(6),
                10_000,
                f.target.clone(),
                test_time(0),
            )
            .unwrap();
        f.trst.track_token(token);
        // A pending expiry return contributes reputation without a signed Receive.
        f.trst.flush_all_expired(test_time(10_000));
        let mut challenge = f.challenge(burst_verification::ChallengeReason::Fraud);
        challenge.timestamp = test_time(10_100);
        sign(&mut challenge, 0);
        f.apply(&challenge, true).unwrap();
        f.select(&[3, 4, 5]);
        for seed in [3, 4, 5] {
            f.apply(&f.vote(seed, 2, 10_100 + seed as u64), true)
                .unwrap();
        }
        let holder = f.store.account_store().get_account(&address(6)).unwrap();
        assert_eq!(holder.trst_balance, 0);
        assert_eq!(holder.expired_trst, 0);
        assert_eq!(holder.revoked_trst, 10_000);
    }

    #[test]
    fn pending_burn_revokes_after_restart_and_receive_keeps_it_unspendable() {
        let mut f = Fixture::new();
        f.verified_target();
        f.payment(
            10,
            BlockType::Open,
            0,
            (BlockHash::ZERO, TxHash::ZERO),
            1500,
        )
        .unwrap();
        let destination =
            BlockHash::new(burst_crypto::decode_address(address(6).as_str()).unwrap());
        let burn = f
            .payment(10, BlockType::Burn, 500, (destination, TxHash::ZERO), 2000)
            .unwrap();
        f = f.reopen();
        assert!(f.trst.wallet_origins[&f.target].contains(&burn.into_tx_hash()));
        assert!(f.trst.get_portfolio(&address(6)).is_none());
        let mut challenge = f.challenge(burst_verification::ChallengeReason::Fraud);
        challenge.timestamp = test_time(2100);
        sign(&mut challenge, 0);
        f.apply(&challenge, true).unwrap();
        f.select(&[3, 4, 5]);
        for seed in [3, 4] {
            f.apply(&f.vote(seed, 2, 2100 + seed as u64), true).unwrap();
        }
        let last = f.vote(5, 2, 2105);
        f.apply(&last, true).unwrap();
        assert_eq!(
            f.store
                .pending_store()
                .get_pending(&address(0), &last.hash.into_tx_hash())
                .unwrap()
                .amount,
            5
        );
        f = f.reopen();
        assert!(f.trst.merger_graph.is_origin_revoked(&burn.into_tx_hash()));
        assert!(f
            .store
            .pending_store()
            .get_pending(&address(6), &burn.into_tx_hash())
            .is_ok());
        f.payment(
            6,
            BlockType::Receive,
            500,
            (burn, burn.into_tx_hash()),
            2110,
        )
        .unwrap();
        f = f.reopen();
        let holder = f.store.account_store().get_account(&address(6)).unwrap();
        assert_eq!(holder.trst_balance, 500);
        assert_eq!(holder.revoked_trst, 500);
        assert_eq!(
            f.trst.transferable_balance(&address(6), test_time(2111)),
            Some(0)
        );
        let destination =
            BlockHash::new(burst_crypto::decode_address(address(1).as_str()).unwrap());
        assert!(f
            .payment(
                6,
                BlockType::Send,
                1,
                (destination, burn.into_tx_hash()),
                2111
            )
            .is_err());
        assert_eq!(
            f.store
                .pending_store()
                .get_pending(&address(0), &last.hash.into_tx_hash())
                .unwrap()
                .amount,
            5
        );

        let mut batch = f.store.tx_begin_write().unwrap();
        verify_wallet(
            &mut batch,
            &f.target,
            test_time(2200),
            &mut f.brn,
            &mut f.trst,
        )
        .unwrap();
        crate::ledger_updater::persist_economics(&mut batch, &f.brn, &f.trst).unwrap();
        batch.commit().unwrap();
        f = f.reopen();
        assert_eq!(
            f.store
                .account_store()
                .get_account(&address(6))
                .unwrap()
                .revoked_trst,
            0
        );
        assert_eq!(
            f.trst.transferable_balance(&address(6), test_time(2201)),
            Some(500)
        );
        f.payment(
            6,
            BlockType::Send,
            1,
            (destination, burn.into_tx_hash()),
            2201,
        )
        .unwrap();
    }

    #[test]
    fn pending_restoration_is_atomic_and_survives_restart_before_delivery() {
        let mut f = Fixture::new();
        f.verified_target();
        f.payment(
            10,
            BlockType::Open,
            0,
            (BlockHash::ZERO, TxHash::ZERO),
            1500,
        )
        .unwrap();
        let destination =
            BlockHash::new(burst_crypto::decode_address(address(6).as_str()).unwrap());
        let burn = f
            .payment(10, BlockType::Burn, 500, (destination, TxHash::ZERO), 2000)
            .unwrap();
        let mut challenge = f.challenge(burst_verification::ChallengeReason::Fraud);
        challenge.timestamp = test_time(2100);
        sign(&mut challenge, 0);
        f.apply(&challenge, true).unwrap();
        f.select(&[3, 4, 5]);
        for seed in [3, 4, 5] {
            f.apply(&f.vote(seed, 2, 2100 + seed as u64), true).unwrap();
        }
        f = f.reopen();
        let pending = f
            .store
            .pending_store()
            .get_pending(&address(6), &burn.into_tx_hash())
            .unwrap();
        assert_eq!(
            pending.provenance[0]
                .revoked_amounts
                .values()
                .filter(|chunk| !chunk.restored)
                .map(|chunk| chunk.amount)
                .sum::<u128>(),
            500
        );
        for commit in [false, true] {
            let mut brn = f.brn.clone();
            let mut trst = f.trst.clone();
            let mut batch = f.store.tx_begin_write().unwrap();
            verify_wallet(&mut batch, &f.target, test_time(2200), &mut brn, &mut trst).unwrap();
            crate::ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
            }
            let stored = f
                .store
                .pending_store()
                .get_pending(&address(6), &burn.into_tx_hash())
                .unwrap();
            if commit {
                assert!(stored.provenance[0]
                    .revoked_amounts
                    .values()
                    .all(|chunk| chunk.restored));
            } else {
                assert_eq!(
                    bincode::serialize(&stored).unwrap(),
                    bincode::serialize(&pending).unwrap()
                );
            }
        }
        f = f.reopen();
        f.payment(
            6,
            BlockType::Receive,
            500,
            (burn, burn.into_tx_hash()),
            2210,
        )
        .unwrap();
        f = f.reopen();
        assert_eq!(
            f.store
                .account_store()
                .get_account(&address(6))
                .unwrap()
                .revoked_trst,
            0
        );
        assert_eq!(
            f.trst.transferable_balance(&address(6), test_time(2211)),
            Some(500)
        );
    }
}
