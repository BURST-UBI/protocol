use super::*;
use crate::write_batch::checkpoint_projection::CheckpointBlock;

pub struct IndexedBlock<'a> {
    pub hash: BlockHash,
    pub account: WalletAddress,
    pub height: u64,
    pub bytes: &'a [u8],
}

/// A physical height-index position; this is not evidence of finality.
pub struct BlockPosition {
    pub hash: BlockHash,
    pub account: WalletAddress,
    pub height: u64,
}

fn position(key: &[u8], hash: &[u8]) -> Result<BlockPosition, StoreError> {
    let invalid = |message: &str| StoreError::Corruption(message.into());
    if key.len() <= 8 { return Err(invalid("invalid retained block row")); }
    let hash = BlockHash::new(hash.try_into().map_err(|_| invalid("invalid retained block hash"))?);
    let (account, height) = key.split_at(key.len() - 8);
    let height = u64::from_be_bytes(height.try_into().expect("checked index length"));
    let account = std::str::from_utf8(account).map_err(|_| invalid("invalid retained block account encoding"))?;
    if !account.starts_with(WalletAddress::PREFIX) || account.len() <= WalletAddress::PREFIX.len() {
        return Err(invalid("invalid retained block account"));
    }
    if height == 0 || hash.is_zero() { return Err(invalid("zero retained block position")); }
    Ok(BlockPosition { hash, account: WalletAddress::new(account.to_owned()), height })
}

/// Progress over one immutable LMDB view. Completing every page checks physical
/// body/index bijection, not signatures, ancestry or permission to retire bodies.
pub struct IndexedBlockCursor<'view, 'env> {
    txn: &'view heed::RoTxn<'env>,
    rows: std::iter::Peekable<heed::RoIter<'view, Bytes, Bytes>>,
    heights: Database<Bytes, Bytes>,
    reverse: Database<Bytes, Bytes>,
    expected: u64,
    visited: u64,
    failed: bool,
    complete: bool,
}

#[derive(Debug)]
pub struct InventoryPage {
    pub rows: usize,
    /// Key/value bytes across the body and two index tables; excludes LMDB page overhead.
    pub bytes: usize,
    pub done: bool,
}

impl<'view, 'env> IndexedBlockCursor<'view, 'env> {
    fn new(txn: &'view heed::RoTxn<'env>, blocks: Database<Bytes, Bytes>,
        heights: Database<Bytes, Bytes>, reverse: Database<Bytes, Bytes>) -> Result<Self, StoreError> {
        let expected = inventory_count(txn, blocks, heights, reverse)?;
        Ok(Self { txn, rows: blocks.iter(txn).map_err(LmdbError::from)?.peekable(),
            heights, reverse, expected, visited: 0, failed: false, complete: false })
    }

    pub fn expected_rows(&self) -> u64 { self.expected }

    /// A callback error or invalid row poisons this cursor. A body that cannot
    /// fit an empty page is an error; rows are never silently skipped.
    pub fn visit_next(
        &mut self, max_rows: usize, max_bytes: usize,
        mut visit: impl FnMut(IndexedBlock<'view>) -> Result<(), StoreError>,
    ) -> Result<InventoryPage, StoreError> {
        let invalid = |message: &str| StoreError::Corruption(message.into());
        if self.failed { return Err(invalid("physical inventory cursor already failed")); }
        self.failed = true;
        if max_rows == 0 || max_bytes == 0 { return Err(invalid("invalid physical inventory page budget")); }
        let mut page = InventoryPage { rows: 0, bytes: 0, done: self.complete };
        if self.complete { self.failed = false; return Ok(page); }
        while page.rows < max_rows {
            let Some(entry) = self.rows.peek() else { break; };
            let (hash, bytes) = match entry {
                Ok((hash, bytes)) => (*hash, *bytes),
                Err(_) => return Err(LmdbError::from(self.rows.next().unwrap().unwrap_err()).into()),
            };
            let (hash, key, charged) = checked_position(self.txn, self.heights, self.reverse, hash, bytes)?;
            if charged > max_bytes { return Err(invalid("retained block exceeds page byte budget")); }
            if charged > max_bytes - page.bytes { break; }
            let position = position(key, hash.as_bytes())?;
            visit(IndexedBlock { hash, account: position.account, height: position.height, bytes })?;
            self.rows.next();
            self.visited = self.visited.checked_add(1).filter(|n| *n <= self.expected)
                .ok_or_else(|| invalid("physical inventory count changed"))?;
            page.rows += 1;
            page.bytes += charged;
        }
        if self.rows.peek().is_none() {
            if self.visited != self.expected { return Err(invalid("incomplete physical inventory")); }
            self.complete = true;
            page.done = true;
        }
        self.failed = false;
        Ok(page)
    }
}

fn inventory_count(txn: &heed::RoTxn<'_>, blocks: Database<Bytes, Bytes>,
    heights: Database<Bytes, Bytes>, reverse: Database<Bytes, Bytes>) -> Result<u64, StoreError> {
    let count = blocks.len(txn).map_err(LmdbError::from)?;
    if heights.len(txn).map_err(LmdbError::from)? != count || reverse.len(txn).map_err(LmdbError::from)? != count {
        return Err(StoreError::Corruption("retained block inventory count or index mismatch".into()));
    }
    Ok(count)
}

fn inventory_bytes(key_bytes: usize, body_bytes: usize) -> Result<usize, StoreError> {
    key_bytes.checked_mul(2).and_then(|n| n.checked_add(body_bytes))
        .and_then(|n| n.checked_add(96))
        .ok_or_else(|| StoreError::Corruption("physical inventory byte overflow".into()))
}

/// The body and both position indexes' logical charge. This is accounting only,
/// not evidence that the supplied account, length or body exists in the ledger.
pub fn physical_inventory_bytes(account: &WalletAddress, body_bytes: usize) -> Result<usize, StoreError> {
    let key_bytes = account.as_str().len().checked_add(8)
        .ok_or_else(|| StoreError::Corruption("physical inventory byte overflow".into()))?;
    inventory_bytes(key_bytes, body_bytes)
}

fn checked_position<'a>(txn: &'a heed::RoTxn<'_>, heights: Database<Bytes, Bytes>,
    reverse: Database<Bytes, Bytes>, hash: &[u8], bytes: &[u8]) -> Result<(BlockHash, &'a [u8], usize), StoreError> {
    let invalid = |message: &str| StoreError::Corruption(message.into());
    let hash = BlockHash::new(hash.try_into().map_err(|_| invalid("invalid retained block hash"))?);
    let key = reverse.get(txn, hash.as_bytes()).map_err(LmdbError::from)?
        .ok_or_else(|| invalid("retained block has no reverse index"))?;
    if hash.is_zero() || bytes.is_empty() || key.len() <= 8 { return Err(invalid("invalid retained block row")); }
    if heights.get(txn, key).map_err(LmdbError::from)? != Some(hash.as_bytes().as_slice()) {
        return Err(invalid("conflicting retained block indexes"));
    }
    let charged = inventory_bytes(key.len(), bytes.len())?;
    Ok((hash, key, charged))
}

pub(crate) fn entries<'a>(
    txn: &'a heed::RoTxn<'_>,
    blocks: Database<Bytes, Bytes>,
    heights: Database<Bytes, Bytes>,
    reverse: Database<Bytes, Bytes>,
    max_rows: usize,
    max_bytes: usize,
) -> Result<impl Iterator<Item = Result<IndexedBlock<'a>, StoreError>> + 'a, StoreError> {
    let invalid = |message: &str| StoreError::Corruption(message.into());
    let count = inventory_count(txn, blocks, heights, reverse)?;
    if count > max_rows as u64 {
        return Err(invalid("retained block inventory count or index mismatch"));
    }
    let mut total = 0usize;
    Ok(blocks
        .iter(txn)
        .map_err(LmdbError::from)?
        .map(move |entry| {
            let (hash, bytes) = entry.map_err(LmdbError::from)?;
            let (hash, key, charged) = checked_position(txn, heights, reverse, hash, bytes)?;
            total = total.checked_add(charged).filter(|n| *n <= max_bytes)
                .ok_or_else(|| invalid("retained block inventory exceeds byte limit"))?;
            let position = position(key, hash.as_bytes())?;
            Ok(IndexedBlock {
                hash,
                account: position.account,
                height: position.height,
                bytes,
            })
        }))
}

pub(crate) fn read(
    txn: &heed::RoTxn<'_>,
    blocks: Database<Bytes, Bytes>,
    heights: Database<Bytes, Bytes>,
    reverse: Database<Bytes, Bytes>,
    max_rows: usize,
    max_bytes: usize,
) -> Result<Vec<CheckpointBlock>, StoreError> {
    entries(txn, blocks, heights, reverse, max_rows, max_bytes)?
        .map(|row| {
            let row = row?;
            Ok(CheckpointBlock {
                hash: row.hash,
                account: row.account,
                height: row.height,
                bytes: row.bytes.to_vec(),
            })
        })
        .collect()
}

/// Bounded physical range progress, not proof of a complete ledger inventory.
/// `end_of_range` covers only the suffix after the supplied key in this view.
#[derive(Debug)]
pub struct IndexedBlockWindow {
    /// Physical table cardinality in this view, not a coverage proof.
    pub physical_rows: u64,
    pub rows: usize,
    pub bytes: usize,
    pub resume_after: Option<BlockHash>,
    pub end_of_range: bool,
}

pub(crate) fn visit_window<'a>(
    txn: &'a heed::RoTxn<'_>, blocks: Database<Bytes, Bytes>,
    heights: Database<Bytes, Bytes>, reverse: Database<Bytes, Bytes>,
    after: Option<BlockHash>, max_rows: usize, max_bytes: usize,
    mut visit: impl FnMut(IndexedBlock<'a>) -> Result<(), StoreError>,
) -> Result<IndexedBlockWindow, StoreError> {
    let invalid = |message: &str| StoreError::Corruption(message.into());
    if max_rows == 0 || max_rows > crate::retention::MAX_PAGE_ROWS
        || max_bytes == 0 || max_bytes > crate::retention::MAX_PAGE_BYTES {
        return Err(invalid("invalid physical inventory window budget"));
    }
    let physical_rows = inventory_count(txn, blocks, heights, reverse)?;
    let bounds = (after.as_ref().map_or(Bound::Unbounded,
        |hash| Bound::Excluded(hash.as_bytes().as_slice())), Bound::<&[u8]>::Unbounded);
    let mut entries = blocks.range(txn, &bounds).map_err(LmdbError::from)?.peekable();
    let mut page = IndexedBlockWindow { physical_rows, rows: 0, bytes: 0, resume_after: after, end_of_range: false };
    while page.rows < max_rows {
        let Some(entry) = entries.peek() else { break; };
        let (hash, bytes) = match entry {
            Ok((hash, bytes)) => (*hash, *bytes),
            Err(_) => return Err(LmdbError::from(entries.next().unwrap().unwrap_err()).into()),
        };
        let (hash, key, charged) = checked_position(txn, heights, reverse, hash, bytes)?;
        if charged > max_bytes {
            return Err(invalid("retained block exceeds window byte budget"));
        }
        if charged > max_bytes - page.bytes { break; }
        let position = position(key, hash.as_bytes())?;
        visit(IndexedBlock { hash, account: position.account, height: position.height, bytes })?;
        entries.next();
        page.rows += 1;
        page.bytes += charged;
        page.resume_after = Some(hash);
    }
    if entries.peek().is_some_and(Result::is_err) {
        return Err(LmdbError::from(entries.next().unwrap().unwrap_err()).into());
    }
    page.end_of_range = entries.peek().is_none();
    Ok(page)
}

impl LmdbBlockStore {
    /// Seek past an exclusive physical hash and borrow a bounded window from
    /// one read view. A deleted boundary remains a valid seek key. Reopening on
    /// a newer view does not establish coverage of inserts behind that key.
    /// Counts and visited row indexes are checked, but this does not authenticate
    /// bodies, prove whole-inventory bijection, or authorize retirement. Callback
    /// effects before an error must be discarded by the caller.
    pub fn visit_indexed_block_window<'a>(
        &self, txn: &'a LmdbReadTransaction<'_>, after: Option<BlockHash>,
        max_rows: usize, max_bytes: usize,
        visit: impl FnMut(IndexedBlock<'a>) -> Result<(), StoreError>,
    ) -> Result<IndexedBlockWindow, StoreError> {
        visit_window(&txn.txn, self.blocks_db, self.height_db, self.block_height_db,
            after, max_rows, max_bytes, visit)
    }

    /// Resolve both physical position indexes without reading a block body.
    /// This is an index lookup, not authentication or evidence of finality.
    pub fn indexed_position_txn(&self, txn: &LmdbReadTransaction<'_>, hash: &BlockHash)
        -> Result<Option<BlockPosition>, StoreError> {
        let Some(key) = self.block_height_db.get(&txn.txn, hash.as_bytes()).map_err(LmdbError::from)? else {
            return Ok(None);
        };
        if self.height_db.get(&txn.txn, key).map_err(LmdbError::from)? != Some(hash.as_bytes().as_slice()) {
            return Err(StoreError::Corruption("conflicting block position indexes".into()));
        }
        position(key, hash.as_bytes()).map(Some)
    }

    /// Page one pinned physical inventory without imposing a whole-ledger row
    /// limit. The caller owns cumulative work/disk budgets and must finish every
    /// page before claiming a complete inventory. Dropping releases progress.
    pub fn indexed_block_cursor<'view, 'env>(&self, txn: &'view LmdbReadTransaction<'env>)
        -> Result<IndexedBlockCursor<'view, 'env>, StoreError> {
        IndexedBlockCursor::new(&txn.txn, self.blocks_db, self.height_db, self.block_height_db)
    }

    /// Reverse physical index order, bounded before traversal. Callers requiring
    /// body/index bijection must consume `indexed_block_entries` in the same
    /// pinned transaction first. This iterator only validates position encoding.
    pub fn retained_positions<'a>(
        &self, txn: &'a LmdbReadTransaction<'_>, max_rows: usize,
    ) -> Result<impl Iterator<Item = Result<BlockPosition, StoreError>> + 'a, StoreError> {
        if self.height_db.len(&txn.txn).map_err(LmdbError::from)? > max_rows as u64 {
            return Err(StoreError::Corruption("retained position count exceeds budget".into()));
        }
        Ok(self.height_db.rev_iter(&txn.txn).map_err(LmdbError::from)?.map(|entry| {
            let (key, hash) = entry.map_err(LmdbError::from)?;
            position(key, hash)
        }))
    }

    /// Check table counts, then borrow rows while validating their index entries
    /// and cumulative byte budget. Consume the iterator to completion, handling
    /// every error, before treating it as a complete inventory check.
    pub fn indexed_block_entries<'a>(
        &self,
        txn: &'a LmdbReadTransaction<'_>,
        max_rows: usize,
        max_bytes: usize,
    ) -> Result<impl Iterator<Item = Result<IndexedBlock<'a>, StoreError>> + 'a, StoreError> {
        entries(
            &txn.txn,
            self.blocks_db,
            self.height_db,
            self.block_height_db,
            max_rows,
            max_bytes,
        )
    }

    /// Inventory physical rows, including sparse histories, from one read view.
    /// Index bijection is checked; block signatures and ancestry are not.
    pub fn indexed_blocks_txn(
        &self,
        txn: &LmdbReadTransaction<'_>,
        max_rows: usize,
        max_bytes: usize,
    ) -> Result<Vec<CheckpointBlock>, StoreError> {
        read(
            &txn.txn,
            self.blocks_db,
            self.height_db,
            self.block_height_db,
            max_rows,
            max_bytes,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LmdbStore;

    fn fixture() -> (tempfile::TempDir, LmdbStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 64, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_block_with_account(
                &BlockHash::new([1; 32]),
                &[1, 2],
                &WalletAddress::new("brst_test"),
                u64::MAX,
            )
            .unwrap();
        batch.commit().unwrap();
        (dir, store)
    }

    #[test]
    fn point_position_uses_pinned_indexes_and_rejects_conflicts() {
        let (_dir, store) = fixture();
        let blocks = store.block_store();
        let hash = BlockHash::new([1; 32]);
        let snapshot = store.tx_begin_read().unwrap();
        let position = blocks.indexed_position_txn(&snapshot, &hash).unwrap().unwrap();
        assert_eq!(position.account, WalletAddress::new("brst_test"));
        assert_eq!(position.height, u64::MAX);
        assert_eq!(position.hash, hash);
        assert!(blocks.indexed_position_txn(&snapshot, &BlockHash::new([2; 32])).unwrap().is_none());
        let mut txn = blocks.env.write_txn().unwrap();
        blocks.height_db.put(&mut txn, &height_key(&position.account, position.height), &[2; 32]).unwrap();
        txn.commit().unwrap();
        assert!(blocks.indexed_position_txn(&snapshot, &hash).unwrap().is_some());
        drop(snapshot);
        assert!(blocks.indexed_position_txn(&store.tx_begin_read().unwrap(), &hash).is_err());
    }

    #[test]
    fn paged_inventory_exceeds_old_whole_scan_limit_without_revisiting_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 64, 128 << 20).unwrap();
        let count = 131_073u64;
        let account = WalletAddress::new("brst_inventory");
        let hash = |n: u64| { let mut bytes = [0; 32]; bytes[..8].copy_from_slice(&n.to_be_bytes()); BlockHash::new(bytes) };
        let mut batch = store.tx_begin_write().unwrap();
        for n in 1..=count { batch.put_block_with_account(&hash(n), &[1, 2], &account, n).unwrap(); }
        batch.commit().unwrap();
        let blocks = store.block_store();
        let txn = store.tx_begin_read().unwrap();
        assert!(blocks.indexed_block_entries(&txn, 131_072, usize::MAX).is_err());
        let mut cursor = blocks.indexed_block_cursor(&txn).unwrap();
        assert_eq!(cursor.expected_rows(), count);
        let mut visited = 0;
        let mut pages = 0;
        loop {
            let before = visited;
            let page = cursor.visit_next(127, 4096, |row| {
                visited += 1;
                assert_eq!(row.hash, hash(visited));
                assert_eq!(row.height, visited);
                assert_eq!(row.account, account);
                assert_eq!(row.bytes, [1, 2]);
                Ok(())
            }).unwrap();
            assert_eq!(page.rows as u64, visited - before);
            assert!(page.rows <= 127 && page.bytes <= 4096);
            pages += 1;
            if page.done { break; }
            assert!(page.rows > 0);
        }
        assert_eq!(visited, count);
        assert!(pages > 1000);
        let page = cursor.visit_next(1, 4096, |_| panic!("completed cursor replayed rows")).unwrap();
        assert!(page.done && page.rows == 0 && page.bytes == 0);
        let mut last = Vec::new();
        let window = blocks.visit_indexed_block_window(&txn, Some(hash(count - 2)), 2, 4096,
            |row| { last.push(row.hash); Ok(()) }).unwrap();
        assert_eq!(last, [hash(count - 1), hash(count)]);
        assert_eq!(window.rows, 2);
        assert_eq!(window.physical_rows, count);
        assert_eq!(window.resume_after, Some(hash(count)));
        assert!(window.end_of_range);
    }

    #[test]
    fn inventory_pages_pin_their_view_and_poison_failed_callbacks_or_budgets() {
        let (_dir, store) = fixture();
        let blocks = store.block_store();
        let txn = store.tx_begin_read().unwrap();
        let mut cursor = blocks.indexed_block_cursor(&txn).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_block_with_account(&BlockHash::new([2; 32]), &[3], &WalletAddress::new("brst_other"), 1).unwrap();
        batch.commit().unwrap();
        let page = cursor.visit_next(1, 1024, |row| { assert_eq!(row.hash, BlockHash::new([1; 32])); Ok(()) }).unwrap();
        assert!(page.done && page.rows == 1);
        for (rows, bytes) in [(0, 1024), (1, 0), (1, 1)] {
            let mut invalid = blocks.indexed_block_cursor(&txn).unwrap();
            assert!(invalid.visit_next(rows, bytes, |_| panic!("invalid page reached callback")).is_err());
            assert!(invalid.visit_next(1, 1024, |_| Ok(())).is_err());
        }
        let mut failed = blocks.indexed_block_cursor(&txn).unwrap();
        assert!(failed.visit_next(1, 1024, |_| Err(StoreError::Corruption("stop".into()))).is_err());
        assert!(failed.visit_next(1, 1024, |_| Ok(())).is_err());
        drop(failed); drop(cursor); drop(txn);
        let txn = store.tx_begin_read().unwrap();
        let mut current = blocks.indexed_block_cursor(&txn).unwrap();
        assert_eq!(current.expected_rows(), 2);
        let first = current.visit_next(1, 1024, |_| Ok(())).unwrap();
        assert!(!first.done && first.rows == 1);
        let last = current.visit_next(1, 1024, |_| Ok(())).unwrap();
        assert!(last.done && last.rows == 1);
    }

    #[test]
    fn writer_inventory_windows_include_pending_rows_and_preserve_abort() {
        let (_dir, store) = fixture();
        let hash = |n| BlockHash::new([n; 32]);
        let account = WalletAddress::new("brst_writer_window");
        let mut writer = store.tx_begin_write().unwrap();
        for n in [2, 3, 4] {
            writer.put_block_with_account(&hash(n), &[n; 3], &account, n as u64).unwrap();
        }
        writer.delete_block(&hash(1)).unwrap();
        let mut seen = Vec::new();
        let first = writer.visit_indexed_block_window(None, 2, 4096, |row| {
            let borrowed = writer.indexed_block_entries(3, 4096).unwrap()
                .map(Result::unwrap).find(|entry| entry.hash == row.hash).unwrap();
            assert_eq!(borrowed.bytes.as_ptr(), row.bytes.as_ptr());
            seen.push(row.hash); Ok(())
        }).unwrap();
        assert_eq!(seen, [hash(2), hash(3)]);
        assert_eq!(first.physical_rows, 3);
        assert!(!first.end_of_range);
        let second = writer.visit_indexed_block_window(first.resume_after, 2, 4096, |row| {
            seen.push(row.hash); Ok(())
        }).unwrap();
        assert_eq!(seen, [hash(2), hash(3), hash(4)]);
        assert!(second.end_of_range);
        assert_eq!(second.resume_after, Some(hash(4)));
        assert!(writer.visit_indexed_block_window(None, 1, 1,
            |_| panic!("oversized row reached callback")).is_err());
        assert!(writer.visit_indexed_block_window(None, 1, 4096,
            |_| Err(StoreError::Corruption("stop".into()))).is_err());
        drop(writer);
        let view = store.tx_begin_read().unwrap();
        let mut committed = Vec::new();
        let page = store.block_store().visit_indexed_block_window(&view, None, 100, 4096,
            |row| { committed.push(row.hash); Ok(()) }).unwrap();
        assert_eq!(committed, [hash(1)]);
        assert!(page.end_of_range);
    }

    #[test]
    fn inventory_windows_seek_exclusively_and_keep_each_read_view() {
        let (_dir, store) = fixture();
        let hash = |n| BlockHash::new([n; 32]);
        let account = WalletAddress::new("brst_window");
        let mut writer = store.tx_begin_write().unwrap();
        for n in [3, 5] { writer.put_block_with_account(&hash(n), &[n, n], &account, n as u64).unwrap(); }
        writer.commit().unwrap();
        let blocks = store.block_store();
        let view = store.tx_begin_read().unwrap();
        let mut seen = Vec::new();
        let first = blocks.visit_indexed_block_window(&view, None, 2, 4096, |row| {
            let borrowed = blocks.blocks_db.get(&view.txn, row.hash.as_bytes()).unwrap().unwrap();
            assert_eq!(row.bytes.as_ptr(), borrowed.as_ptr());
            seen.push(row.hash); Ok(())
        }).unwrap();
        assert_eq!(seen, [hash(1), hash(3)]);
        assert_eq!(first.physical_rows, 3);
        assert_eq!(first.resume_after, Some(hash(3)));
        assert!(!first.end_of_range);
        let mut writer = store.tx_begin_write().unwrap();
        writer.delete_block(&hash(3)).unwrap();
        for n in [2, 4, 6] { writer.put_block_with_account(&hash(n), &[n, n], &account, n as u64).unwrap(); }
        writer.commit().unwrap();
        seen.clear();
        let old = blocks.visit_indexed_block_window(&view, first.resume_after, 100, 4096,
            |row| { seen.push(row.hash); Ok(()) }).unwrap();
        assert_eq!(seen, [hash(5)]);
        assert!(old.end_of_range);
        drop(view);
        let view = store.tx_begin_read().unwrap();
        seen.clear();
        let current = blocks.visit_indexed_block_window(&view, first.resume_after, 100, 4096,
            |row| { seen.push(row.hash); Ok(()) }).unwrap();
        assert_eq!(seen, [hash(4), hash(5), hash(6)]);
        assert_eq!(current.physical_rows, 5);
        assert!(current.end_of_range, "a suffix end does not cover the new row behind the bookmark");
        let charged = 2 * (account.as_str().len() + 8) + 96 + 2;
        let one = blocks.visit_indexed_block_window(&view, Some(hash(3)), 100, charged,
            |row| { assert_eq!(row.hash, hash(4)); Ok(()) }).unwrap();
        assert_eq!(one.rows, 1);
        assert_eq!(one.bytes, charged);
        assert_eq!(one.resume_after, Some(hash(4)));
        assert!(!one.end_of_range);
        let empty = blocks.visit_indexed_block_window(&view, Some(hash(255)), 1, 4096,
            |_| panic!("read beyond maximum hash")).unwrap();
        assert!(empty.end_of_range);
        assert_eq!((empty.rows, empty.bytes, empty.resume_after), (0, 0, Some(hash(255))));
        let mut all = Vec::new();
        let mut after = None;
        loop {
            let page = blocks.visit_indexed_block_window(&view, after, 2, 4096,
                |row| { all.push(row.hash); Ok(()) }).unwrap();
            after = page.resume_after;
            if page.end_of_range { break; }
            assert!(page.rows > 0);
        }
        assert_eq!(all, [hash(1), hash(2), hash(4), hash(5), hash(6)]);
    }

    #[test]
    fn inventory_windows_bound_work_and_do_not_claim_prefix_integrity() {
        let (_dir, store) = fixture();
        let blocks = store.block_store();
        let hash = BlockHash::new([3; 32]);
        let account = WalletAddress::new("brst_window");
        let mut writer = store.tx_begin_write().unwrap();
        writer.put_block_with_account(&hash, &[3], &account, 1).unwrap();
        writer.commit().unwrap();
        let view = store.tx_begin_read().unwrap();
        for (rows, bytes) in [(0, 4096), (101, 4096), (1, 0), (1, (64 << 20) + 1), (1, 1)] {
            assert!(blocks.visit_indexed_block_window(&view, None, rows, bytes,
                |_| panic!("invalid or insufficient window budget reached callback")).is_err());
        }
        let mut calls = 0;
        assert!(blocks.visit_indexed_block_window(&view, None, 100, 4096,
            |_| { calls += 1; Err(StoreError::Corruption("visitor failed".into())) }).is_err());
        assert_eq!(calls, 1);
        drop(view);
        // Equal table counts alone are not a bijection proof. A range does not
        // inspect the corrupt earlier row, and its result must never claim so.
        let mut writer = blocks.env.write_txn().unwrap();
        blocks.height_db.put(&mut writer, &height_key(&WalletAddress::new("brst_test"), u64::MAX), &[9; 32]).unwrap();
        writer.commit().unwrap();
        let view = store.tx_begin_read().unwrap();
        assert!(blocks.visit_indexed_block_window(&view, None, 100, 4096, |_| Ok(())).is_err());
        let suffix = blocks.visit_indexed_block_window(&view, Some(BlockHash::new([1; 32])), 100, 4096,
            |row| { assert_eq!(row.hash, hash); Ok(()) }).unwrap();
        assert_eq!(suffix.rows, 1);
        assert!(suffix.end_of_range);
        drop(view);
        let mut writer = blocks.env.write_txn().unwrap();
        blocks.block_height_db.delete(&mut writer, hash.as_bytes()).unwrap();
        writer.commit().unwrap();
        assert!(blocks.visit_indexed_block_window(&store.tx_begin_read().unwrap(), Some(hash), 1, 4096,
            |_| panic!("mismatched table counts reached callback")).is_err());
    }

    #[test]
    fn retained_positions_stream_reverse_sparse_heights_from_a_pinned_view() {
        let (_dir, store) = fixture();
        let mut batch = store.tx_begin_write().unwrap();
        for (byte, account, height) in [(2, "brst_test", 1), (3, "brst_abcd", 9), (4, "brst_abcd", 3)] {
            batch.put_block_with_account(&BlockHash::new([byte; 32]), &[byte], &WalletAddress::new(account), height).unwrap();
        }
        batch.commit().unwrap();
        let blocks = store.block_store();
        let txn = store.tx_begin_read().unwrap();
        assert_eq!(blocks.indexed_block_entries(&txn, 4, 4096).unwrap().collect::<Result<Vec<_>, _>>().unwrap().len(), 4);
        let read = || blocks.retained_positions(&txn, 4).unwrap().map(|p| {
            let p = p.unwrap(); (p.hash, p.account, p.height)
        }).collect::<Vec<_>>();
        let expected = vec![(1, "brst_test", u64::MAX), (2, "brst_test", 1), (3, "brst_abcd", 9), (4, "brst_abcd", 3)]
            .into_iter().map(|(b,a,h)| (BlockHash::new([b;32]), WalletAddress::new(a), h)).collect::<Vec<_>>();
        assert_eq!(read(), expected);
        assert!(blocks.retained_positions(&txn, 3).is_err());
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_block_with_account(&BlockHash::new([5;32]), &[5], &WalletAddress::new("brst_test"), 2).unwrap();
        batch.commit().unwrap();
        assert_eq!(read(), expected);
        drop(txn);
        assert_eq!(blocks.retained_positions(&store.tx_begin_read().unwrap(), 5).unwrap().count(), 5);
        for (key, hash) in [(vec![], vec![1;32]), (height_key(&WalletAddress::new("brst_test"),0), vec![1;32]),
            ([b"bad".as_slice(), &1u64.to_be_bytes()].concat(), vec![1;32]), (height_key(&WalletAddress::new("brst_test"),1), vec![0;32]),
            (height_key(&WalletAddress::new("brst_test"),1), vec![1;31])] {
            assert!(position(&key, &hash).is_err());
        }
    }

    #[test]
    fn sparse_inventory_is_bounded_and_consistent() {
        let (_dir, store) = fixture();
        let blocks = store.block_store();
        let txn = store.tx_begin_read().unwrap();
        let rows = blocks.indexed_blocks_txn(&txn, 1, 1024).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].height, u64::MAX);
        assert_eq!(rows[0].bytes, [1, 2]);
        assert!(blocks.indexed_blocks_txn(&txn, 0, 1024).is_err());
        assert!(blocks.indexed_blocks_txn(&txn, 1, 1).is_err());
        let borrowed = blocks
            .indexed_block_entries(&txn, 1, 1024)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(borrowed[0].bytes, [1, 2]);
        assert!(blocks
            .indexed_block_entries(&txn, 1, 1)
            .unwrap()
            .next()
            .unwrap()
            .is_err());
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_block_with_account(
                &BlockHash::new([2; 32]),
                &[3],
                &WalletAddress::new("brst_other"),
                1,
            )
            .unwrap();
        assert_eq!(batch.indexed_blocks(2, 1024).unwrap().len(), 2);
        assert_eq!(
            batch
                .indexed_block_entries(2, 1024)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
                .len(),
            2
        );
        batch.commit().unwrap();
        assert_eq!(borrowed[0].bytes, [1, 2]);
        assert_eq!(
            blocks.indexed_block_entries(&txn, 1, 1024).unwrap().count(),
            1
        );
        assert_eq!(blocks.indexed_blocks_txn(&txn, 1, 1024).unwrap().len(), 1);
        drop(txn);
        assert_eq!(
            blocks
                .indexed_blocks_txn(&store.tx_begin_read().unwrap(), 2, 1024)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn inventory_rejects_orphan_conflicting_and_malformed_rows() {
        for corruption in 0..8 {
            let (_dir, store) = fixture();
            let blocks = store.block_store();
            let mut txn = blocks.env.write_txn().unwrap();
            let hash = [1; 32];
            let key = height_key(&WalletAddress::new("brst_test"), u64::MAX);
            match corruption {
                0 => {
                    blocks.height_db.delete(&mut txn, &key).unwrap();
                }
                1 => {
                    blocks.block_height_db.delete(&mut txn, &hash).unwrap();
                }
                2 => {
                    blocks.blocks_db.put(&mut txn, &[2; 32], &[1]).unwrap();
                }
                3 => {
                    blocks.height_db.put(&mut txn, &key, &[2; 32]).unwrap();
                }
                4 => {
                    blocks.blocks_db.put(&mut txn, &hash, &[]).unwrap();
                }
                5..=7 => {
                    let bad_key = match corruption {
                        5 => height_key(&WalletAddress::new("brst_test"), 0),
                        6 => [b"invalid".as_slice(), &1u64.to_be_bytes()].concat(),
                        _ => [b"\xff".as_slice(), &1u64.to_be_bytes()].concat(),
                    };
                    blocks.height_db.delete(&mut txn, &key).unwrap();
                    blocks.height_db.put(&mut txn, &bad_key, &hash).unwrap();
                    blocks
                        .block_height_db
                        .put(&mut txn, &hash, &bad_key)
                        .unwrap();
                }
                _ => unreachable!(),
            }
            txn.commit().unwrap();
            assert!(
                blocks
                    .indexed_blocks_txn(&store.tx_begin_read().unwrap(), 4, 1024)
                    .is_err(),
                "corruption {corruption}"
            );
            assert!(
                store
                    .tx_begin_write()
                    .unwrap()
                    .indexed_blocks(4, 1024)
                    .is_err(),
                "writer corruption {corruption}"
            );
            let view = store.tx_begin_read().unwrap();
            let mut cursor_result = blocks.indexed_block_cursor(&view);
            if let Ok(cursor) = &mut cursor_result {
                let error = loop {
                    match cursor.visit_next(1, 1024, |_| Ok(())) {
                        Err(error) => break error,
                        Ok(page) => assert!(!page.done, "corrupt inventory completed: {corruption}"),
                    }
                };
                assert!(matches!(error, StoreError::Corruption(_)));
                assert!(cursor.visit_next(1, 1024, |_| Ok(())).is_err());
            }
            drop(cursor_result); drop(view);
            let batch = store.tx_begin_write().unwrap();
            let result = batch
                .indexed_block_entries(4, 1024)
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>());
            assert!(result.is_err(), "borrowed writer corruption {corruption}");
        }
    }
}
