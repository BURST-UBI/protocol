use super::*;
use burst_ledger::StateBlock;

/// Origin at the recovered cut. Only SnapshotPrefix is certified historical
/// membership; local cementation is not a portable finality certificate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BodyOrigin {
    SnapshotPrefix,
    CementedSuffix,
    ProvisionalSuffix,
}

/// A checked body at this recovery context, not an execution/deletion permit.
/// Historical work thresholds are not rechecked and future availability is not
/// promised. Keep the retention cut guard for any eventual publication.
pub(crate) struct ResolvedBody {
    block: StateBlock,
    origin: BodyOrigin,
    context: crate::confirmed_replay::snapshot::history::HistoryContext,
}
impl ResolvedBody {
    pub(crate) fn into_parts(self) -> (StateBlock, BodyOrigin) {
        (self.block, self.origin)
    }
    pub(crate) fn block(&self) -> &StateBlock {
        &self.block
    }
    pub(crate) fn origin(&self) -> BodyOrigin {
        self.origin
    }
    pub(crate) fn context(&self) -> crate::confirmed_replay::snapshot::history::HistoryContext {
        self.context
    }
}

impl RecoveredInstalledMetadata {
    /// Resolve from already checked replay archives or exact observed local
    /// bytes. Unknown hashes stay unresolved even if a raw block row exists.
    /// This does not assert that the live store still equals the recovered cut.
    pub(crate) fn resolve_body(
        &self,
        store: &LmdbStore,
        view: &burst_store_lmdb::LmdbReadTransaction<'_>,
        hash: &BlockHash,
    ) -> Result<Option<ResolvedBody>, StoreError> {
        let context = self.recovered.continuation.history_context();
        let result = |block, origin| {
            Some(ResolvedBody {
                block,
                origin,
                context,
            })
        };
        // finish_staged checked every witness context against this base and
        // excludes duplicate witness/suffix identities. Scratch reads verify
        // the exact retained bytes; no peer-provided trust tag is accepted here.
        for tail in [self.applied.tail(), self.cemented.tail()] {
            if tail.context() != context {
                return Err(invalid("resolved archive has a foreign recovery context"));
            }
            if let Some(block) = tail.retained().input_block(hash)? {
                return Ok(result(block.into_owned(), BodyOrigin::SnapshotPrefix));
            }
        }
        if let Some(block) = self.cemented.tail().archive().input_block(hash)? {
            return Ok(result(block.into_owned(), BodyOrigin::CementedSuffix));
        }
        if let Some(block) = self.applied.tail().archive().input_block(hash)? {
            return Ok(result(block.into_owned(), BodyOrigin::ProvisionalSuffix));
        }
        let Some((block, height)) = self.inventory.observed_body(store, view, hash)? else {
            return Ok(None);
        };
        if self
            .recovered
            .continuation
            .state()
            .account(&block.account)
            .is_none_or(|account| height > account.block_count)
        {
            return Err(invalid(
                "observed suffix body is absent from checked replay archives",
            ));
        }
        Ok(result(block, BodyOrigin::SnapshotPrefix))
    }
}
