use super::*;
use crate::confirmed_replay::scratch_index::ScratchIndex;
mod root_pages;
use root_pages::{RootPages, RootCursor};
use crate::confirmed_replay::snapshot::SelectionSource;
use burst_types::BlockHash;

mod replayed;
mod root_builder;
mod observed;
mod closure;
pub(crate) use closure::{BodyClosureLimits, OwnedRetirement};

#[derive(Clone, Copy)]
enum Source {
    State = 1,
    Frontier = 2,
    Work = 4,
    Signer = 8,
    Round = 16,
    Undo = 32,
    Replay = 64,
}

/// Direct body requirements, not a closed dependency set. Absence never
/// authorizes deletion. Economic rows, signer records and proof context remain
/// owned by the checked cut; resolution/archive/publication are still required.
pub(crate) struct BodyRequirements<'proof, 'db> {
    cut: RecoveredTrstReferences<'proof, 'db>,
    roots: Roots,
    rollback_checked: bool,
}
// Initial roots share immutable page descriptors. Later source flags and new
// dependencies use the existing authenticated mutable index.
struct RootRows {
    index: ScratchIndex,
    base: Option<RootPages>,
    directory: std::path::PathBuf,
}
impl RootRows {
    fn new(directory: &std::path::Path) -> Result<Self, StoreError> {
        Ok(Self { index: ScratchIndex::new(directory)?, base: None, directory: directory.to_owned() })
    }
    fn decode(value: [u8; 32]) -> Result<u16, StoreError> {
        let sources = u16::from_be_bytes([value[0], value[1]]);
        if sources == 0 || sources & !127 != 0 || value[2..].iter().any(|byte| *byte != 0) {
            return Err(invalid("invalid required body source flags"));
        }
        Ok(sources)
    }
    fn get(&self, key: &[u8; 32]) -> Result<Option<u16>, StoreError> {
        let base = self.base.as_ref().map(|base| base.get(key)).transpose()?.flatten();
        let added = self.index.get(key)?.map(Self::decode).transpose()?;
        Ok(match (base, added) {
            (None, None) => None,
            (base, added) => Some(base.unwrap_or(0) | added.unwrap_or(0)),
        })
    }
    fn put(&mut self, key: [u8; 32], sources: &u16, compact: bool) -> Result<bool, StoreError> {
        let mut value = [0; 32];
        value[..2].copy_from_slice(&sources.to_be_bytes());
        Self::decode(value)?;
        let existed = self.get(&key)?.is_some();
        self.index.insert(key, value)?;
        if compact { self.index.compact_if_amplified(&self.directory)?; }
        Ok(existed)
    }
    fn visit(&self, mut visit: impl FnMut([u8; 32], u16) -> Result<(), StoreError>) -> Result<(), StoreError> {
        let mut cursor = self.cursor()?;
        while !cursor.visit_next(128, |key, value| visit(key, Self::decode(value)?))?.done {}
        Ok(())
    }
    fn cursor(&self) -> Result<RootCursor, StoreError> { RootCursor::new(self.base.as_ref(), self.index.cursor()?) }
    #[cfg(test)]
    fn truncate_for_test(&self) { self.index.truncate_for_test(); if let Some(base) = &self.base { base.truncate_for_test(); } }
}

struct Roots {
    rows: RootRows,
    count: u64,
    max_count: Option<u64>,
    bulk: Option<root_builder::RootBuilder>,
    source_override: Option<Source>,
}
impl Roots {
    fn new(directory: &std::path::Path) -> Result<Self, StoreError> {
        Ok(Self {
            rows: RootRows::new(directory)?,
            count: 0,
            max_count: None,
            bulk: None,
            source_override: None,
        })
    }
    fn new_bulk(directory: &std::path::Path) -> Result<Self, StoreError> {
        let mut roots = Self::new(directory)?;
        roots.bulk = Some(root_builder::RootBuilder::new(directory));
        Ok(roots)
    }
    fn finish_bulk(mut self) -> Result<Self, StoreError> {
        if let Some(builder) = self.bulk.take() {
            let (rows, count) = builder.finish()?;
            if self.max_count.is_some_and(|max| count > max) { return Err(invalid("body roots exceed budget")); }
            self.rows = rows;
            self.count = count;
        }
        Ok(self)
    }
    fn add(&mut self, hash: BlockHash, source: Source) -> Result<(), StoreError> {
        self.add_with_compaction(hash, source, true)
    }
    fn add_until(&mut self, hash: BlockHash, source: Source, cancelled: &dyn Fn() -> bool) -> Result<(), StoreError> {
        if cancelled() { return Err(invalid("body root collection cancelled")); }
        self.add_with_compaction(hash, source, false)?;
        if !self.rows.index.compact_until(&self.rows.directory, cancelled)? {
            return Err(invalid("body root compaction cancelled"));
        }
        Ok(())
    }
    fn add_with_compaction(&mut self, hash: BlockHash, source: Source, compact: bool) -> Result<(), StoreError> {
        let source = self.source_override.unwrap_or(source);
        if hash.is_zero() {
            return Err(invalid("zero required body root"));
        }
        if let Some(builder) = &mut self.bulk {
            return builder.push(*hash.as_bytes(), source as u16);
        }
        let old = self.rows.get(hash.as_bytes())?;
        let next = old.unwrap_or(0) | source as u16;
        if old == Some(next) {
            return Ok(());
        }
        if old.is_none() && self.max_count.is_some_and(|limit| self.count >= limit) {
            return Err(invalid("observed body roots exceed budget"));
        }
        if self.rows.put(*hash.as_bytes(), &next, compact)? != old.is_some() {
            return Err(invalid("required body index changed during insertion"));
        }
        if old.is_none() {
            self.count = self
                .count
                .checked_add(1)
                .ok_or_else(|| invalid("required body count overflow"))?;
        }
        Ok(())
    }
    fn replay_block(&mut self, block: &burst_ledger::StateBlock) -> Result<(), StoreError> {
        self.add(block.hash, Source::Replay)?;
        for dependency in block.dependencies() {
            self.add(dependency, Source::Replay)?;
        }
        Ok(())
    }
    fn trst(&mut self, reference: TrstReference<'_>) -> Result<(), StoreError> {
        match reference {
            TrstReference::Block(hash) => self.add(hash, Source::State),
            // These remain certified economic state, not body addresses.
            TrstReference::Account(_)
            | TrstReference::Token(_)
            | TrstReference::Graph(_)
            | TrstReference::Revision(_) => Ok(()),
        }
    }
    fn governance(
        &mut self,
        reference: burst_governance::record_references::Reference<'_>,
    ) -> Result<(), StoreError> {
        use burst_governance::record_references::Reference;
        match reference {
            Reference::Block(hash) => self.add(hash, Source::State),
            Reference::Account(_)
            | Reference::Proposal(_)
            | Reference::Genesis(_)
            | Reference::DelegationScope(_)
            | Reference::Grant(_)
            | Reference::Boundary(_) => Ok(()),
        }
    }
    fn visit(
        &self,
        mut visit: impl FnMut(BlockHash) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        if self.bulk.is_some() { return Err(invalid("body roots not finalized")); }
        let mut count = 0u64;
        self.rows.visit(|key, sources| {
            let hash = BlockHash::new(key);
            if hash.is_zero() || sources == 0 || sources & !127 != 0 {
                return Err(invalid("invalid required body entry"));
            }
            visit(hash)?;
            count = count
                .checked_add(1)
                .ok_or_else(|| invalid("required body count overflow"))?;
            Ok(())
        })?;
        if count != self.count {
            return Err(invalid("required body inventory differs from index"));
        }
        Ok(())
    }
}

impl PreparedRecoveryProjection {
    /// Body references from both sides of replay-derived provisional deltas.
    /// Complete economic images remain in this preparation. This is direct
    /// reference extraction, not transitive closure or deletion authority.
    #[cfg(test)]
    pub(crate) fn visit_rollback_body_roots(
        &self,
        directory: &std::path::Path,
        visit: impl FnMut(BlockHash) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let mut roots = Roots::new_bulk(directory)?;
        let anchor = self.metadata.recovered().continuation.anchor();
        self.visit_replayed_changes(|record| {
            roots.replayed_change(record, anchor.network(), anchor.genesis_hash())
        })?;
        roots.finish_bulk()?.visit(visit)
    }

    #[cfg(test)]
    pub(crate) fn body_requirements<'proof, 'db>(
        &'proof self, store: &'db LmdbStore,
    ) -> Result<BodyRequirements<'proof, 'db>, StoreError> {
        BodyRequirements::collect_with_projection(self.metadata.trst_references(store)?, Some(self))
    }

    #[cfg(test)]
    pub(crate) fn body_requirements_view<'proof, 'db>(
        &'proof self, store: &'db LmdbStore,
        view: &'db burst_store_lmdb::LmdbReadTransaction<'db>,
    ) -> Result<BodyRequirements<'proof, 'db>, StoreError> {
        BodyRequirements::collect_with_projection(self.metadata.trst_references_view(store, view)?, Some(self))
    }
    pub(crate) fn body_requirements_view_until<'proof, 'db>(
        &'proof self, store: &'db LmdbStore,
        view: &'db burst_store_lmdb::LmdbReadTransaction<'db>,
        cancelled: impl Fn() -> bool,
        compaction_cancelled: std::sync::Arc<dyn Fn(&'static str) -> bool + Send + Sync>,
    ) -> Result<Option<BodyRequirements<'proof, 'db>>, StoreError> {
        if cancelled() { return Ok(None); }
        let cancellation = std::sync::Arc::new(root_builder::Cancellation::new(compaction_cancelled));
        let result = BodyRequirements::collect_with_projection_until(
            self.metadata.trst_references_view(store, view)?, Some(self), &cancelled, Some(cancellation.clone()),
        );
        if cancellation.stopped() { return Ok(None); }
        result
    }

}

impl<'proof, 'db> BodyRequirements<'proof, 'db> {
    #[cfg(test)]
    pub(super) fn collect(
        cut: RecoveredTrstReferences<'proof, 'db>,
    ) -> Result<Self, StoreError> {
        Self::collect_with_projection(cut, None)
    }
    #[cfg(test)]
    fn collect_with_projection(
        cut: RecoveredTrstReferences<'proof, 'db>,
        projection: Option<&PreparedRecoveryProjection>,
    ) -> Result<Self, StoreError> {
        Self::collect_with_projection_until(cut, projection, &|| false, None)?
            .ok_or_else(|| invalid("uncancelled root collection stopped"))
    }
    // Requires a fresh checked reader: a partially consumed reader would omit
    // TRST rows already visited by its caller.
    fn collect_with_projection_until(
        mut cut: RecoveredTrstReferences<'proof, 'db>,
        projection: Option<&PreparedRecoveryProjection>,
        cancelled: &impl Fn() -> bool,
        compaction: Option<std::sync::Arc<root_builder::Cancellation>>,
    ) -> Result<Option<Self>, StoreError> {
        if cancelled() { return Ok(None); }
        let mut roots = Roots::new_bulk(cut.store.env().path())?;
        roots.bulk.as_mut().expect("new bulk root builder").cancellation = compaction;
        roots.add(
            cut.metadata
                .recovered()
                .continuation
                .anchor()
                .genesis_hash(),
            Source::Replay,
        )?;
        macro_rules! scan {
            ($reader:expr, $visit:expr) => {{
                let mut reader = $reader;
                loop {
                    if cancelled() { return Ok(None); }
                    if reader.visit_next_page(100, 64 << 20, $visit)?.done {
                        break;
                    }
                }
            }};
        }
        loop {
            if cancelled() { return Ok(None); }
            if cut.visit_next_page(100, 64 << 20, |r| roots.trst(r))?.done {
                break;
            }
        }
        scan!(cut.brn_wallet_references()?, |r| {
            use burst_brn::retention::Reference;
            match r {
                Reference::Block(hash) => roots.add(hash, Source::State),
                Reference::Account(_)
                | Reference::Outcome(_)
                | Reference::Stake(_)
                | Reference::Boundary(_) => Ok(()),
            }
        });
        scan!(cut.receipt_references(), |r| {
            use crate::verification_commit::retention::Reference;
            match r {
                Reference::Block(hash) => roots.add(hash, Source::State),
                Reference::Account(_)
                | Reference::Checkpoint(_)
                | Reference::CommitteeContext(_)
                | Reference::Parameters(_)
                | Reference::Outcome(_)
                | Reference::RewardOrigin(_)
                | Reference::Boundary(_) => Ok(()),
            }
        });
        scan!(cut.verification_references()?, |r| {
            use burst_verification::orchestrator::records::retention::Reference;
            match r {
                Reference::Block(hash) => roots.add(hash, Source::State),
                Reference::Account(_)
                | Reference::Checkpoint(_)
                | Reference::CommitteeContext(_)
                | Reference::Outcome(_)
                | Reference::Parameters(_)
                | Reference::Boundary(_) => Ok(()),
            }
        });
        scan!(cut.governance_references()?, |r| roots.governance(r));
        scan!(cut.delegation_references()?, |r| roots.governance(r));
        scan!(cut.account_work()?, |r| {
            use burst_store_lmdb::retention::AccountWorkRecord;
            match r {
                AccountWorkRecord::Account(account) if !account.head.is_zero() => {
                    roots.add(account.head, Source::Frontier)
                }
                AccountWorkRecord::Account(_) => Ok(()),
                AccountWorkRecord::PendingConfirmation(hash) => roots.add(hash, Source::Work),
            }
        });
        scan!(cut.payment_bodies(), |hash| roots.add(hash, Source::Signer));
        scan!(cut.checkpoint_rounds(), |round| {
            for manifest in round.manifests.into_iter().flatten() {
                for head in manifest.heads() {
                    roots.add(head.hash, Source::Round)?;
                }
            }
            Ok(())
        });
        scan!(cut.rollback_records()?, |r| {
            if let burst_store_lmdb::retention::RollbackRecord::Journal(journal) = r {
                roots.add(journal.block, Source::Undo)?;
            }
            // Preserve the observed carrier conservatively. JournalGuard checks
            // encoding and unchanged bytes, not the economic claims in images.
            // Closure must use changes::derive and finality::reconcile; decoding
            // these beforeimages cannot authenticate restoration dependencies.
            Ok(())
        });
        // Every applied suffix body is needed to reconstruct this cut, including
        // intermediate account blocks that are no longer a frontier. Its old
        // dependency witnesses are required even when no current row names them.
        let archive = cut.metadata.applied.tail().archive();
        let mut interrupted = false;
        let result = archive.visit_input::<StoreError>(|block| {
            if cancelled() { interrupted = true; return Err(invalid("root collection cancelled")); }
            roots.replay_block(block)
        });
        if interrupted { return Ok(None); }
        result?;
        for hash in &cut.metadata.economics.state.replay_dependencies {
            if cancelled() { return Ok(None); }
            roots.add(*hash, Source::Replay)?;
        }
        for receipt in cut
            .metadata
            .recovered()
            .continuation
            .state()
            .selection_receipts
            .iter()
        {
            if cancelled() { return Ok(None); }
            for head in receipt.map_err(invalid)?.heads() {
                roots.add(head.hash, Source::Replay)?;
            }
        }
        for selection in cut.signer_selections().iter_selections() {
            if cancelled() { return Ok(None); }
            for head in selection.map_err(invalid)?.heads() {
                roots.add(head.hash, Source::Replay)?;
            }
        }
        if let Some(projection) = projection {
            let anchor = projection.metadata.recovered().continuation.anchor();
            // References from replayed before/after images are rollback roots,
            // regardless of the source category used by each row decoder.
            roots.source_override = Some(Source::Undo);
            let mut interrupted = false;
            let result = projection.visit_replayed_changes(|record| {
                if cancelled() { interrupted = true; return Err(invalid("root collection cancelled")); }
                roots.replayed_change(record, anchor.network(), anchor.genesis_hash())
            });
            if interrupted { return Ok(None); }
            result?;
            roots.source_override = None;
        }
        if cancelled() { return Ok(None); }
        let roots = roots.finish_bulk()?;
        if cancelled() { return Ok(None); }
        Ok(Some(Self { cut, roots, rollback_checked: projection.is_some() }))
    }
    pub(crate) fn has_root(&self, hash: BlockHash) -> Result<bool, StoreError> {
        Ok(self.roots.rows.get(hash.as_bytes())?.is_some())
    }
    #[cfg(test)]
    pub(crate) fn visit_roots(
        &self,
        visit: impl FnMut(BlockHash) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.roots.visit(visit)
    }
    /// Visit every required root, explicitly reporting unresolved bodies.
    /// A missing body never authorizes retirement or an assumed archive fetch.
    #[cfg(test)]
    pub(crate) fn visit_resolved_roots(
        &self,
        mut visit: impl FnMut(BlockHash, Option<ResolvedBody>) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.roots.visit(|hash| {
            let body = self.cut.metadata.resolve_body(self.cut.store, self.cut.reader.view(), &hash)?;
            visit(hash, body)
        })
    }

    #[cfg(test)]
    pub(crate) fn check_unchanged_cut(
        &self,
        batch: &LmdbWriteTransaction<'_>,
    ) -> Result<(), StoreError> {
        self.cut.check_unchanged_cut(batch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_root_flags_bound_storage_and_keep_seed_membership_pinned() {
        use crate::confirmed_replay::scratch_file::ScratchLimit;
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(directory.path(), 3 << 20).unwrap();
        let keys: Vec<_> = (0..4096u64).map(|n| BlockHash::new(burst_crypto::blake2b_256(&n.to_be_bytes()))).collect();
        let mut roots = Roots::new(directory.path()).unwrap();
        for hash in &keys[..64] { roots.add(*hash, Source::State).unwrap(); }
        let mut seed = roots.rows.cursor().unwrap();
        for hash in &keys {
            roots.add(*hash, Source::State).unwrap();
            roots.add(*hash, Source::Work).unwrap();
        }
        assert_eq!(roots.count, keys.len() as u64);
        let before = limit.used_temporary_bytes().unwrap();
        for hash in &keys {
            roots.add(*hash, Source::State).unwrap();
            assert_eq!(roots.rows.get(hash.as_bytes()).unwrap(), Some(5));
        }
        assert_eq!(limit.used_temporary_bytes().unwrap(), before, "duplicate flags must not rewrite indexes");
        let mut seen = std::collections::HashSet::new();
        while !seed.visit_next(7, |key, flags| {
            assert_eq!(RootRows::decode(flags)?, 1);
            assert!(keys[..64].contains(&BlockHash::new(key)));
            assert!(seen.insert(key));
            Ok(())
        }).unwrap().done {}
        assert_eq!(seen.len(), 64);
        assert!(RootRows::decode([0;32]).is_err());
        let mut malformed = [0;32]; malformed[1] = 128;
        assert!(RootRows::decode(malformed).is_err());
        malformed[1] = 1; malformed[31] = 1;
        assert!(RootRows::decode(malformed).is_err());
        roots.rows.truncate_for_test();
        assert!(roots.visit(|_| Ok(())).is_err());
        assert!(roots.rows.get(keys[0].as_bytes()).is_err());
        drop(seed);
        drop(roots);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    }

    #[test]
    fn body_roots_deduplicate_sources_without_casting_token_or_commitment_ids() {
        let dir = tempfile::tempdir().unwrap();
        let mut roots = Roots::new(dir.path()).unwrap();
        let hash = BlockHash::new([1; 32]);
        for _ in 0..1000 {
            roots.add(hash, Source::Work).unwrap();
        }
        roots.add(hash, Source::Signer).unwrap();
        assert_eq!(roots.count, 1);
        assert_eq!(
            roots.rows.get(hash.as_bytes()).unwrap(),
            Some(Source::Work as u16 | Source::Signer as u16)
        );
        let logical = burst_types::TxHash::new([2; 32]);
        for reference in [
            TrstReference::Token(logical),
            TrstReference::Graph(logical),
            TrstReference::Revision(logical),
        ] {
            roots.trst(reference).unwrap();
        }
        roots
            .governance(burst_governance::record_references::Reference::Grant(
                BlockHash::new([2; 32]),
            ))
            .unwrap();
        assert!(roots.rows.get(logical.as_bytes()).unwrap().is_none());
        let mut visited = Vec::new();
        roots
            .visit(|h| {
                visited.push(h);
                Ok(())
            })
            .unwrap();
        assert_eq!(visited, vec![hash]);
        assert!(roots.add(BlockHash::ZERO, Source::State).is_err());
    }
    #[test]
    fn body_roots_keep_intermediate_replay_bodies_and_explicit_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let mut roots = Roots::new(dir.path()).unwrap();
        let params = burst_types::ProtocolParams::default();
        let mut block = crate::genesis_state::block(burst_types::NetworkId::Dev, &params);
        let previous = BlockHash::new([21; 32]);
        let source = BlockHash::new([22; 32]);
        let target = BlockHash::new([23; 32]);
        let release = BlockHash::new([24; 32]);
        block.block_type = burst_ledger::BlockType::Receive;
        block.previous = previous;
        block.link = source;
        block.target_head = target;
        block.release_sources = vec![release];
        block.hash = block.compute_hash();
        let intermediate = block.hash;
        roots.replay_block(&block).unwrap();
        block.previous = intermediate;
        block.block_type = burst_ledger::BlockType::Send;
        // A Send link is an address encoding, not a body dependency.
        block.link = BlockHash::new([25; 32]);
        block.hash = block.compute_hash();
        roots.replay_block(&block).unwrap();
        for hash in [previous, source, target, release, intermediate, block.hash] {
            assert_eq!(roots.rows.get(hash.as_bytes()).unwrap(), Some(Source::Replay as u16));
        }
        assert_eq!(roots.count, 6);
        assert!(roots.rows.get(block.link.as_bytes()).unwrap().is_none());
    }
    #[test]
    fn body_roots_propagate_corrupt_scratch_and_visitor_errors() {
        let dir = tempfile::tempdir().unwrap();
        let mut roots = Roots::new(dir.path()).unwrap();
        let hash = BlockHash::new([1; 32]);
        roots.add(hash, Source::State).unwrap();
        assert!(roots.visit(|_| Err(invalid("stop"))).is_err());
        roots.rows.truncate_for_test();
        assert!(roots.visit(|_| Ok(())).is_err());
        assert!(roots.add(hash, Source::State).is_err());
    }
}
