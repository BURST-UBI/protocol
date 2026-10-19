//! Storage mechanics for a replay-authenticated checkpoint projection.
//! The node must verify the certificate and supply replay-derived rows; hashes
//! and account snapshots alone never authorize this operation.

use super::*;
use std::collections::{HashMap, HashSet};

mod compact;
mod indexes;
mod undo;
pub use compact::CompactProjection;

const MAX_ROWS: usize = 131_072;
const MAX_BYTES: usize = 64 << 20;
pub type CheckpointRows = Vec<(Vec<u8>, Vec<u8>)>;
pub type BorrowedCheckpointRow<'a> = (&'a [u8], &'a [u8]);
type ProjectionIter<'a> = Box<dyn Iterator<Item = Result<(&'a [u8], &'a [u8]), heed::Error>> + 'a>;

const LIFECYCLE_PREFIXES: &[&[u8]] = &[
    b"verification_stake/",
    b"verification_receipt/",
    b"verification_latest_outcome/",
    b"verification_certified_outcome/",
    b"verification_outcome_sources/",
    b"verification_transition_sources/",
    b"verification_identity_source/",
    b"verification_identity_binding/",
    b"verification_identity_bound/",
    b"verification_activation/",
    b"verification_deadline/",
    b"verification_default/",
    b"governance_endorsement/",
    b"key_delegation/v1/",
    b"key_delegation/v2/",
];

pub fn is_lifecycle_key(key: &str) -> bool {
    key == "verification_orchestrator"
        || LIFECYCLE_PREFIXES
            .iter()
            .any(|prefix| key.as_bytes().starts_with(prefix))
}

#[derive(Clone)]
pub struct CheckpointBlock {
    pub hash: BlockHash,
    pub account: WalletAddress,
    pub height: u64,
    pub bytes: Vec<u8>,
}

fn invalid(message: &str) -> StoreError {
    StoreError::Corruption(message.into())
}

impl LmdbWriteTransaction<'_> {
    /// Borrow metadata records with a per-record bound, not a lifetime inventory
    /// bound. Consumers that collect rows must impose their own operation budget.
    pub fn checkpoint_metadata_entries<'a>(
        &'a self,
        prefix: &'a [u8],
    ) -> Result<impl Iterator<Item = Result<BorrowedCheckpointRow<'a>, StoreError>> + 'a, StoreError>
    {
        let rows = self
            .env
            .meta_db
            .prefix_iter(&self.txn, prefix)
            .map_err(LmdbError::from)?;
        Ok(rows.map(|row| {
            let (key, value) = row.map_err(LmdbError::from)?;
            if key
                .len()
                .checked_add(value.len())
                .is_none_or(|bytes| bytes > MAX_BYTES)
            {
                return Err(invalid("checkpoint metadata row exceeds byte limit"));
            }
            Ok((key, value))
        }))
    }

    /// Borrow pending rows from the writer without copying the whole inventory.
    /// Each value remains bounded; total ledger size is not a decoding budget.
    pub fn pending_entries(
        &self,
    ) -> Result<impl Iterator<Item = Result<BorrowedCheckpointRow<'_>, StoreError>> + '_, StoreError>
    {
        let rows = self
            .env
            .pending_db
            .iter(&self.txn)
            .map_err(LmdbError::from)?;
        Ok(rows.map(|row| {
            let (key, bytes) = row.map_err(LmdbError::from)?;
            if key
                .len()
                .checked_add(bytes.len())
                .is_none_or(|size| size > MAX_BYTES)
            {
                return Err(invalid("pending row exceeds byte limit"));
            }
            Ok((key, bytes))
        }))
    }

    /// Visit replay-owned records without collecting a lifetime inventory.
    /// The orchestrator is checked separately; signing journals are not projection state.
    pub fn visit_checkpoint_lifecycle(
        &self,
        mut visit: impl FnMut(&[u8], &[u8]) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        for prefix in LIFECYCLE_PREFIXES {
            for row in self.checkpoint_metadata_entries(prefix)? {
                let (key, value) = row?;
                visit(key, value)?;
            }
        }
        Ok(())
    }

    /// Bounded collecting adapter for callers that need owned records.
    pub fn checkpoint_lifecycle(&self) -> Result<CheckpointRows, StoreError> {
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        self.visit_checkpoint_lifecycle(|key, value| {
            bytes = bytes
                .checked_add(key.len())
                .and_then(|n| n.checked_add(value.len()))
                .filter(|n| *n <= MAX_BYTES)
                .ok_or_else(|| invalid("lifecycle inventory exceeds byte limit"))?;
            if rows.len() == MAX_ROWS {
                return Err(invalid("lifecycle inventory exceeds row limit"));
            }
            rows.push((key.to_vec(), value.to_vec()));
            Ok(())
        })?;
        Ok(rows)
    }

    /// Installation checks must precede all projection writes and cannot join
    /// an ordinary block's undo transaction.
    pub fn require_clean_checkpoint_boundary(&self) -> Result<(), StoreError> {
        if self.failed || self.dirty || self.undo.is_some() {
            return Err(invalid("checkpoint installation requires a clean writer"));
        }
        Ok(())
    }

    /// Bounded raw metadata from this writer's consistent view. Binary-keyed
    /// confirmation queues cannot use the string-keyed pagination API.
    pub fn checkpoint_metadata(&self, prefix: &[u8]) -> Result<CheckpointRows, StoreError> {
        self.checkpoint_rows(UndoTable::Meta, (!prefix.is_empty()).then_some(prefix))
    }

    /// Visit borrowed rows under the same byte/row bounds as checkpoint_metadata.
    /// Errors stop traversal before any later rows are visited.
    pub fn visit_checkpoint_metadata(
        &self,
        prefix: &[u8],
        visit: impl FnMut(&[u8], &[u8]) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.visit_checkpoint_rows(
            UndoTable::Meta,
            (!prefix.is_empty()).then_some(prefix),
            visit,
        )
    }

    /// Replace canonical ledger indexes, preserving all cemented positions and
    /// durable confirmation handoffs. Excluded raw blocks are archived outside
    /// the applied ledger. Economics, authority and epoch MUST be installed in
    /// this same transaction. Failure poisons the writer.
    pub fn replace_checkpoint_ledger(
        &mut self,
        expected_epoch: BlockHash,
        blocks: &[CheckpointBlock],
        accounts: &[AccountInfo],
    ) -> Result<Vec<BlockHash>, StoreError> {
        let result = self.replace_checkpoint_ledger_inner(expected_epoch, blocks, accounts);
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    fn replace_checkpoint_ledger_inner(
        &mut self,
        expected_epoch: BlockHash,
        blocks: &[CheckpointBlock],
        accounts: &[AccountInfo],
    ) -> Result<Vec<BlockHash>, StoreError> {
        if self.failed
            || self.undo.is_some()
            || self.dirty
            || self.payment_epoch()? != expected_epoch
            || blocks.is_empty()
            || blocks.len() > MAX_ROWS
            || accounts.len() > MAX_ROWS
        {
            return Err(invalid("invalid checkpoint projection boundary"));
        }
        let mut positions = HashMap::new();
        let mut hashes = HashSet::new();
        let mut byte_count = 0usize;
        for block in blocks {
            byte_count = byte_count
                .checked_add(block.bytes.len())
                .filter(|size| *size <= MAX_BYTES)
                .ok_or_else(|| invalid("checkpoint projection exceeds byte limit"))?;
            if block.hash.is_zero()
                || block.height == 0
                || block.bytes.is_empty()
                || !hashes.insert(block.hash)
                || positions
                    .insert((block.account.clone(), block.height), block.hash)
                    .is_some()
            {
                return Err(invalid("invalid checkpoint block index"));
            }
            match self.get_block(&block.hash) {
                Ok(existing) if existing != block.bytes => {
                    return Err(invalid("checkpoint changes stored block bytes"))
                }
                Ok(_) | Err(StoreError::NotFound(_)) => (),
                Err(error) => return Err(error),
            }
        }
        let mut seen = HashSet::new();
        let mut indexed = 0usize;
        for account in accounts {
            if !seen.insert(&account.address)
                || account.confirmation_height != account.block_count
                || (account.block_count == 0 && !account.head.is_zero())
                || account.block_count > MAX_ROWS as u64
            {
                return Err(invalid("invalid replayed checkpoint account"));
            }
            for height in 1..=account.block_count {
                let hash = positions
                    .get(&(account.address.clone(), height))
                    .ok_or_else(|| invalid("checkpoint account has a missing height"))?;
                if height == account.block_count && *hash != account.head {
                    return Err(invalid("checkpoint account head mismatch"));
                }
                indexed += 1;
                if indexed > MAX_ROWS {
                    return Err(invalid("checkpoint index exceeds row limit"));
                }
            }
        }
        if indexed != blocks.len() {
            return Err(invalid("checkpoint has unowned blocks"));
        }
        let mut confirmed_count = 0usize;
        for account in self.account_entries()? {
            let account = account?;
            if account.block_count > MAX_ROWS as u64 {
                return Err(invalid("local history requires incremental installation"));
            }
            for height in 1..=account.confirmation_height {
                confirmed_count += 1;
                if confirmed_count > MAX_ROWS {
                    return Err(invalid("cemented history exceeds installation limit"));
                }
                let hash = self
                    .block_at_height(&account.address, height)?
                    .ok_or_else(|| invalid("cemented checkpoint predecessor is missing"))?;
                if positions.get(&(account.address.clone(), height)) != Some(&hash) {
                    return Err(invalid("checkpoint would replace cemented history"));
                }
            }
        }
        for (count, entry) in self
            .env
            .meta_db
            .prefix_iter(&self.txn, crate::confirmation_queue::PREFIX)
            .map_err(LmdbError::from)?
            .enumerate()
        {
            let (key, value) = entry.map_err(LmdbError::from)?;
            let hash = key[crate::confirmation_queue::PREFIX.len()..]
                .try_into()
                .map(BlockHash::new)
                .map_err(|_| invalid("invalid durable confirmation hash"))?;
            if count >= MAX_ROWS || value != [1] || !hashes.contains(&hash) {
                return Err(invalid("checkpoint excludes durable confirmation work"));
            }
        }
        // A checkpoint does not reconstruct legacy transaction/governance tables.
        // Never leave unsupported provisional effects behind while replacing balances.
        for db in [
            self.env.transactions_db,
            self.env.account_txs_db,
            self.env.proposals_db,
            self.env.votes_db,
            self.env.delegations_db,
        ] {
            if db.len(&self.txn).map_err(LmdbError::from)? != 0 {
                return Err(invalid(
                    "checkpoint projection requires unsupported table reconstruction",
                ));
            }
        }
        let existing = self.checkpoint_rows(UndoTable::Blocks, None)?;
        let mut retired = Vec::new();
        for (key, value) in existing {
            let hash = BlockHash::new(
                key.as_slice()
                    .try_into()
                    .map_err(|_| invalid("invalid stored block hash"))?,
            );
            if !hashes.contains(&hash) {
                let archive = format!("checkpoint_orphan/v1/{expected_epoch}/{hash}");
                if self.get_meta(&archive)?.is_some_and(|old| old != value) {
                    return Err(invalid("checkpoint orphan evidence changed"));
                }
                self.put_meta(&archive, &value)?;
                self.delete_raw(UndoTable::Blocks, &key)?;
                retired.push(hash);
            }
        }
        for table in [
            UndoTable::Accounts,
            UndoTable::Frontiers,
            UndoTable::Heights,
            UndoTable::BlockHeights,
            UndoTable::Pending,
            UndoTable::BrnWallets,
            UndoTable::TrstOrigins,
            UndoTable::TrstExpiry,
            UndoTable::TrstReverse,
        ] {
            self.clear_checkpoint_rows(table, None)?;
        }
        // Local before-images no longer describe the certified projection.
        self.clear_checkpoint_rows(UndoTable::Meta, Some(crate::block_undo::PREFIX))?;
        for block in blocks {
            self.put_block_with_account(&block.hash, &block.bytes, &block.account, block.height)?;
            if self.confirmation_pending(&block.hash)? {
                self.acknowledge_confirmation(&block.hash)?;
            }
        }
        let mut verified = 0u64;
        for account in accounts {
            self.put_account(
                &account.address,
                &bincode::serialize(account).map_err(LmdbError::from)?,
            )?;
            if account.block_count > 0 {
                self.put_frontier(&account.address, &account.head)?;
            }
            verified += u64::from(account.state.is_verified());
        }
        self.put_meta("verified_count", &verified.to_be_bytes())?;
        retired.sort_by_key(|hash| *hash.as_bytes());
        Ok(retired)
    }

    fn checkpoint_rows(
        &self,
        table: UndoTable,
        prefix: Option<&[u8]>,
    ) -> Result<CheckpointRows, StoreError> {
        let mut rows = Vec::new();
        self.visit_checkpoint_rows(table, prefix, |key, value| {
            rows.push((key.to_vec(), value.to_vec()));
            Ok(())
        })?;
        Ok(rows)
    }

    fn visit_checkpoint_rows(
        &self,
        table: UndoTable,
        prefix: Option<&[u8]>,
        mut visit: impl FnMut(&[u8], &[u8]) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let db = table.database(self.env);
        let mut total = 0usize;
        let iter: ProjectionIter<'_> = match prefix {
            Some(prefix) => Box::new(db.prefix_iter(&self.txn, prefix).map_err(LmdbError::from)?),
            None => Box::new(db.iter(&self.txn).map_err(LmdbError::from)?),
        };
        for (count, row) in iter.enumerate() {
            let (key, value) = row.map_err(LmdbError::from)?;
            total = total
                .checked_add(key.len())
                .and_then(|n| n.checked_add(value.len()))
                .filter(|n| *n <= MAX_BYTES)
                .ok_or_else(|| invalid("checkpoint scan exceeds byte limit"))?;
            if count == MAX_ROWS {
                return Err(invalid("checkpoint scan exceeds row limit"));
            }
            visit(key, value)?;
        }
        Ok(())
    }

    fn clear_checkpoint_rows(
        &mut self,
        table: UndoTable,
        prefix: Option<&[u8]>,
    ) -> Result<(), StoreError> {
        if self.undo.is_some() {
            return Err(invalid("projection replacement cannot join block undo"));
        }
        let Some(prefix) = prefix else {
            self.dirty = true;
            table
                .database(self.env)
                .clear(&mut self.txn)
                .map_err(LmdbError::from)?;
            if table == UndoTable::Pending {
                self.env
                    .pending_sender_db
                    .clear(&mut self.txn)
                    .map_err(LmdbError::from)?;
            }
            return Ok(());
        };
        loop {
            // Delete only this namespace; keep at most one key batch resident.
            // Values belong to the old projection and need not be decoded.
            let keys = table
                .database(self.env)
                .prefix_iter(&self.txn, prefix)
                .map_err(LmdbError::from)?
                .take(256)
                .map(|row| {
                    let (key, _) = row.map_err(LmdbError::from)?;
                    if key.len() > MAX_BYTES / 256 {
                        return Err(invalid("checkpoint replacement key exceeds batch limit"));
                    }
                    Ok(key.to_vec())
                })
                .collect::<Result<Vec<_>, StoreError>>()?;
            if keys.is_empty() {
                break;
            }
            for key in keys {
                self.delete_raw(table, &key)?;
            }
        }
        Ok(())
    }

    /// Only replay-owned lifecycle namespaces may be replaced. Agreement,
    /// final-vote journals, epoch markers and unrelated application settings stay.
    pub fn clear_checkpoint_lifecycle(&mut self) -> Result<(), StoreError> {
        let result = (|| {
            if self.failed || self.undo.is_some() {
                return Err(invalid("invalid lifecycle replacement"));
            }
            for prefix in LIFECYCLE_PREFIXES {
                self.clear_checkpoint_rows(UndoTable::Meta, Some(prefix))?;
            }
            self.delete_raw(UndoTable::Meta, b"verification_orchestrator")?;
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Certified replacement only: token rows and their provisional undo must
    /// be replaced together by the caller. Never join ordinary block undo.
    pub fn clear_checkpoint_token_rows(&mut self, prefix: &[u8]) -> Result<(), StoreError> {
        let result = (|| {
            if self.failed || !matches!(prefix, b"trst_state/v1/" | b"trst_graph/v1/") {
                return Err(invalid("invalid token projection namespace"));
            }
            self.clear_checkpoint_rows(UndoTable::Meta, Some(prefix))
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LmdbStore;
    use burst_store::{block::BlockStore, meta::MetaStore};

    #[test]
    fn token_projection_replacement_is_scoped_atomic_and_rejects_block_undo() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let prefixes = ["trst_state/v1/", "trst_graph/v1/"];
        let mut batch = store.tx_begin_write().unwrap();
        for prefix in prefixes {
            for row in 0..513 {
                batch
                    .put_meta(&format!("{prefix}{row:04}"), b"old")
                    .unwrap();
            }
        }
        batch.put_meta("trst_state/v1-other", b"keep").unwrap();
        batch.put_meta("checkpoint_vote/keep", b"promise").unwrap();
        batch.commit().unwrap();
        for commit in [false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            for prefix in prefixes {
                batch
                    .clear_checkpoint_token_rows(prefix.as_bytes())
                    .unwrap();
                assert_eq!(
                    batch
                        .checkpoint_metadata_entries(prefix.as_bytes())
                        .unwrap()
                        .count(),
                    0
                );
                batch.put_meta(&format!("{prefix}new"), b"new").unwrap();
            }
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
                assert_eq!(
                    store.meta_store().get_meta("trst_state/v1/0000").unwrap(),
                    b"old"
                );
            }
        }
        store.force_sync().unwrap();
        drop(store);
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let batch = store.tx_begin_write().unwrap();
        for prefix in prefixes {
            assert_eq!(
                batch
                    .checkpoint_metadata_entries(prefix.as_bytes())
                    .unwrap()
                    .count(),
                1
            );
            assert_eq!(
                batch.get_meta(&format!("{prefix}new")).unwrap().unwrap(),
                b"new"
            );
        }
        assert_eq!(
            batch.get_meta("trst_state/v1-other").unwrap().unwrap(),
            b"keep"
        );
        assert_eq!(
            batch.get_meta("checkpoint_vote/keep").unwrap().unwrap(),
            b"promise"
        );
        drop(batch);
        for prefix in [b"".as_slice(), b"checkpoint_vote/", b"trst_state/"] {
            let mut batch = store.tx_begin_write().unwrap();
            assert!(batch.clear_checkpoint_token_rows(prefix).is_err());
            assert!(batch.commit().is_err());
        }
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .begin_block_undo(BlockHash::new([9; 32]), WalletAddress::new("brst_test"), 1)
            .unwrap();
        assert!(batch
            .clear_checkpoint_token_rows(prefixes[0].as_bytes())
            .is_err());
        assert!(batch.commit().is_err());
    }

    #[test]
    fn lifecycle_replacement_crosses_old_scan_limit_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 64 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        for number in 0..=MAX_ROWS {
            batch
                .put_meta(&format!("verification_receipt/{number:08}"), b"old")
                .unwrap();
        }
        for key in [
            "verification_receipt-other",
            "application-preference",
            "checkpoint_round/v1/keep",
        ] {
            batch.put_meta(key, b"keep").unwrap();
        }
        batch.commit().unwrap();
        for commit in [false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            let mut count = 0;
            batch
                .visit_checkpoint_lifecycle(|key, value| {
                    assert!(key.starts_with(b"verification_receipt/"));
                    assert_eq!(value, b"old");
                    count += 1;
                    Ok(())
                })
                .unwrap();
            assert_eq!(count, MAX_ROWS + 1);
            let mut visited = 0;
            assert!(batch
                .visit_checkpoint_lifecycle(|_, _| {
                    visited += 1;
                    Err(invalid("stop traversal"))
                })
                .is_err());
            assert_eq!(visited, 1);
            batch.clear_checkpoint_lifecycle().unwrap();
            assert!(batch
                .get_meta("verification_receipt/00000000")
                .unwrap()
                .is_none());
            batch
                .put_meta("verification_receipt/replacement", b"new")
                .unwrap();
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
                assert_eq!(
                    store
                        .meta_store()
                        .get_meta("verification_receipt/00000000")
                        .unwrap(),
                    b"old"
                );
                assert!(store
                    .meta_store()
                    .get_meta("verification_receipt/replacement")
                    .is_err());
            }
        }
        store.force_sync().unwrap();
        drop(store);
        let store = LmdbStore::open(directory.path(), 32, 64 << 20).unwrap();
        let batch = store.tx_begin_write().unwrap();
        let rows = batch.checkpoint_metadata(b"verification_receipt/").unwrap();
        assert_eq!(
            rows,
            vec![(
                b"verification_receipt/replacement".to_vec(),
                b"new".to_vec()
            )]
        );
        for key in [
            "verification_receipt-other",
            "application-preference",
            "checkpoint_round/v1/keep",
        ] {
            assert_eq!(batch.get_meta(key).unwrap().unwrap(), b"keep");
        }
    }

    #[test]
    fn metadata_visitor_borrows_writer_rows_and_stops_on_error() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta("scan/2", &vec![7; 1 << 20]).unwrap();
        batch.put_meta("scan/1", b"first").unwrap();
        batch.put_meta("scan-other", b"excluded").unwrap();
        let pointer = batch
            .env
            .meta_db
            .get(&batch.txn, b"scan/2")
            .unwrap()
            .unwrap()
            .as_ptr();
        let mut keys = Vec::new();
        batch
            .visit_checkpoint_metadata(b"scan/", |key, value| {
                keys.push(key.to_vec());
                if key == b"scan/2" {
                    assert_eq!(value.len(), 1 << 20);
                    assert_eq!(value.as_ptr(), pointer);
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(keys, [b"scan/1".to_vec(), b"scan/2".to_vec()]);
        let mut visited = 0;
        assert!(batch
            .visit_checkpoint_metadata(b"scan/", |_, _| {
                visited += 1;
                Err(invalid("stop"))
            })
            .is_err());
        assert_eq!(visited, 1);
        drop(batch);
        assert!(store.meta_store().get_meta("scan/1").is_err());
    }

    fn hash(n: u8) -> BlockHash {
        BlockHash::new([n; 32])
    }

    fn fixture(store: &LmdbStore) -> (Vec<CheckpointBlock>, Vec<AccountInfo>) {
        let address = WalletAddress::new("brst_checkpoint_account");
        let mut account = AccountInfo::unopened(address.clone());
        account.head = hash(2);
        account.block_count = 2;
        account.confirmation_height = 1;
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, hash(1).as_bytes())
            .unwrap();
        batch
            .put_block_with_account(&hash(1), b"first", &address, 1)
            .unwrap();
        batch
            .put_block_with_account(&hash(2), b"tail", &address, 2)
            .unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch.put_frontier(&address, &hash(2)).unwrap();
        batch
            .put_meta("application-preference", b"preserve")
            .unwrap();
        batch.commit().unwrap();
        account.head = hash(1);
        account.block_count = 1;
        (
            vec![CheckpointBlock {
                hash: hash(1),
                account: address,
                height: 1,
                bytes: b"first".to_vec(),
            }],
            vec![account],
        )
    }

    #[test]
    fn checkpoint_projection_abort_and_handoff_are_atomic() {
        use burst_store::pending::{PendingInfo, PendingStore};
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let (blocks, accounts) = fixture(&store);
        let sender = accounts[0].address.clone();
        let destination = WalletAddress::new("brst_destination");
        store
            .pending_store()
            .put_pending(
                &destination,
                &TxHash::new([7; 32]),
                &PendingInfo {
                    return_credit: false,
                    source: sender.clone(),
                    amount: 15,
                    timestamp: Timestamp::new(1),
                    provenance: Vec::new(),
                },
            )
            .unwrap();
        let archive = format!("checkpoint_orphan/v1/{}/{}", hash(1), hash(2));
        for commit in [false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            assert_eq!(
                batch
                    .replace_checkpoint_ledger(hash(1), &blocks, &accounts)
                    .unwrap(),
                vec![hash(2)]
            );
            assert!(batch.get_block(&hash(2)).is_err());
            assert_eq!(batch.get_meta(&archive).unwrap().unwrap(), b"tail");
            batch
                .advance_payment_epoch(&hash(1), &hash(1), &hash(3), &HashSet::from([hash(1)]))
                .unwrap();
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
            }
            assert_eq!(store.block_store().get_block(&hash(2)).is_ok(), !commit);
            assert_eq!(
                store
                    .pending_store()
                    .get_pending_from_account(&sender)
                    .unwrap()
                    .len(),
                usize::from(!commit)
            );
            assert_eq!(
                store.meta_store().payment_epoch().unwrap(),
                hash(if commit { 3 } else { 1 })
            );
            assert_eq!(store.meta_store().get_meta(&archive).is_ok(), commit);
            assert_eq!(
                store
                    .meta_store()
                    .get_meta("application-preference")
                    .unwrap(),
                b"preserve"
            );
        }
    }

    #[test]
    fn checkpoint_projection_can_replace_corrupt_pending_and_orphaned_sender_indexes() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let (blocks, accounts) = fixture(&store);
        let key = crate::pending::pending_key_raw(&accounts[0].address, &[8; 32]);
        {
            let mut txn = store.env().write_txn().unwrap();
            store
                .pending_db
                .put(&mut txn, &key, b"damaged pending bytes")
                .unwrap();
            store
                .pending_sender_db
                .put(&mut txn, b"orphaned sender entry", b"missing primary key")
                .unwrap();
            txn.commit().unwrap();
        }
        for commit in [false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            batch
                .replace_checkpoint_ledger(hash(1), &blocks, &accounts)
                .unwrap();
            batch
                .advance_payment_epoch(&hash(1), &hash(1), &hash(3), &HashSet::from([hash(1)]))
                .unwrap();
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
            }
            let txn = store.env().read_txn().unwrap();
            assert_eq!(store.pending_db.len(&txn).unwrap(), u64::from(!commit));
            assert_eq!(
                store.pending_sender_db.len(&txn).unwrap(),
                u64::from(!commit)
            );
            drop(txn);
            assert_eq!(
                store.meta_store().payment_epoch().unwrap(),
                hash(if commit { 3 } else { 1 })
            );
        }
    }

    #[test]
    fn failed_checkpoint_projection_and_failed_handoff_poison_all_writes() {
        for invalid_index in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
            let (mut blocks, accounts) = fixture(&store);
            let mut batch = store.tx_begin_write().unwrap();
            if invalid_index {
                blocks[0].height = 2;
                assert!(batch
                    .replace_checkpoint_ledger(hash(1), &blocks, &accounts)
                    .is_err());
            } else {
                batch
                    .replace_checkpoint_ledger(hash(1), &blocks, &accounts)
                    .unwrap();
                assert!(batch
                    .advance_payment_epoch(&hash(1), &hash(1), &hash(1), &HashSet::new())
                    .is_err());
            }
            let _ = batch.put_meta("should-not-commit", b"no");
            assert!(batch.commit().is_err());
            assert!(store.meta_store().get_meta("should-not-commit").is_err());
            assert_eq!(store.block_store().get_block(&hash(2)).unwrap(), b"tail");
            assert_eq!(store.meta_store().payment_epoch().unwrap(), hash(1));
        }
    }

    #[test]
    fn checkpoint_projection_cannot_join_reversible_block_writes() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let (blocks, accounts) = fixture(&store);
        for key in [
            "checkpoint_installed/v1",
            "checkpoint_orphan/v1/evidence",
            "checkpoint_history/v1/evidence",
            "checkpoint_snapshot/v1/base",
        ] {
            let mut batch = store.tx_begin_write().unwrap();
            batch
                .begin_block_undo(hash(4), accounts[0].address.clone(), 3)
                .unwrap();
            assert!(batch.put_meta(key, b"no").is_err());
            assert!(batch.commit().is_err());
        }
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .begin_block_undo(hash(4), accounts[0].address.clone(), 3)
            .unwrap();
        assert!(batch
            .replace_checkpoint_ledger(hash(1), &blocks, &accounts)
            .is_err());
        assert!(batch.commit().is_err());
        assert_eq!(store.block_store().get_block(&hash(2)).unwrap(), b"tail");
    }
}
