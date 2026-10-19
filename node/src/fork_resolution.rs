//! Transactional fork installation by the block-processing task.
use crate::{
    block_processor::BlockProcessor, bounded_backlog::BoundedBacklog, ledger_cache::LedgerCache,
    local_broadcaster::LocalBroadcaster, payment_transition, rollback_plan,
};
use bincode::Options;
use burst_consensus::{ActiveElections, PriorityScheduler, RepWeightCache};
use burst_ledger::{DagFrontier, StateBlock};
use burst_store::StoreError;
use burst_store_lmdb::{LmdbStore, LmdbWriteTransaction};
use burst_types::{BlockHash, NetworkId, ProtocolParams, Timestamp};
use burst_verification::VerificationOrchestrator;
use std::{collections::HashSet, sync::Arc};
use tokio::sync::{Mutex, RwLock};

#[cfg(test)]
mod tests;

fn invalid(message: &str) -> StoreError {
    StoreError::Corruption(message.into())
}

/// Payload cleanup never erases final-vote reservations or ordinary history.
/// The bounded cache is read before taking the writer; entries are immutable.
pub(crate) fn prune_finalized(store: &LmdbStore) -> Result<usize, StoreError> {
    let candidates = store.meta_store().fork_candidates()?;
    if candidates.is_empty() {
        return Ok(0);
    }
    let mut batch = store.tx_begin_write()?;
    let mut removed = 0;
    for (hash, bytes) in candidates {
        let block: StateBlock = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(burst_types::MAX_P2P_MESSAGE_BYTES as u64)
            .reject_trailing_bytes()
            .deserialize(&bytes)
            .map_err(|error| invalid(&error.to_string()))?;
        if block.hash != hash || block.compute_hash() != hash || block.validate_format().is_err() {
            return Err(invalid("cached fork identity or format mismatch"));
        }
        let key = burst_crypto::decode_address(block.account.as_str())
            .ok_or_else(|| invalid("cached fork signer is invalid"))?;
        if !burst_crypto::verify_signature(
            hash.as_bytes(),
            &block.signature,
            &burst_types::PublicKey(key),
        ) {
            return Err(invalid("cached fork signature is invalid"));
        }
        removed += usize::from(batch.prune_finalized_fork_candidate(
            &hash,
            &block.account,
            &block.previous,
        )?);
    }
    if removed > 0 {
        batch.commit()?;
    }
    Ok(removed)
}

pub(crate) fn competitor(
    batch: &LmdbWriteTransaction<'_>,
    block: &StateBlock,
) -> Result<Option<BlockHash>, StoreError> {
    let account = match batch.get_account(&block.account) {
        Ok(account) => account,
        Err(StoreError::NotFound(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    if account.block_count == 0 || account.head == block.previous {
        return Ok(None);
    }
    let height = if block.previous.is_zero() {
        1
    } else {
        batch
            .block_height(&block.account, &block.previous)?
            .checked_add(1)
            .ok_or_else(|| invalid("fork height overflow"))?
    };
    let target = batch
        .block_at_height(&block.account, height)?
        .ok_or_else(|| invalid("fork incumbent missing"))?;
    if target == block.hash {
        return Ok(None);
    }
    Ok(Some(target))
}

fn supports_governance(block: &StateBlock, network: NetworkId) -> bool {
    crate::governance_commit::CandidateGovernance::applies_to(&block.block_type)
        && block.block_type != burst_ledger::BlockType::GovernanceActivation
        && (!crate::governance_content::is_full(block)
            || crate::governance_content::enabled(network))
        && (block.delegation_payload.is_empty()
            || cfg!(test)
            || crate::release_policy::delegation(network))
}

fn require_supported_branch(
    removed: &[StateBlock],
    verification: bool,
    governance: bool,
    network: NetworkId,
) -> Result<(), StoreError> {
    if removed.iter().any(|block| {
        !(payment_transition::supports(&block.block_type)
            || (verification && crate::verification_transition::supports(&block.block_type))
            || (governance && supports_governance(block, network)))
    }) {
        return Err(invalid("fork contains unsupported lifecycle changes"));
    }
    Ok(())
}

struct CandidateState {
    payment: payment_transition::PaymentState,
    verification: Option<VerificationOrchestrator>,
    governance: Option<crate::governance_commit::CandidateGovernance>,
}

#[derive(Clone, Copy)]
pub(crate) struct GovernanceInput<'a> {
    pub genesis: BlockHash,
    pub content: Option<&'a [u8]>,
}

fn apply_candidate(
    batch: &mut LmdbWriteTransaction<'_>,
    block: &StateBlock,
    params: &ProtocolParams,
    network: NetworkId,
    verification_enabled: bool,
    admission_time: Option<Timestamp>,
    governance_input: Option<GovernanceInput<'_>>,
) -> Result<CandidateState, StoreError> {
    if governance_input.is_some() && !verification_enabled {
        return Err(invalid(
            "governance fork execution requires verification state",
        ));
    }
    if !verification_enabled {
        let payment = match admission_time {
            Some(now) => payment_transition::apply(batch, block, params, now)?,
            None => payment_transition::apply_elected(batch, block, params)?,
        };
        return Ok(CandidateState {
            payment,
            verification: None,
            governance: None,
        });
    }
    if governance_input.is_some() && supports_governance(block, network) {
        payment_transition::authenticate_signed_contents(block, params)?;
    } else {
        payment_transition::authenticate_contents(block, params)?;
    }
    if let Some(now) = admission_time {
        payment_transition::require_admission_time(block, now)?;
    }
    let mut payment = payment_transition::restore(batch, params)?;
    let mut verification =
        crate::endorsement_commit::restore(batch).map_err(StoreError::Corruption)?;
    let mut governance = governance_input
        .map(|input| {
            crate::governance_commit::CandidateGovernance::restore(
                batch,
                params.clone(),
                network,
                input.genesis,
            )
            .map_err(StoreError::Corruption)
        })
        .transpose()?;
    let effects = crate::economic_candidate::stage(
        batch,
        block,
        params,
        &mut (&mut payment.brn, &mut payment.trst),
    )
    .map_err(StoreError::Corruption)?;
    if crate::governance_content::is_full(block) {
        let content = governance_input
            .and_then(|input| input.content)
            .ok_or_else(|| invalid("full proposal fork content is missing"))?;
        crate::governance_content::stage(batch, block, content).map_err(StoreError::Corruption)?;
    }
    if crate::verification_transition::supports(&block.block_type) {
        crate::verification_transition::apply(
            batch,
            block,
            &effects.effect,
            params,
            network,
            &mut verification,
            (&mut payment.brn, &mut payment.trst),
        )
        .map_err(StoreError::Corruption)?;
    }
    if supports_governance(block, network) {
        governance
            .as_mut()
            .ok_or_else(|| invalid("governance fork execution is unavailable"))?
            .apply(
                batch,
                block,
                &effects.effect,
                &mut payment.brn,
                &mut payment.trst,
            )
            .map_err(StoreError::Corruption)?;
    }
    crate::bootstrap_phase::observe(batch, params).map_err(StoreError::Corruption)?;
    crate::ledger_updater::refresh_token_counters(
        batch,
        &block.account,
        &payment.trst,
        block.timestamp,
    )
    .map_err(StoreError::Corruption)?;
    crate::ledger_updater::persist_economics(batch, &payment.brn, &payment.trst)
        .map_err(StoreError::Corruption)?;
    Ok(CandidateState {
        payment,
        verification: Some(verification),
        governance,
    })
}

/// All candidate writes, including undo, are discarded. Cache only after this
/// succeeds. The caller serializes with the live economic engines.
pub(crate) fn evaluate(
    store: &LmdbStore,
    block: &StateBlock,
    params: &ProtocolParams,
    now: Timestamp,
    network: NetworkId,
    verification: bool,
    governance_input: Option<GovernanceInput<'_>>,
) -> Result<Option<BlockHash>, StoreError> {
    let mut batch = store.tx_begin_write()?;
    let Some(target) = competitor(&batch, block)? else {
        return Ok(None);
    };
    let (candidate, _) = rollback_plan::stage_with(&mut batch, target, block, |batch, block| {
        apply_candidate(
            batch,
            block,
            params,
            network,
            verification,
            Some(now),
            governance_input,
        )
    })?;
    require_supported_branch(
        &candidate.removed,
        verification,
        governance_input.is_some(),
        network,
    )?;
    Ok(Some(target))
}

#[derive(Clone)]
pub(crate) struct GovernanceRuntime {
    pub genesis: BlockHash,
    pub governance: Arc<Mutex<burst_governance::GovernanceEngine>>,
    pub delegation: Arc<Mutex<burst_governance::DelegationEngine>>,
    pub constitution: Arc<Mutex<burst_consti::ConstiEngine>>,
}

#[derive(Clone)]
pub(crate) struct Runtime {
    pub store: Arc<LmdbStore>,
    pub brn: Arc<Mutex<burst_brn::BrnEngine>>,
    pub trst: Arc<Mutex<burst_trst::TrstEngine>>,
    pub processor: Arc<Mutex<BlockProcessor>>,
    pub frontier: Arc<RwLock<DagFrontier>>,
    pub weights: Arc<RwLock<RepWeightCache>>,
    pub scheduler: Arc<Mutex<PriorityScheduler>>,
    pub elections: Arc<RwLock<ActiveElections>>,
    pub backlog: Arc<Mutex<BoundedBacklog>>,
    pub broadcaster: Arc<Mutex<LocalBroadcaster>>,
    pub counts: Arc<LedgerCache>,
}

impl Runtime {
    pub async fn install(
        &self,
        hash: BlockHash,
        params: &ProtocolParams,
        network: NetworkId,
        verification: Option<&Arc<Mutex<VerificationOrchestrator>>>,
        governance: Option<&GovernanceRuntime>,
    ) -> Result<Vec<crate::block_retention::RetainedBlock>, StoreError> {
        let bytes = self
            .store
            .meta_store()
            .fork_candidate(&hash)?
            .ok_or_else(|| invalid("fork payload unavailable"))?;
        let block: StateBlock =
            bincode::deserialize(&bytes).map_err(|e| invalid(&e.to_string()))?;
        if block.hash != hash {
            return Err(invalid("fork payload identity mismatch"));
        }
        let content = self.store.meta_store().fork_content(&hash)?;
        // Match admission's governance -> delegation -> constitution -> BRN ->
        // TRST -> verification -> processor -> frontier,
        // and scheduler's scheduler -> elections order. No awaits inside LMDB.
        let mut live_governance = match governance {
            Some(runtime) => Some((
                runtime.governance.lock().await,
                runtime.delegation.lock().await,
                runtime.constitution.lock().await,
            )),
            None => None,
        };
        let mut brn = self.brn.lock().await;
        let mut trst = self.trst.lock().await;
        let mut live_verification = match verification {
            Some(verification) => Some(verification.lock().await),
            None => None,
        };
        let mut processor = self.processor.lock().await;
        let mut frontier = self.frontier.write().await;
        let _weights = self.weights.write().await;
        let mut scheduler = self.scheduler.lock().await;
        let mut elections = self.elections.write().await;
        let mut backlog = self.backlog.lock().await;
        let mut broadcaster = self.broadcaster.lock().await;
        let root = crate::ledger_updater::election_root(&block);
        if elections.candidate_root(&hash) != Some(root)
            || !elections
                .confirmed_elections()
                .iter()
                .any(|e| e.winner == hash)
        {
            return Err(invalid("fork no longer has a confirmed election"));
        }
        let (candidate, heads, counts) = {
            let mut batch = self.store.tx_begin_write()?;
            let target = competitor(&batch, &block)?
                .ok_or_else(|| invalid("fork position is no longer competing"))?;
            let (candidate, height) =
                rollback_plan::stage_with(&mut batch, target, &block, |batch, block| {
                    apply_candidate(
                        batch,
                        block,
                        params,
                        network,
                        verification.is_some(),
                        None,
                        governance.map(|runtime| GovernanceInput {
                            genesis: runtime.genesis,
                            content: content.as_deref(),
                        }),
                    )
                })?;
            require_supported_branch(
                &candidate.removed,
                verification.is_some(),
                governance.is_some(),
                network,
            )?;
            let removed: HashSet<_> = candidate.removed.iter().map(|b| b.hash).collect();
            if elections
                .confirmed_elections()
                .iter()
                .any(|e| removed.contains(&e.winner))
            {
                return Err(invalid("fork would remove another confirmed election"));
            }
            let affected: HashSet<_> = candidate
                .removed
                .iter()
                .map(|removed| removed.account.clone())
                .chain(std::iter::once(block.account.clone()))
                .collect();
            let heads = affected
                .into_iter()
                .map(|address| {
                    let head = match batch.get_account(&address) {
                        Ok(account) => (account.block_count != 0).then_some(account.head),
                        Err(StoreError::NotFound(_)) => None,
                        Err(error) => return Err(error),
                    };
                    Ok((address, head))
                })
                .collect::<Result<Vec<_>, StoreError>>()?;
            let counts = batch.ledger_counts()?;
            if !crate::causal_cementation::dependencies_confirmed(&batch, &block)? {
                return Err(StoreError::Backend(
                    "fork dependencies are not confirmed".into(),
                ));
            }
            batch.commit_elected_block(&hash, &block.account, height)?;
            (candidate, heads, counts)
        };
        // Every fallible operation precedes commit. Publish under the same locks
        // that prevent another block, expiry or weight refresh using old state.
        *brn = candidate.state.payment.brn;
        *trst = candidate.state.payment.trst;
        if let (Some((live, delegation, constitution)), Some(restored)) =
            (live_governance.as_mut(), candidate.state.governance)
        {
            **live = restored.governance;
            **delegation = restored.delegation;
            **constitution = restored.constitution;
        }
        if let (Some(live), Some(restored)) =
            (live_verification.as_mut(), candidate.state.verification)
        {
            **live = restored;
        }
        for (account, head) in heads {
            if let Some(head) = head {
                frontier.update(account, head);
            } else {
                frontier.remove(&account);
            }
        }
        // Replacing a provisional branch cannot change confirmed authority.
        self.counts.reset(counts);
        let removed: HashSet<_> = candidate.removed.iter().map(|b| b.hash).collect();
        processor.forget_replaced(&removed);
        for old in &candidate.removed {
            backlog.remove(&old.hash);
            scheduler.remove(&old.hash);
            broadcaster.confirmed(&old.hash);
            let old_root = crate::ledger_updater::election_root(old);
            if old_root != root {
                elections.remove_root(&old_root);
            }
        }
        processor.publish_committed(&block, &mut frontier);
        elections.cleanup_confirmed_where(|winner| *winner == hash);
        let released = processor.release_retained(&hash);
        tracing::info!(%hash, removed = removed.len(), "installed elected fork and durable confirmation handoff");
        Ok(released)
    }
}
