//! Group related store operations into one atomic LMDB write transaction,
//! sharing the commit and synchronization cost.
//!
//! # Usage
//!
//! ```ignore
//! let mut batch = env.tx_begin_write()?;
//! batch.put_block(&hash, &block_bytes)?;
//! batch.put_frontier(&account, &head)?;
//! batch.put_account_info(&account_info, false)?;
//! batch.commit()?;
//! ```
//!
//! If the batch is dropped without calling [`LmdbWriteTransaction::commit`], all
//! operations are rolled back (the underlying LMDB transaction is aborted).

use heed::{RoTxn, RwTxn};

use burst_store::account::AccountInfo;
use burst_store::{ReadTxn, StoreError, WriteTxn};
use burst_types::{BlockHash, Timestamp, TxHash, WalletAddress};

use crate::block_undo::{BlockUndo, UndoRecorder, UndoTable};
use crate::environment::LmdbEnvironment;
use crate::LmdbError;

pub(crate) mod auxiliary_cache;
pub mod checkpoint_projection;
mod checkpoint_heads;
mod governance_records;
pub use governance_records::{prepare_governance_undo, GovernanceUndoProjection};
mod verification_records;
pub(crate) mod payment_epoch;
pub(crate) mod vote_fence;

pub type BrnWalletEntry<'a> = (WalletAddress, &'a [u8]);

/// A consistent snapshot shared across store reads. Pass it to `*_txn` methods
/// when account information, its frontier and its latest block must come from
/// the same database generation. Separate convenience reads can see different
/// generations if a writer commits between them.
pub struct LmdbReadTransaction<'a> {
    pub(crate) txn: RoTxn<'a>,
    pub(crate) generation: Option<usize>,
    environment: std::sync::Weak<std::sync::Mutex<Option<(usize, auxiliary_cache::AuxiliaryCache)>>>,
}

impl<'a> LmdbReadTransaction<'a> {
    /// ID of the pinned transaction, or None for an unmanaged `from_ro` wrapper.
    /// Never compare generations from different environments.
    pub fn generation(&self) -> Option<usize> {
        self.generation
    }

    pub(crate) fn belongs_to(&self, env: &LmdbEnvironment) -> bool {
        self.environment.ptr_eq(&std::sync::Arc::downgrade(&env.auxiliary_cache))
    }

    /// Detach only this view's identity. The token retains neither the reader
    /// nor the environment and grants no authentication or deletion authority.
    pub fn clean_storage_cut(&self) -> Option<CleanStorageCut> {
        Some(CleanStorageCut {
            environment: self.environment.clone(),
            generation: self.generation?,
        })
    }

    /// Begin a new read transaction against the environment.
    pub(crate) fn new(env: &'a LmdbEnvironment) -> Result<Self, StoreError> {
        let txn = env.env().read_txn().map_err(LmdbError::from)?;
        Ok(Self::from_pinned(env, txn))
    }

    fn from_pinned(env: &LmdbEnvironment, txn: RoTxn<'a>) -> Self {
        // Metadata can become visible before LMDB publishes it to new readers.
        // Only the transaction itself identifies the snapshot it actually pins.
        let generation = Some(txn.id());
        Self { txn, generation, environment: std::sync::Arc::downgrade(&env.auxiliary_cache) }
    }

    /// Wrap an already-opened heed read transaction. Used by self-committing
    /// store methods to delegate to their `*_txn` counterparts.
    pub(crate) fn from_ro(txn: RoTxn<'a>) -> Self {
        Self { txn, generation: None, environment: std::sync::Weak::new() }
    }
}

/// Identity of an unchanged committed view. This carries no authentication or
/// publication authority and retains no database transaction or environment.
#[derive(Clone)]
pub struct CleanStorageCut {
    environment: std::sync::Weak<std::sync::Mutex<Option<(usize, auxiliary_cache::AuxiliaryCache)>>>,
    generation: usize,
}

impl CleanStorageCut {
    /// Exact committed-view identity; even soft-only commits are different views.
    /// This comparison grants no authentication or permission to publish.
    pub fn same_committed_view(&self, other: &Self) -> bool {
        self.generation == other.generation && self.environment.ptr_eq(&other.environment)
    }

    pub(crate) fn matches_generation(&self, env: &LmdbEnvironment, generation: usize) -> bool {
        self.generation == generation
            && self.environment.ptr_eq(&std::sync::Arc::downgrade(&env.auxiliary_cache))
    }

    /// Compare detached views for body-dependency reuse only. Soft sequence
    /// reservations cannot add roots; final votes, gaps and other writes reject.
    /// This does not authenticate either view or authorize publication. A caller
    /// publishing changes must still recheck its proof under the writer lock.
    pub fn body_dependencies_unchanged(
        &self,
        later: &Self,
        env: &LmdbEnvironment,
    ) -> Result<bool, StoreError> {
        let identity = std::sync::Arc::downgrade(&env.auxiliary_cache);
        if !self.environment.ptr_eq(&identity) || !later.environment.ptr_eq(&identity) {
            return Ok(false);
        }
        if self.generation == later.generation { return Ok(true); }
        let commits = env.sequence_commits.lock()
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        Ok(commits.covers(self.generation, later.generation))
    }
}

impl ReadTxn for LmdbReadTransaction<'_> {}

/// Stage related store mutations for one atomic commit. Dropping the transaction
/// uncommitted aborts them. Publish corresponding in-memory state only after
/// [`LmdbWriteTransaction::commit`] succeeds.
pub struct LmdbWriteTransaction<'a> {
    txn: RwTxn<'a>,
    env: &'a LmdbEnvironment,
    undo: Option<UndoRecorder>,
    block_group: Option<block_group::Budget>,
    failed: bool,
    dirty: bool,
    auxiliary_cache: std::cell::RefCell<auxiliary_cache::AuxiliaryCache>,
    base_txn_id: usize,
    changed_value: bool,
    body_changes: Option<crate::retention::changes::Pending>,
    governance_cache: std::cell::RefCell<Option<burst_governance::GovernanceEngine>>,
}

impl ReadTxn for LmdbWriteTransaction<'_> {}
impl WriteTxn for LmdbWriteTransaction<'_> {}

impl<'a> LmdbWriteTransaction<'a> {
    /// Borrow this writer's current rows without exposing mutation operations.
    pub fn read_view(&self) -> crate::read_view::LedgerReadView<'_> {
        crate::read_view::LedgerReadView::new(self.env, &self.txn, self.clean_storage_cut())
    }

    /// Start optional mutation coverage under an untouched exclusive writer.
    /// This observes local bytes only; it is not a replay or retirement proof.
    pub fn watch_body_changes(&self, max_rows: usize, max_bytes: usize)
        -> Result<crate::retention::BodyChangeWatch, StoreError> {
        if self.clean_storage_cut().is_none() {
            return Err(StoreError::Corruption("body change watch requires a clean writer".into()));
        }
        self.env.body_changes.watch(self.base_txn_id, max_rows, max_bytes)
    }

    fn invalidate_body_changes(&mut self) {
        if let Some(changes) = self.body_changes.as_mut() { changes.invalidate(); }
    }

    /// A sealed journal is outside reversible recording, but still contributes
    /// a carrier and local images to optional dependency-change observation.
    fn observe_sealed_journal(&mut self, key: &[u8], bytes: &[u8]) -> Result<(), StoreError> {
        if let Some(changes) = self.body_changes.as_mut() {
            let before = self.env.meta_db.get(&self.txn, key).map_err(LmdbError::from)?;
            if before != Some(bytes) {
                changes.record(UndoTable::Meta, key, before, Some(bytes));
                // The observed journal change advances the native writer.
                self.changed_value = true;
            }
        }
        Ok(())
    }

    /// Capture only an untouched writer. A weak allocation identity prevents
    /// address reuse from matching a different environment after it is closed.
    pub fn clean_storage_cut(&self) -> Option<CleanStorageCut> {
        if self.changed_value || self.require_clean_checkpoint_boundary().is_err() {
            return None;
        }
        Some(CleanStorageCut {
            environment: std::sync::Arc::downgrade(&self.env.auxiliary_cache),
            generation: self.base_txn_id,
        })
    }

    pub fn matches_clean_storage_cut(&self, cut: &CleanStorageCut) -> bool {
        !self.changed_value && self.require_clean_checkpoint_boundary().is_ok()
            && cut.generation == self.base_txn_id
            && cut.environment.ptr_eq(&std::sync::Arc::downgrade(&self.env.auxiliary_cache))
    }

    /// Pin committed TRST rows while this writer excludes intervening commits.
    /// Callers may check their authenticated recovery proof first, then release
    /// the writer before scanning. This method does not authenticate that proof.
    /// Uncommitted edits cannot be represented by the returned read transaction.
    pub fn trst_reference_reader(&self) -> Result<crate::retention::TrstReferenceReader<'a>, StoreError> {
        self.require_clean_checkpoint_boundary()?;
        if self.changed_value {
            return Err(StoreError::Corruption(
                "retention read view requires an unchanged writer".into(),
            ));
        }
        self.env.trst_reference_reader()
    }

    /// Constant-size cut recheck while holding the exclusive writer. Call before
    /// staging publication edits, and publish in this same transaction. Any
    /// intervening commit rejects the old view, including new queue entries or
    /// higher signing sequences. This does not prove dependency closure or grant
    /// deletion authority; it only prevents publishing from a stale storage cut.
    pub fn check_retention_cut(&self, reader: &crate::retention::TrstReferenceReader<'_>) -> Result<(), StoreError> {
        self.require_clean_checkpoint_boundary()?;
        if self.changed_value {
            return Err(StoreError::Corruption("retention cut requires an unchanged writer".into()));
        }
        reader.check_cut(self.env, self.base_txn_id)
    }

    /// Recheck body dependencies, tolerating only a fully covered sequence-only
    /// reservation interval. This never restores signer records; callers retain
    /// newer counters. Final votes, ledger/queue edits and unknown commits reject.
    /// Complete authenticated closure remains the caller's separate obligation.
    pub fn check_body_retention_cut(&self, reader: &crate::retention::TrstReferenceReader<'_>) -> Result<(), StoreError> {
        self.require_clean_checkpoint_boundary()?;
        if self.changed_value {
            return Err(StoreError::Corruption("body retention cut requires an unchanged writer".into()));
        }
        reader.check_body_cut(self.env, self.base_txn_id)
    }

    /// Revalidate an owned body-dependency proof's detached cut before any
    /// publication edits. The cut itself proves neither closure nor authority.
    /// Only fully covered soft sequence commits may intervene; newer counters
    /// stay in this writer and must never be restored from the old proof.
    pub fn check_detached_body_retention_cut(&self, cut: &CleanStorageCut) -> Result<(), StoreError> {
        let current = self.clean_storage_cut().ok_or_else(|| StoreError::Corruption(
            "body retention cut requires an unchanged clean writer".into(),
        ))?;
        if !cut.body_dependencies_unchanged(&current, self.env)? {
            return Err(StoreError::Corruption(
                "body retention dependencies changed or commit coverage is missing".into(),
            ));
        }
        Ok(())
    }

    /// The ledger environment directory. Callers own any scratch files placed
    /// here; LMDB commit and abort do not manage those files.
    pub fn storage_directory(&self) -> &std::path::Path {
        self.env.env().path()
    }

    /// Borrow the physical block inventory from this writer's current view.
    /// Consume the iterator and handle every error to finish validation; creating
    /// it alone checks table counts, not every row.
    pub fn indexed_block_entries(
        &self,
        max_rows: usize,
        max_bytes: usize,
    ) -> Result<
        impl Iterator<Item = Result<crate::block::IndexedBlock<'_>, StoreError>> + '_,
        StoreError,
    > {
        crate::block::inventory::entries(
            &self.txn,
            self.env.blocks_db,
            self.env.height_db,
            self.env.block_height_db,
            max_rows,
            max_bytes,
        )
    }

    /// Borrow a bounded physical hash window from this writer's current view.
    /// Uncommitted writes are visible. The caller must preserve total coverage
    /// and budgets; reaching a suffix end alone proves neither completeness nor finality.
    pub fn visit_indexed_block_window<'view>(
        &'view self, after: Option<BlockHash>, max_rows: usize, max_bytes: usize,
        visit: impl FnMut(crate::block::IndexedBlock<'view>) -> Result<(), StoreError>,
    ) -> Result<crate::block::IndexedBlockWindow, StoreError> {
        self.read_view().visit_indexed_block_window(after, max_rows, max_bytes, visit)
    }

    /// Collect the bounded physical inventory when the caller needs owned bodies.
    pub fn indexed_blocks(
        &self,
        max_rows: usize,
        max_bytes: usize,
    ) -> Result<Vec<checkpoint_projection::CheckpointBlock>, StoreError> {
        crate::block::inventory::read(
            &self.txn,
            self.env.blocks_db,
            self.env.height_db,
            self.env.block_height_db,
            max_rows,
            max_bytes,
        )
    }

    /// Limit this and subsequent local writers until the guard is dropped.
    /// Protected readers must drop before the guard. This bounds new file pages,
    /// not RAM or other processes; callers must handle GrowthLimit by aborting.
    pub fn limit_file_extent(&mut self, bytes: usize) -> Result<heed::FileGrowthLimit, StoreError> {
        self.txn.limit_file_extent(bytes).map_err(LmdbError::from).map_err(StoreError::from)
    }

    /// Live metadata-tree pages in this writer's view, without scanning records.
    /// Excludes retired/COW pages, allocator free lists and other databases;
    /// these counts do not measure remaining transaction allocation capacity.
    pub fn metadata_page_stats(&self) -> Result<heed::DatabaseStat, StoreError> {
        self.env.meta_db.stat(&self.txn)
            .map_err(LmdbError::from).map_err(StoreError::from)
    }

    pub fn growth_outcome(&self) -> Option<heed::GrowthOutcome> { self.txn.growth_outcome() }

    pub fn reserve_growth_retry(&self) -> Result<Option<heed::GrowthRetry>, StoreError> {
        self.txn.reserve_growth_retry().map_err(LmdbError::from).map_err(StoreError::from)
    }

    pub fn try_limit_file_extent(&mut self, bytes: usize) -> Result<Option<heed::FileGrowthLimit>, StoreError> {
        self.txn.try_limit_file_extent(bytes).map_err(LmdbError::from).map_err(StoreError::from)
    }

    /// Begin a new write transaction.
    pub(crate) fn new(env: &'a LmdbEnvironment) -> Result<Self, StoreError> {
        let txn = env.env().write_txn().map_err(LmdbError::from)?;
        // The exclusive LMDB writer pins this committed generation until commit.
        let base_txn_id = env.env().info().last_txn_id;
        let governance_cache = env
            .governance_cache
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(id, _)| *id == base_txn_id)
            .map(|(_, engine)| engine.clone());
        let auxiliary_cache = env
            .auxiliary_cache
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(id, _)| *id == base_txn_id)
            .map(|(_, cache)| cache.clone())
            .unwrap_or_default();
        Ok(Self {
            auxiliary_cache: std::cell::RefCell::new(auxiliary_cache),
            base_txn_id,
            changed_value: false,
            body_changes: env.body_changes.begin(),
            governance_cache: std::cell::RefCell::new(governance_cache),
            txn,
            env,
            undo: None,
            block_group: None,
            failed: false,
            dirty: false,
        })
    }

    /// Start before applying a block. Reversible writes join its journal. In a
    /// block group, calling this again seals the previous journal and starts
    /// another; all blocks still commit together.
    pub fn begin_block_undo(
        &mut self,
        block: BlockHash,
        account: WalletAddress,
        height: u64,
    ) -> Result<(), StoreError> {
        if self.failed
            || ((self.dirty || self.undo.is_some()) && self.block_group.is_none())
            || (self.dirty && self.undo.is_none())
            || block.is_zero()
            || height == 0
            || self
                .env
                .meta_db
                .get(&self.txn, &crate::block_undo::key(&block))
                .map_err(LmdbError::from)?
                .is_some()
            || self
                .env
                .blocks_db
                .get(&self.txn, block.as_bytes())
                .map_err(LmdbError::from)?
                .is_some()
        {
            self.failed = true;
            return Err(StoreError::Corruption(
                "invalid or duplicate block undo boundary".into(),
            ));
        }
        if self.block_group.is_some() {
            if let Err(error) = self.advance_block_group() {
                self.failed = true;
                return Err(error);
            }
            // The previous active journal was not in meta_db during the first
            // duplicate check. Sealing it must not permit reuse of its identity.
            if self.env.meta_db.get(&self.txn, &crate::block_undo::key(&block))
                .map_err(LmdbError::from)?.is_some() {
                self.failed = true;
                return Err(StoreError::Corruption("duplicate grouped block undo boundary".into()));
            }
        }
        self.undo = Some(UndoRecorder::new(block, account, height));
        Ok(())
    }

    pub fn block_undo(&self, block: &BlockHash) -> Result<Option<BlockUndo>, StoreError> {
        self.read_view().block_undo(block)
    }

    /// Restore one unconfirmed account head inside the caller's replacement
    /// transaction. The ledger layer must first walk cross-account dependencies
    /// and call this in descendant-first order. This is not consensus authority.
    ///
    /// Rechecks the journal against this writer snapshot, restores all recorded
    /// values, and removes the consumed journal. A failed check poisons the whole
    /// transaction, including earlier successful rewinds. No engine may be
    /// published until the replacement transaction commits.
    pub fn rewind_block(&mut self, block: &BlockHash) -> Result<(), StoreError> {
        self.rewind_block_with_snapshots(block, |_, _, _, _| {
            Err(StoreError::Corruption(
                "undo conflicts with current state".into(),
            ))
        })
    }

    /// As `rewind_block`, with a conflict-checked reconciler for disjoint TRST
    /// and verification/governance snapshot entries. The hook cannot alter primary indexes,
    /// accounts, or durable consensus state. Any failure still poisons the writer.
    pub fn rewind_block_with_snapshots(
        &mut self,
        block: &BlockHash,
        mut reconcile: impl FnMut(&[u8], &[u8], &[u8], &[u8]) -> Result<Vec<u8>, StoreError>,
    ) -> Result<(), StoreError> {
        let result = (|| {
            let invalid = |message: &str| StoreError::Corruption(message.into());
            if self.failed || self.dirty || self.undo.is_some() || self.block_group.is_some() {
                return Err(invalid("rewind must precede candidate writes"));
            }
            if self.confirmation_pending(block)? {
                return Err(invalid("cannot rewind a durable confirmation decision"));
            }
            let mut record = self
                .block_undo(block)?
                .ok_or_else(|| invalid("rewind requires a historical undo record"))?;
            let account = self.get_account(&record.account)?;
            if account.address != record.account
                || account.head != *block
                || account.block_count != record.height
                || account.confirmation_height >= record.height
                || self.block_height(&record.account, block)? != record.height
            {
                return Err(invalid(
                    "rewind requires an unconfirmed indexed account head",
                ));
            }
            let account_key = record.account.as_str().as_bytes();
            let mut height_key = account_key.to_vec();
            height_key.extend_from_slice(&record.height.to_be_bytes());
            // An incomplete journal must not remove economics while leaving the
            // block reachable through one of its primary indexes.
            let change = |table, key: &[u8]| {
                record
                    .changes
                    .iter()
                    .find(|c| c.table == table && c.key == key)
                    .ok_or_else(|| invalid("rewind journal lacks a primary ledger write"))
            };
            for (table, key) in [
                (UndoTable::Blocks, block.as_bytes().as_slice()),
                (UndoTable::Heights, height_key.as_slice()),
                (UndoTable::BlockHeights, block.as_bytes().as_slice()),
            ] {
                let entry = change(table, key)?;
                if entry.before.is_some() || entry.after.is_none() {
                    return Err(invalid("rewind journal does not describe a new block"));
                }
            }
            let previous = change(UndoTable::Accounts, account_key)?
                .before
                .as_deref()
                .map(bincode::deserialize::<AccountInfo>)
                .transpose()
                .map_err(LmdbError::from)?;
            if previous.as_ref().is_some_and(|a| {
                a.address != record.account
                    || a.block_count != record.height - 1
                    || a.confirmation_height > a.block_count
            }) || (previous.is_none() && record.height != 1)
            {
                return Err(invalid("rewind journal has an invalid predecessor account"));
            }
            if let Some(previous) = previous.as_ref().filter(|a| a.block_count != 0) {
                if self.block_height(&record.account, &previous.head)? != record.height - 1 {
                    return Err(invalid(
                        "rewind predecessor is not on the indexed account chain",
                    ));
                }
            }
            let frontier = change(UndoTable::Frontiers, account_key)?;
            let previous_head = previous
                .as_ref()
                .filter(|a| a.block_count != 0)
                .map(|a| a.head.as_bytes().as_slice());
            if frontier.after.as_deref() != Some(block.as_bytes().as_slice())
                || frontier.before.as_deref() != previous_head
            {
                return Err(invalid("rewind journal has an invalid frontier transition"));
            }
            for change in &mut record.changes {
                change.reconcile_current_with_snapshots(
                    self.undo_value(change.table, &change.key)?,
                    &mut reconcile,
                )?;
            }
            let trst_rewind = self.prepare_trst_rewind(&record.changes)?;
            for change in &record.changes {
                if change.table == UndoTable::BrnMeta
                    && change.key == b"governance_engine"
                    && self.uses_governance_records()?
                {
                    self.replace_governance_undo_snapshot(change.before.as_deref(), false)?;
                } else {
                    self.mutate_raw(change.table, &change.key, change.before.as_deref())?;
                }
            }
            self.delete_raw(UndoTable::Meta, &crate::block_undo::key(block))?;
            self.finish_trst_rewind(trst_rewind)?;
            // A completed rewind is a new recording boundary. The replacement
            // journal must start at the common ancestor, not the removed branch.
            self.dirty = false;
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Read the writer's current rollback image. Verification and governance
    /// snapshots may be reconstructed from normalized rows rather than copied
    /// from one physical key.
    pub fn undo_value(&self, table: UndoTable, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        if table == UndoTable::Meta && key == b"verification_orchestrator" && self.uses_verification_records()? {
            return self.verification_undo_snapshot_with_overlay(&Default::default(), 64 << 20);
        }
        if table == UndoTable::Meta && self.uses_virtual_verification_target(key)? {
            return self.verification_target_undo_with_overlay(
                std::str::from_utf8(key).map_err(|e| StoreError::Corruption(e.to_string()))?,
                &Default::default(), 64 << 20);
        }
        if table == UndoTable::BrnMeta
            && key == b"governance_engine"
            && self.uses_governance_records()?
        {
            return self.governance_undo_snapshot();
        }
        self.undo_value_ref(table, key)
            .map(|value| value.map(<[u8]>::to_vec))
    }

    /// Borrow bytes from this writer; the borrow cannot survive a mutation.
    pub fn undo_value_ref(
        &self,
        table: UndoTable,
        key: &[u8],
    ) -> Result<Option<&[u8]>, StoreError> {
        self.read_view().undo_value_ref(table, key)
    }

    /// Decode one account at a time from this writer's uncommitted view.
    pub fn account_entries(
        &self,
    ) -> Result<impl Iterator<Item = Result<AccountInfo, StoreError>> + '_, StoreError> {
        self.read_view().account_entries()
    }

    pub fn bounded_accounts(&self, limit: usize) -> Result<Vec<AccountInfo>, StoreError> {
        let mut accounts = Vec::new();
        for account in self.account_entries()? {
            if accounts.len() == limit {
                return Err(StoreError::Corruption(
                    "account scan requires incremental traversal".into(),
                ));
            }
            accounts.push(account?);
        }
        Ok(accounts)
    }

    /// Bounded restoration reads from this transaction, never a separate reader
    /// that would still see the losing branch during replacement.
    pub fn bounded_brn_entries(
        &self,
        limit: usize,
        byte_limit: usize,
    ) -> Result<impl Iterator<Item = Result<BrnWalletEntry<'_>, StoreError>> + '_, StoreError> {
        self.read_view().bounded_brn_entries(limit, byte_limit)
    }

    pub fn ledger_counts(&self) -> Result<(u64, u64, u64), StoreError> {
        Ok((
            self.env.blocks_db.len(&self.txn).map_err(LmdbError::from)?,
            self.env
                .accounts_db
                .len(&self.txn)
                .map_err(LmdbError::from)?,
            self.env
                .pending_db
                .len(&self.txn)
                .map_err(LmdbError::from)?,
        ))
    }

    pub fn block_at_height(
        &self,
        account: &WalletAddress,
        height: u64,
    ) -> Result<Option<BlockHash>, StoreError> {
        self.read_view().block_at_height(account, height)
    }

    /// Remove a journal only when the block's indexed height is already confirmed
    /// in this writer's view. The caller establishes consensus finality.
    pub fn prune_cemented_undo(&mut self, block: &BlockHash) -> Result<(), StoreError> {
        if let Some(record) = self.block_undo(block)? {
            let account = self.get_account(&record.account)?;
            if self.block_height(&record.account, block)? != record.height
                || account.confirmation_height < record.height
            {
                self.failed = true;
                return Err(StoreError::Corruption(
                    "cannot prune unconfirmed undo record".into(),
                ));
            }
            self.delete_raw(UndoTable::Meta, &crate::block_undo::key(block))?;
        }
        Ok(())
    }

    /// The ledger caller must authenticate the cached payload's account and
    /// predecessor. Only a cemented account position permits discarding it;
    /// elapsed time and an in-memory election result are not sufficient.
    pub fn prune_finalized_fork_candidate(
        &mut self,
        hash: &BlockHash,
        account: &WalletAddress,
        previous: &BlockHash,
    ) -> Result<bool, StoreError> {
        if self.failed || self.undo.is_some() {
            self.failed = true;
            return Err(StoreError::Corruption(
                "fork cleanup cannot join provisional application".into(),
            ));
        }
        let info = match self.get_account(account) {
            Ok(info) => info,
            Err(StoreError::NotFound(_)) => return Ok(false),
            Err(error) => return Err(error),
        };
        let height = if previous.is_zero() {
            1
        } else {
            match self.block_height(account, previous) {
                Ok(height) => height
                    .checked_add(1)
                    .ok_or_else(|| StoreError::Corruption("fork height overflow".into()))?,
                // Missing history is not evidence that this candidate lost.
                Err(StoreError::NotFound(_)) => return Ok(false),
                Err(error) => return Err(error),
            }
        };
        if height > info.confirmation_height {
            return Ok(false);
        }
        if info.address != *account
            || info.confirmation_height > info.block_count
            || self.block_at_height(account, height)?.is_none()
        {
            self.failed = true;
            return Err(StoreError::Corruption(
                "fork cleanup has an invalid confirmed index".into(),
            ));
        }
        let key = crate::fork_candidate::key(hash);
        if self
            .env
            .meta_db
            .get(&self.txn, &key)
            .map_err(LmdbError::from)?
            .is_none()
        {
            return Ok(false);
        }
        self.delete_raw(UndoTable::Meta, &key)?;
        self.delete_raw(UndoTable::Meta, &crate::fork_candidate::content_key(hash))?;
        Ok(true)
    }

    fn mutate_raw(
        &mut self,
        table: UndoTable,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> Result<(), StoreError> {
        let result = (|| {
            if self.undo.is_some() && table == UndoTable::BrnMeta && key == burst_brn::wallet_storage::FORMAT_KEY {
                return Err(StoreError::Corruption("wallet format cannot change inside block undo".into()));
            }
            if table == UndoTable::Meta && key == b"verification_orchestrator" && self.uses_verification_records()? {
                return self.replace_verification_undo_snapshot(value);
            }
            if table == UndoTable::Meta && self.uses_virtual_verification_target(key)? {
                return self.replace_verification_target_undo(key, value);
            }
            if table == UndoTable::BrnMeta
                && key == b"governance_engine"
                && self.uses_governance_records()?
            {
                return self.replace_governance_undo_snapshot(value, true);
            }
            let db = table.database(self.env);
            let before = db.get(&self.txn, key).map_err(LmdbError::from)?;
            let changed = before != value;
            if changed {
                if let Some(changes) = self.body_changes.as_mut() { changes.record(table, key, before, value); }
                self.auxiliary_cache.get_mut().mark_counter_account(table, key);
                self.auxiliary_cache.get_mut().mark_pending_expiry(table, key);
                self.changed_value = true;
                self.auxiliary_cache.get_mut().update_brn(table, key, before, value);
                if table == UndoTable::Meta {
                    self.auxiliary_cache.get_mut().update_confirmations(key, value);
                    self.auxiliary_cache.get_mut().update_trst(key, before, value);
                    self.auxiliary_cache.get_mut().invalidate(key);
                }
                if (table == UndoTable::Meta
                    && (key.starts_with(b"governance_state/")
                        || key == b"governance_records_format"))
                    || (table == UndoTable::BrnMeta && key == b"governance_engine")
                {
                    self.governance_cache.get_mut().take();
                    self.auxiliary_cache.get_mut().invalidate_projection();
                }
            }
            if let Some(undo) = self.undo.as_mut() {
                if before == value {
                    return Ok(());
                }
                undo.record(table, key, before, value)?;
            }
            self.dirty = true;
            if table == UndoTable::Accounts {
                if crate::verified_index::update(&mut self.txn, self.env.accounts_db, self.env.verified_accounts_db, key, value)? {
                    self.auxiliary_cache.get_mut().clear_electorate(table);
                }
            }
            if table == UndoTable::Pending {
                crate::pending::update_pending_indexes(
                    &mut self.txn,
                    self.env.pending_db,
                    self.env.pending_sender_db,
                    self.env.pending_amount_db,
                    key,
                    value,
                )?;
            }
            if table == UndoTable::Meta {
                crate::graph_position::update(
                    &mut self.txn,
                    self.env.meta_db,
                    self.env.graph_position_db,
                    key,
                    value,
                )?;
            }
            // Keep secondary-index validation/update semantics above, but do
            // not dirty the primary LMDB page for byte-identical publication.
            if !changed {
                return Ok(());
            }
            match value {
                Some(value) => db.put(&mut self.txn, key, value).map_err(LmdbError::from)?,
                None => {
                    db.delete(&mut self.txn, key).map_err(LmdbError::from)?;
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn put_raw(&mut self, table: UndoTable, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.mutate_raw(table, key, Some(value))
    }

    fn delete_raw(&mut self, table: UndoTable, key: &[u8]) -> Result<(), StoreError> {
        self.mutate_raw(table, key, None)
    }

    /// Return one past the greatest surviving edge position, or zero if empty.
    /// Interior gaps remain; a removed highest position can be reused.
    pub fn next_graph_edge_position(
        &self,
        parent: &TxHash,
        downstream: bool,
    ) -> Result<u64, StoreError> {
        crate::graph_position::next_position(
            &self.txn,
            self.env.meta_db,
            self.env.graph_position_db,
            parent,
            downstream,
        )
    }

    pub fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError> {
        self.read_view().get_block(hash)
    }

    /// Check both height indexes in the same snapshot as the write transaction.
    pub fn block_height(
        &self,
        account: &WalletAddress,
        hash: &BlockHash,
    ) -> Result<u64, StoreError> {
        self.read_view().block_height(account, hash)
    }

    pub fn get_account(&self, account: &WalletAddress) -> Result<AccountInfo, StoreError> {
        self.read_view().get_account(account)
    }

    pub fn verified_account_count(&self) -> Result<u64, StoreError> {
        match self
            .env
            .meta_db
            .get(&self.txn, b"verified_count")
            .map_err(LmdbError::from)?
        {
            None => Err(StoreError::Corruption(
                "verified account count missing".into(),
            )),
            Some(bytes) => bytes
                .try_into()
                .map(u64::from_be_bytes)
                .map_err(|_| StoreError::Corruption("invalid verified account count".into())),
        }
    }

    pub fn verified_accounts(&self) -> Result<Vec<WalletAddress>, StoreError> {
        let accounts = crate::verified_index::addresses(&self.txn, self.env.verified_accounts_db)?;
        if accounts.len() > u32::MAX as usize {
            return Err(StoreError::Corruption(
                "governance electorate exceeds supported size".into(),
            ));
        }
        Ok(accounts)
    }

    /// Record a caller-validated confirmation height, preserving other account
    /// fields from this writer's view. Never decreases the height or advances it
    /// beyond the account's block count; does not authenticate consensus.
    pub fn advance_confirmation_height(
        &mut self,
        account: &WalletAddress,
        height: u64,
    ) -> Result<(), StoreError> {
        let mut info = self.get_account(account)?;
        if height > info.block_count {
            return Err(StoreError::Corruption(
                "confirmation exceeds account height".into(),
            ));
        }
        info.confirmation_height = info.confirmation_height.max(height);
        let bytes = bincode::serialize(&info).map_err(LmdbError::from)?;
        self.put_account(account, &bytes)
    }

    /// Put a block into the batch (raw bytes, keyed by block hash).
    pub fn put_block(&mut self, hash: &BlockHash, block_bytes: &[u8]) -> Result<(), StoreError> {
        self.put_raw(UndoTable::Blocks, hash.as_bytes(), block_bytes)?;
        Ok(())
    }

    /// Put a block and update the height indexes.
    ///
    /// `height` should be the block's sequence number in the account chain
    /// (1-based). Callers can derive this from `AccountInfo.block_count + 1`
    /// for existing accounts, or `1` for the open block. This avoids an
    /// expensive reverse range scan of `height_db` on every block.
    pub fn put_block_with_account(
        &mut self,
        hash: &BlockHash,
        block_bytes: &[u8],
        account: &WalletAddress,
        height: u64,
    ) -> Result<(), StoreError> {
        self.put_raw(UndoTable::Blocks, hash.as_bytes(), block_bytes)?;

        let mut height_key = account.as_str().as_bytes().to_vec();
        height_key.extend_from_slice(&height.to_be_bytes());
        self.put_raw(UndoTable::Heights, &height_key, hash.as_bytes())?;

        self.put_raw(UndoTable::BlockHeights, hash.as_bytes(), &height_key)?;

        Ok(())
    }

    /// Delete a block from the store.
    pub fn delete_block(&mut self, hash: &BlockHash) -> Result<(), StoreError> {
        if let Some(key) = self
            .env
            .block_height_db
            .get(&self.txn, hash.as_bytes())
            .map_err(LmdbError::from)?
            .map(<[u8]>::to_vec)
        {
            if self
                .env
                .height_db
                .get(&self.txn, &key)
                .map_err(LmdbError::from)?
                == Some(hash.as_bytes().as_slice())
            {
                self.delete_raw(UndoTable::Heights, &key)?;
            }
            self.delete_raw(UndoTable::BlockHeights, hash.as_bytes())?;
        }
        self.delete_raw(UndoTable::Blocks, hash.as_bytes().as_slice())?;
        Ok(())
    }

    /// Store the 32-byte frontier hash for this account.
    pub fn put_frontier(
        &mut self,
        account: &WalletAddress,
        head: &BlockHash,
    ) -> Result<(), StoreError> {
        self.put_raw(
            UndoTable::Frontiers,
            account.as_str().as_bytes(),
            head.as_bytes(),
        )?;
        Ok(())
    }

    /// Delete a frontier entry.
    pub fn delete_frontier(&mut self, account: &WalletAddress) -> Result<(), StoreError> {
        self.delete_raw(UndoTable::Frontiers, account.as_str().as_bytes())?;
        Ok(())
    }

    /// Store serialized account information and update the verified-address index.
    /// Unlike `put_account_info`, this does not adjust the verified-human counter
    /// or preserve an existing confirmation height.
    pub fn put_account(&mut self, address: &WalletAddress, data: &[u8]) -> Result<(), StoreError> {
        self.put_raw(UndoTable::Accounts, address.as_str().as_bytes(), data)?;
        Ok(())
    }

    /// Put an [`AccountInfo`] into the batch, serialising it automatically.
    ///
    /// Maintains the verified-human counter and preserves confirmation height.
    /// Previous state is read inside this transaction; the legacy caller hint
    /// is ignored because it can be stale. Challenged humans remain verified.
    pub fn put_account_info(
        &mut self,
        info: &AccountInfo,
        _was_verified: bool,
    ) -> Result<(), StoreError> {
        let old = self
            .env
            .accounts_db
            .get(&self.txn, info.address.as_str().as_bytes())
            .map_err(LmdbError::from)?
            .map(bincode::deserialize::<AccountInfo>)
            .transpose()
            .map_err(LmdbError::from)?;
        let was_verified = old.as_ref().is_some_and(|a| a.state.is_verified());
        let mut info = info.clone();
        if let Some(old) = old {
            info.confirmation_height = info.confirmation_height.max(old.confirmation_height);
        }
        let bytes = bincode::serialize(&info).map_err(LmdbError::from)?;
        self.put_raw(
            UndoTable::Accounts,
            info.address.as_str().as_bytes(),
            &bytes,
        )?;

        let now_verified = info.state.is_verified();
        if was_verified != now_verified {
            let count = self.verified_account_count()?;
            let new_count = if now_verified {
                count.saturating_add(1)
            } else {
                count.saturating_sub(1)
            };
            self.put_raw(UndoTable::Meta, b"verified_count", &new_count.to_be_bytes())?;
        }
        Ok(())
    }

    /// Put a transaction into the batch (raw bytes, keyed by tx hash).
    pub fn put_transaction(&mut self, hash: &TxHash, tx_bytes: &[u8]) -> Result<(), StoreError> {
        self.put_raw(UndoTable::Transactions, hash.as_bytes(), tx_bytes)?;
        Ok(())
    }

    /// Put a transaction and update the per-account index using composite key.
    pub fn put_transaction_with_account(
        &mut self,
        hash: &TxHash,
        tx_bytes: &[u8],
        account: &WalletAddress,
    ) -> Result<(), StoreError> {
        self.put_raw(UndoTable::Transactions, hash.as_bytes(), tx_bytes)?;

        let mut account_tx_key = account.as_str().as_bytes().to_vec();
        account_tx_key.extend_from_slice(hash.as_bytes());
        self.put_raw(UndoTable::AccountTransactions, &account_tx_key, &[])?;
        Ok(())
    }

    /// Delete transaction bytes. The per-account transaction index is unchanged.
    pub fn delete_transaction(&mut self, hash: &TxHash) -> Result<(), StoreError> {
        self.delete_raw(UndoTable::Transactions, hash.as_bytes().as_slice())?;
        Ok(())
    }

    /// Put a pending entry into the batch using binary composite key.
    pub fn put_pending(
        &mut self,
        destination: &WalletAddress,
        source_hash_bytes: &[u8; 32],
        data: &[u8],
    ) -> Result<(), StoreError> {
        let key = crate::pending::pending_key_raw(destination, source_hash_bytes);
        self.put_raw(UndoTable::Pending, &key, data)?;
        Ok(())
    }

    /// Delete a pending entry using binary composite key.
    pub fn delete_pending(
        &mut self,
        destination: &WalletAddress,
        source_hash_bytes: &[u8; 32],
    ) -> Result<(), StoreError> {
        let key = crate::pending::pending_key_raw(destination, source_hash_bytes);
        self.delete_raw(UndoTable::Pending, &key)?;
        Ok(())
    }

    /// Remove the handoff only in the same transaction as completed cementation.
    pub fn acknowledge_confirmation(&mut self, hash: &BlockHash) -> Result<(), StoreError> {
        self.delete_raw(UndoTable::Meta, &crate::confirmation_queue::key(hash))?;
        Ok(())
    }

    pub fn confirmation_pending(&self, hash: &BlockHash) -> Result<bool, StoreError> {
        match self
            .env
            .meta_db
            .get(&self.txn, &crate::confirmation_queue::key(hash))
            .map_err(LmdbError::from)?
        {
            None => Ok(false),
            Some([1]) => Ok(true),
            Some(_) => Err(StoreError::Corruption(
                "invalid confirmation queue record".into(),
            )),
        }
    }

    /// Check block presence and persist the consensus handoff in this writer.
    /// A separate existence check can race branch replacement.
    pub fn handoff_confirmation(&mut self, hash: &BlockHash) -> Result<(), StoreError> {
        let result = (|| {
            if self.undo.is_some() || hash.is_zero() {
                return Err(StoreError::Corruption(
                    "confirmation handoff requires a completed candidate".into(),
                ));
            }
            self.get_block(hash)?;
            if self.confirmation_pending(hash)? {
                return Ok(());
            }
            if self.confirmation_queue_len()? >= crate::confirmation_queue::CAPACITY {
                return Err(StoreError::Backend("confirmation queue full".into()));
            }
            self.put_raw(UndoTable::Meta, &crate::confirmation_queue::key(hash), &[1])
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    pub fn put_brn_wallet(
        &mut self,
        address: &WalletAddress,
        value: &[u8],
    ) -> Result<(), StoreError> {
        if self.uses_brn_wallet_records()? {
            let state = burst_brn::BrnWalletState::from_persisted_bytes(value)
                .map_err(|error| StoreError::Serialization(error.to_string()))?;
            return self.persist_brn_wallet_records(address, Some(&state));
        }
        self.put_raw(UndoTable::BrnWallets, address.as_str().as_bytes(), value)?;
        Ok(())
    }

    pub fn delete_brn_wallet(&mut self, address: &WalletAddress) -> Result<(), StoreError> {
        if self.uses_brn_wallet_records()? {
            return self.persist_brn_wallet_records(address, None);
        }
        self.delete_raw(UndoTable::BrnWallets, address.as_str().as_bytes())
    }

    pub fn delete_brn_meta(&mut self, key: &[u8]) -> Result<(), StoreError> {
        self.delete_raw(UndoTable::BrnMeta, key)
    }

    pub fn put_brn_meta(&mut self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.put_raw(UndoTable::BrnMeta, key, value)?;
        Ok(())
    }

    /// Put a meta key/value pair into the batch.
    pub fn put_meta(&mut self, key: &str, value: &[u8]) -> Result<(), StoreError> {
        self.put_raw(UndoTable::Meta, key.as_bytes(), value)?;
        Ok(())
    }

    pub fn delete_meta(&mut self, key: &str) -> Result<(), StoreError> {
        self.delete_raw(UndoTable::Meta, key.as_bytes())
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.read_view().get_meta(key)
    }

    /// Check presence in this writer's view without copying the stored value.
    pub fn contains_meta(&self, key: &str) -> Result<bool, StoreError> {
        self.read_view().contains_meta(key)
    }

    pub fn get_pending(
        &self,
        destination: &WalletAddress,
        source: &TxHash,
    ) -> Result<burst_store::pending::PendingInfo, StoreError> {
        self.read_view().get_pending(destination, source)
    }

    /// Decode and collect every pending transfer in this writer's view.
    /// This method has no row or aggregate byte limit.
    pub fn all_pending(
        &self,
    ) -> Result<Vec<(WalletAddress, TxHash, burst_store::pending::PendingInfo)>, StoreError> {
        let mut pending = Vec::new();
        for entry in self
            .env
            .pending_db
            .iter(&self.txn)
            .map_err(LmdbError::from)?
        {
            let (key, bytes) = entry.map_err(LmdbError::from)?;
            pending.push(crate::pending::decode_pending_entry(key, bytes)?);
        }
        Ok(pending)
    }

    /// Record a TRST origin index entry and its reverse mapping.
    pub fn put_origin_index(
        &mut self,
        origin_hash: &TxHash,
        tx_hash: &TxHash,
    ) -> Result<(), StoreError> {
        let mut key = [0u8; 64];
        key[..32].copy_from_slice(origin_hash.as_bytes());
        key[32..].copy_from_slice(tx_hash.as_bytes());
        self.put_raw(UndoTable::TrstOrigins, &key[..], &[])?;

        let mut reverse_entry = match self
            .env
            .trst_reverse_db
            .get(&self.txn, tx_hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?
        {
            Some(bytes) => bytes.to_vec(),
            None => vec![0u8; 40],
        };
        if reverse_entry.len() >= 32 {
            reverse_entry[..32].copy_from_slice(origin_hash.as_bytes());
        }
        self.put_raw(
            UndoTable::TrstReverse,
            tx_hash.as_bytes().as_slice(),
            &reverse_entry,
        )?;
        Ok(())
    }

    /// Record a TRST expiry index entry and update the reverse mapping.
    pub fn put_expiry_index(
        &mut self,
        expiry: Timestamp,
        tx_hash: &TxHash,
    ) -> Result<(), StoreError> {
        let mut key = [0u8; 40];
        key[..8].copy_from_slice(&expiry.as_secs().to_be_bytes());
        key[8..].copy_from_slice(tx_hash.as_bytes());
        self.put_raw(UndoTable::TrstExpiry, &key[..], &[])?;

        let mut reverse_entry = match self
            .env
            .trst_reverse_db
            .get(&self.txn, tx_hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?
        {
            Some(bytes) => bytes.to_vec(),
            None => vec![0u8; 40],
        };
        if reverse_entry.len() >= 40 {
            reverse_entry[32..40].copy_from_slice(&expiry.as_secs().to_be_bytes());
        }
        self.put_raw(
            UndoTable::TrstReverse,
            tx_hash.as_bytes().as_slice(),
            &reverse_entry,
        )?;
        Ok(())
    }

    /// Commit only if the candidate has its own atomic undo boundary. Used by
    /// branch replacement so an application callback cannot omit journaling or
    /// accidentally record the removed branch as the candidate's predecessor.
    pub fn commit_block(
        self,
        expected: &BlockHash,
        account: &WalletAddress,
        height: u64,
    ) -> Result<(), StoreError> {
        if self.block_group.is_some() || !self
            .undo
            .as_ref()
            .is_some_and(|undo| undo.matches(expected, account, height))
        {
            return Err(StoreError::Corruption(
                "candidate undo boundary missing or mismatched".into(),
            ));
        }
        self.commit()
    }

    /// Install an elected replacement and its durable cementation handoff
    /// together. The local decision is intentionally outside the undo journal.
    pub fn commit_elected_block(
        mut self,
        expected: &BlockHash,
        account: &WalletAddress,
        height: u64,
    ) -> Result<(), StoreError> {
        if self.failed
            || self.block_group.is_some()
            || !self
                .undo
                .as_ref()
                .is_some_and(|undo| undo.matches(expected, account, height))
        {
            return Err(StoreError::Corruption(
                "invalid elected candidate boundary".into(),
            ));
        }
        let (key, bytes) = self.undo.take().unwrap().finish()?;
        self.observe_sealed_journal(&key, &bytes)?;
        self.env
            .meta_db
            .put(&mut self.txn, &key, &bytes)
            .map_err(LmdbError::from)?;
        self.handoff_confirmation(expected)?;
        self.commit()
    }

    /// Attach metadata only while applying this exact uncommitted block.
    /// Content sidecars must not be added after a block was committed or under
    /// another block's undo boundary. Failure poisons all staged writes.
    pub fn put_block_meta(
        &mut self,
        block: &BlockHash,
        account: &WalletAddress,
        height: u64,
        key: &str,
        value: &[u8],
    ) -> Result<(), StoreError> {
        if self.failed
            || !self
                .undo
                .as_ref()
                .is_some_and(|undo| undo.matches(block, account, height))
        {
            self.failed = true;
            return Err(StoreError::Corruption(
                "block metadata requires its admission boundary".into(),
            ));
        }
        self.put_meta(key, value)
    }

    /// Commit a tracked change and return this transaction's identity, captured
    /// while its exclusive writer is still held. Never observe an environment
    /// generation after commit: that could identify an unrelated later writer.
    /// The token proves identity only, not what changed or deletion authority.
    /// Empty/untracked transactions must use `commit` and cannot mint this token.
    pub fn commit_with_storage_cut(self) -> Result<CleanStorageCut, StoreError> {
        if !self.changed_value || self.base_txn_id.checked_add(1) != Some(self.txn.id()) {
            return Err(StoreError::Corruption(
                "committed storage cut requires a tracked advancing transaction".into(),
            ));
        }
        let cut = CleanStorageCut {
            environment: std::sync::Arc::downgrade(&self.env.auxiliary_cache),
            generation: self.txn.id(),
        };
        self.commit()?;
        Ok(cut)
    }

    /// Seal the active undo journal and commit the batch, then publish its caches.
    /// Durability follows the environment's LMDB flags; this method does not add
    /// a separate sync barrier.
    pub fn commit(mut self) -> Result<(), StoreError> {
        #[cfg(test)]
        let probe = COMMIT_PROBE.with(|slot| slot.borrow().is_some())
            .then(std::time::Instant::now);
        if self.failed {
            return Err(StoreError::Corruption(
                "cannot commit a failed write batch".into(),
            ));
        }
        if self.block_group.is_some() && self.undo.is_none() {
            return Err(StoreError::Corruption("block group has no active journal".into()));
        }
        self.seal_block_undo()?;
        let cached = self.governance_cache.into_inner();
        let auxiliary = self.auxiliary_cache.into_inner();
        // Only a proven changed value guarantees that LMDB advances its ID.
        // Untracked direct writes can cause a conservative miss, never a hit
        // tagged with an unobserved later writer's generation.
        let cache_id = self.base_txn_id + usize::from(self.changed_value);
        let next_id = if self.changed_value { self.txn.id() } else { self.base_txn_id };
        let commit = || self.txn.commit().map_err(LmdbError::from).map_err(StoreError::from);
        if let Some(changes) = self.body_changes {
            changes.commit(self.base_txn_id, next_id, commit)?;
        } else { commit()?; }
        #[cfg(test)]
        let database_ns = probe.map(|start| start.elapsed().as_nanos());
        if !auxiliary.is_empty() {
            let mut cache = self.env.auxiliary_cache.lock().unwrap();
            if cache.as_ref().is_none_or(|(id, _)| *id <= cache_id) {
                *cache = Some((cache_id, auxiliary));
            }
        }
        #[cfg(test)]
        let auxiliary_ns = probe.map(|start| start.elapsed().as_nanos());
        if let Some(engine) = cached {
            let mut cache = self.env.governance_cache.lock().unwrap();
            if cache.as_ref().is_none_or(|(id, _)| *id <= cache_id) {
                *cache = Some((cache_id, engine));
            }
        }
        #[cfg(test)]
        if let Some(start) = probe {
            let database_ns = database_ns.unwrap();
            let auxiliary_ns = auxiliary_ns.unwrap();
            COMMIT_PROBE.with(|slot| slot.borrow_mut().as_mut().unwrap().push([
                database_ns,
                auxiliary_ns - database_ns,
                start.elapsed().as_nanos() - auxiliary_ns,
            ]));
        }
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    static COMMIT_PROBE: std::cell::RefCell<Option<Vec<[u128; 3]>>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(test)]
fn probe_commit(batch: LmdbWriteTransaction<'_>) -> [u128; 3] {
    COMMIT_PROBE.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some(Vec::new());
    });
    let result = batch.commit();
    let samples = COMMIT_PROBE.with(|slot| slot.borrow_mut().take().unwrap());
    result.unwrap();
    assert_eq!(samples.len(), 1);
    samples[0]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LmdbEnvironment;
    use burst_store::block::BlockStore;
    use burst_store::frontier::FrontierStore;

    fn temp_env() -> (tempfile::TempDir, LmdbEnvironment) {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let env =
            LmdbEnvironment::open(dir.path(), crate::REQUIRED_DATABASES, 10 * 1024 * 1024).expect("failed to open env");
        (dir, env)
    }

    #[test]
    fn committed_storage_cut_identifies_only_its_own_successful_writer() {
        let (_directory, env) = temp_env();
        let mut writer = env.tx_begin_write().unwrap();
        writer.put_meta("owned_cut", b"first").unwrap();
        let native_id = writer.txn.id();
        let cut = writer.commit_with_storage_cut().unwrap();
        assert_eq!(cut.generation, native_id);
        env.tx_begin_write().unwrap().check_detached_body_retention_cut(&cut).unwrap();

        // A later writer must not become part of the returned proof, even if it
        // restores the same bytes. Each cut retains its own native generation.
        let mut writer = env.tx_begin_write().unwrap();
        writer.put_meta("owned_cut", b"second").unwrap();
        writer.put_meta("owned_cut", b"first").unwrap();
        let later = writer.commit_with_storage_cut().unwrap();
        let current = env.tx_begin_write().unwrap();
        assert!(current.matches_clean_storage_cut(&later));
        assert!(current.check_detached_body_retention_cut(&cut).is_err());
        drop(current);
        let (_other_directory, other) = temp_env();
        assert!(other.tx_begin_write().unwrap().check_detached_body_retention_cut(&later).is_err());
    }

    #[test]
    fn committed_storage_cut_rejects_empty_untracked_and_failed_transactions() {
        let (_directory, env) = temp_env();
        let before = env.tx_begin_read().unwrap().clean_storage_cut().unwrap();
        assert!(env.tx_begin_write().unwrap().commit_with_storage_cut().is_err());
        let mut writer = env.tx_begin_write().unwrap();
        writer.delete_meta("absent_cut_key").unwrap();
        assert!(writer.commit_with_storage_cut().is_err());
        assert!(env.tx_begin_write().unwrap().matches_clean_storage_cut(&before));

        let mut writer = env.tx_begin_write().unwrap();
        env.meta_db.put(&mut writer.txn, b"untracked_cut", b"raw").unwrap();
        assert!(writer.commit_with_storage_cut().is_err());
        let current = env.tx_begin_write().unwrap();
        assert!(!current.contains_meta("untracked_cut").unwrap());
        assert!(current.matches_clean_storage_cut(&before));
        drop(current);

        let mut writer = env.tx_begin_write().unwrap();
        writer.put_meta("failed_cut", b"uncommitted").unwrap();
        writer.failed = true;
        assert!(writer.commit_with_storage_cut().is_err());
        let current = env.tx_begin_write().unwrap();
        assert!(!current.contains_meta("failed_cut").unwrap());
        assert!(current.matches_clean_storage_cut(&before));
    }

    #[test]
    fn borrowed_economics_reads_preserve_writer_view_and_inventory_limits() {
        let (_dir, env) = temp_env();
        let mut batch = LmdbWriteTransaction::new(&env).unwrap();
        let first = WalletAddress::new("brst_a");
        let second = WalletAddress::new("brst_b");
        batch.put_brn_wallet(&first, b"one").unwrap();
        assert!(!batch.contains_meta("merger_graph").unwrap());
        batch.put_meta("merger_graph", b"before").unwrap();
        assert!(batch.contains_meta("merger_graph").unwrap());
        batch.put_meta("empty", b"").unwrap();
        assert!(batch.contains_meta("empty").unwrap());
        batch.delete_meta("empty").unwrap();
        assert!(!batch.contains_meta("empty").unwrap());
        assert_eq!(
            batch
                .undo_value_ref(UndoTable::Meta, b"merger_graph")
                .unwrap(),
            Some(b"before".as_slice())
        );
        batch.put_meta("merger_graph", b"after").unwrap();
        assert_eq!(
            batch
                .undo_value_ref(UndoTable::Meta, b"merger_graph")
                .unwrap(),
            Some(b"after".as_slice())
        );
        let budget = first.as_str().len() + 3;
        assert_eq!(
            batch
                .bounded_brn_entries(1, budget)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .1,
            b"one"
        );
        assert!(batch
            .bounded_brn_entries(0, budget)
            .unwrap()
            .next()
            .unwrap()
            .is_err());
        assert!(batch
            .bounded_brn_entries(1, budget - 1)
            .unwrap()
            .next()
            .unwrap()
            .is_err());
        batch.put_brn_wallet(&second, b"two").unwrap();
        assert!(batch
            .bounded_brn_entries(1, 2 * budget)
            .unwrap()
            .nth(1)
            .unwrap()
            .is_err());
        assert!(batch
            .bounded_brn_entries(2, budget)
            .unwrap()
            .nth(1)
            .unwrap()
            .is_err());
        assert_eq!(
            batch
                .bounded_brn_entries(2, 2 * budget)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
                .len(),
            2
        );
        batch
            .env
            .brn_wallets_db
            .put(&mut batch.txn, b"invalid", b"bad")
            .unwrap();
        assert!(batch
            .bounded_brn_entries(3, 100)
            .unwrap()
            .nth(2)
            .unwrap()
            .is_err());
        drop(batch);
        let batch = LmdbWriteTransaction::new(&env).unwrap();
        assert!(batch
            .undo_value_ref(UndoTable::Meta, b"merger_graph")
            .unwrap()
            .is_none());
        assert_eq!(batch.bounded_brn_entries(2, 100).unwrap().count(), 0);
        assert!(!batch.contains_meta("merger_graph").unwrap());
    }

    #[test]
    fn account_entries_use_writer_view_and_propagate_corruption() {
        let (_dir, env) = temp_env();
        let mut batch = LmdbWriteTransaction::new(&env).unwrap();
        let mut account = AccountInfo::unopened(WalletAddress::new("brst_stream"));
        batch.put_account_info(&account, false).unwrap();
        assert_eq!(batch.account_entries().unwrap().count(), 1);
        assert!(batch.bounded_accounts(0).is_err());
        account.block_count = 7;
        account.confirmation_height = 6;
        batch.put_account_info(&account, false).unwrap();
        let read = batch.account_entries().unwrap().next().unwrap().unwrap();
        assert_eq!(read.confirmation_height, 6);
        assert_eq!(read.block_count, 7);
        for bytes in [
            vec![0xff],
            bincode::serialize(&AccountInfo::unopened(WalletAddress::new("brst_wrong_key")))
                .unwrap(),
            {
                account.confirmation_height = 8;
                bincode::serialize(&account).unwrap()
            },
        ] {
            env.accounts_db
                .put(&mut batch.txn, account.address.as_str().as_bytes(), &bytes)
                .unwrap();
            assert!(batch.account_entries().unwrap().next().unwrap().is_err());
        }
        drop(batch);
        assert_eq!(
            LmdbWriteTransaction::new(&env)
                .unwrap()
                .account_entries()
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn undo_records_every_written_table_and_survives_restart() {
        use burst_store::pending::{PendingInfo, PendingStore};
        use std::collections::BTreeMap;
        let (dir, env) = temp_env();
        let hash = BlockHash::new([61; 32]);
        let account = WalletAddress::new("brst_undo_test");
        let removed_brn = WalletAddress::new("brst_deleted_brn_test");
        let tables = [
            UndoTable::Blocks,
            UndoTable::Heights,
            UndoTable::BlockHeights,
            UndoTable::Frontiers,
            UndoTable::Accounts,
            UndoTable::Transactions,
            UndoTable::AccountTransactions,
            UndoTable::Pending,
            UndoTable::Meta,
            UndoTable::BrnWallets,
            UndoTable::BrnMeta,
            UndoTable::TrstOrigins,
            UndoTable::TrstExpiry,
            UndoTable::TrstReverse,
        ];
        let snapshot = |env: &LmdbEnvironment| {
            let txn = env.env().read_txn().unwrap();
            let mut rows = BTreeMap::new();
            for table in tables {
                for row in table.database(env).iter(&txn).unwrap() {
                    let (key, value) = row.unwrap();
                    if table == UndoTable::Meta && key.starts_with(crate::block_undo::PREFIX) {
                        continue;
                    }
                    rows.insert((table, key.to_vec()), value.to_vec());
                }
            }
            rows
        };
        let mut seed = env.tx_begin_write().unwrap();
        let pending_bytes = |amount| {
            bincode::serialize(&PendingInfo {
                return_credit: false,
                source: account.clone(),
                amount,
                timestamp: Timestamp::new(1),
                provenance: Vec::new(),
            })
            .unwrap()
        };
        seed.put_pending(&account, hash.as_bytes(), &pending_bytes(10))
            .unwrap();
        seed.put_meta("engine", b"original engine").unwrap();
        seed.put_brn_wallet(&removed_brn, b"original BRN wallet")
            .unwrap();
        seed.commit().unwrap();
        let before = snapshot(&env);
        let mut batch = env.tx_begin_write().unwrap();
        batch.begin_block_undo(hash, account.clone(), 1).unwrap();
        batch
            .put_block_with_account(&hash, b"block", &account, 1)
            .unwrap();
        batch.put_frontier(&account, &hash).unwrap();
        let info = AccountInfo {
            address: account.clone(),
            state: burst_types::WalletState::Verified,
            verified_at: None,
            head: hash,
            block_count: 1,
            confirmation_height: 0,
            representative: account.clone(),
            total_brn_burned: 0,
            total_brn_staked: 0,
            trst_balance: 0,
            expired_trst: 0,
            revoked_trst: 0,
            epoch: 0,
            verifier_opted_in_at: None,
            orv_evicted: false,
        };
        batch
            .put_account(&account, &bincode::serialize(&info).unwrap())
            .unwrap();
        let tx = TxHash::new([62; 32]);
        batch
            .put_transaction_with_account(&tx, b"transaction", &account)
            .unwrap();
        batch.delete_pending(&account, hash.as_bytes()).unwrap();
        batch
            .put_pending(&account, tx.as_bytes(), &pending_bytes(20))
            .unwrap();
        batch.put_meta("engine", b"intermediate engine").unwrap();
        batch.put_meta("engine", b"final engine").unwrap();
        batch.put_brn_wallet(&account, b"brn wallet").unwrap();
        batch.delete_brn_wallet(&removed_brn).unwrap();
        batch.put_brn_meta(b"rates", b"brn rates").unwrap();
        batch.put_origin_index(&TxHash::new([63; 32]), &tx).unwrap();
        batch.put_expiry_index(Timestamp::new(50), &tx).unwrap();
        batch.commit().unwrap();
        let after = snapshot(&env);
        let outgoing = env
            .pending_store()
            .get_pending_from_account(&account)
            .unwrap();
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].1, tx);
        assert_eq!(outgoing[0].2.amount, 20);
        drop(env);
        let env = LmdbEnvironment::open(dir.path(), crate::REQUIRED_DATABASES, 10 * 1024 * 1024).unwrap();
        let txn = env.tx_begin_write().unwrap();
        assert_eq!(txn.verified_accounts().unwrap(), vec![account.clone()]);
        let record = txn.block_undo(&hash).unwrap().unwrap();
        assert_eq!(record.account, account);
        assert_eq!(record.height, 1);
        let recorded: BTreeMap<_, _> = record
            .changes
            .iter()
            .map(|c| {
                (
                    (c.table, c.key.clone()),
                    (c.before.clone(), c.after.clone()),
                )
            })
            .collect();
        let keys: std::collections::BTreeSet<_> =
            before.keys().chain(after.keys()).cloned().collect();
        let expected: BTreeMap<_, _> = keys
            .into_iter()
            .filter_map(|key| {
                let old = before.get(&key).cloned();
                let new = after.get(&key).cloned();
                (old != new).then_some((key, (old, new)))
            })
            .collect();
        assert_eq!(recorded, expected);
        assert_eq!(
            record
                .changes
                .iter()
                .map(|c| c.table)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            tables.len()
        );
        drop(txn);
        let mut batch = env.tx_begin_write().unwrap();
        batch.rewind_block(&hash).unwrap();
        assert!(batch.verified_accounts().unwrap().is_empty());
        batch.commit().unwrap();
        assert_eq!(snapshot(&env), before);
        assert!(env.tx_begin_write().unwrap().verified_accounts().unwrap().is_empty());
        let outgoing = env
            .pending_store()
            .get_pending_from_account(&account)
            .unwrap();
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].1.as_bytes(), hash.as_bytes());
        assert_eq!(outgoing[0].2.amount, 10);
        drop(env);
        let env = LmdbEnvironment::open(dir.path(), crate::REQUIRED_DATABASES, 10 * 1024 * 1024).unwrap();
        assert_eq!(snapshot(&env), before);
        assert!(env.tx_begin_write().unwrap().verified_accounts().unwrap().is_empty());
        assert!(env
            .tx_begin_write()
            .unwrap()
            .block_undo(&hash)
            .unwrap()
            .is_none());
    }

    #[test]
    fn undo_and_state_abort_together_and_late_recording_is_rejected() {
        let (_dir, env) = temp_env();
        let hash = BlockHash::new([71; 32]);
        let account = WalletAddress::new("brst_test");
        {
            let mut batch = env.tx_begin_write().unwrap();
            batch.begin_block_undo(hash, account.clone(), 1).unwrap();
            batch.put_block(&hash, b"aborted").unwrap();
        }
        let mut batch = env.tx_begin_write().unwrap();
        assert!(batch.block_undo(&hash).unwrap().is_none());
        assert!(batch.get_block(&hash).is_err());
        batch.put_block(&hash, b"not recorded").unwrap();
        assert!(batch.begin_block_undo(hash, account, 1).is_err());
        assert!(batch.commit().is_err());
        assert!(env.block_store().get_block(&hash).is_err());
    }

    #[test]
    fn identical_publication_preserves_lmdb_generation_and_index_guards() {
        let (_directory, env) = temp_env();
        let account = AccountInfo::unopened(WalletAddress::new("brst_identical_publication"));
        let mut writer = env.tx_begin_write().unwrap();
        writer.put_account_info(&account, false).unwrap();
        writer.put_meta("verification_identity_source/v1/test", &[7; 32]).unwrap();
        writer.put_brn_meta(b"publication-test", b"same").unwrap();
        writer.commit().unwrap();
        let generation = env.env().info().last_txn_id;
        let mut writer = env.tx_begin_write().unwrap();
        writer.put_account_info(&account, false).unwrap();
        writer.put_meta("verification_identity_source/v1/test", &[7; 32]).unwrap();
        writer.put_brn_meta(b"publication-test", b"same").unwrap();
        writer.delete_meta("absent-publication-test").unwrap();
        assert!(writer.dirty, "attempted writes still invalidate clean-writer guards");
        assert!(!writer.changed_value);
        writer.commit().unwrap();
        assert_eq!(env.env().info().last_txn_id, generation,
            "identical primary values must not create a durable writer generation");

        let mut writer = env.tx_begin_write().unwrap();
        writer.put_meta("verification_identity_source/v1/test", &[8; 32]).unwrap();
        writer.commit().unwrap();
        assert!(env.env().info().last_txn_id > generation);
        let mut writer = env.tx_begin_write().unwrap();
        writer.put_meta("verification_identity_source/v1/test", &[9; 32]).unwrap();
        drop(writer);
        let mut writer = env.tx_begin_write().unwrap();
        assert_eq!(writer.get_meta("verification_identity_source/v1/test").unwrap(), Some(vec![8; 32]));
        // Equality of the primary row must not hide a corrupt membership index.
        env.verified_accounts_db.put(&mut writer.txn, account.address.as_str().as_bytes(), &[1]).unwrap();
        assert!(writer.put_account_info(&account, false).unwrap_err().to_string().contains("verified index disagrees"));
        assert!(writer.commit().is_err());
        let writer = env.tx_begin_write().unwrap();
        assert!(env.verified_accounts_db.get(&writer.txn, account.address.as_str().as_bytes()).unwrap().is_none());
    }

    #[test]
    fn no_op_changes_are_omitted_and_journal_overwrite_poisoned() {
        let (_dir, env) = temp_env();
        let hash = BlockHash::new([72; 32]);
        let account = WalletAddress::new("brst_test");
        let mut batch = env.tx_begin_write().unwrap();
        batch.begin_block_undo(hash, account.clone(), 1).unwrap();
        batch.put_block(&hash, b"block").unwrap();
        batch.put_frontier(&account, &hash).unwrap();
        batch.delete_frontier(&account).unwrap();
        batch.put_meta("unchanged", b"same").unwrap();
        batch.put_meta("unchanged", b"same").unwrap();
        batch.commit().unwrap();
        let mut batch = env.tx_begin_write().unwrap();
        let record = batch.block_undo(&hash).unwrap().unwrap();
        assert_eq!(record.changes.len(), 2);
        assert!(!record
            .changes
            .iter()
            .any(|c| c.table == UndoTable::Frontiers));
        assert!(batch.begin_block_undo(hash, account, 1).is_err());
        assert!(batch.commit().is_err());
        let mut batch = env.tx_begin_write().unwrap();
        batch
            .begin_block_undo(
                BlockHash::new([73; 32]),
                WalletAddress::new("brst_other"),
                1,
            )
            .unwrap();
        assert!(batch
            .put_raw(
                UndoTable::Meta,
                &crate::block_undo::key(&hash),
                b"overwrite"
            )
            .is_err());
        assert!(batch.commit().is_err());
        assert_eq!(
            env.tx_begin_write()
                .unwrap()
                .block_undo(&hash)
                .unwrap()
                .unwrap(),
            record
        );
    }

    #[test]
    fn batch_put_block_and_frontier_committed() {
        let (_dir, env) = temp_env();

        let hash = BlockHash::new([1u8; 32]);
        let account = WalletAddress::new(
            "brst_1genesis1ive1111111111111111111111111111111111111111111111111111111",
        );
        let block_bytes = b"fake-block-data";

        let mut batch = env.tx_begin_write().expect("write_batch");
        batch.put_block(&hash, block_bytes).expect("put_block");
        batch.put_frontier(&account, &hash).expect("put_frontier");
        batch.commit().expect("commit");

        let block_store = env.block_store();
        let stored = block_store.get_block(&hash).expect("get_block");
        assert_eq!(stored, block_bytes);

        let frontier_store = env.frontier_store();
        let head = frontier_store.get_frontier(&account).expect("get_frontier");
        assert_eq!(head, hash);
    }

    #[test]
    fn dropped_batch_does_not_persist() {
        let (_dir, env) = temp_env();

        let hash = BlockHash::new([2u8; 32]);
        let block_bytes = b"should-not-persist";

        {
            let mut batch = env.tx_begin_write().expect("write_batch");
            batch.put_block(&hash, block_bytes).expect("put_block");
        }

        let block_store = env.block_store();
        let result = block_store.get_block(&hash);
        assert!(result.is_err(), "dropped batch should not persist");
    }

    #[test]
    fn batch_multiple_blocks() {
        let (_dir, env) = temp_env();

        let mut batch = env.tx_begin_write().expect("write_batch");

        let hashes: Vec<BlockHash> = (0..10)
            .map(|i| {
                let mut bytes = [0u8; 32];
                bytes[0] = i;
                BlockHash::new(bytes)
            })
            .collect();

        for (i, hash) in hashes.iter().enumerate() {
            let data = format!("block-{i}");
            batch.put_block(hash, data.as_bytes()).expect("put_block");
        }

        batch.commit().expect("commit");

        let block_store = env.block_store();
        for (i, hash) in hashes.iter().enumerate() {
            let stored = block_store.get_block(hash).expect("get_block");
            assert_eq!(stored, format!("block-{i}").as_bytes());
        }
    }

    #[test]
    fn checkpoint_account_clear_replaces_electorate_atomically() {
        let (_dir, env) = temp_env();
        let mut original = AccountInfo::unopened(WalletAddress::new("brst_original"));
        original.state = burst_types::WalletState::Verified;
        let mut seed = env.tx_begin_write().unwrap();
        seed.put_account_info(&original, false).unwrap();
        seed.commit().unwrap();
        let mut replacement = AccountInfo::unopened(WalletAddress::new("brst_replacement"));
        replacement.state = burst_types::WalletState::Challenged;
        for commit in [false, true] {
            let mut batch = env.tx_begin_write().unwrap();
            batch.clear_checkpoint_rows(UndoTable::Accounts, None).unwrap();
            assert!(batch.verified_accounts().unwrap().is_empty());
            batch.put_account(&replacement.address, &bincode::serialize(&replacement).unwrap()).unwrap();
            assert_eq!(batch.verified_accounts().unwrap(), vec![replacement.address.clone()]);
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
            }
            assert_eq!(
                env.tx_begin_write().unwrap().verified_accounts().unwrap(),
                vec![if commit { replacement.address.clone() } else { original.address.clone() }]
            );
        }
    }

    #[test]
    fn batch_put_account() {
        let (_dir, env) = temp_env();

        let address = WalletAddress::new(
            "brst_1genesis1ive1111111111111111111111111111111111111111111111111111111",
        );
        let data = bincode::serialize(&AccountInfo::unopened(address.clone())).unwrap();

        let mut batch = env.tx_begin_write().expect("write_batch");
        batch.put_account(&address, &data).expect("put_account");
        batch.commit().expect("commit");

        let rtxn = env.env().read_txn().expect("read_txn");
        let stored = env
            .accounts_db
            .get(&rtxn, address.as_str().as_bytes())
            .expect("get")
            .expect("account should exist");
        assert_eq!(stored, data);
    }

    #[test]
    fn batch_put_account_info() {
        let (_dir, env) = temp_env();

        let info = AccountInfo {
            address: WalletAddress::new(
                "brst_1genesis1ive1111111111111111111111111111111111111111111111111111111",
            ),
            state: burst_types::WalletState::Unverified,
            verified_at: None,
            head: BlockHash::new([0u8; 32]),
            block_count: 1,
            confirmation_height: 0,
            representative: WalletAddress::new(
                "brst_1genesis1ive1111111111111111111111111111111111111111111111111111111",
            ),
            total_brn_burned: 0,
            trst_balance: 0,
            total_brn_staked: 0,
            expired_trst: 0,
            revoked_trst: 0,
            epoch: 0,
            verifier_opted_in_at: None,
            orv_evicted: false,
        };

        let mut batch = env.tx_begin_write().expect("write_batch");
        batch
            .put_account_info(&info, false)
            .expect("put_account_info");
        batch.commit().expect("commit");

        use burst_store::account::AccountStore;
        let store = env.account_store();
        let loaded = store.get_account(&info.address).expect("get_account");
        assert_eq!(loaded.address, info.address);
        assert_eq!(loaded.block_count, 1);
        store.advance_confirmation_height(&info.address, 1).unwrap();
        // A block processor snapshot read before cementation must not undo it.
        let mut successor = info.clone();
        successor.block_count = 2;
        successor.head = BlockHash::new([2; 32]);
        let mut batch = env.tx_begin_write().unwrap();
        batch.put_account_info(&successor, false).unwrap();
        batch.commit().unwrap();
        store.advance_confirmation_height(&info.address, 1).unwrap();
        let loaded = store.get_account(&info.address).unwrap();
        assert_eq!(loaded.head, successor.head);
        assert_eq!(loaded.block_count, 2);
        assert_eq!(loaded.confirmation_height, 1);
        assert!(store.advance_confirmation_height(&info.address, 3).is_err());
        store.put_account(&successor).unwrap();
        assert_eq!(
            store
                .get_account(&info.address)
                .unwrap()
                .confirmation_height,
            1
        );
    }

    #[test]
    fn batch_put_pending() {
        let (_dir, env) = temp_env();

        let dest = WalletAddress::new(
            "brst_1genesis1ive1111111111111111111111111111111111111111111111111111111",
        );
        let source_hash = [0xABu8; 32];
        let data = bincode::serialize(&burst_store::pending::PendingInfo {
            return_credit: false,
            source: WalletAddress::new("brst_sender"),
            amount: 5,
            timestamp: burst_types::Timestamp::new(1),
            provenance: Vec::new(),
        })
        .unwrap();

        let mut batch = env.tx_begin_write().expect("write_batch");
        batch
            .put_pending(&dest, &source_hash, &data)
            .expect("put_pending");
        batch.commit().expect("commit");

        let key = crate::pending::pending_key_raw(&dest, &source_hash);
        let rtxn = env.env().read_txn().expect("read_txn");
        let stored = env
            .pending_db
            .get(&rtxn, &key)
            .expect("get")
            .expect("pending should exist");
        assert_eq!(stored, data.as_slice());
    }

    #[test]
    fn batch_delete_block() {
        let (_dir, env) = temp_env();

        let hash = BlockHash::new([3u8; 32]);
        let block_bytes = b"to-be-deleted";

        let mut batch = env.tx_begin_write().expect("write_batch");
        batch.put_block(&hash, block_bytes).expect("put_block");
        batch.commit().expect("commit");

        let mut batch = env.tx_begin_write().expect("write_batch");
        batch.delete_block(&hash).expect("delete_block");
        batch.commit().expect("commit");

        let block_store = env.block_store();
        assert!(block_store.get_block(&hash).is_err());
    }

    #[test]
    fn batch_put_meta() {
        let (_dir, env) = temp_env();

        let mut batch = env.tx_begin_write().expect("write_batch");
        batch.put_meta("schema_version", b"42").expect("put_meta");
        batch.commit().expect("commit");

        let rtxn = env.env().read_txn().expect("read_txn");
        let stored = env
            .meta_db
            .get(&rtxn, "schema_version".as_bytes())
            .expect("get")
            .expect("meta should exist");
        assert_eq!(stored, b"42");
    }

    #[test]
    fn batch_delete_removes_both_height_indexes_atomically() {
        let (_dir, env) = temp_env();
        let hash = BlockHash::new([42; 32]);
        let account = WalletAddress::new("brst_test");
        let mut batch = env.tx_begin_write().unwrap();
        batch
            .put_block_with_account(&hash, b"block", &account, 1)
            .unwrap();
        batch.commit().unwrap();
        // Aborted deletion must retain the block AND both index entries.
        {
            let mut batch = env.tx_begin_write().unwrap();
            batch.delete_block(&hash).unwrap();
        }
        let blocks = env.block_store();
        assert!(blocks.exists(&hash).unwrap());
        assert_eq!(blocks.block_at_height(&account, 1).unwrap(), Some(hash));
        assert_eq!(blocks.height_of_block(&hash).unwrap(), Some(1));
        let mut batch = env.tx_begin_write().unwrap();
        batch.delete_block(&hash).unwrap();
        batch.commit().unwrap();
        assert!(!blocks.exists(&hash).unwrap());
        assert_eq!(blocks.block_at_height(&account, 1).unwrap(), None);
        assert_eq!(blocks.height_of_block(&hash).unwrap(), None);
        assert!(blocks.get_account_blocks(&account).unwrap().is_empty());
    }

    #[test]
    fn deleting_old_block_does_not_delete_replacement_height_index() {
        let (_dir, env) = temp_env();
        let old = BlockHash::new([1; 32]);
        let replacement = BlockHash::new([2; 32]);
        let account = WalletAddress::new("brst_test");
        for batched in [true, false] {
            let mut batch = env.tx_begin_write().unwrap();
            batch
                .put_block_with_account(&old, b"old", &account, 1)
                .unwrap();
            batch
                .put_block_with_account(&replacement, b"replacement", &account, 1)
                .unwrap();
            batch.commit().unwrap();
            if batched {
                let mut batch = env.tx_begin_write().unwrap();
                batch.delete_block(&old).unwrap();
                batch.commit().unwrap();
            } else {
                env.block_store().delete_block(&old).unwrap();
            }
            assert_eq!(
                env.block_store().block_at_height(&account, 1).unwrap(),
                Some(replacement)
            );
            assert_eq!(env.block_store().height_of_block(&old).unwrap(), None);
            assert_eq!(
                env.block_store().height_of_block(&replacement).unwrap(),
                Some(1)
            );
            assert!(env.block_store().exists(&replacement).unwrap());
        }
    }
}

mod block_group;

impl crate::read_view::LedgerReadView<'_> {
    pub fn block_at_height(
        &self,
        account: &WalletAddress,
        height: u64,
    ) -> Result<Option<BlockHash>, StoreError> {
        let mut key = account.as_str().as_bytes().to_vec();
        key.extend_from_slice(&height.to_be_bytes());
        self.env
            .height_db
            .get(&self.txn, &key)
            .map_err(LmdbError::from)?
            .map(|bytes| {
                bytes
                    .try_into()
                    .map(BlockHash::new)
                    .map_err(|_| StoreError::Corruption("invalid height index".into()))
            })
            .transpose()
    }

    pub fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError> {
        self.env
            .blocks_db
            .get(&self.txn, hash.as_bytes())
            .map_err(LmdbError::from)?
            .map(<[u8]>::to_vec)
            .ok_or_else(|| StoreError::NotFound(hash.to_string()))
    }

    pub fn block_height(
        &self,
        account: &WalletAddress,
        hash: &BlockHash,
    ) -> Result<u64, StoreError> {
        let key = self
            .env
            .block_height_db
            .get(&self.txn, hash.as_bytes())
            .map_err(LmdbError::from)?
            .ok_or_else(|| StoreError::NotFound(hash.to_string()))?;
        let prefix = account.as_str().as_bytes();
        if key.len() != prefix.len() + 8
            || !key.starts_with(prefix)
            || self
                .env
                .height_db
                .get(&self.txn, key)
                .map_err(LmdbError::from)?
                != Some(hash.as_bytes().as_slice())
        {
            return Err(StoreError::Corruption(
                "block is not on the indexed account chain".into(),
            ));
        }
        Ok(u64::from_be_bytes(key[prefix.len()..].try_into().unwrap()))
    }

    pub fn get_account(&self, account: &WalletAddress) -> Result<AccountInfo, StoreError> {
        let bytes = self
            .env
            .accounts_db
            .get(&self.txn, account.as_str().as_bytes())
            .map_err(LmdbError::from)?
            .ok_or_else(|| StoreError::NotFound(account.to_string()))?;
        bincode::deserialize(bytes).map_err(|error| StoreError::Serialization(error.to_string()))
    }

    pub fn get_pending(
        &self,
        destination: &WalletAddress,
        source: &TxHash,
    ) -> Result<burst_store::pending::PendingInfo, StoreError> {
        let key = crate::pending::pending_key_raw(destination, source.as_bytes());
        let bytes = self
            .env
            .pending_db
            .get(&self.txn, &key)
            .map_err(LmdbError::from)?
            .ok_or_else(|| StoreError::NotFound(source.to_string()))?;
        bincode::deserialize(bytes)
            .map_err(LmdbError::from)
            .map_err(Into::into)
    }

    pub fn block_undo(&self, block: &BlockHash) -> Result<Option<BlockUndo>, StoreError> {
        self.env
            .meta_db
            .get(&self.txn, &crate::block_undo::key(block))
            .map_err(LmdbError::from)?
            .map(|bytes| BlockUndo::decode(block, bytes))
            .transpose()
    }
}
