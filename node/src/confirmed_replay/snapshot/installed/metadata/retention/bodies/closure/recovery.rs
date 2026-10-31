use super::*;
use crate::checkpoint_sync::witness;
use crate::confirmed_replay::snapshot::history::HistoricalBlock;
use std::{collections::HashSet, sync::Arc};
use tokio::sync::RwLock;

impl<'proof, 'db> BodyRequirements<'proof, 'db> {
    /// One bounded recovery attempt. Peer account hints never authenticate bodies;
    /// the witness service walks independently authenticated heads.
    /// The pinned read view survives retrieval; publication must recheck its cut.
    #[cfg(test)]
    pub(crate) async fn close_from_peers(
        self,
        limits: BodyClosureLimits,
        registry: &Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
    ) -> Result<ClosedBodyRequirements<'proof, 'db>, BodyClosureError> {
        self.close_with_peer_budget(ClosureBudget::new(limits, None)?, registry)
            .await
    }

    pub(crate) async fn close_retained_from_peers(
        self,
        limits: BodyClosureLimits,
        registry: &Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
    ) -> Result<ClosedBodyRequirements<'proof, 'db>, BodyClosureError> {
        if !self.rollback_checked {
            return Err(BodyClosureError::MissingRollbackProof);
        }
        let budget = self.retained_budget(limits).await?;
        self.close_with_peer_budget(budget, registry).await
    }

    async fn close_with_peer_budget(
        mut self,
        budget: ClosureBudget,
        registry: &Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
    ) -> Result<ClosedBodyRequirements<'proof, 'db>, BodyClosureError> {
        if !self.rollback_checked {
            return Err(BodyClosureError::MissingRollbackProof);
        }
        let mut wanted = Vec::new();
        let cut = &self.cut;
        let mut walk = ClosureWalk::with_budget(&self.roots, cut.store.env().path(), budget)?;
        while !walk.advance(
            &mut self.roots,
            128,
            |hash| resolve(cut, None, hash),
            |hash| {
                budget.check_peer_count(wanted.len() as u64 + 1)?;
                wanted.push(hash);
                Ok(())
            },
        )? {
            tokio::task::yield_now().await;
        }
        let mut work = walk.finish()?;
        if wanted.is_empty() {
            return Ok(ClosedBodyRequirements {
                required: self,
                _work: work,
                _recovered: None,
            });
        }
        let base = &self.cut.metadata.recovered().continuation;
        // Discovery needs one independently authenticated head per candidate
        // account. Check the account count before building the witness roots.
        if base.state().accounts.len() > crate::confirmed_history::MAX_BLOCKS as usize {
            return Err(BodyClosureError::Budget("witness account roots"));
        }
        let roots = Arc::new(
            base.stored_history_roots(self.cut.store.env().path())
                .map_err(invalid)?,
        );
        let committed = base.committed_history_cursors(&wanted).map_err(invalid)?;
        let expected: HashSet<_> = wanted.iter().copied().collect();
        let proofs = tokio::time::timeout(
            crate::checkpoint_sync::TRANSFER_TIMEOUT,
            witness::request_with_committed(
                registry,
                witness::Roots::Stored(roots),
                wanted,
                committed,
            ),
        )
        .await
        .map_err(|_| invalid("required body recovery timed out"))??;
        let recovered = self.stage_proofs(budget, &mut work, expected, proofs)?;
        Ok(ClosedBodyRequirements {
            required: self,
            _work: work,
            _recovered: Some(recovered),
        })
    }

    fn stage_proofs(
        &self,
        budget: ClosureBudget,
        work: &mut ClosureWork,
        mut expected: HashSet<BlockHash>,
        proofs: Vec<HistoricalBlock>,
    ) -> Result<StagedBlocks, BodyClosureError> {
        let base = &self.cut.metadata.recovered().continuation;
        let mut staged = StagedBlocks::new(self.cut.store.env().path())?;
        let mut peer_bytes = 0u64;
        for proof in proofs {
            let block = proof.block();
            if proof.context() != base.history_context()
                || block.compute_hash() != block.hash
                || !expected.remove(&block.hash)
                || base.state().account(&block.account).is_none_or(|account| {
                    proof.height() == 0
                        || proof.height() > account.block_count
                        || (proof.height() == account.block_count && block.hash != account.head)
                })
            {
                return Err(invalid("foreign or unexpected required body witness").into());
            }
            budget.check_peer_count(staged.len() as u64 + 1)?;
            let length = bincode::serialized_size(block).map_err(invalid)?;
            peer_bytes = budget.peer_bytes(peer_bytes, length)?;
            work.bytes = work
                .bytes
                .checked_add(length)
                .filter(|bytes| *bytes <= budget.total.bytes)
                .ok_or(BodyClosureError::Budget("body bytes"))?;
            staged.insert(block)?;
        }
        if !expected.is_empty() {
            return Err(invalid("incomplete required body witnesses").into());
        }
        Ok(staged)
    }
}
