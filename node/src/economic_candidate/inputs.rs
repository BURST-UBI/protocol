//! Owned ledger inputs; no transaction or live lock escapes capture.
use super::*;
use crate::{block_processor::BlockProcessor, ledger_updater};
use bincode::Options;
use burst_store_lmdb::LmdbWriteTransaction;
use burst_types::BlockHash;

pub(crate) struct LedgerInputs {
    pub(super) account: Option<AccountInfo>,
    previous: Option<StateBlock>,
    pub(super) previous_brn_spent: u128,
    pending: Option<PendingInfo>,
    block_hash: BlockHash,
    params_hash: BlockHash,
}

impl LedgerInputs {
    pub(crate) fn capture(
        batch: &impl crate::verification_commit::dependencies::view::DependencyView,
        block: &StateBlock,
        params: &ProtocolParams,
    ) -> Result<Self, String> {
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
        let previous_brn_spent = previous.as_ref().map_or(0, |p| p.brn_spent);
        if let Some(previous) = &previous {
            BlockProcessor::validate_balance_transition(
                block,
                previous_brn_spent,
                previous.trst_balance,
            )?;
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
        Ok(Self {
            account,
            previous,
            previous_brn_spent,
            pending,
            block_hash: block.compute_hash(),
            params_hash: params.params_hash(),
        })
    }

    pub(super) fn context<'a>(&'a self, params: &'a ProtocolParams) -> EconomicContext<'a> {
        EconomicContext {
            account: self.account.as_ref(),
            previous_brn_spent: self.previous_brn_spent,
            pending: self.pending.as_ref(),
            params,
        }
    }

    /// Revalidate the exact economic reads before staging a prepared result.
    /// Engine roots, complete authority rules and lifecycle reads are separate
    /// dependencies: this method alone cannot authorize parallel publication.
    pub(crate) fn recheck(
        &self,
        batch: &LmdbWriteTransaction<'_>,
        block: &StateBlock,
        params: &ProtocolParams,
    ) -> Result<(), String> {
        if block.compute_hash() != self.block_hash || params.params_hash() != self.params_hash {
            return Err("prepared economic block or parameters changed".into());
        }
        // Repeats installed-epoch/cutoff, predecessor and signing-grant checks.
        let current = Self::capture(batch, block, params)?;
        let encode = |input: &Self| {
            bincode::serialize(&(&input.account, &input.previous, &input.pending))
                .map_err(|e| e.to_string())
        };
        if encode(self)? != encode(&current)? {
            return Err("prepared economic ledger dependency changed".into());
        }
        Ok(())
    }
}
