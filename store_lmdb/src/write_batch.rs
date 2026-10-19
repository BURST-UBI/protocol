//! Write batching — groups multiple store operations into a single LMDB write
//! transaction, amortising the cost of the fsync that each commit performs.
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

pub mod checkpoint_projection;
pub(crate) mod payment_epoch;
pub(crate) mod vote_fence;

pub type BrnWalletEntry<'a> = (WalletAddress, &'a [u8]);

/// A caller-owned read-only LMDB transaction: a consistent snapshot across many
/// store reads. Threaded through store `*_txn` read methods (rsnano model) so a
/// composite read (e.g. account info + its frontier + latest block) sees one
/// atomic view. The self-committing trait methods each open a short-lived one
/// internally via [`LmdbReadTransaction::from_ro`].
pub struct LmdbReadTransaction<'a> {
    pub(crate) txn: RoTxn<'a>,
}

impl<'a> LmdbReadTransaction<'a> {
    /// Begin a new read transaction against the environment.
    pub(crate) fn new(env: &'a LmdbEnvironment) -> Result<Self, StoreError> {
        let txn = env.env().read_txn().map_err(LmdbError::from)?;
        Ok(Self { txn })
    }

    /// Wrap an already-opened heed read transaction. Used by self-committing
    /// store methods to delegate to their `*_txn` counterparts.
    pub(crate) fn from_ro(txn: RoTxn<'a>) -> Self {
        Self { txn }
    }
}

impl ReadTxn for LmdbReadTransaction<'_> {}

/// A caller-owned LMDB write transaction: all mutations made through it commit
/// atomically on [`LmdbWriteTransaction::commit`], or roll back if it is dropped
/// uncommitted. This is the primitive the rsnano-style port threads through
/// store methods so an entire logical operation (e.g. applying a block: ledger
/// + BRN + rep-weights + governance) commits as one transaction.
///
/// It also amortises fsync cost — a single commit per transaction.
pub struct LmdbWriteTransaction<'a> {
    txn: RwTxn<'a>,
    env: &'a LmdbEnvironment,
    undo: Option<UndoRecorder>,
    failed: bool,
    dirty: bool,
}

impl ReadTxn for LmdbWriteTransaction<'_> {}
impl WriteTxn for LmdbWriteTransaction<'_> {}

impl<'a> LmdbWriteTransaction<'a> {
    /// Directory for transaction-scoped scratch files, separate from ledger rows.
    pub fn storage_directory(&self) -> &std::path::Path {
        self.env.env().path()
    }

    /// Recheck the physical block inventory inside the caller's atomic boundary.
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

    /// Begin a new write transaction.
    pub(crate) fn new(env: &'a LmdbEnvironment) -> Result<Self, StoreError> {
        let txn = env.env().write_txn().map_err(LmdbError::from)?;
        Ok(Self {
            txn,
            env,
            undo: None,
            failed: false,
            dirty: false,
        })
    }

    /// Start before the first write of a block's application. All subsequent
    /// batch writes join the same local undo record and commit atomically.
    pub fn begin_block_undo(
        &mut self,
        block: BlockHash,
        account: WalletAddress,
        height: u64,
    ) -> Result<(), StoreError> {
        if self.failed
            || self.dirty
            || self.undo.is_some()
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
        self.undo = Some(UndoRecorder::new(block, account, height));
        Ok(())
    }

    pub fn block_undo(&self, block: &BlockHash) -> Result<Option<BlockUndo>, StoreError> {
        self.env
            .meta_db
            .get(&self.txn, &crate::block_undo::key(block))
            .map_err(LmdbError::from)?
            .map(|bytes| BlockUndo::decode(block, bytes))
            .transpose()
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
            if self.failed || self.dirty || self.undo.is_some() {
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
                change.match_current_with_snapshots(
                    self.undo_value(change.table, &change.key)?,
                    &mut reconcile,
                )?;
            }
            for change in &record.changes {
                self.mutate_raw(change.table, &change.key, change.before.as_deref())?;
            }
            self.delete_raw(UndoTable::Meta, &crate::block_undo::key(block))?;
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

    /// Current bytes in the same snapshot as a rollback preflight.
    pub fn undo_value(&self, table: UndoTable, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        self.undo_value_ref(table, key)
            .map(|value| value.map(<[u8]>::to_vec))
    }

    /// Borrow bytes from this writer; the borrow cannot survive a mutation.
    pub fn undo_value_ref(
        &self,
        table: UndoTable,
        key: &[u8],
    ) -> Result<Option<&[u8]>, StoreError> {
        table
            .database(self.env)
            .get(&self.txn, key)
            .map_err(LmdbError::from)
            .map_err(Into::into)
    }

    /// Decode one account at a time from this writer's uncommitted view.
    pub fn account_entries(
        &self,
    ) -> Result<impl Iterator<Item = Result<AccountInfo, StoreError>> + '_, StoreError> {
        let entries = self
            .env
            .accounts_db
            .iter(&self.txn)
            .map_err(LmdbError::from)?;
        Ok(entries.map(|entry| {
            let (key, bytes) = entry.map_err(LmdbError::from)?;
            crate::account::decode_account_entry(key, bytes)
        }))
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
        let mut count = 0usize;
        let mut total = 0usize;
        let rows = self
            .env
            .brn_wallets_db
            .iter(&self.txn)
            .map_err(LmdbError::from)?;
        Ok(rows.map(move |row| {
            let (key, bytes) = row.map_err(LmdbError::from)?;
            total = total
                .checked_add(key.len())
                .and_then(|n| n.checked_add(bytes.len()))
                .ok_or_else(|| StoreError::Corruption("BRN snapshot size overflow".into()))?;
            if count == limit || total > byte_limit {
                return Err(StoreError::Corruption(
                    "BRN snapshot exceeds restoration limit".into(),
                ));
            }
            let address = crate::brn::decode_wallet_address(key)?;
            count += 1;
            Ok((address, bytes))
        }))
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

    /// Prune only after this transaction has made the block irreversible.
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
            let db = table.database(self.env);
            if let Some(undo) = self.undo.as_mut() {
                let before = db.get(&self.txn, key).map_err(LmdbError::from)?;
                if before == value {
                    return Ok(());
                }
                undo.record(table, key, before.map(<[u8]>::to_vec), value)?;
            }
            self.dirty = true;
            if table == UndoTable::Pending {
                crate::pending::update_sender_index(
                    &mut self.txn,
                    self.env.pending_db,
                    self.env.pending_sender_db,
                    key,
                    value,
                )?;
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

    // ── Block operations ────────────────────────────────────────────────

    pub fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError> {
        self.env
            .blocks_db
            .get(&self.txn, hash.as_bytes())
            .map_err(LmdbError::from)?
            .map(<[u8]>::to_vec)
            .ok_or_else(|| StoreError::NotFound(hash.to_string()))
    }

    /// Check both height indexes in the same snapshot as the write transaction.
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
        let mut accounts = Vec::new();
        for entry in self
            .env
            .accounts_db
            .iter(&self.txn)
            .map_err(LmdbError::from)?
        {
            let (_, bytes) = entry.map_err(LmdbError::from)?;
            let account: AccountInfo = bincode::deserialize(bytes).map_err(LmdbError::from)?;
            if account.state.is_verified() {
                accounts.push(account.address);
            }
        }
        if accounts.len() > u32::MAX as usize {
            return Err(StoreError::Corruption(
                "governance electorate exceeds supported size".into(),
            ));
        }
        Ok(accounts)
    }

    /// Advance finality without replacing concurrently updated account fields.
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

        let mut hk = account.as_str().as_bytes().to_vec();
        hk.extend_from_slice(&height.to_be_bytes());
        self.put_raw(UndoTable::Heights, &hk, hash.as_bytes())?;

        self.put_raw(UndoTable::BlockHeights, hash.as_bytes(), &hk)?;

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

    // ── Frontier operations ─────────────────────────────────────────────

    /// Put a frontier update into the batch.
    ///
    /// Mirrors [`LmdbFrontierStore::put_frontier`] — stores the 32-byte hash directly.
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

    // ── Account operations ──────────────────────────────────────────────

    /// Put an account info into the batch (pre-serialised bytes).
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

    // ── Transaction operations ──────────────────────────────────────────

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

        let mut ck = account.as_str().as_bytes().to_vec();
        ck.extend_from_slice(hash.as_bytes());
        self.put_raw(UndoTable::AccountTransactions, &ck, &[])?;
        Ok(())
    }

    /// Delete a transaction from the store.
    pub fn delete_transaction(&mut self, hash: &TxHash) -> Result<(), StoreError> {
        self.delete_raw(UndoTable::Transactions, hash.as_bytes().as_slice())?;
        Ok(())
    }

    // ── Pending operations ──────────────────────────────────────────────

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

    // ── Meta operations ─────────────────────────────────────────────────

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
            for (count, entry) in self
                .env
                .meta_db
                .prefix_iter(&self.txn, crate::confirmation_queue::PREFIX)
                .map_err(LmdbError::from)?
                .enumerate()
            {
                entry.map_err(LmdbError::from)?;
                if count >= crate::confirmation_queue::CAPACITY - 1 {
                    return Err(StoreError::Backend("confirmation queue full".into()));
                }
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
        self.put_raw(UndoTable::BrnWallets, address.as_str().as_bytes(), value)?;
        Ok(())
    }

    pub fn delete_brn_wallet(&mut self, address: &WalletAddress) -> Result<(), StoreError> {
        self.delete_raw(UndoTable::BrnWallets, address.as_str().as_bytes())
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
        self.env
            .meta_db
            .get(&self.txn, key.as_bytes())
            .map(|value| value.map(|bytes| bytes.to_vec()))
            .map_err(|e| StoreError::Backend(e.to_string()))
    }

    // ── TRST index operations ─────────────────────────────────────────

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
            if key.len() <= 32 {
                return Err(StoreError::Corruption("invalid pending key".into()));
            }
            let (address, hash) = key.split_at(key.len() - 32);
            let address =
                std::str::from_utf8(address).map_err(|e| StoreError::Corruption(e.to_string()))?;
            if !address.starts_with("brst_") {
                return Err(StoreError::Corruption("invalid pending destination".into()));
            }
            let mut source = [0; 32];
            source.copy_from_slice(hash);
            let info = bincode::deserialize(bytes).map_err(LmdbError::from)?;
            pending.push((WalletAddress::new(address), TxHash::new(source), info));
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

        let mut rev_val = match self
            .env
            .trst_reverse_db
            .get(&self.txn, tx_hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?
        {
            Some(bytes) => bytes.to_vec(),
            None => vec![0u8; 40],
        };
        if rev_val.len() >= 32 {
            rev_val[..32].copy_from_slice(origin_hash.as_bytes());
        }
        self.put_raw(
            UndoTable::TrstReverse,
            tx_hash.as_bytes().as_slice(),
            &rev_val,
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

        let mut rev_val = match self
            .env
            .trst_reverse_db
            .get(&self.txn, tx_hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?
        {
            Some(bytes) => bytes.to_vec(),
            None => vec![0u8; 40],
        };
        if rev_val.len() >= 40 {
            rev_val[32..40].copy_from_slice(&expiry.as_secs().to_be_bytes());
        }
        self.put_raw(
            UndoTable::TrstReverse,
            tx_hash.as_bytes().as_slice(),
            &rev_val,
        )?;
        Ok(())
    }

    // ── Commit / rollback ───────────────────────────────────────────────

    /// Commit only if the candidate has its own atomic undo boundary. Used by
    /// branch replacement so an application callback cannot omit journaling or
    /// accidentally record the removed branch as the candidate's predecessor.
    pub fn commit_block(
        self,
        expected: &BlockHash,
        account: &WalletAddress,
        height: u64,
    ) -> Result<(), StoreError> {
        if !self
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

    /// Commit all batched operations in a single write transaction.
    ///
    /// This is the only fsync in the entire batch.
    pub fn commit(mut self) -> Result<(), StoreError> {
        if self.failed {
            return Err(StoreError::Corruption(
                "cannot commit a failed write batch".into(),
            ));
        }
        if let Some(undo) = self.undo.take() {
            let (key, bytes) = undo.finish()?;
            self.env
                .meta_db
                .put(&mut self.txn, &key, &bytes)
                .map_err(LmdbError::from)?;
        }
        self.txn.commit().map_err(LmdbError::from)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LmdbEnvironment;
    use burst_store::block::BlockStore;
    use burst_store::frontier::FrontierStore;

    /// Helper: open a temporary LMDB environment.
    fn temp_env() -> (tempfile::TempDir, LmdbEnvironment) {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let env =
            LmdbEnvironment::open(dir.path(), 30, 10 * 1024 * 1024).expect("failed to open env");
        (dir, env)
    }

    #[test]
    fn borrowed_economics_reads_preserve_writer_view_and_inventory_limits() {
        let (_dir, env) = temp_env();
        let mut batch = LmdbWriteTransaction::new(&env).unwrap();
        let first = WalletAddress::new("brst_a");
        let second = WalletAddress::new("brst_b");
        batch.put_brn_wallet(&first, b"one").unwrap();
        batch.put_meta("merger_graph", b"before").unwrap();
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
            state: burst_types::WalletState::Unverified,
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
        let env = LmdbEnvironment::open(dir.path(), 30, 10 * 1024 * 1024).unwrap();
        let txn = env.tx_begin_write().unwrap();
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
        batch.commit().unwrap();
        assert_eq!(snapshot(&env), before);
        let outgoing = env
            .pending_store()
            .get_pending_from_account(&account)
            .unwrap();
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].1.as_bytes(), hash.as_bytes());
        assert_eq!(outgoing[0].2.amount, 10);
        drop(env);
        let env = LmdbEnvironment::open(dir.path(), 30, 10 * 1024 * 1024).unwrap();
        assert_eq!(snapshot(&env), before);
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

        // Write via batch
        let mut batch = env.tx_begin_write().expect("write_batch");
        batch.put_block(&hash, block_bytes).expect("put_block");
        batch.put_frontier(&account, &hash).expect("put_frontier");
        batch.commit().expect("commit");

        // Verify block is readable
        let block_store = env.block_store();
        let stored = block_store.get_block(&hash).expect("get_block");
        assert_eq!(stored, block_bytes);

        // Verify frontier is readable
        let frontier_store = env.frontier_store();
        let head = frontier_store.get_frontier(&account).expect("get_frontier");
        assert_eq!(head, hash);
    }

    #[test]
    fn dropped_batch_does_not_persist() {
        let (_dir, env) = temp_env();

        let hash = BlockHash::new([2u8; 32]);
        let block_bytes = b"should-not-persist";

        // Start a batch but drop it without committing
        {
            let mut batch = env.tx_begin_write().expect("write_batch");
            batch.put_block(&hash, block_bytes).expect("put_block");
            // batch is dropped here — implicit rollback
        }

        // Block should NOT be in the store
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

        // Verify all blocks are readable
        let block_store = env.block_store();
        for (i, hash) in hashes.iter().enumerate() {
            let stored = block_store.get_block(hash).expect("get_block");
            assert_eq!(stored, format!("block-{i}").as_bytes());
        }
    }

    #[test]
    fn batch_put_account() {
        let (_dir, env) = temp_env();

        let address = WalletAddress::new(
            "brst_1genesis1ive1111111111111111111111111111111111111111111111111111111",
        );
        let data = b"account-info-bytes";

        let mut batch = env.tx_begin_write().expect("write_batch");
        batch.put_account(&address, data).expect("put_account");
        batch.commit().expect("commit");

        // Verify via a raw read transaction
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

        // Verify via the account store
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

        // Verify via raw read using the same binary composite key
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

        // First commit the block
        let mut batch = env.tx_begin_write().expect("write_batch");
        batch.put_block(&hash, block_bytes).expect("put_block");
        batch.commit().expect("commit");

        // Delete in a new batch
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
