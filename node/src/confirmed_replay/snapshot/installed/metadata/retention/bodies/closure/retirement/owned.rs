use super::super::super::observed::ObservedBodyRoots;
use super::*;
#[cfg(test)]
use crate::confirmed_replay::retirement::catalogue::CandidateBuilder;
use crate::confirmed_replay::retirement::catalogue::{Candidate, CandidateRows};
use bincode::Options;
use burst_store_lmdb::retention::BodyChangeWatch;
use burst_store_lmdb::write_batch::CleanStorageCut;

/// Authenticated private catalogue for one complete checked closure. It owns no
/// metadata borrow or database reader. Only this object's successful deletions
/// or complete conservative observation reconciliation may advance its cut.
pub(crate) struct OwnedRetirement {
    rows: CandidateRows,
    cut: CleanStorageCut,
    after: Option<BlockHash>,
    usable: bool,
    changes: BodyChangeWatch,
    own_commit: Option<CleanStorageCut>,
    protected: Roots,
    context: crate::confirmed_replay::snapshot::history::HistoryContext,
}

pub(crate) struct OwnedPage<'a> {
    owner: &'a mut OwnedRetirement,
    hashes: Vec<BlockHash>,
    bytes: u64,
    progress: RetirementProgress,
}

impl ClosedBodyRequirements<'_, '_> {
    /// Classify physical rows once while the original checked reader is pinned.
    /// Required and young rows remain in the catalogue so page examination
    /// budgets retain their meaning. No partial catalogue can be published.
    #[cfg(test)]
    pub(crate) fn own_retirement(
        &self,
        before: Timestamp,
        after: Option<BlockHash>,
        cancelled: impl Fn() -> bool,
    ) -> Result<OwnedRetirement, StoreError> {
        let source = &self.required.cut;
        let cut = source
            .reader
            .view()
            .clean_storage_cut()
            .ok_or_else(|| invalid("retirement catalogue has no storage identity"))?;
        // Install coverage at the checked reader boundary, before classification.
        // The pinned proof still supplies original candidates; concurrent writes
        // can only revoke eligibility when prepare_page reconciles observations.
        let changes = {
            let writer = source.store.tx_begin_write()?;
            writer.check_detached_body_retention_cut(&cut)?;
            writer.watch_body_changes(65_536, 64 << 20)?
        };
        self.classify_retirement(before, after, cancelled, changes)
    }

    pub(crate) fn own_captured_retirement(
        &self,
        before: Timestamp,
        after: Option<BlockHash>,
        cancelled: impl Fn() -> bool,
        changes: BodyChangeWatch,
    ) -> Result<OwnedRetirement, StoreError> {
        let source = &self.required.cut;
        let cut = source
            .reader
            .view()
            .clean_storage_cut()
            .ok_or_else(|| invalid("retirement catalogue has no storage identity"))?;
        changes.check_unconsumed_since(source.store, &cut)?;
        self.classify_retirement(before, after, cancelled, changes)
    }

    fn classify_retirement(
        &self,
        before: Timestamp,
        after: Option<BlockHash>,
        cancelled: impl Fn() -> bool,
        changes: BodyChangeWatch,
    ) -> Result<OwnedRetirement, StoreError> {
        let source = &self.required.cut;
        let cut = source
            .reader
            .view()
            .clean_storage_cut()
            .ok_or_else(|| invalid("retirement catalogue has no storage identity"))?;
        let hard = BodyClosureLimits::default();
        let (max_rows, max_bytes) = source
            .metadata
            .inventory
            .catalogue_limits(source.store.env().path())?;
        let rows = CandidateRows::classify(
            source.metadata.inventory.body_observations(),
            source.store.env().path(),
            max_rows,
            max_bytes,
            |hash, length| {
                if cancelled() {
                    return Err(invalid("retirement catalogue cancelled"));
                }
                let body = source
                    .metadata
                    .resolve_body(source.store, source.reader.view(), &hash)?
                    .ok_or_else(|| invalid("retirement candidate is unresolved"))?;
                let prefix = body.origin() == BodyOrigin::SnapshotPrefix;
                let eligible = !self.has_body(hash)? && prefix && body.block().timestamp <= before;
                Ok(Candidate {
                    body_bytes: length,
                    eligible,
                    in_snapshot_prefix: prefix,
                })
            },
        )?;
        let mut protected = Roots::new(source.store.env().path())?;
        protected.max_count = Some(hard.bodies);
        Ok(OwnedRetirement {
            rows,
            cut,
            after,
            usable: true,
            changes,
            own_commit: None,
            protected,
            context: source.metadata.recovered().continuation.history_context(),
        })
    }
}

impl OwnedRetirement {
    #[cfg(test)]
    pub(crate) fn is_current(&self, store: &LmdbStore) -> Result<bool, StoreError> {
        if !self.usable {
            return Ok(false);
        }
        let current = store
            .tx_begin_read()?
            .clean_storage_cut()
            .ok_or_else(|| invalid("retirement catalogue has no current storage identity"))?;
        self.cut.body_dependencies_unchanged(&current, store)
    }

    #[cfg(test)]
    pub(crate) fn reconcile(&mut self, store: &LmdbStore) -> Result<(), StoreError> {
        self.reconcile_until(store, || false)
    }

    /// Observations can revoke deletion eligibility, never grant it. Resolve new
    /// dependencies at one pinned cut, stopping at original certified prefix
    /// members or previously closed protection graphs. Raw bodies remain untrusted.
    pub(crate) fn reconcile_until(
        &mut self,
        store: &LmdbStore,
        cancelled: impl Fn() -> bool,
    ) -> Result<(), StoreError> {
        if !self.usable {
            return Err(invalid("retirement catalogue already failed"));
        }
        self.usable = false;
        if cancelled() {
            return Err(invalid("retirement reconciliation cancelled"));
        }
        let hard = BodyClosureLimits::default();
        let mut observed = ObservedBodyRoots::new(
            store.env().path(),
            self.context.network,
            self.context.genesis,
            self.context.epoch,
            hard.bodies,
        )?;
        let own = self.own_commit.as_ref();
        let view = self
            .changes
            .visit_commits_with_snapshot(store, |commit, row| {
                if cancelled() {
                    return Err(invalid("retirement reconciliation cancelled"));
                }
                if own.is_some_and(|cut| commit.matches_cut(cut)) {
                    return Ok(());
                }
                observed.observe_until(row, &cancelled)
            })?;
        let current = view
            .clean_storage_cut()
            .ok_or_else(|| invalid("reconciliation has no storage cut"))?;
        let mut roots = observed.into_roots()?;
        let mut walk = super::super::ClosureWalk::new(&roots, store.env().path(), hard)
            .map_err(|error| invalid(format!("retirement observation closure: {error}")))?;
        loop {
            if cancelled() {
                return Err(invalid("retirement reconciliation cancelled"));
            }
            let complete = walk
                .advance(
                    &mut roots,
                    128,
                    |hash| {
                        let bytes = store.block_store().get_block_txn_bounded(
                            &view,
                            &hash,
                            hard.bytes as usize,
                        )?;
                        let block: burst_ledger::StateBlock = bincode::DefaultOptions::new()
                            .with_fixint_encoding()
                            .with_limit(bytes.len() as u64)
                            .reject_trailing_bytes()
                            .deserialize(&bytes)
                            .map_err(invalid)?;
                        let closed = self
                            .rows
                            .get(hash.as_bytes())?
                            .is_some_and(|row| row.in_snapshot_prefix)
                            || self.protected.rows.get(hash.as_bytes())?.is_some();
                        // Here the walker tag is only a traversal boundary: original prefix
                        // membership or a previously closed protection graph. Complete
                        // observations include removals, so missing descendants still fail.
                        // It never grants a raw body authenticated membership or eligibility.
                        Ok(Some((
                            block,
                            if closed {
                                BodyOrigin::SnapshotPrefix
                            } else {
                                BodyOrigin::ProvisionalSuffix
                            },
                        )))
                    },
                    |hash| Err(super::super::BodyClosureError::Unresolved(hash)),
                )
                .map_err(|error| invalid(format!("retirement observation closure: {error}")))?;
            if complete {
                break;
            }
        }
        walk.finish()
            .map_err(|error| invalid(format!("retirement observation closure: {error}")))?;
        roots.visit(|hash| self.protected.add_until(hash, Source::Work, &cancelled))?;
        self.cut = current;
        self.own_commit = None;
        self.usable = true;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn prepare_page(
        &mut self,
        store: &LmdbStore,
        max_bodies: usize,
        max_bytes: u64,
        max_examined: usize,
    ) -> Result<OwnedPage<'_>, StoreError> {
        self.prepare_page_until(store, max_bodies, max_bytes, max_examined, || false)
    }

    pub(crate) fn prepare_page_until(
        &mut self,
        store: &LmdbStore,
        max_bodies: usize,
        max_bytes: u64,
        max_examined: usize,
        cancelled: impl Fn() -> bool,
    ) -> Result<OwnedPage<'_>, StoreError> {
        let hard = BodyClosureLimits::default();
        if max_bodies == 0
            || max_bodies as u64 > hard.bodies
            || max_bytes == 0
            || max_bytes > hard.bytes
            || max_examined == 0
            || max_examined as u64 > hard.bodies
        {
            return Err(invalid("invalid physical retirement budget"));
        }
        self.reconcile_until(store, &cancelled)?;
        self.usable = false; // Abandoned/failed pages cannot silently advance reuse.
        let mut rows = self.rows.scan(self.after)?.peekable();
        let mut hashes = Vec::new();
        let mut bytes = 0u64;
        let mut progress = RetirementProgress {
            examined: 0,
            resume_after: self.after,
            complete: false,
            blocked_body_bytes: None,
        };
        while rows.peek().is_some() && progress.examined < max_examined && hashes.len() < max_bodies
        {
            if cancelled() {
                return Err(invalid("retirement page cancelled"));
            }
            let (hash, candidate) = rows.next().unwrap()?;
            progress.examined += 1;
            if candidate.eligible && self.protected.rows.get(hash.as_bytes())?.is_none() {
                let next = bytes
                    .checked_add(candidate.body_bytes)
                    .ok_or_else(|| invalid("retirement byte overflow"))?;
                if next > max_bytes {
                    progress.blocked_body_bytes = Some(candidate.body_bytes);
                    break;
                }
                hashes.push(hash);
                bytes = next;
            }
            progress.resume_after = Some(hash);
        }
        if rows.peek().is_some_and(Result::is_err) {
            return Err(rows.next().unwrap().unwrap_err());
        }
        progress.complete = progress.blocked_body_bytes.is_none() && rows.peek().is_none();
        drop(rows);
        if cancelled() {
            return Err(invalid("retirement page cancelled"));
        }
        Ok(OwnedPage {
            owner: self,
            hashes,
            bytes,
            progress,
        })
    }
}

impl OwnedPage<'_> {
    pub(crate) fn progress(&self) -> RetirementProgress {
        self.progress
    }
    pub(crate) fn hashes(&self) -> &[BlockHash] {
        &self.hashes
    }

    /// Publication exclusion and counts must be owned by the coordinator.
    /// No await separates deletion, exact-cut rebasing and additive accounting.
    #[cfg(test)]
    pub(crate) fn commit_counted(
        self,
        store: &LmdbStore,
        counts: &crate::ledger_cache::LedgerCache,
    ) -> Result<(usize, u64), StoreError> {
        self.commit(store, counts, false, |_| {})?
            .ok_or_else(|| invalid("strict retirement commit deferred"))
    }

    /// None means a covered change preceded publication. The stale page is
    /// discarded; the catalogue remains owned but requires fresh reconciliation.
    pub(crate) fn try_commit_counted(
        self,
        store: &LmdbStore,
        counts: &crate::ledger_cache::LedgerCache,
    ) -> Result<Option<(usize, u64)>, StoreError> {
        self.commit(store, counts, true, |_| {})
    }

    #[cfg(test)]
    pub(crate) fn commit_with_probe(
        self,
        store: &LmdbStore,
        counts: &crate::ledger_cache::LedgerCache,
        probe: impl Fn(&str),
    ) -> Result<(usize, u64), StoreError> {
        self.commit(store, counts, false, probe)?
            .ok_or_else(|| invalid("strict retirement commit deferred"))
    }

    fn commit(
        self,
        store: &LmdbStore,
        counts: &crate::ledger_cache::LedgerCache,
        allow_defer: bool,
        probe: impl Fn(&str),
    ) -> Result<Option<(usize, u64)>, StoreError> {
        if !cfg!(test) {
            return Err(invalid(
                "physical body retirement requires coordinator and semantic qualification",
            ));
        }
        if counts.block_count() < self.hashes.len() as u64 {
            return Err(invalid("physical body count is behind retirement batch"));
        }
        let mut writer = store.tx_begin_write()?;
        if allow_defer {
            let current = writer
                .clean_storage_cut()
                .ok_or_else(|| invalid("retirement publication requires a clean writer"))?;
            if !self
                .owner
                .cut
                .body_dependencies_unchanged(&current, store)?
            {
                self.owner.changes.check_covered_cut(store, &current)?;
                // No mutation, cursor advance, count publication or observation
                // consumption occurred. A new page must reconcile this watch.
                self.owner.usable = true;
                return Ok(None);
            }
        }
        writer.check_detached_body_retention_cut(&self.owner.cut)?;
        if !self.hashes.is_empty() {
            for hash in &self.hashes {
                writer.delete_block(hash)?;
                probe("deleted");
            }
            probe("prepared");
            self.owner.cut = writer.commit_with_storage_cut()?;
            probe("committed");
            self.owner.own_commit = Some(self.owner.cut.clone());
            counts.retire_blocks(self.hashes.len() as u64);
        }
        self.owner.after = self.progress.resume_after;
        self.owner.usable = true;
        Ok(Some((self.hashes.len(), self.bytes)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger_cache::LedgerCache;
    use burst_store::block::BlockStore;
    use burst_types::{NetworkId, ProtocolParams};

    #[test]
    fn fixed_catalogue_preserves_classification_paging_and_resource_limits() {
        use crate::confirmed_replay::scratch_file::ScratchLimit;
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(directory.path(), 2_200_000).unwrap();
        let key = |n: u64| {
            let mut key = [0; 32];
            key[..8].copy_from_slice(&n.to_be_bytes());
            key
        };
        let candidate = |n: u64| Candidate {
            body_bytes: n + 1,
            eligible: n % 3 == 0,
            in_snapshot_prefix: n % 3 != 2,
        };
        let mut builder = CandidateBuilder::new(directory.path(), 32000, 320000).unwrap();
        for n in 0..32000 {
            builder.push(key(n), &candidate(n)).unwrap();
        }
        let rows = builder.finish().unwrap();
        assert_eq!(
            limit.used_temporary_bytes().unwrap(),
            32000 * 64 + 500 * 194 - 102
        );
        for n in 0..32000 {
            assert_eq!(rows.get(&key(n)).unwrap(), Some(candidate(n)));
        }
        let mut visited = rows.scan(None).unwrap();
        for n in 0..32000 {
            assert_eq!(
                visited.next().unwrap().unwrap(),
                (BlockHash::new(key(n)), candidate(n))
            );
        }
        assert!(visited.next().is_none());
        let suffix = rows.scan(Some(BlockHash::new(key(12345)))).unwrap();
        assert_eq!(
            suffix.map(|row| row.unwrap().0).collect::<Vec<_>>(),
            (12346..32000)
                .map(|n| BlockHash::new(key(n)))
                .collect::<Vec<_>>()
        );
        drop(visited);
        let mut corrupt = rows.scan(None).unwrap();
        rows.truncate_for_test();
        assert!(rows.get(&key(0)).is_err());
        assert!(corrupt.next().unwrap().is_err());
        assert!(corrupt.next().is_none());
        drop(corrupt);
        drop(rows);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
        for (max_rows, max_bytes) in [(0, 10), (1, 9)] {
            let mut builder = CandidateBuilder::new(directory.path(), max_rows, max_bytes).unwrap();
            assert!(builder.push(key(1), &candidate(1)).is_err());
            assert!(builder.finish().is_err());
        }
        let mut duplicate = CandidateBuilder::new(directory.path(), 2, 20).unwrap();
        duplicate.push(key(1), &candidate(1)).unwrap();
        assert!(duplicate.push(key(1), &candidate(1)).is_err());
        assert!(duplicate.finish().is_err());
        for flags in [1, 4, 128, 255] {
            let mut value = [0; 32];
            value[8] = flags;
            assert!(Candidate::decode(value).is_err());
        }
        for byte in 9..32 {
            let mut value = candidate(0).encode();
            value[byte] = 1;
            assert!(Candidate::decode(value).is_err());
        }
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    }

    // This fixture supplies prefix classifications directly; it tests catalogue
    // reuse and publication, not authentication of the original snapshot.
    fn fixture(
        path: &std::path::Path,
    ) -> (LmdbStore, OwnedRetirement, Vec<burst_ledger::StateBlock>) {
        let store = LmdbStore::open(path, 32, 32 << 20).unwrap();
        let mut blocks = Vec::new();
        let mut candidates = Vec::new();
        let mut writer = store.tx_begin_write().unwrap();
        for timestamp in [1, 2] {
            let mut block = crate::genesis_state::block(NetworkId::Dev, &ProtocolParams::default());
            block.timestamp = Timestamp::new(timestamp);
            block.hash = block.compute_hash();
            let bytes = bincode::serialize(&block).unwrap();
            writer.put_block(&block.hash, &bytes).unwrap();
            candidates.push((
                *block.hash.as_bytes(),
                Candidate {
                    body_bytes: bytes.len() as u64,
                    eligible: true,
                    in_snapshot_prefix: true,
                },
            ));
            blocks.push(block);
        }
        candidates.sort_unstable_by_key(|(key, _)| *key);
        let mut builder = CandidateBuilder::new(path, candidates.len(), 1 << 20).unwrap();
        for (key, row) in candidates {
            builder.push(key, &row).unwrap();
        }
        let rows = builder.finish().unwrap();
        let cut = writer.commit_with_storage_cut().unwrap();
        let changes = store
            .tx_begin_write()
            .unwrap()
            .watch_body_changes(64, 1 << 20)
            .unwrap();
        let mut protected = Roots::new(path).unwrap();
        protected.max_count = Some(BodyClosureLimits::default().bodies);
        let context = crate::confirmed_replay::snapshot::history::HistoryContext {
            network: NetworkId::Dev,
            genesis: BlockHash::new([1; 32]),
            epoch: BlockHash::new([2; 32]),
            state_hash: BlockHash::new([3; 32]),
        };
        (
            store,
            OwnedRetirement {
                rows,
                cut,
                after: None,
                usable: true,
                changes,
                own_commit: None,
                protected,
                context,
            },
            blocks,
        )
    }

    #[test]
    fn new_suffix_protects_dependencies_without_adding_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let (store, mut owned, blocks) = fixture(dir.path());
        let mut suffix = blocks[0].clone();
        suffix.previous = blocks[0].hash;
        suffix.timestamp = Timestamp::new(3);
        suffix.hash = suffix.compute_hash();
        let mut writer = store.tx_begin_write().unwrap();
        writer
            .put_block(&suffix.hash, &bincode::serialize(&suffix).unwrap())
            .unwrap();
        writer.put_frontier(&suffix.account, &suffix.hash).unwrap();
        writer.commit().unwrap();
        let counts = LedgerCache::new(3, 0, 0);
        let page = owned.prepare_page(&store, 8, 64 << 20, 8).unwrap();
        assert_eq!(page.hashes(), &[blocks[1].hash]);
        page.commit_counted(&store, &counts).unwrap();
        owned.reconcile(&store).unwrap();
        assert!(store.block_store().exists(&suffix.hash).unwrap());
        assert!(store.block_store().exists(&blocks[0].hash).unwrap());
        assert!(!store.block_store().exists(&blocks[1].hash).unwrap());
        assert_eq!(counts.block_count(), 2);
        assert!(owned.rows.get(suffix.hash.as_bytes()).unwrap().is_none());
        // A later removal of a dependency must not hide behind cached closure.
        let mut writer = store.tx_begin_write().unwrap();
        writer.delete_block(&blocks[0].hash).unwrap();
        writer.commit().unwrap();
        assert!(owned.reconcile(&store).is_err());
        assert!(!owned.is_current(&store).unwrap());
    }

    #[test]
    fn own_deletion_does_not_hide_later_same_body_reinsertion() {
        let dir = tempfile::tempdir().unwrap();
        let (store, mut owned, blocks) = fixture(dir.path());
        let counts = LedgerCache::new(2, 0, 0);
        let page = owned.prepare_page(&store, 1, 64 << 20, 8).unwrap();
        let deleted = page.hashes()[0];
        page.commit_counted(&store, &counts).unwrap();
        let block = blocks.iter().find(|b| b.hash == deleted).unwrap();
        let mut writer = store.tx_begin_write().unwrap();
        writer
            .put_block(&deleted, &bincode::serialize(block).unwrap())
            .unwrap();
        writer.commit().unwrap();
        counts.inc_block_count();
        owned.reconcile(&store).unwrap();
        assert!(owned
            .protected
            .rows
            .get(deleted.as_bytes())
            .unwrap()
            .is_some());
        assert!(store.block_store().exists(&deleted).unwrap());
        assert_eq!(counts.block_count(), 2);
    }

    #[test]
    fn missing_roots_cancellation_and_publication_races_reject_reuse() {
        for mode in 0..8 {
            let dir = tempfile::tempdir().unwrap();
            let (store, mut owned, blocks) = fixture(dir.path());
            let counts = LedgerCache::new(2, 0, 0);
            match mode {
                0 => {
                    let mut writer = store.tx_begin_write().unwrap();
                    writer
                        .put_frontier(&blocks[0].account, &BlockHash::new([99; 32]))
                        .unwrap();
                    writer.commit().unwrap();
                    assert!(owned.reconcile(&store).is_err());
                }
                1 => assert!(owned.reconcile_until(&store, || true).is_err()),
                2 => {
                    let polls = std::cell::Cell::new(0);
                    let result = owned.prepare_page_until(&store, 2, 64 << 20, 8, || {
                        polls.set(polls.get() + 1);
                        // Allow preflight, empty reconciliation and one candidate,
                        // then stop before the second candidate enters the page.
                        polls.get() == 4
                    });
                    let error = match result {
                        Err(error) => error,
                        Ok(_) => panic!("cancelled retirement page remained publishable"),
                    };
                    assert!(matches!(error, StoreError::Corruption(ref message)
                        if message == "retirement page cancelled"));
                    assert!(owned.prepare_page(&store, 2, 64 << 20, 8).is_err());
                }
                3 => {
                    let page = owned.prepare_page(&store, 1, 64 << 20, 8).unwrap();
                    let mut writer = store.tx_begin_write().unwrap();
                    writer
                        .put_frontier(&blocks[0].account, &page.hashes()[0])
                        .unwrap();
                    writer.commit().unwrap();
                    assert!(page.commit_counted(&store, &counts).is_err());
                }
                4 => {
                    let before = owned.after;
                    let page = owned.prepare_page(&store, 1, 64 << 20, 8).unwrap();
                    let protected = page.hashes()[0];
                    let mut writer = store.tx_begin_write().unwrap();
                    writer.put_frontier(&blocks[0].account, &protected).unwrap();
                    writer.commit().unwrap();
                    assert!(page.try_commit_counted(&store, &counts).unwrap().is_none());
                    assert_eq!(owned.after, before);
                    assert_eq!(counts.block_count(), 2);
                    assert!(owned.usable);
                    let retry = owned.prepare_page(&store, 2, 64 << 20, 8).unwrap();
                    assert!(!retry.hashes().contains(&protected));
                    assert_eq!(retry.hashes().len(), 1);
                    assert_eq!(
                        retry
                            .try_commit_counted(&store, &counts)
                            .unwrap()
                            .unwrap()
                            .0,
                        1
                    );
                    assert_eq!(counts.block_count(), 1);
                    assert!(store.block_store().exists(&protected).unwrap());
                    continue;
                }
                5 => {
                    let page = owned.prepare_page(&store, 1, 64 << 20, 8).unwrap();
                    let foreign = tempfile::tempdir().unwrap();
                    let foreign = LmdbStore::open(foreign.path(), 32, 32 << 20).unwrap();
                    assert!(page.try_commit_counted(&foreign, &counts).is_err());
                }
                6 => {
                    let page = owned.prepare_page(&store, 1, 64 << 20, 8).unwrap();
                    let mut writer = store.tx_begin_write().unwrap();
                    for n in 0..65 {
                        writer.put_meta(&format!("overflow-{n}"), &[1]).unwrap();
                    }
                    writer.commit().unwrap();
                    assert!(page.try_commit_counted(&store, &counts).is_err());
                }
                _ => {
                    let limits = crate::body_retirement::capture_budget::CaptureLimits {
                        duration: std::time::Duration::from_secs(1),
                        growth_bytes: 1 << 20,
                        headroom_bytes: 32 << 20,
                    };
                    let poll =
                        crate::body_retirement::capture_budget::CapturePoll::start(&store, limits)
                            .unwrap();
                    assert!(owned.reconcile_until(&store, || poll.cancelled()).is_err());
                    assert!(poll.finish().unwrap());
                }
            }
            assert!(!owned.is_current(&store).unwrap());
            assert_eq!(counts.block_count(), 2);
            for block in blocks {
                assert!(store.block_store().exists(&block.hash).unwrap());
            }
        }
    }
}
