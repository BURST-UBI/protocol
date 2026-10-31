//! Payment-local preparation separated from writer validation and persistence.
use super::*;
use burst_store_lmdb::LmdbWriteTransaction;

pub(crate) struct CapturedPayment {
    block: StateBlock,
    params: ProtocolParams,
    inputs: inputs::LedgerInputs,
    before_brn: BrnEngine,
    before_trst: TrstEngine,
}

pub(crate) struct PreparedPayment {
    captured: CapturedPayment,
    economics: StagedBlockEffects,
    brn: BrnEngine,
    trst: TrstEngine,
}

impl CapturedPayment {
    pub(crate) fn eligible(kind: &BlockType) -> bool {
        matches!(
            kind,
            BlockType::GovernanceEndorse
                | BlockType::Challenge
                | BlockType::VerificationVote
                | BlockType::Endorse
                | BlockType::Burn
                | BlockType::Send
                | BlockType::Receive
                | BlockType::RejectReceive
                | BlockType::ChangeRepresentative
                | BlockType::ChangeSigner
                | BlockType::Merge
        )
    }

    /// Capture supported operations from stable holdings and graph storage.
    /// The caller authenticates the block and checks admission; lifecycle and
    /// outcome effects remain separate writer-side operations.
    pub(crate) fn capture(
        batch: &LmdbWriteTransaction<'_>,
        block: &StateBlock,
        params: &ProtocolParams,
    ) -> Result<Option<Self>, PreparedStageError> {
        if !Self::eligible(&block.block_type)
            || !batch
                .uses_stable_trst_records()
                .map_err(|e| PreparedStageError::Abort(e.to_string()))?
            || !batch
                .uses_graph_records()
                .map_err(|e| PreparedStageError::Abort(e.to_string()))?
        {
            return Ok(None);
        }
        let inputs = inputs::LedgerInputs::capture(batch, block, params)
            .map_err(PreparedStageError::BeforeWrite)?;
        let state = crate::payment_transition::restore(batch, params)
            .map_err(|e| PreparedStageError::Abort(e.to_string()))?;
        Ok(Some(Self {
            block: block.clone(),
            params: params.clone(),
            inputs,
            before_brn: state.brn,
            before_trst: state.trst,
        }))
    }

    /// Speculation never waits for a writer. Only clean, complete engines from
    /// this same read snapshot are reusable; cold/dirty caches defer to admission.
    pub(crate) fn capture_read(
        store: &burst_store_lmdb::LmdbStore,
        block: &StateBlock,
        params: &ProtocolParams,
    ) -> Result<Option<Self>, PreparedStageError> {
        if !Self::eligible(&block.block_type) {
            return Ok(None);
        }
        let snapshot = crate::verification_commit::dependencies::view::Snapshot::new(store)
            .map_err(|e| PreparedStageError::Abort(e.to_string()))?;
        let Some((before_brn, before_trst)) = store
            .cached_payment_engines(
                snapshot.view(),
                params.trst_expiry_secs,
                131_072,
                64 * 1024 * 1024,
            )
            .map_err(|e| PreparedStageError::Abort(e.to_string()))?
        else {
            return Ok(None);
        };
        let inputs = inputs::LedgerInputs::capture(&snapshot, block, params)
            .map_err(PreparedStageError::BeforeWrite)?;
        Ok(Some(Self {
            block: block.clone(),
            params: params.clone(),
            inputs,
            before_brn,
            before_trst,
        }))
    }

    /// Pure candidate computation; this object owns no writer or live guard.
    pub(crate) fn prepare(self) -> Result<PreparedPayment, String> {
        let mut engines = StagedEngines::new(&self.before_brn, &self.before_trst);
        let economics = super::apply_with_engines(
            &self.block,
            self.inputs.context(&self.params),
            &mut engines,
        )?;
        let (brn, trst) = engines.into_staged();
        let brn = brn.unwrap_or_else(|| self.before_brn.clone());
        let trst = trst.unwrap_or_else(|| self.before_trst.clone());
        Ok(PreparedPayment {
            captured: self,
            economics,
            brn,
            trst,
        })
    }
}

/// Only BeforeWrite permits ordinary validation in the same transaction. It is
/// not an invalid-block verdict: dependencies may have advanced since capture.
#[derive(Debug)]
pub(crate) enum PreparedStageError {
    BeforeWrite(String),
    Abort(String),
}

pub(crate) struct StagedPayment {
    pub(crate) state: crate::payment_transition::PaymentState,
    pub(crate) economics: StagedBlockEffects,
}

impl PreparedPayment {
    /// Retry stale preparation once against the current writer. Only a
    /// before-write result permits recomputation; write faults still abort.
    pub(crate) fn stage_revalidated(
        self,
        batch: &mut LmdbWriteTransaction<'_>,
        params: &ProtocolParams,
    ) -> Result<Option<StagedPayment>, PreparedStageError> {
        let block = self.captured.block.clone();
        match self.stage_with_economics(batch, params) {
            Ok(Some(staged)) => return Ok(Some(staged)),
            Err(error @ PreparedStageError::Abort(_)) => return Err(error),
            Ok(None) | Err(PreparedStageError::BeforeWrite(_)) => (),
        }
        let Some(current) = CapturedPayment::capture(batch, &block, params)? else {
            return Ok(None);
        };
        current
            .prepare()
            .map_err(PreparedStageError::BeforeWrite)?
            .stage_with_economics(batch, params)
    }

    #[cfg(test)]
    pub(crate) fn stage(
        self,
        batch: &mut LmdbWriteTransaction<'_>,
        params: &ProtocolParams,
    ) -> Result<Option<crate::payment_transition::PaymentState>, PreparedStageError> {
        self.stage_with_economics(batch, params)
            .map(|result| result.map(|staged| staged.state))
    }

    /// Recheck and rebase before writes. Once staging starts, any failure requires
    /// abort, even if it happened before the first successful database mutation.
    /// Authoritative snapshot faults also require abort, never serial fallback.
    /// The caller applies lifecycle effects, commits, then publishes the engines.
    pub(crate) fn stage_with_economics(
        self,
        batch: &mut LmdbWriteTransaction<'_>,
        params: &ProtocolParams,
    ) -> Result<Option<StagedPayment>, PreparedStageError> {
        let capture = &self.captured;
        // A broken authoritative snapshot is not a speculative dependency race.
        // Check it even if the captured account also became stale.
        let current = crate::payment_transition::restore(batch, params)
            .map_err(|e| PreparedStageError::Abort(e.to_string()))?;
        let rebased = (|| -> Result<_, String> {
            capture.inputs.recheck(batch, &capture.block, params)?;
            // Pending bases also depend on the graph/ownership context checked by
            // holdings rebasing. The only wallet read is the acting account.
            let reads = std::slice::from_ref(&capture.block.account);
            let allocates_stake = match &self.economics.effect {
                EconomicResult::VerificationVote { stake, .. } => *stake > 0,
                EconomicResult::Challenge { stake_amount, .. } => *stake_amount > 0,
                _ => false,
            };
            let brn = if allocates_stake {
                current
                    .brn
                    .rebase_wallet_stake(&capture.before_brn, &self.brn, reads)?
            } else {
                current
                    .brn
                    .rebase_wallet_changes(&capture.before_brn, &self.brn, reads)?
            };
            let Some(trst) = batch
                .rebase_trst_payment(
                    &capture.before_trst,
                    &self.trst,
                    reads,
                    params.trst_expiry_secs,
                )
                .map_err(|e| e.to_string())?
            else {
                return Ok(None);
            };
            Ok(Some((current, brn, trst)))
        })()
        .map_err(PreparedStageError::BeforeWrite)?;
        let Some((current, brn, trst)) = rebased else {
            return Ok(None);
        };
        (|| -> Result<(), String> {
            crate::ledger_updater::stage_block(
                batch,
                &capture.block,
                capture.inputs.account.as_ref(),
                capture.inputs.previous_brn_spent,
                self.economics.pending.as_ref(),
                self.economics.indices.as_ref(),
            )?;
            crate::ledger_updater::refresh_token_counters(
                batch,
                &capture.block.account,
                &trst,
                capture.block.timestamp,
            )?;
            crate::ledger_updater::persist_brn_delta(batch, &current.brn, &brn)?;
            batch
                .persist_stable_trst_transition(&current.trst, &trst)
                .map_err(|e| e.to_string())
        })()
        .map_err(PreparedStageError::Abort)?;
        Ok(Some(StagedPayment {
            state: crate::payment_transition::PaymentState { brn, trst },
            economics: self.economics,
        }))
    }
}
