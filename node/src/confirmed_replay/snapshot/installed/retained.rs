use super::super::account_inventory::{AccountInventory, AccountInventoryCapture};
use super::super::{history::{HistoricalBlock, HistoryCursor}, SnapshotContinuation};
use super::{invalid, *};
#[cfg(test)]
use crate::confirmed_replay::scratch_records::{SortedRecordsBuilder};
use crate::{checkpoint_sync::witness, connection_registry::ConnectionRegistry};
use burst_store_lmdb::block::IndexedBlock;
use burst_store_lmdb::read_view::LedgerReadView;
use burst_store_lmdb::write_batch::CleanStorageCut;
use burst_store_lmdb::recovery_inputs::InputWriteError;
use burst_types::{BlockHash, WalletAddress};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};
use tokio::sync::RwLock;

mod cache;
mod observations;
mod walk_sequence;
use walk_sequence::{WalkSequence, WalkSequenceIter};
use observations::{ObservationRows, ObservationBuilder, Observations};
pub(crate) use observations::{BodyObservations, BodyObservationPages};

const MAX_ROWS: usize = 131_072;
const MAX_BYTES: usize = 64 << 20;

/// Without a shared scratch policy, capture limits total rows and raw bytes.
/// With one, raw bytes are bounded per page and tracked temporary files share
/// the directory budget. Both modes require complete inventory traversal.
#[derive(Clone)]
struct InventoryLimits {
    rows: usize,
    raw_bytes: Option<u64>,
    scratch_bytes: u64,
    lease: Option<crate::confirmed_replay::scratch_file::ScratchLimit>,
}
impl InventoryLimits {
    fn legacy(rows: usize, bytes: usize) -> Self {
        Self { rows, raw_bytes: Some(bytes as u64), scratch_bytes: bytes as u64, lease: None }
    }

    fn configured(directory: &std::path::Path) -> Result<Self, StoreError> {
        let Some(lease) = crate::confirmed_replay::scratch_file::ScratchLimit::current(directory).map_err(invalid)? else {
            return Ok(Self::legacy(MAX_ROWS, MAX_BYTES));
        };
        let scratch_bytes = lease.temporary_capacity();
        // Every indexed key alone occupies 32 bytes. This cheap admission bound
        // cannot replace actual aggregate charging of records and their indexes.
        let rows = usize::try_from(scratch_bytes / 32).unwrap_or(usize::MAX);
        Ok(Self { rows, raw_bytes: None, scratch_bytes, lease: Some(lease) })
    }

    fn check_directory(&self, directory: &std::path::Path) -> Result<(), StoreError> {
        if let Some(lease) = &self.lease { lease.check_persistent(directory, 0).map_err(invalid)?; }
        Ok(())
    }

    #[cfg(test)]
    fn builder<T: serde::Serialize + serde::de::DeserializeOwned>(&self, directory: &std::path::Path)
        -> Result<SortedRecordsBuilder<T>, StoreError> {
        self.check_directory(directory)?;
        SortedRecordsBuilder::new(directory, self.rows, self.scratch_bytes)
    }

    fn page_bytes(&self, consumed: u64) -> Result<usize, StoreError> {
        let remaining = match self.raw_bytes {
            Some(limit) => limit.checked_sub(consumed).filter(|bytes| *bytes != 0)
                .ok_or_else(|| invalid("retained inventory exceeds byte budget"))?,
            None => MAX_BYTES as u64,
        };
        Ok(remaining.min(MAX_BYTES as u64) as usize)
    }

    fn visit_writer<'a>(&self, batch: &'a LmdbWriteTransaction<'_>,
        visit: impl FnMut(IndexedBlock<'a>) -> Result<(), StoreError>) -> Result<u64, StoreError> {
        self.visit_view(&batch.read_view(), visit)
    }

    fn visit_view<'a>(&self, batch: &LedgerReadView<'a>,
        mut visit: impl FnMut(IndexedBlock<'a>) -> Result<(), StoreError>) -> Result<u64, StoreError> {
        self.check_directory(batch.storage_directory())?;
        let mut after = None;
        let mut visited = 0usize;
        let mut bytes = 0u64;
        loop {
            let remaining = self.rows.checked_sub(visited).filter(|rows| *rows != 0)
                .ok_or_else(|| invalid("retained inventory exceeds row budget"))?;
            let page = batch.visit_indexed_block_window(after, remaining.min(100), self.page_bytes(bytes)?, &mut visit)?;
            if page.physical_rows > self.rows as u64 { return Err(invalid("retained inventory exceeds row budget")); }
            visited = visited.checked_add(page.rows).filter(|rows| *rows <= self.rows)
                .ok_or_else(|| invalid("retained inventory row accounting overflow"))?;
            bytes = bytes.checked_add(page.bytes as u64)
                .ok_or_else(|| invalid("retained inventory byte accounting overflow"))?;
            after = page.resume_after;
            if page.end_of_range {
                if visited as u64 != page.physical_rows { return Err(invalid("retained inventory is incomplete")); }
                return Ok(visited as u64);
            }
            if page.rows == 0 { return Err(invalid("retained inventory made no progress")); }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Observation {
    hash: BlockHash,
    account: WalletAddress,
    height: u64,
    length: usize,
    digest: [u8; 32],
}

impl Observation {
    fn read_bytes(&self, store: &LmdbStore, view: &burst_store_lmdb::LmdbReadTransaction<'_>)
        -> Result<Vec<u8>, StoreError> {
        let bytes = store.block_store().get_block_txn_bounded(
            view, &self.hash, self.length.min(burst_types::MAX_P2P_MESSAGE_BYTES),
        )?;
        if bytes.len() != self.length || burst_crypto::blake2b_256(&bytes) != self.digest {
            return Err(invalid("retained payload differs from observation"));
        }
        Ok(bytes)
    }
}

impl From<&IndexedBlock<'_>> for Observation {
    fn from(row: &IndexedBlock<'_>) -> Self {
        Self {
            hash: row.hash,
            account: row.account.clone(),
            height: row.height,
            length: row.bytes.len(),
            digest: burst_crypto::blake2b_256(row.bytes),
        }
    }
}

pub(super) struct RetainedInventory {
    rows: Arc<ObservationRows>,
    accounts: AccountInventory,
    limits: InventoryLimits,
    count: u64,
}

/// Owned progress; callers may supply a pinned reader or open one per page.
/// Every supplied view must have unchanged body dependencies; partial captures
/// never publish a proof.
struct InventoryCapture {
    #[cfg(test)]
    last_page_cached: bool,
    header: cache::Header,
    cache: Option<cache::InventoryCache>,
    store: Arc<LmdbStore>,
    cut: CleanStorageCut,
    expected_rows: u64,
    visited: u64,
    after: Option<BlockHash>,
    rows: ObservationBuilder,
    bytes: u64,
    limits: InventoryLimits,
    complete: bool,
    failed: bool,
}

struct CapturedInventory {
    rows: Arc<ObservationRows>,
    cut: CleanStorageCut,
    count: u64,
    limits: InventoryLimits,
}

impl InventoryCapture {
    #[cfg(test)]
    fn new(store: Arc<LmdbStore>, max_rows: usize, max_bytes: usize) -> Result<Self, StoreError> {
        Self::with_limits(store, InventoryLimits::legacy(max_rows, max_bytes))
    }

    fn configured(store: Arc<LmdbStore>) -> Result<Self, StoreError> {
        let limits = InventoryLimits::configured(store.env().path())?;
        Self::with_limits(store, limits)
    }

    fn with_limits(store: Arc<LmdbStore>, limits: InventoryLimits) -> Result<Self, StoreError> {
        let view = store.tx_begin_read()?;
        Self::with_view(store.clone(), limits, &view)
    }

    fn with_view(store: Arc<LmdbStore>, limits: InventoryLimits,
        view: &burst_store_lmdb::LmdbReadTransaction<'_>) -> Result<Self, StoreError> {
        store.read_view(view)?; // Reject a reader from another environment.
        let expected_rows = store.block_store().indexed_block_cursor(view)?.expected_rows();
        if expected_rows > limits.rows as u64 || limits.scratch_bytes == 0 {
            return Err(invalid("retained inventory exceeds capture limits"));
        }
        let cut = view.clean_storage_cut().ok_or_else(|| invalid("retained inventory has no storage identity"))?;
        let header = cache::Header { version: 1, rows: expected_rows,
            revision: u64::try_from(view.generation().ok_or_else(|| invalid("missing inventory revision hint"))?).map_err(invalid)? };
        limits.check_directory(store.env().path())?;
        let rows = ObservationBuilder::new(store.env().path(), limits.rows, limits.scratch_bytes)?;
        Ok(Self { #[cfg(test)] last_page_cached: false, header, cache: None, store, cut, expected_rows, visited: 0, after: None, rows,
            bytes: 0, limits, complete: false, failed: false })
    }

    fn prepare_cache(&mut self, session: &crate::confirmed_replay::snapshot::history::DurableHistorySession)
        -> Result<bool, StoreError> {
        match session.select_inventory_checked()
            .and_then(|inputs| cache::InventoryCache::prepare(inputs, self.header)) {
            Ok(cached) => { let ready = cached.is_some(); self.cache = cached; Ok(ready) }
            Err(InputWriteError::Capacity) => { self.cache = None; Ok(true) }
            Err(InputWriteError::Storage(error)) => Err(error),
        }
    }

    fn advance(&mut self, max_rows: usize, max_bytes: usize) -> Result<bool, StoreError> {
        if self.failed { return Err(invalid("retained inventory capture already failed")); }
        if max_rows == 0 || max_bytes == 0 {
            self.failed = true;
            return Err(invalid("invalid retained capture page budget"));
        }
        if self.complete { return Ok(true); }
        let store = self.store.clone();
        let view = store.tx_begin_read().inspect_err(|_| self.failed = true)?;
        self.advance_view(&view, max_rows, max_bytes)
    }

    fn advance_view(&mut self, view: &burst_store_lmdb::LmdbReadTransaction<'_>,
        max_rows: usize, max_bytes: usize) -> Result<bool, StoreError> {
        if self.failed { return Err(invalid("retained inventory capture already failed")); }
        self.failed = true;
        if max_rows == 0 || max_bytes == 0 { return Err(invalid("invalid retained capture page budget")); }
        if self.complete { self.failed = false; return Ok(true); }
        let max_rows = max_rows.min(burst_store_lmdb::retention::MAX_PAGE_ROWS);
        let max_bytes = max_bytes.min(self.limits.page_bytes(self.bytes)?)
            .min(burst_store_lmdb::retention::MAX_PAGE_BYTES);
        self.store.read_view(view)?;
        let current = view.clean_storage_cut().ok_or_else(|| invalid("retained inventory has no storage identity"))?;
        if !self.cut.body_dependencies_unchanged(&current, &self.store)? {
            return Err(invalid("retained inventory changed between pages"));
        }
        let cached = self.cache.as_ref().map(|cache| cache.page()).transpose()?.flatten();
        let from_cache = cached.is_some();
        #[cfg(test)]
        { self.last_page_cached = from_cache; }
        let mut physical_bytes = None;
        let page = match cached {
            Some(page) => page,
            None => {
                let mut observations = Vec::new();
                let page = self.store.block_store().visit_indexed_block_window(view, self.after,
                    max_rows, max_bytes, |row| { observations.push(Observation::from(&row)); Ok(()) })?;
                if page.physical_rows != self.expected_rows { return Err(invalid("retained inventory cardinality changed")); }
                physical_bytes = Some(page.bytes);
                cache::Page { rows: observations, end: page.end_of_range }
            }
        };
        if page.rows.len() > max_rows || (page.rows.is_empty() && !page.end) {
            return Err(invalid("retained inventory page exceeds row budget or makes no progress"));
        }
        let mut after = self.after;
        let mut bytes = 0usize;
        for row in &page.rows {
            if row.hash.is_zero() || row.height == 0 || row.length == 0
                || !row.account.as_str().starts_with(WalletAddress::PREFIX)
                || row.account.as_str().len() <= WalletAddress::PREFIX.len()
                || after.is_some_and(|hash| row.hash <= hash) {
                return Err(invalid("invalid or unordered retained inventory observation"));
            }
            bytes = bytes.checked_add(burst_store_lmdb::block::physical_inventory_bytes(&row.account, row.length)?)
                .filter(|bytes| *bytes <= max_bytes).ok_or_else(|| invalid("retained inventory page exceeds byte budget"))?;
            self.rows.push(*row.hash.as_bytes(), row)?;
            after = Some(row.hash);
        }
        if physical_bytes.is_some_and(|expected| expected != bytes) {
            return Err(invalid("retained inventory page accounting differs"));
        }
        let visited = self.visited.checked_add(page.rows.len() as u64).filter(|n| *n <= self.expected_rows)
            .ok_or_else(|| invalid("retained capture row accounting overflow"))?;
        if page.end && visited != self.expected_rows { return Err(invalid("retained inventory capture is incomplete")); }
        if let Some(cache) = &mut self.cache {
            match cache.accept(&page, from_cache) {
                Ok(()) => {}
                Err(InputWriteError::Capacity) => self.cache = None,
                Err(InputWriteError::Storage(error)) => return Err(error),
            }
        }
        self.visited = visited;
        self.bytes = self.bytes.checked_add(bytes as u64).ok_or_else(|| invalid("retained capture byte accounting overflow"))?;
        self.after = after;
        self.complete = page.end;
        // The supplied cut identifies this page's storage view. Cached observations
        // remain raw inputs, checked by planning and the publication writer.
        self.cut = current;
        self.failed = false;
        Ok(self.complete)
    }

    fn finish(self) -> Result<CapturedInventory, StoreError> {
        if self.failed || !self.complete { return Err(invalid("retained inventory capture is incomplete")); }
        Ok(CapturedInventory { rows: Arc::new(self.rows.finish()?), cut: self.cut,
            count: self.visited, limits: self.limits })
    }
}

fn observation(rows: &ObservationRows, hash: BlockHash) -> Result<Observation, StoreError> {
    let row = rows.get(hash.as_bytes())?.ok_or_else(|| invalid("retained observation is missing"))?;
    if row.hash != hash { return Err(invalid("retained observation key differs")); }
    Ok(row)
}

impl RetainedInventory {
    fn scan(&self, after: Option<BlockHash>) -> Result<Observations, StoreError> {
        self.rows.scan(after)
    }

    pub(super) fn body_observations(&self) -> BodyObservations {
        BodyObservations::new(self.rows.clone(), self.count)
    }

    pub(super) fn observed_hashes_after(&self, after: Option<BlockHash>)
        -> Result<impl Iterator<Item = Result<(BlockHash, usize), StoreError>>, StoreError> {
        Ok(self.scan(after)?.map(|row| row.map(|row| (row.hash, row.length))))
    }

    /// Decode the captured body after checking its bytes, hash and account.
    /// This read does not verify signatures or finality; callers must establish
    /// whether the block belongs to the authenticated prefix or replayed suffix.
    pub(super) fn observed_body(
        &self, store: &LmdbStore, view: &burst_store_lmdb::LmdbReadTransaction<'_>, hash: &BlockHash,
    ) -> Result<Option<(burst_ledger::StateBlock, u64)>, StoreError> {
        use bincode::Options;
        let Some(row) = self.rows.get(hash.as_bytes())? else { return Ok(None); };
        if row.hash != *hash { return Err(invalid("retained observation key differs")); }
        let bytes = row.read_bytes(store, view)?;
        let block: burst_ledger::StateBlock = bincode::DefaultOptions::new().with_fixint_encoding()
            .with_limit(bytes.len() as u64).reject_trailing_bytes().deserialize(&bytes).map_err(invalid)?;
        if row.height == 0 || block.hash != *hash || block.compute_hash() != *hash || block.account != row.account {
            return Err(invalid("resolved retained body differs from authenticated position"));
        }
        Ok(Some((block, row.height)))
    }

    pub(super) fn capture_successor(
        &self,
        batch: &LmdbWriteTransaction<'_>,
        previous: &crate::confirmed_replay::ConfirmedReplay,
        supplied: &HashMap<BlockHash, (WalletAddress, u64, [u8; 32])>,
    ) -> Result<Self, StoreError> {
        // Merge physical rows with the scratch observations incrementally.
        // The caller-supplied proof map remains in memory.
        let mut old = self.scan(None)?.peekable();
        self.limits.check_directory(batch.storage_directory())?;
        let mut rows = ObservationBuilder::new(batch.storage_directory(), self.limits.rows, self.limits.scratch_bytes)?;
        let count = self.limits.visit_writer(batch, |row| {
            let row = Observation::from(&row);
            while old.peek().is_some_and(|old| old.as_ref().is_ok_and(|old| old.hash < row.hash)) {
                old.next();
            }
            if old.peek().is_some_and(Result::is_err) { return Err(old.next().unwrap().unwrap_err()); }
            let inherited = old.peek().is_some_and(|old| old.as_ref().is_ok_and(|old| *old == row))
                && previous.account(&row.account).is_some_and(|account| row.height <= account.block_count);
            let installed = supplied.get(&row.hash).is_some_and(|(account, height, digest)| {
                *account == row.account && *height == row.height && *digest == row.digest
            });
            if !inherited && !installed { return Err(invalid("post-install retained row has no authenticated origin")); }
            rows.push(*row.hash.as_bytes(), &row)
        })?;
        Ok(Self { rows: Arc::new(rows.finish()?), accounts: AccountInventory::read(batch.account_entries()?)?,
            limits: self.limits.clone(), count })
    }

    /// Only a recovered inventory under an explicit shared scratch policy can
    /// contribute local closure work. Scan without collecting another inventory
    /// and yield every 128 rows; the caller handles cancellation.
    pub(super) async fn closure_allowance(&self, directory: &std::path::Path)
        -> Result<Option<(u64, u64)>, StoreError> {
        if self.limits.lease.is_none() { return Ok(None); }
        self.limits.check_directory(directory)?;
        let mut count = 0u64;
        let mut bytes = 0u64;
        for row in self.observed_hashes_after(None)? {
            let (_, length) = row?;
            count = count.checked_add(1).ok_or_else(|| invalid("closure inventory count overflow"))?;
            bytes = bytes.checked_add(u64::try_from(length).map_err(invalid)?)
                .ok_or_else(|| invalid("closure inventory bytes overflow"))?;
            if count % 128 == 0 { tokio::task::yield_now().await; }
        }
        if count != self.count { return Err(invalid("closure inventory count differs")); }
        Ok(Some((count, bytes)))
    }

    pub(super) fn catalogue_limits(&self, directory: &std::path::Path) -> Result<(usize, u64), StoreError> {
        self.limits.check_directory(directory)?;
        Ok((self.limits.rows, self.limits.scratch_bytes))
    }

    #[cfg(test)]
    pub(super) fn catalogue_builder<T: serde::Serialize + serde::de::DeserializeOwned>(&self,
        directory: &std::path::Path) -> Result<SortedRecordsBuilder<T>, StoreError> {
        self.limits.builder(directory)
    }

    pub(super) fn verify_unchanged(
        &self,
        batch: &LmdbWriteTransaction<'_>,
    ) -> Result<(), StoreError> {
        self.verify_view(&batch.read_view())
    }

    /// Compare every authenticated observation with one borrowed storage view.
    /// Matching a historical snapshot does not authorize live publication.
    pub(super) fn verify_view(&self, view: &LedgerReadView<'_>) -> Result<(), StoreError> {
        self.verify_view_with_poll(view, &|_| Ok(()))
    }

    pub(super) fn verify_view_with_poll(&self, view: &LedgerReadView<'_>, poll: &dyn Fn(&'static str) -> Result<(), StoreError>) -> Result<(), StoreError> {
        poll("metadata:inventory-start")?;
        let mut expected = self.scan(None)?;
        let count = self.limits.visit_view(view, |actual| {
            poll("metadata:inventory-row")?;
            let row = expected.next().transpose()?
                .ok_or_else(|| invalid("retained history gained a physical row"))?;
            if Observation::from(&actual) != row { return Err(invalid("retained history changed during recovery")); }
            Ok(())
        })?;
        if count != self.count || expected.next().transpose()?.is_some() {
            return Err(invalid("retained history coverage changed during recovery"));
        }
        if AccountInventory::read(view.account_entries()?.map(|row| {
            let row = row?;
            poll("metadata:inventory-account")?;
            Ok(row)
        }))? != self.accounts {
            return Err(invalid("retained history changed during recovery"));
        }
        Ok(())
    }
}

// One authenticated iterator follows the contiguous account ranges. Lookahead
// holds at most one witness request; the iterator itself holds one 64-row page.
struct WalkHashes {
    rows: WalkSequenceIter<BlockHash>,
    pending: VecDeque<BlockHash>,
    position: u64,
}
impl WalkHashes {
    fn new(sequence: &WalkSequence<BlockHash>) -> Self {
        Self { rows: sequence.iter(), pending: VecDeque::new(), position: 0 }
    }

    fn peek(&mut self, offset: usize) -> Result<BlockHash, StoreError> {
        if offset >= 128 { return Err(invalid("retained hash lookahead exceeds witness budget")); }
        while self.pending.len() <= offset {
            let hash = self.rows.next().transpose()?
                .ok_or_else(|| invalid("missing retained walk hash"))?;
            self.pending.push_back(hash);
        }
        Ok(self.pending[offset])
    }

    fn pop(&mut self) -> Result<BlockHash, StoreError> {
        let hash = self.peek(0)?;
        self.pending.pop_front();
        self.position += 1;
        Ok(hash)
    }
}

struct WalkRows {
    observation: Option<Observation>,
    hashes: WalkHashes,
    next: u64,
    end: u64,
}
impl WalkRows {
    fn accept_witness_page(&mut self, store: &LmdbStore,
        view: &burst_store_lmdb::LmdbReadTransaction<'_>, observations: &ObservationRows,
        cursor: &mut HistoryCursor, proofs: Vec<super::super::history::HistoricalBlock>, wanted: Vec<BlockHash>,
    ) -> Result<(), StoreError> {
        let mut proofs: HashMap<_, _> = proofs
            .into_iter()
            .map(|proof| (proof.block().hash, proof))
            .collect();
        if proofs.len() != wanted.len() {
            return Err(invalid("incomplete retained history proofs"));
        }
        for hash in wanted {
            let row = self.observed_front(observations)?
                .ok_or_else(|| invalid("missing retained walk row"))?;
            let proof = proofs
                .remove(&hash)
                .ok_or_else(|| invalid("missing retained history proof"))?;
            if proof.height() != row.height || proof.block().account != row.account {
                return Err(invalid("retained history index differs from proof"));
            }
            cursor.seek_proven(&proof).map_err(invalid)?;
            // Validate the local signature too: membership alone does not
            // authenticate bytes excluded from the signed block hash.
            cursor
                .accept(&[row.read_bytes(store, view)?])
                .map_err(invalid)?;
            self.pop_front()?;
        }
        Ok(())
    }

    fn is_empty(&self) -> bool { self.next == self.end }
    fn front(&mut self) -> Result<Option<BlockHash>, StoreError> {
        if self.is_empty() { return Ok(None); }
        if self.next != self.hashes.position { return Err(invalid("retained hash cursor differs from range")); }
        self.hashes.peek(0).map(Some)
    }
    fn observed_front(&mut self, observations: &ObservationRows) -> Result<Option<Observation>, StoreError> {
        let Some(hash) = self.front()? else { return Ok(None); };
        if self.observation.is_none() {
            self.observation = Some(observation(observations, hash)?);
        }
        // The private index is immutable. Reuse its authenticated descriptor,
        // never the live body bytes that must still match it on every read.
        Ok(self.observation.clone())
    }
    fn pop_front(&mut self) -> Result<BlockHash, StoreError> {
        self.front()?.ok_or_else(|| invalid("missing retained walk row"))?;
        let hash = self.hashes.pop()?;
        self.observation = None;
        self.next += 1;
        Ok(hash)
    }
    fn wanted(&mut self) -> Result<Vec<BlockHash>, StoreError> {
        if self.next != self.hashes.position { return Err(invalid("retained hash cursor differs from range")); }
        (0..(self.end - self.next).min(128) as usize)
            .map(|offset| self.hashes.peek(offset)).collect()
    }
}

struct WalkPlan {
    authenticated_heads: Vec<HistoricalBlock>,
    accounts: WalkSequenceIter<(u64, u64)>,
    hashes: Option<WalkHashes>,
    hash_count: u64,
    observations: Arc<ObservationRows>,
    store: Arc<LmdbStore>,
}
impl WalkPlan {
    fn next_walk(&mut self, base: &SnapshotContinuation) -> Result<Option<AccountWalk>, StoreError> {
        let Some((start, end)) = self.accounts.next().transpose()? else {
            let hashes = self.hashes.as_mut().ok_or_else(|| invalid("retained walk still active"))?;
            if hashes.position != self.hash_count || !hashes.pending.is_empty() || hashes.rows.next().transpose()?.is_some() {
                return Err(invalid("retained walk hash coverage differs"));
            }
            return Ok(None);
        };
        if start >= end || end > self.hash_count { return Err(invalid("invalid retained walk range")); }
        let hashes = self.hashes.take().ok_or_else(|| invalid("retained walk still active"))?;
        if hashes.position != start { return Err(invalid("retained walk ranges are not contiguous")); }
        let mut rows = WalkRows { observation: None, hashes, next: start, end };
        let account = rows.observed_front(&self.observations)?
            .ok_or_else(|| invalid("missing retained account observation"))?.account;
        let cursor = base.history_cursor(&account).map_err(invalid)?
            .ok_or_else(|| invalid("retained history has no authenticated account"))?;
        let authenticated_head = loop {
            let Some(proof) = self.authenticated_heads.last() else { break None; };
            if proof.block().account.as_str() < account.as_str() { break None; }
            let proof = self.authenticated_heads.pop().expect("inspected head proof");
            if proof.block().account == account { break Some(proof); }
        };
        Ok(Some(AccountWalk { cursor, authenticated_head, rows,
            observations: self.observations.clone(), store: self.store.clone() }))
    }

    fn finish_walk(&mut self, rows: WalkRows) -> Result<(), StoreError> {
        if !rows.is_empty() || rows.next != rows.hashes.position || self.hashes.is_some() {
            return Err(invalid("retained walk finished with incomplete range"));
        }
        self.hashes = Some(rows.hashes);
        Ok(())
    }
}

struct AccountWalk {
    authenticated_head: Option<HistoricalBlock>,
    cursor: HistoryCursor,
    rows: WalkRows,
    observations: Arc<ObservationRows>,
    store: Arc<LmdbStore>,
}

impl AccountWalk {
    fn accept_durable_page(&mut self, view: &burst_store_lmdb::LmdbReadTransaction<'_>,
        page: Vec<super::super::history::HistoricalBlock>) -> Result<(), StoreError> {
        for proof in page {
            let Some(hash) = self.rows.front()? else { break; };
            let row = self.rows.observed_front(&self.observations)?
                .ok_or_else(|| invalid("missing retained walk row"))?;
            if proof.height() > row.height { continue; }
            if proof.height() != row.height || proof.block().hash != hash || proof.block().account != row.account {
                return Err(invalid("retained index differs from durable proof"));
            }
            self.cursor.seek_proven(&proof).map_err(invalid)?;
            self.cursor.accept(&[row.read_bytes(&self.store, view)?]).map_err(invalid)?;
            self.rows.pop_front()?;
        }
        Ok(())
    }

    /// Return true when another local page may remain; false at a gap or end.
    fn local_prefix(&mut self) -> Result<bool, StoreError> {
        let store = self.store.clone();
        let view = store.tx_begin_read()?;
        self.local_prefix_view(&view)
    }

    fn local_prefix_view(&mut self, view: &burst_store_lmdb::LmdbReadTransaction<'_>) -> Result<bool, StoreError> {
        self.store.read_view(view)?;
        for _ in 0..128 {
            let Some(hash) = self.rows.front()? else { return Ok(false); };
            let wanted = self.rows.observed_front(&self.observations)?
                .ok_or_else(|| invalid("missing retained walk row"))?;
            let Some(head) = self.cursor.next() else {
                return Err(invalid("retained row lies beyond exhausted history"));
            };
            // Committed heads are omitted from the outstanding work list, but
            // the cursor must still authenticate them to reach older rows.
            let row = if head.hash == hash { wanted } else {
                if head.height <= wanted.height {
                    return Err(invalid("retained index differs from authenticated history"));
                }
                let Some(row) = self.observations.get(head.hash.as_bytes())? else {
                    return Ok(false);
                };
                row
            };
            if row.hash != head.hash || row.account != head.account || row.height != head.height {
                return Err(invalid("retained block index differs from authenticated position"));
            }
            let bytes = row.read_bytes(&self.store, &view)?;
            if self.authenticated_head.as_ref().is_some_and(|proof| proof.block().hash == row.hash) {
                let proof = self.authenticated_head.take().expect("matching authenticated head");
                // The signed hash excludes signature/work bytes. Reuse only the
                // exact body that tail planning authenticated, under this cursor.
                if bincode::serialize(proof.block()).map_err(invalid)? != bytes {
                    return Err(invalid("retained head differs from authenticated proof"));
                }
                self.cursor.resume_suffix(vec![proof]).map_err(invalid)?;
            } else {
                self.cursor.accept(&[bytes]).map_err(invalid)?;
            }
            if row.hash == hash { self.rows.pop_front()?; }
        }
        Ok(!self.rows.is_empty())
    }
}

fn plan(
    store: &Arc<LmdbStore>,
    base: &SnapshotContinuation,
    control: &super::metadata::RecoveryControl,
    captured: CapturedInventory,
) -> Result<Option<(RetainedInventory, WalkPlan)>, StoreError> {
    let txn = store.tx_begin_read()?;
    plan_view(store, &txn, base, control, captured)
}

fn plan_view(
    store: &Arc<LmdbStore>,
    txn: &burst_store_lmdb::LmdbReadTransaction<'_>,
    base: &SnapshotContinuation,
    control: &super::metadata::RecoveryControl,
    captured: CapturedInventory,
) -> Result<Option<(RetainedInventory, WalkPlan)>, StoreError> {
    store.read_view(txn)?;
    let current = txn.clean_storage_cut().ok_or_else(|| invalid("retained planning has no storage identity"))?;
    if !captured.cut.body_dependencies_unchanged(&current, store)? {
        return Err(invalid("retained inventory changed before account planning"));
    }
    let account_store = store.account_store();
    let mut accounts = AccountInventoryCapture::new(account_store.account_entries(&txn)?);
    loop {
        let done = accounts.advance(128)?;
        if control.checkpoint("private-recovery:account-inventory-page") { return Ok(None); }
        if done { break; }
    }
    let accounts = accounts.finish()?;
    let block_store = store.block_store();
    let directory = store.env().path();
    let inventory = RetainedInventory { rows: captured.rows, accounts, limits: captured.limits, count: captured.count };
    let mut committed = crate::confirmed_replay::scratch_index::ScratchIndex::new(directory)?;
    let mut history = base.committed_history_walk();
    loop {
        let mut compaction_cancelled = false;
        let result = history.advance(128,
            |hash| Ok(inventory.rows.get(hash.as_bytes())?.is_some()),
            |mut cursor| {
                let head = cursor.next().ok_or_else(|| invalid("missing committed history head"))?;
                let row = observation(&inventory.rows, head.hash)?;
                if head.account != row.account || head.height != row.height {
                    return Err(invalid("retained block differs from committed history head").into());
                }
                // Validate every emitted head before deduplication: contradictory
                // receipt positions cannot be hidden by an already-seen hash.
                if committed.get(row.hash.as_bytes())?.is_none() {
                    let bytes = row.read_bytes(store, &txn)?;
                    cursor.accept(&[bytes])?;
                    committed.insert(*row.hash.as_bytes(), [0; 32])?;
                    if !committed.compact_until(directory,
                        || control.checkpoint("private-recovery:committed-history-compaction"))? {
                        compaction_cancelled = true;
                        return Err(invalid("committed history compaction cancelled").into());
                    }
                }
                Ok(())
            },
        );
        if compaction_cancelled { return Ok(None); }
        let done = result.map_err(invalid)?;
        if control.checkpoint("private-recovery:committed-history-page") { return Ok(None); }
        if done { break; }
    }
    let mut hashes = WalkSequence::builder(directory, inventory.limits.rows, inventory.limits.scratch_bytes)?;
    let mut hash_count = 0u64;
    let mut groups = WalkSequence::builder(directory, inventory.limits.rows, inventory.limits.scratch_bytes)?;
    let mut current: Option<(WalletAddress, u64, u64)> = None;
    let mut previous_height = 0;
    let mut positions = block_store.retained_positions(&txn, inventory.limits.rows)?;
    let mut position_count = 0u64;
    loop {
        let mut visited = 0;
        for position in positions.by_ref().take(128) {
            visited += 1;
            let position = position?;
            position_count = position_count.checked_add(1).filter(|count| *count <= inventory.count)
                .ok_or_else(|| invalid("retained positions exceed captured inventory"))?;
            let row = observation(&inventory.rows, position.hash)?;
            if row.account != position.account || row.height != position.height {
                return Err(invalid("retained position differs from observation"));
            }
            if current.as_ref().is_none_or(|(account, _, _)| *account != row.account) {
                if let Some((account, _, start)) = current.take() {
                    if account.as_str() <= row.account.as_str() {
                        return Err(invalid("retained accounts are not in reverse order"));
                    }
                    if start < hash_count { groups.push(&(start, hash_count))?; }
                }
                let height = account_store.get_account_txn(&txn, &row.account)?.block_count;
                current = Some((row.account.clone(), height, hash_count));
            } else if row.height >= previous_height {
                return Err(invalid("retained account heights are not decreasing"));
            }
            previous_height = row.height;
            if row.height > current.as_ref().expect("current account").1 {
                return Err(invalid("retained block exceeds local account height"));
            }
            if committed.get(row.hash.as_bytes())?.is_some() { continue; }
            if base.state().account(&row.account).is_some_and(|account| row.height <= account.block_count) {
                let next = hash_count.checked_add(1)
                    .filter(|count| *count <= inventory.limits.rows as u64)
                    .ok_or_else(|| invalid("retained walk exceeds row budget"))?;
                next.checked_mul(32).filter(|bytes| *bytes <= inventory.limits.scratch_bytes)
                    .ok_or_else(|| invalid("retained walk exceeds payload budget"))?;
                hashes.push(&row.hash)?;
                hash_count = next;
            }
        }
        if control.checkpoint("private-recovery:retained-position-page") { return Ok(None); }
        if visited < 128 { break; }
    }
    if position_count != inventory.count { return Err(invalid("retained position coverage is incomplete")); }
    if let Some((_, _, start)) = current {
        if start < hash_count { groups.push(&(start, hash_count))?; }
    }
    let hashes = hashes.finish()?;
    let walks = WalkPlan { authenticated_heads: Vec::new(), accounts: groups.finish()?.iter(), hashes: Some(WalkHashes::new(&hashes)), hash_count,
        observations: inventory.rows.clone(), store: store.clone() };
    Ok(Some((inventory, walks)))
}

pub(super) async fn verify(
    store: Arc<LmdbStore>,
    recovered: RecoveredSnapshot,
    registry: &Arc<RwLock<ConnectionRegistry>>,
    control: super::metadata::RecoveryControl,
    mut authenticated_heads: Vec<HistoricalBlock>,
) -> Result<Option<(RecoveredSnapshot, RetainedInventory)>, StoreError> {
    let directory = store.env().path().to_path_buf();
    let source = store.clone();
    let mut capture = tokio::task::spawn_blocking(move || InventoryCapture::configured(source))
        .await.map_err(invalid)??;
    let mut durable_session = None;
    if let Some((limit, map_bytes)) = &control.durable_inputs {
        use super::super::history::DurableHistorySession;
        let shutdown = control.shutdown.as_ref().ok_or_else(|| invalid("durable recovery requires shutdown ownership"))?;
        let Some(mut session) = DurableHistorySession::open(directory.clone(), recovered.continuation.history_context(),
            *map_bytes, limit.clone(), shutdown).await? else { return Ok(None); };
        loop {
            let (next_session, next_capture, ready) = tokio::task::spawn_blocking(move || {
                let ready = capture.prepare_cache(&session)?;
                Ok::<_, StoreError>((session, capture, ready))
            }).await.map_err(invalid)??;
            session = next_session;
            capture = next_capture;
            if control.checkpoint("private-recovery:inventory-cache-page") { return Ok(None); }
            if ready { break; }
        }
        durable_session = Some(session);
    }
    loop {
        let (next, done) = tokio::task::spawn_blocking(move || {
            let done = capture.advance(100, MAX_BYTES)?;
            Ok::<_, StoreError>((capture, done))
        }).await.map_err(invalid)??;
        capture = next;
        #[cfg(test)]
        if control.checkpoint(if capture.last_page_cached {
            "private-recovery:inventory-cached-page"
        } else {
            "private-recovery:inventory-physical-page"
        }) { return Ok(None); }
        if control.checkpoint("private-recovery:retained-inventory-page") { return Ok(None); }
        if done { break; }
    }
    let planning_control = control.clone();
    let planned = tokio::task::spawn_blocking(move || {
        let captured = capture.finish()?;
        let Some((inventory, mut walks)) = plan(&store, &recovered.continuation, &planning_control, captured)? else {
            return Ok(None);
        };
        authenticated_heads.sort_unstable_by(|a, b| a.block().account.as_str().cmp(b.block().account.as_str()));
        walks.authenticated_heads = authenticated_heads;
        Ok::<_, StoreError>(Some((recovered, inventory, walks)))
    }).await.map_err(invalid)??;
    let Some((mut recovered, inventory, mut walks)) = planned else { return Ok(None); };
    if control.checkpoint("private-recovery:retained-planned") { return Ok(None); }
    let mut pending_walk = None;
    loop {
        let local_control = control.clone();
        let local = tokio::task::spawn_blocking(move || {
            let started = std::time::Instant::now();
            let mut accounts = 0u64;
            let mut pages = 0u64;
            // Keep local disk work on this worker until a peer is needed. Each
            // page still releases its reader and checks shutdown before continuing.
            let walk = 'accounts: loop {
                let next = match pending_walk.take() {
                    Some(walk) => Some(walk),
                    None => walks.next_walk(&recovered.continuation)?,
                };
                let Some(mut walk) = next else { break None; };
                accounts += 1;
                loop {
                    let more_local = walk.local_prefix()?;
                    pages += 1;
                    if local_control.checkpoint("private-recovery:local-history-page") {
                        tracing::debug!(accounts, pages, elapsed_ms = started.elapsed().as_millis(),
                            "retained local history cancelled");
                        return Ok(None);
                    }
                    if walk.rows.is_empty() { break; }
                    if !more_local { break 'accounts Some(walk); }
                }
                walks.finish_walk(walk.rows)?;
            };
            tracing::debug!(accounts, pages, elapsed_ms = started.elapsed().as_millis(),
                needs_peer = walk.is_some(), "retained local history advanced");
            Ok::<_, StoreError>(Some((recovered, walks, walk)))
        }).await.map_err(invalid)??;
        let Some((next_recovered, next_walks, walk)) = local else { return Ok(None); };
        pending_walk = None;
        recovered = next_recovered;
        walks = next_walks;
        let Some(mut walk) = walk else { break; };
        if let Some((limit, map_bytes)) = &control.durable_inputs {
            use super::super::history::DurableHistorySession;
            let shutdown = control.shutdown.as_ref().ok_or_else(|| invalid("durable recovery requires shutdown ownership"))?;
            if durable_session.is_none() {
                durable_session = DurableHistorySession::open(directory.clone(), recovered.continuation.history_context(),
                    *map_bytes, limit.clone(), shutdown).await?;
                if durable_session.is_none() { return Ok(None); }
            }
            let row = walk.rows.observed_front(&walk.observations)?.ok_or_else(|| invalid("missing retained row"))?;
            let cursor = recovered.continuation.history_cursor(&row.account).map_err(invalid)?
                .ok_or_else(|| invalid("missing durable account root"))?;
            let session = durable_session.take().expect("opened durable session");
            let (session, inputs) = session.select_with_capacity(cursor, shutdown).await?;
            let Some(mut inputs) = inputs else { return Ok(None); };
            while !walk.rows.is_empty() {
                let (next, page) = inputs.next_page(registry, shutdown).await?;
                inputs = next;
                let Some(page) = page else { return Ok(None); };
                if page.is_empty() { return Err(invalid("durable history ended before retained rows")); }
                walk = tokio::task::spawn_blocking(move || {
                    let store = walk.store.clone();
                    let view = store.tx_begin_read()?;
                    walk.accept_durable_page(&view, page)?;
                    drop(view);
                    Ok::<_, StoreError>(walk)
                }).await.map_err(invalid)??;
                if control.checkpoint("private-recovery:durable-history-page") { return Ok(None); }
            }
            durable_session = Some(session);
            walks.finish_walk(walk.rows)?;
            continue;
        }
        // Keep each retained response below the existing witness budgets.
        let wanted = walk.rows.wanted()?;
        let Some(proofs) = control.peer_wait(
            witness::fetch(registry, walk.cursor, wanted.clone()),
        ).await? else { return Ok(None); };
        let mut cursor = recovered
            .continuation
            .history_cursor(&walk.rows.observed_front(&walk.observations)?.ok_or_else(|| invalid("missing retained history row"))?.account)
            .map_err(invalid)?
            .ok_or_else(|| invalid("missing retained history root"))?;
        pending_walk = Some(tokio::task::spawn_blocking(move || {
            let view = walk.store.tx_begin_read()?;
            walk.rows.accept_witness_page(&walk.store, &view, &walk.observations, &mut cursor, proofs, wanted)?;
            drop(view);
            Ok::<_, StoreError>(AccountWalk {
                cursor,
                authenticated_head: walk.authenticated_head,
                rows: walk.rows,
                observations: walk.observations,
                store: walk.store,
            })
        })
        .await
        .map_err(invalid)??);
    }
    Ok(Some((recovered, inventory)))
}

/// Run only on the worker that owns `view`. Disk phases stay on that worker;
/// peer waits remain cancellable and never move or reopen its native reader.
pub(super) async fn verify_captured(
    store: Arc<LmdbStore>,
    recovered: RecoveredSnapshot,
    view: &burst_store_lmdb::LmdbReadTransaction<'_>,
    registry: &Arc<RwLock<ConnectionRegistry>>,
    control: super::metadata::RecoveryControl,
) -> Result<Option<(RecoveredSnapshot, RetainedInventory)>, StoreError> {
    let directory = store.env().path().to_path_buf();
    let limits = InventoryLimits::configured(store.env().path())?;
    let mut capture = InventoryCapture::with_view(store.clone(), limits, view)?;
    let mut durable_session = None;
    if let Some((limit, map_bytes)) = &control.durable_inputs {
        use super::super::history::DurableHistorySession;
        let shutdown = control.shutdown.as_ref().ok_or_else(|| invalid("durable recovery requires shutdown ownership"))?;
        let Some(session) = DurableHistorySession::open(directory.clone(), recovered.continuation.history_context(),
            *map_bytes, limit.clone(), shutdown).await? else { return Ok(None); };
        loop {
            let ready = capture.prepare_cache(&session)?;
            if control.checkpoint("private-recovery:inventory-cache-page") { return Ok(None); }
            if ready { break; }
        }
        durable_session = Some(session);
    }
    loop {
        let done = capture.advance_view(view, 100, MAX_BYTES)?;
        #[cfg(test)]
        if control.checkpoint(if capture.last_page_cached {
            "private-recovery:inventory-cached-page"
        } else {
            "private-recovery:inventory-physical-page"
        }) { return Ok(None); }
        if control.checkpoint("private-recovery:retained-inventory-page") { return Ok(None); }
        if done { break; }
    }
    let captured = capture.finish()?;
    let Some((inventory, mut walks)) = plan_view(&store, view, &recovered.continuation, &control, captured)?
        else { return Ok(None); };
    if control.checkpoint("private-recovery:retained-planned") { return Ok(None); }
    loop {
        let Some(mut walk) = walks.next_walk(&recovered.continuation)? else { break; };
        loop {
            let more_local = walk.local_prefix_view(view)?;
            if control.checkpoint("private-recovery:local-history-page") { return Ok(None); }
            if walk.rows.is_empty() {
                break;
            }
            if more_local { continue; }
            if let Some((limit, map_bytes)) = &control.durable_inputs {
                use super::super::history::DurableHistorySession;
                let shutdown = control.shutdown.as_ref().ok_or_else(|| invalid("durable recovery requires shutdown ownership"))?;
                if durable_session.is_none() {
                    durable_session = DurableHistorySession::open(directory.clone(), recovered.continuation.history_context(),
                        *map_bytes, limit.clone(), shutdown).await?;
                    if durable_session.is_none() { return Ok(None); }
                }
                let row = walk.rows.observed_front(&walk.observations)?.ok_or_else(|| invalid("missing retained row"))?;
                let cursor = recovered.continuation.history_cursor(&row.account).map_err(invalid)?
                    .ok_or_else(|| invalid("missing durable account root"))?;
                let session = durable_session.take().expect("opened durable session");
                let (session, inputs) = session.select_with_capacity(cursor, shutdown).await?;
                let Some(mut inputs) = inputs else { return Ok(None); };
                while !walk.rows.is_empty() {
                    let (next, page) = inputs.next_page(registry, shutdown).await?;
                    inputs = next;
                    let Some(page) = page else { return Ok(None); };
                    if page.is_empty() { return Err(invalid("durable history ended before retained rows")); }
                    walk.accept_durable_page(view, page)?;
                    if control.checkpoint("private-recovery:durable-history-page") { return Ok(None); }
                }
                durable_session = Some(session);
                break;
            }
            // Keep each retained response below the existing witness budgets.
            let wanted = walk.rows.wanted()?;
            let Some(proofs) = control.peer_wait(
                witness::fetch(registry, walk.cursor, wanted.clone()),
            ).await? else { return Ok(None); };
            let mut cursor = recovered
                .continuation
                .history_cursor(&walk.rows.observed_front(&walk.observations)?.ok_or_else(|| invalid("missing retained history row"))?.account)
                .map_err(invalid)?
                .ok_or_else(|| invalid("missing retained history root"))?;
            walk = {
                walk.rows.accept_witness_page(&walk.store, view, &walk.observations, &mut cursor, proofs, wanted)?;
                AccountWalk {
                    cursor,
                    authenticated_head: walk.authenticated_head,
                    rows: walk.rows,
                    observations: walk.observations,
                    store: walk.store,
                }
            };
        }
        walks.finish_walk(walk.rows)?;
    }
    Ok(Some((recovered, inventory)))
}

#[cfg(test)]
mod resolution_tests {
    use super::*;
    use burst_store::{block::BlockStore, meta::MetaStore};

    #[test]
    fn configured_capture_fails_closed_on_shared_budget_and_foreign_directory() {
        use crate::confirmed_replay::scratch_file::ScratchLimit;
        let dir = tempfile::tempdir().unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 32, 16 << 20).unwrap());
        let mut writer = store.tx_begin_write().unwrap();
        for n in 1..=8 { writer.put_block_with_account(&BlockHash::new([n; 32]), &[n],
            &WalletAddress::new("brst_bounded"), n as u64).unwrap(); }
        writer.commit().unwrap();
        let lease = ScratchLimit::acquire(dir.path(), 512).unwrap();
        let limits = InventoryLimits::configured(dir.path()).unwrap();
        assert!(limits.builder::<u64>(foreign.path()).is_err());
        let mut capture = InventoryCapture::configured(store).unwrap();
        drop(lease);
        assert!(capture.advance(100, MAX_BYTES).is_err());
        assert!(capture.advance(100, MAX_BYTES).is_err());
        assert!(capture.finish().is_err(), "incomplete capture must never become an inventory");
        drop(limits);
        assert!(ScratchLimit::current(dir.path()).unwrap().is_none());
    }

    #[tokio::test]
    async fn closure_allowance_requires_policy_and_complete_inventory() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 64, 64 << 20).unwrap());
        let account = crate::genesis_state::block(NetworkId::Dev, &ProtocolParams::default()).account;
        let mut writer = store.tx_begin_write().unwrap();
        for n in 1..=260u64 {
            let mut key = [0; 32];
            key[..8].copy_from_slice(&n.to_be_bytes());
            writer.put_block_with_account(&BlockHash::new(key), &[7; 17], &account, n).unwrap();
        }
        writer.commit().unwrap();
        for configured in [false, true] {
            let lease = configured.then(|| crate::confirmed_replay::scratch_file::ScratchLimit::acquire(dir.path(), 4 << 20).unwrap());
            let mut capture = InventoryCapture::configured(store.clone()).unwrap();
            while !capture.advance(100, MAX_BYTES).unwrap() {}
            let captured = capture.finish().unwrap();
            let writer = store.tx_begin_write().unwrap();
            let mut inventory = RetainedInventory { rows: captured.rows, limits: captured.limits,
                count: captured.count, accounts: AccountInventory::read(writer.account_entries().unwrap()).unwrap() };
            drop(writer);
            assert_eq!(inventory.closure_allowance(dir.path()).await.unwrap(),
                configured.then_some((260, 260 * 17)));
            if configured {
                inventory.count += 1;
                assert!(inventory.closure_allowance(dir.path()).await.is_err());
                let foreign = tempfile::tempdir().unwrap();
                assert!(inventory.closure_allowance(foreign.path()).await.is_err());
            }
            drop(inventory);
            if let Some(lease) = lease { assert_eq!(lease.used_temporary_bytes().unwrap(), 0); }
        }
    }

    #[test]
    #[ignore = "retained physical inventory capacity resource probe"]
    fn budgeted_inventory_crosses_legacy_row_and_raw_byte_limits() {
        use crate::confirmed_replay::scratch_file::ScratchLimit;
        use crate::confirmed_history::{ConfirmedHistory, HistoryHead};
        // Synthetic physical rows exercise inventory coverage/accounting only.
        // Supplied positions below stand in for the caller's installation proof;
        // the separate installed-recovery integration tests check authentication.
        for (count, body_size, scratch_bytes) in [(MAX_ROWS as u64 + 1, 2, 192 << 20), (1025, 64 << 10, 16 << 20)] {
            let started = std::time::Instant::now();
            let dir = tempfile::tempdir().unwrap().keep();
            let store = Arc::new(LmdbStore::open(dir.as_path(), 64, 256 << 20).unwrap());
            let params = ProtocolParams::default();
            let genesis = crate::genesis_state::block(NetworkId::Dev, &params);
            let account = genesis.account.clone();
            let hash = |n: u64| { let mut key = [0; 32]; key[..8].copy_from_slice(&n.to_be_bytes()); BlockHash::new(key) };
            let body = vec![7; body_size];
            let mut writer = store.tx_begin_write().unwrap();
            for n in 1..=count { writer.put_block_with_account(&hash(n), &body, &account, n).unwrap(); }
            writer.commit().unwrap();
            // Preserve the physical baseline even if a later phase fails.
            std::fs::write(dir.join("capacity-probe.json"), format!(
                "{{\"rows\":{count},\"bodyBytes\":{body_size},\"scratchBytes\":{scratch_bytes}}}\n",
            )).unwrap();
            eprintln!("inventory_capacity_fixture {}", dir.display());
            match InventoryCapture::configured(store.clone()) {
                Err(_) => assert!(count > MAX_ROWS as u64),
                Ok(mut legacy) => {
                    loop {
                        match legacy.advance(100, MAX_BYTES) {
                            Err(_) => break,
                            Ok(done) => assert!(!done, "legacy whole-set limit unexpectedly admitted inventory"),
                        }
                    }
                    assert!(legacy.finish().is_err());
                }
            }
            let lease = ScratchLimit::acquire(dir.as_path(), scratch_bytes).unwrap();
            let mut capture = InventoryCapture::configured(store.clone()).unwrap();
            while !capture.advance(100, MAX_BYTES).unwrap() {}
            let captured = capture.finish().unwrap();
            assert_eq!(captured.count, count);
            let writer = store.tx_begin_write().unwrap();
            let inventory = RetainedInventory { rows: captured.rows, limits: captured.limits, count,
                accounts: AccountInventory::read(writer.account_entries().unwrap()).unwrap() };
            inventory.verify_unchanged(&writer).unwrap();
            drop(writer);
            let view = store.tx_begin_read().unwrap();
            let mut expected_height = count;
            for position in store.block_store().retained_positions(&view, inventory.limits.rows).unwrap() {
                let position = position.unwrap();
                assert_eq!(position.height, expected_height);
                assert_eq!(observation(&inventory.rows, position.hash).unwrap().height, expected_height);
                expected_height -= 1;
            }
            assert_eq!(expected_height, 0);
            drop(view);
            let history = ConfirmedHistory::from_blocks(NetworkId::Dev, genesis.hash, genesis.timestamp.as_secs(),
                &[HistoryHead { account: account.clone(), height: 1, hash: genesis.hash }], &[], vec![genesis]).unwrap();
            let previous = crate::confirmed_replay::ConfirmedReplay::replay(&history, NetworkId::Dev, &params).unwrap();
            let digest = burst_crypto::blake2b_256(&body);
            let mut supplied = (1..=count).map(|n| (hash(n), (account.clone(), n, digest)))
                .collect::<HashMap<_, _>>();
            let last = supplied.remove(&hash(count)).unwrap();
            let writer = store.tx_begin_write().unwrap();
            assert!(inventory.capture_successor(&writer, &previous, &supplied).is_err(), "missing installation proof");
            supplied.insert(hash(count), last);
            let successor = inventory.capture_successor(&writer, &previous, &supplied).unwrap();
            successor.verify_unchanged(&writer).unwrap();
            drop(writer);
            drop(supplied);
            drop(inventory);
            drop(lease);
            assert!(ScratchLimit::current(dir.as_path()).unwrap().is_some());
            let mut catalogue = successor.catalogue_builder::<u64>(dir.as_path()).unwrap();
            for (hash, length) in successor.observed_hashes_after(None).unwrap().map(Result::unwrap) {
                catalogue.push(*hash.as_bytes(), &(length as u64)).unwrap();
            }
            let catalogue = catalogue.finish().unwrap();
            assert_eq!(catalogue.get(hash(count).as_bytes()).unwrap(), Some(body_size as u64));
            let mut writer = store.tx_begin_write().unwrap();
            writer.put_block_with_account(&hash(count), &[8], &account, count).unwrap();
            assert!(successor.verify_unchanged(&writer).is_err());
            drop(writer);
            successor.verify_unchanged(&store.tx_begin_write().unwrap()).unwrap();
            drop(catalogue);
            drop(successor);
            assert!(ScratchLimit::current(dir.as_path()).unwrap().is_none());
            eprintln!("inventory_capacity rows={count} body_bytes={} scratch_limit={scratch_bytes} elapsed_ms={}",
                count * body_size as u64, started.elapsed().as_millis());
        }
    }

    #[test]
    fn paged_capture_matches_legacy_inventory_and_keeps_one_view() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 64, 16 << 20).unwrap());
        let account = WalletAddress::new("brst_capture");
        let hash = |n: u64| { let mut bytes = [0; 32]; bytes[..8].copy_from_slice(&n.to_be_bytes()); BlockHash::new(bytes) };
        let mut writer = store.tx_begin_write().unwrap();
        for n in 1..=301 { writer.put_block_with_account(&hash(n), &[1, 2], &account, n).unwrap(); }
        writer.commit().unwrap();
        let view = store.tx_begin_read().unwrap();
        let expected = store.block_store().indexed_block_entries(&view, MAX_ROWS, MAX_BYTES).unwrap()
            .map(|row| Observation::from(&row.unwrap())).collect::<Vec<_>>();
        drop(view);
        let mut capture = InventoryCapture::new(store.clone(), MAX_ROWS, MAX_BYTES).unwrap();
        assert!(!capture.advance(3, 4096).unwrap());
        // A new same-thread reader succeeds: capture owns no LMDB reader.
        drop(store.tx_begin_read().unwrap());
        let mut pages = 1;
        while !capture.advance(3, 4096).unwrap() { pages += 1; }
        assert!(pages >= 100);
        let rows = capture.finish().unwrap().rows;
        let mut actual = Vec::new();
        rows.visit(|_, row| { actual.push(row); Ok(()) }).unwrap();
        assert_eq!(actual, expected);
        assert!(rows.get(hash(302).as_bytes()).unwrap().is_none());
        let writer = store.tx_begin_write().unwrap();
        let inventory = RetainedInventory { rows: rows.clone(), limits: InventoryLimits::legacy(MAX_ROWS, MAX_BYTES), count: 301,
            accounts: AccountInventory::read(writer.account_entries().unwrap()).unwrap() };
        inventory.verify_unchanged(&writer).unwrap();
        drop(writer);
        // Publication comparison must cross every 100-row boundary and reject
        // changes in the writer view even when the database has not committed.
        for n in [1, 100, 101, 200, 301] {
            let mut writer = store.tx_begin_write().unwrap();
            writer.put_block_with_account(&hash(n), &[9, 9], &account, n).unwrap();
            assert!(inventory.verify_unchanged(&writer).is_err(), "changed row{n}");
            drop(writer);
            let mut writer = store.tx_begin_write().unwrap();
            writer.delete_block(&hash(n)).unwrap();
            assert!(inventory.verify_unchanged(&writer).is_err(), "missing row{n}");
        }
        let mut writer = store.tx_begin_write().unwrap();
        writer.put_block_with_account(&hash(302), &[3], &account, 302).unwrap();
        assert!(inventory.verify_unchanged(&writer).is_err(), "extra physical suffix");
        drop(writer);
        inventory.verify_unchanged(&store.tx_begin_write().unwrap()).unwrap();

        // Keep one logical physical view by rejecting relevant intervening writes.
        let mut capture = InventoryCapture::new(store.clone(), MAX_ROWS, MAX_BYTES).unwrap();
        assert!(!capture.advance(3, 4096).unwrap());
        let mut writer = store.tx_begin_write().unwrap();
        writer.put_block_with_account(&hash(302), &[3], &account, 302).unwrap();
        writer.commit().unwrap();
        assert!(capture.advance(3, 4096).unwrap_err().to_string().contains("changed between pages"));
        assert!(capture.advance(3, 4096).is_err());
        assert!(capture.finish().is_err());

        // A deliberately pinned capture compares against its own historical
        // snapshot, while the ordinary writer guard must reject the live change.
        let mut writer = store.tx_begin_write().unwrap();
        writer.delete_block(&hash(302)).unwrap();
        writer.commit().unwrap();
        let pinned = store.tx_begin_read().unwrap();
        let mut capture = InventoryCapture::with_view(store.clone(),
            InventoryLimits::legacy(MAX_ROWS, MAX_BYTES), &pinned).unwrap();
        assert!(!capture.advance_view(&pinned, 3, 4096).unwrap());
        let mut writer = store.tx_begin_write().unwrap();
        writer.put_block_with_account(&hash(302), &[3], &account, 302).unwrap();
        writer.commit().unwrap();
        while !capture.advance_view(&pinned, 3, 4096).unwrap() {}
        let captured = capture.finish().unwrap();
        assert_eq!(captured.count, 301);
        assert!(captured.rows.get(hash(302).as_bytes()).unwrap().is_none());
        inventory.verify_view(&store.read_view(&pinned).unwrap()).unwrap();
        assert!(inventory.verify_unchanged(&store.tx_begin_write().unwrap()).is_err());
        // Isolate account comparison: restore bodies, then change only accounts.
        let mut writer = store.tx_begin_write().unwrap();
        writer.delete_block(&hash(302)).unwrap();
        writer.put_account_info(&burst_store::account::AccountInfo::unopened(account), false).unwrap();
        writer.commit().unwrap();
        inventory.verify_view(&store.read_view(&pinned).unwrap()).unwrap();
        assert!(inventory.verify_unchanged(&store.tx_begin_write().unwrap()).unwrap_err()
            .to_string().contains("retained history changed during recovery"));
    }

    #[test]
    fn capture_requires_completion_and_preserves_cumulative_limits() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 64, 16 << 20).unwrap());
        let account = WalletAddress::new("brst_capture");
        let mut writer = store.tx_begin_write().unwrap();
        for n in 1..=10 {
            writer.put_block_with_account(&BlockHash::new([n; 32]), &[n], &account, n as u64).unwrap();
        }
        writer.commit().unwrap();
        assert!(InventoryCapture::new(store.clone(), 9, MAX_BYTES).is_err());
        let mut partial = InventoryCapture::new(store.clone(), MAX_ROWS, MAX_BYTES).unwrap();
        assert!(!partial.advance(1, 1024).unwrap());
        assert!(partial.finish().is_err());
        let mut limited = InventoryCapture::new(store.clone(), MAX_ROWS, 512).unwrap();
        let mut pages = 0;
        loop {
            match limited.advance(1, 512) {
                Ok(done) => { assert!(!done); pages += 1; assert!(pages < 10); },
                Err(_) => break,
            }
        }
        assert!(pages > 0, "budget must fail across pages, not only at construction");
        assert!(limited.advance(1, MAX_BYTES).is_err());
        assert!(limited.finish().is_err());
        assert_eq!(store.block_store().block_count().unwrap(), 10);

        let other_directory = tempfile::tempdir().unwrap();
        let other = LmdbStore::open(other_directory.path(), 64, 16 << 20).unwrap();
        let foreign = other.tx_begin_read().unwrap();
        assert!(InventoryCapture::with_view(store.clone(),
            InventoryLimits::legacy(MAX_ROWS, MAX_BYTES), &foreign).is_err());
        let mut capture = InventoryCapture::new(store.clone(), MAX_ROWS, MAX_BYTES).unwrap();
        assert!(capture.advance_view(&foreign, 1, 1024).is_err());
        assert!(capture.advance(1, 1024).is_err());
        assert!(capture.finish().is_err());
    }

    #[test]
    fn capture_reuses_only_covered_soft_commits_and_requires_full_range_coverage() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 64, 16 << 20).unwrap());
        let account = WalletAddress::new("brst_capture");
        let voter = WalletAddress::new("brst_capture_voter");
        let epoch = BlockHash::new([91; 32]);
        let mut writer = store.tx_begin_write().unwrap();
        for n in 1..=5 {
            writer.put_block_with_account(&BlockHash::new([n; 32]), &[n], &account, n as u64).unwrap();
        }
        writer.put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, epoch.as_bytes()).unwrap();
        writer.commit().unwrap();
        let mut capture = InventoryCapture::new(store.clone(), MAX_ROWS, MAX_BYTES).unwrap();
        for n in 1..=5 {
            store.meta_store().reserve_consensus_vote(&epoch, &voter, &account,
                &BlockHash::ZERO, &BlockHash::new([1; 32]), false, 0).unwrap();
            assert_eq!(capture.advance(1, 1024).unwrap(), n == 5);
            drop(store.tx_begin_read().unwrap());
        }
        let captured = capture.finish().unwrap();
        store.tx_begin_write().unwrap().check_detached_body_retention_cut(&captured.cut).unwrap();
        let mut seen = 0;
        captured.rows.visit(|_, _| { seen += 1; Ok(()) }).unwrap();
        assert_eq!(seen, 5);
        let mut changed = InventoryCapture::new(store.clone(), MAX_ROWS, MAX_BYTES).unwrap();
        assert!(!changed.advance(1, 1024).unwrap());
        store.meta_store().put_meta("uncovered_capture_change", &[1]).unwrap();
        store.meta_store().delete_meta("uncovered_capture_change").unwrap();
        assert!(changed.advance(1, 1024).is_err());
        assert!(changed.finish().is_err());
        let mut skipped = InventoryCapture::new(store.clone(), MAX_ROWS, MAX_BYTES).unwrap();
        skipped.after = Some(BlockHash::new([4; 32]));
        assert!(skipped.advance(100, MAX_BYTES).unwrap_err().to_string().contains("incomplete"));
        assert!(skipped.finish().is_err());
    }

    #[test]
    fn retained_walk_ranges_bound_witness_pages_and_reject_scratch_damage() {
        let dir = tempfile::tempdir().unwrap();
        // The paged walk and its index must fit in 16 KiB.
        let limit = crate::confirmed_replay::scratch_file::ScratchLimit::acquire(dir.path(), 16 << 10).unwrap();
        let mut hashes = WalkSequence::builder(dir.path(), 301, 16 << 10).unwrap();
        let expected: Vec<_> = (0u64..301).map(|i| BlockHash::new(burst_crypto::blake2b_256(&i.to_be_bytes()))).collect();
        for hash in &expected { hashes.push(hash).unwrap(); }
        let hashes = Arc::new(hashes.finish().unwrap());
        let mut rows = WalkRows { observation: None, hashes: WalkHashes::new(&hashes), next: 1, end: 300 };
        assert_eq!(rows.hashes.pop().unwrap(), expected[0]);
        let mut seen = Vec::new();
        while !rows.is_empty() {
            let wanted = rows.wanted().unwrap();
            assert!(wanted.len() <= 128 && !wanted.is_empty());
            assert_eq!(rows.front().unwrap(), wanted.first().copied());
            for hash in wanted { assert_eq!(rows.pop_front().unwrap(), hash); seen.push(hash); }
        }
        assert_eq!(seen, expected[1..300]);
        assert!(rows.front().unwrap().is_none());
        assert!(rows.wanted().unwrap().is_empty());
        assert!(rows.pop_front().is_err());
        drop(rows);
        let mut missing = WalkRows { observation: None, hashes: WalkHashes::new(&hashes), next: 301, end: 302 };
        assert!(missing.front().is_err());
        assert!(missing.wanted().is_err());
        assert!(missing.pop_front().is_err());
        assert_eq!(missing.next, 301);
        drop(missing);
        let mut rows = WalkRows { observation: None, hashes: WalkHashes::new(&hashes), next: 0, end: 301 };
        let mut peeked = WalkRows { observation: None, hashes: WalkHashes::new(&hashes), next: 0, end: 301 };
        assert_eq!(peeked.front().unwrap(), Some(expected[0]));
        assert_eq!(peeked.front().unwrap(), Some(expected[0]));
        assert_eq!(peeked.next, 0);
        hashes.truncate_for_test();
        // The entire first page was already authenticated. Unread pages still
        // require storage authentication, and cannot advance after damage.
        for hash in &expected[..64] { assert_eq!(peeked.pop_front().unwrap(), *hash); }
        assert!(peeked.front().is_err());
        assert!(peeked.pop_front().is_err());
        assert_eq!(peeked.next, 64);
        drop(peeked);
        assert!(rows.front().is_err());
        assert!(rows.wanted().is_err());
        assert!(rows.pop_front().is_err());
        assert_eq!(rows.next, 0);
        drop(rows);
        drop(hashes);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    }

    #[test]
    fn retained_hash_iterator_reads_each_page_once_across_account_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let mut sequence = WalkSequence::builder(dir.path(), 1025, 64 << 10).unwrap();
        let expected: Vec<_> = (0u64..1025)
            .map(|n| BlockHash::new(burst_crypto::blake2b_256(&n.to_be_bytes()))).collect();
        for hash in &expected { sequence.push(hash).unwrap(); }
        let sequence = sequence.finish().unwrap();
        let mut hashes = WalkHashes::new(&sequence);
        // Short accounts share pages; long accounts cross both page and witness
        // boundaries. Account discovery, repeated peeks and wanted() never reread.
        let mut start = 0;
        for end in [1, 2, 63, 64, 65, 300, 301, 1025] {
            assert_eq!(hashes.peek(0).unwrap(), expected[start as usize]);
            let mut rows = WalkRows { observation: None, hashes, next: start, end };
            while !rows.is_empty() {
                let wanted = rows.wanted().unwrap();
                assert_eq!(rows.wanted().unwrap(), wanted);
                assert_eq!(rows.front().unwrap(), wanted.first().copied());
                for hash in wanted {
                    assert_eq!(hash, expected[rows.next as usize]);
                    assert_eq!(rows.pop_front().unwrap(), hash);
                }
                assert!(rows.hashes.pending.len() <= 128);
            }
            hashes = rows.hashes;
            start = end;
        }
        assert_eq!(hashes.position, 1025);
        assert!(hashes.rows.next().is_none());
        assert_eq!(hashes.rows.pages_read(), 1025u64.div_ceil(64));
        assert!(hashes.peek(128).is_err());
    }

    #[test]
    fn retained_walk_reuses_only_the_current_authenticated_observation() {
        let dir = tempfile::tempdir().unwrap();
        let mut observations = ObservationBuilder::new(dir.path(), 3, 4096).unwrap();
        let mut sequence = WalkSequence::builder(dir.path(), 3, 4096).unwrap();
        let expected: Vec<_> = (0..3).map(|n| Observation {
            hash: BlockHash::new([n; 32]), account: WalletAddress::new("brst_observed"),
            height: 3 - n as u64, length: 37, digest: [n; 32],
        }).collect();
        for row in &expected {
            observations.push(*row.hash.as_bytes(), row).unwrap();
            sequence.push(&row.hash).unwrap();
        }
        let observations = observations.finish().unwrap();
        let sequence = sequence.finish().unwrap();
        let mut rows = WalkRows { observation: None, hashes: WalkHashes::new(&sequence), next: 0, end: 3 };
        for row in &expected[..2] {
            for _ in 0..128 {
                assert_eq!(rows.observed_front(&observations).unwrap().as_ref(), Some(row));
                assert_eq!(rows.front().unwrap(), Some(row.hash));
            }
            assert_eq!(observations.lookup_count(), rows.next as usize + 1);
            if rows.next == 0 { assert_eq!(rows.pop_front().unwrap(), row.hash); }
        }
        observations.truncate_for_test();
        // Only the already authenticated descriptor survives. Advancing requires
        // a new lookup; corruption cannot supply a descriptor for the next hash.
        assert_eq!(rows.observed_front(&observations).unwrap(), Some(expected[1].clone()));
        assert_eq!(rows.pop_front().unwrap(), expected[1].hash);
        assert!(rows.observed_front(&observations).is_err());
        assert!(rows.observed_front(&observations).is_err());
        assert_eq!(rows.next, 2);
        assert!(rows.observation.is_none());
    }

    #[test]
    fn observed_body_lookup_preserves_pinned_bytes_and_rejects_changed_unsigned_work() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut block = crate::genesis_state::block(NetworkId::Dev, &ProtocolParams::default());
        let original = bincode::serialize(&block).unwrap();
        store.block_store().put_block(&block.hash, &original).unwrap();
        let mut rows = ObservationBuilder::new(dir.path(), MAX_ROWS, MAX_BYTES as u64).unwrap();
        rows.push(*block.hash.as_bytes(), &Observation { hash: block.hash, account: block.account.clone(), height: 1,
            length: original.len(), digest: burst_crypto::blake2b_256(&original) }).unwrap();
        let inventory = RetainedInventory {
            limits: InventoryLimits::legacy(MAX_ROWS, MAX_BYTES), count: 1,
            rows: Arc::new(rows.finish().unwrap()),
            accounts: AccountInventory::read(std::iter::empty::<Result<burst_store::account::AccountInfo, StoreError>>()).unwrap(),
        };
        let view = store.tx_begin_read().unwrap();
        assert_eq!(inventory.observed_body(&store, &view, &block.hash).unwrap().unwrap().1, 1);
        block.work = block.work.wrapping_add(1);
        assert_eq!(block.hash, block.compute_hash(), "work is excluded from the signed hash");
        store.block_store().put_block(&block.hash, &bincode::serialize(&block).unwrap()).unwrap();
        assert_eq!(bincode::serialize(&inventory.observed_body(&store, &view, &block.hash).unwrap().unwrap().0).unwrap(), original);
        drop(view);
        let view = store.tx_begin_read().unwrap();
        assert!(inventory.observed_body(&store, &view, &block.hash).is_err());
        assert!(inventory.observed_body(&store, &view, &BlockHash::new([99; 32])).unwrap().is_none());
        drop(view);
        let mut oversized = original.clone();
        oversized.push(0);
        store.block_store().put_block(&block.hash, &oversized).unwrap();
        let view = store.tx_begin_read().unwrap();
        let error = inventory.observed_body(&store, &view, &block.hash).err().expect("oversized body rejects");
        assert!(error.to_string().contains("byte budget"));
        inventory.rows.truncate_for_test();
        assert!(inventory.observed_hashes_after(None).is_err());
        assert!(inventory.observed_body(&store, &view, &block.hash).is_err());
    }
}
