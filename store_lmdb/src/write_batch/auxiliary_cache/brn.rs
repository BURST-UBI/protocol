use super::*;
use burst_brn::{BrnEngine, BrnWalletState};

#[derive(Clone)]
pub(super) struct BrnCache {
    engine: BrnEngine,
    wallet_bytes: usize,
    metadata_dirty: bool,
    sequence_dirty: bool,
    uses_rate_records: bool,
    uses_wallet_records: bool,
}

impl BrnCache {
    pub(super) fn clean_snapshot(&self, max_rows: usize, max_bytes: usize) -> Result<Option<BrnEngine>, StoreError> {
        if self.engine.wallets.len() > max_rows || self.wallet_bytes > max_bytes {
            return Err(StoreError::Corruption("BRN snapshot exceeds restoration limit".into()));
        }
        Ok((!self.metadata_dirty && !self.sequence_dirty).then(|| self.engine.clone()))
    }
}

impl AuxiliaryCache {
    pub(crate) fn clear_brn(&mut self, table: UndoTable) {
        if matches!(table, UndoTable::BrnWallets | UndoTable::BrnMeta) {
            self.brn = None;
            self.brn_rates = None;
        }
    }

    /// Update or invalidate the cache after a raw write or rewind. Typed writes
    /// detach the cache and update it directly to avoid decoding their own output.
    /// Invalid wallet bytes discard the cache; a later read reports corruption.
    pub(crate) fn update_brn(
        &mut self,
        table: UndoTable,
        key: &[u8],
        before: Option<&[u8]>,
        after: Option<&[u8]>,
    ) {
        if table == UndoTable::BrnMeta && key.starts_with(b"wallet_records/") {
            self.brn = None;
            return;
        }
        if table == UndoTable::BrnMeta && (key == b"rate_history" || key.starts_with(b"rate_history/")) {
            self.brn_rates = None;
        }
        let Some(cache) = self.brn.as_mut() else { return };
        if table == UndoTable::BrnMeta {
            if key == b"rate_history" || key.starts_with(b"rate_history/") {
                cache.metadata_dirty = true;
            } else if key == b"next_stake_id" {
                cache.sequence_dirty = true;
            }
        } else if table == UndoTable::BrnWallets {
            if cache.uses_wallet_records {
                self.brn = None;
                return;
            }
            let change = (|| {
                let address = crate::brn::decode_wallet_address(key).ok()?;
                let state = after.map(BrnWalletState::from_persisted_bytes).transpose().ok()?;
                let old_bytes = before.map_or(0, |bytes| key.len() + bytes.len());
                let new_bytes = after.map_or(0, |bytes| key.len() + bytes.len());
                let total = cache.wallet_bytes.checked_sub(old_bytes)?.checked_add(new_bytes)?;
                Some((address, state, total))
            })();
            if let Some((address, state, total)) = change {
                if let Some(state) = state {
                    cache.engine.wallets.insert(address, state);
                } else {
                    cache.engine.wallets.remove(&address);
                }
                cache.wallet_bytes = total;
            } else {
                self.brn = None;
            }
        }
    }
}

impl LmdbWriteTransaction<'_> {
    /// Persist the canonical wallet bytes while retaining its already-built
    /// release indexes in a warm cache. Raw writes and rewind still decode rows.
    pub fn put_brn_wallet_state(
        &mut self,
        address: &WalletAddress,
        state: &BrnWalletState,
    ) -> Result<(), StoreError> {
        if self.uses_brn_wallet_records()? {
            return self.persist_brn_wallet_records(address, Some(state));
        }
        let bytes = bincode::serialize(state)
            .map_err(|error| StoreError::Serialization(error.to_string()))?;
        let key = address.as_str().as_bytes();
        let before_len = self.undo_value_ref(UndoTable::BrnWallets, key)?.map(|row| row.len());
        let cached = self.auxiliary_cache.get_mut().brn.take();
        // The normal writer owns journal recording, mutation flags and poisoning
        // on failure. With the cache detached, it need not decode these bytes.
        self.put_brn_wallet(address, &bytes)?;
        if let Some(mut cache) = cached {
            let total = (|| {
                crate::brn::decode_wallet_address(key).ok()?;
                let old = before_len.map_or(Some(0), |len| key.len().checked_add(len))?;
                let new = key.len().checked_add(bytes.len())?;
                cache.wallet_bytes.checked_sub(old)?.checked_add(new)
            })();
            if let Some(total) = total {
                cache.engine.wallets.insert(address.clone(), state.clone());
                cache.wallet_bytes = total;
                self.auxiliary_cache.get_mut().brn = Some(cache);
            }
        }
        Ok(())
    }

    pub fn uses_brn_wallet_records(&self) -> Result<bool, StoreError> {
        self.read_view().uses_brn_wallet_records()
    }

    /// Select records only for an empty wallet inventory with no historical
    /// block journals. Existing state must use authenticated projection migration.
    pub fn initialize_brn_wallet_records_if_empty(&mut self) -> Result<bool, StoreError> {
        if self.uses_brn_wallet_records()? { return Ok(true); }
        if self.undo.is_some() || self.env.brn_wallets_db.len(&self.txn).map_err(LmdbError::from)? != 0
            || self.env.meta_db.prefix_iter(&self.txn, crate::block_undo::PREFIX).map_err(LmdbError::from)?
                .next().transpose().map_err(LmdbError::from)?.is_some() {
            return Ok(false);
        }
        self.put_brn_meta(burst_brn::wallet_storage::FORMAT_KEY, &[1])?;
        Ok(true)
    }

    /// Delta persistence for a format already installed with matching journals.
    /// This method never chooses or migrates the local format.
    pub(crate) fn persist_brn_wallet_records(
        &mut self, address: &WalletAddress, after: Option<&BrnWalletState>,
    ) -> Result<(), StoreError> {
        use burst_brn::wallet_storage;
        let result = (|| {
            let mut cached = self.auxiliary_cache.get_mut().brn.take();
            let before = if let Some(cache) = &cached {
                cache.engine.get_wallet(address).cloned()
            } else {
                crate::brn::records::decode_wallet(&self.txn, self.env.brn_meta_db, address,
                    self.undo_value_ref(UndoTable::BrnWallets, address.as_str().as_bytes())?, usize::MAX)?.0
            };
            let prefix = wallet_storage::row_prefix(address)
                .map_err(|error| StoreError::Serialization(error.to_string()))?;
            wallet_storage::visit_delta(before.as_ref(), after, |relative, old, new| {
                let (table, key) = if relative == wallet_storage::HEADER {
                    (UndoTable::BrnWallets, address.as_str().as_bytes().to_vec())
                } else { (UndoTable::BrnMeta, [prefix.as_slice(), relative].concat()) };
                let error = |error: StoreError| burst_brn::BrnError::Other(error.to_string());
                if self.undo_value_ref(table, &key).map_err(error)? != old.as_deref() {
                    return Err(burst_brn::BrnError::Other("BRN wallet cache differs from stored rows".into()));
                }
                if let Some(cache) = cached.as_mut() {
                    let old_bytes = old.as_ref().map_or(0, |value| key.len() + value.len());
                    let new_bytes = new.as_ref().map_or(0, |value| key.len() + value.len());
                    cache.wallet_bytes = cache.wallet_bytes.checked_sub(old_bytes)
                        .and_then(|size| size.checked_add(new_bytes))
                        .ok_or_else(|| burst_brn::BrnError::Other("BRN snapshot size overflow".into()))?;
                }
                self.mutate_raw(table, &key, new.as_deref()).map_err(error)
            }).map_err(|error| StoreError::Corruption(error.to_string()))?;
            if let Some(cache) = cached.as_mut() {
                if let Some(after) = after { cache.engine.wallets.insert(address.clone(), after.clone()); }
                else { cache.engine.wallets.remove(address); }
            }
            self.auxiliary_cache.get_mut().brn = cached;
            Ok(())
        })();
        if result.is_err() { self.failed = true; }
        result
    }

    pub fn uses_brn_rate_records(&self) -> Result<bool, StoreError> {
        self.read_view().uses_brn_rate_records()
    }

    /// Existing aggregate histories can have older aggregate undo records.
    /// Preserve that format until an authenticated journal migration replaces
    /// them; fresh and already-normalized inventories use scoped row writes.
    pub fn persist_brn_rate_history(&mut self, rates: &burst_brn::RateHistory) -> Result<(), StoreError> {
        if !self.uses_brn_rate_records()?
            && self.undo_value_ref(UndoTable::BrnMeta, b"rate_history")?.is_some() {
            let bytes = bincode::serialize(rates).map_err(|error| StoreError::Serialization(error.to_string()))?;
            self.put_brn_meta(b"rate_history", &bytes)
        } else {
            self.put_brn_rate_history(rates)
        }
    }

    /// Append only new rate rows when the cache proves the existing prefix matches.
    /// Otherwise, encode the full history and collect all old rate keys for replacement.
    /// Changes join the active block journal, if any.
    pub fn put_brn_rate_history(&mut self, rates: &burst_brn::RateHistory) -> Result<(), StoreError> {
        use burst_brn::rate_storage::{self, HEADER, ROW_PREFIX};
        let start = {
            let cache = self.auxiliary_cache.borrow();
            cache.brn_rates.as_ref().and_then(|before| rates.appended_rows_start(before))
                .or_else(|| cache.brn.as_ref()
                    .filter(|cache| cache.uses_rate_records && !cache.metadata_dirty)
                    .and_then(|cache| rates.appended_rows_start(&cache.engine.rate_history)))
        };
        if start == Some(rates.segments().len()) { return Ok(()); }
        let encoded: Vec<_> = match start {
            Some(start) => rates.segments().skip(start).into_iter().enumerate()
                .map(|(index, segment)| (rate_storage::row_key((start + index) as u64), rate_storage::row_value(&segment)))
                .collect(),
            None => rate_storage::encode(rates)
                .map_err(|error| StoreError::Corruption(error.to_string()))?.collect(),
        };
        let removed: Vec<Vec<u8>> = if start.is_none() {
            self.env.brn_meta_db.prefix_iter(&self.txn, ROW_PREFIX).map_err(LmdbError::from)?
                .map(|row| row.map(|(key, _)| key.to_vec()).map_err(LmdbError::from))
                .collect::<Result<_, _>>()?
        } else { Vec::new() };
        let result = (|| {
            for key in removed { self.delete_raw(UndoTable::BrnMeta, &key)?; }
            for (key, value) in encoded { self.put_brn_meta(&key, &value)?; }
            self.delete_raw(UndoTable::BrnMeta, b"rate_history")?;
            self.put_brn_meta(HEADER, &(rates.segments().len() as u64).to_be_bytes())?;
            self.auxiliary_cache.get_mut().brn_rates = Some(rates.clone());
            if let Some(cache) = self.auxiliary_cache.get_mut().brn.as_mut() {
                cache.engine.rate_history = rates.clone();
                cache.metadata_dirty = false;
                cache.uses_rate_records = true;
            }
            Ok(())
        })();
        if result.is_err() { self.failed = true; }
        result
    }

    /// Read BRN state including this writer's changes, reusing its cached snapshot.
    /// `max_rows` counts wallets; `max_bytes` counts their stored keys and values,
    /// including child records. Both limits apply on cache hits. They exclude rate
    /// history, other metadata, decoded allocations and LMDB page overhead.
    pub fn read_brn_engine(&self, max_rows: usize, max_bytes: usize) -> Result<BrnEngine, StoreError> {
        let invalid = |reason: String| StoreError::Corruption(reason);
        let metadata = || self.read_view().read_brn_metadata();
        let mut auxiliary = self.auxiliary_cache.borrow_mut();
        if let Some(cache) = auxiliary.brn.as_mut() {
            if cache.engine.wallets.len() > max_rows || cache.wallet_bytes > max_bytes {
                return Err(invalid("BRN snapshot exceeds restoration limit".into()));
            }
            if cache.metadata_dirty {
                let mut engine = metadata()?;
                engine.wallets = cache.engine.wallets.clone();
                cache.engine = engine;
                cache.uses_rate_records = self.undo_value_ref(UndoTable::BrnMeta, burst_brn::rate_storage::HEADER)?.is_some();
                cache.metadata_dirty = false;
                cache.sequence_dirty = false;
            } else if cache.sequence_dirty {
                let ids = self.undo_value_ref(UndoTable::BrnMeta, b"next_stake_id")?
                    .ok_or_else(|| invalid("replacement requires complete economic snapshots".into()))?;
                cache.engine.restore_stake_sequence(ids)
                    .map_err(|error| invalid(error.to_string()))?;
                cache.sequence_dirty = false;
            }
            return Ok(cache.engine.clone());
        }
        let (engine, wallet_bytes, uses_wallet_records) = self.read_view().read_brn_snapshot(max_rows, max_bytes)?;
        auxiliary.brn = Some(BrnCache {
            engine: engine.clone(), wallet_bytes, metadata_dirty: false, sequence_dirty: false,
            uses_rate_records: self.undo_value_ref(UndoTable::BrnMeta, burst_brn::rate_storage::HEADER)?.is_some(),
            uses_wallet_records,
        });
        Ok(engine)
    }
}

#[cfg(test)]
mod tests;

impl crate::read_view::LedgerReadView<'_> {
    fn read_brn_metadata(&self) -> Result<BrnEngine, StoreError> {
        let invalid = |reason: String| StoreError::Corruption(reason);

        let ids = self.undo_value_ref(UndoTable::BrnMeta, b"next_stake_id")?
            .ok_or_else(|| invalid("replacement requires complete economic snapshots".into()))?;
        let rates = burst_brn::rate_storage::decode_stored(
            self.undo_value_ref(UndoTable::BrnMeta, b"rate_history")?,
            self.undo_value_ref(UndoTable::BrnMeta, burst_brn::rate_storage::HEADER)?,
            self.env.brn_meta_db.prefix_iter(&self.txn, burst_brn::rate_storage::ROW_PREFIX)
                .map_err(LmdbError::from)?
                .map(|row| row.map_err(|error| burst_brn::BrnError::Other(error.to_string()))),
        ).map_err(|error| invalid(error.to_string()))?
            .ok_or_else(|| invalid("replacement requires complete economic snapshots".into()))?;
        BrnEngine::from_decoded_entries(
            Some(ids), Some(rates),
            std::iter::empty::<Result<(WalletAddress, &[u8]), burst_brn::BrnError>>(),
        ).map_err(|error| invalid(error.to_string()))
        }

    fn read_brn_snapshot(&self, max_rows: usize, max_bytes: usize) -> Result<(BrnEngine, usize, bool), StoreError> {
        let mut engine = self.read_brn_metadata()?;
        let (wallet_bytes, uses_wallet_records) = self.visit_brn_wallets(max_rows, max_bytes,
            &|_| Ok(()), |address, state| {
                engine.wallets.insert(address, state);
                Ok(())
            })?;
        Ok((engine, wallet_bytes, uses_wallet_records))
    }

    fn visit_brn_wallets(
        &self, max_rows: usize, max_bytes: usize,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
        mut visit: impl FnMut(WalletAddress, BrnWalletState) -> Result<(), StoreError>,
    ) -> Result<(usize, bool), StoreError> {
        let invalid = |reason: String| StoreError::Corruption(reason);
        if self.env.brn_wallets_db.len(&self.txn).map_err(LmdbError::from)? > max_rows as u64 {
            return Err(invalid("BRN snapshot exceeds restoration limit".into()));
        }
        let uses_wallet_records = crate::brn::records::validate_inventory(&self.txn, self.env.brn_wallets_db, self.env.brn_meta_db, max_bytes)?;
        let mut wallet_bytes = 0usize;
        for row in self.bounded_brn_entries(max_rows, max_bytes)? {
            poll("storage:brn-wallet")?;
            let (address, bytes) = row?;
            let state = if uses_wallet_records {
                let (state, size) = crate::brn::records::decode_wallet(&self.txn, self.env.brn_meta_db,
                    &address, Some(bytes), max_bytes.saturating_sub(wallet_bytes))?;
                wallet_bytes += size;
                state.ok_or_else(|| invalid("missing BRN wallet header".into()))?
            } else {
                wallet_bytes = wallet_bytes.checked_add(address.as_str().len())
                    .and_then(|total| total.checked_add(bytes.len()))
                    .ok_or_else(|| invalid("BRN snapshot size overflow".into()))?;
                BrnWalletState::from_persisted_bytes(bytes).map_err(|error| invalid(error.to_string()))?
            };
            visit(address, state)?;
        }
        Ok((wallet_bytes, uses_wallet_records))
    }

    /// Compare stored BRN state with `expected`, decoding one wallet at a time.
    /// Wallet count and stored-byte limits exclude rate history and decoded memory.
    /// Normalized rates are compared row by row; aggregate rates are decoded whole.
    /// Poll before metadata, each normalized rate row and each wallet. Inventory
    /// validation and decoding an individual wallet do not poll.
    pub fn verify_brn_engine_with_poll(
        &self, expected: &BrnEngine, max_rows: usize, max_bytes: usize,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let mismatch = || StoreError::Corruption("persisted BRN state differs from applied replay".into());
        poll("storage:brn-metadata")?;
        self.verify_brn_metadata(expected, poll)?;
        let mut count = 0usize;
        self.visit_brn_wallets(max_rows, max_bytes, poll, |address, state| {
            if expected.wallets.get(&address) != Some(&state) { return Err(mismatch()); }
            count = count.checked_add(1).ok_or_else(mismatch)?;
            Ok(())
        })?;
        if count != expected.wallets.len() { return Err(mismatch()); }
        Ok(())
    }

    fn verify_brn_metadata(
        &self, expected: &BrnEngine,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        use burst_brn::rate_storage;
        let mismatch = || StoreError::Corruption("persisted BRN metadata differs from applied replay".into());
        let Some(header) = self.undo_value_ref(UndoTable::BrnMeta, rate_storage::HEADER)? else {
            let metadata = self.read_brn_metadata()?;
            if metadata.rate_history != expected.rate_history || metadata.next_stake_id() != expected.next_stake_id() {
                return Err(mismatch());
            }
            return Ok(());
        };
        let sequence = expected.next_stake_id();
        if sequence == 0
            || self.undo_value_ref(UndoTable::BrnMeta, b"next_stake_id")? != Some(sequence.to_be_bytes().as_slice())
            || self.undo_value_ref(UndoTable::BrnMeta, b"rate_history")?.is_some()
            || header != (expected.rate_history.segments().len() as u64).to_be_bytes() {
            return Err(mismatch());
        }
        // The existing encoder validates nonempty, contiguous segment ends and
        // monotonic starts. Exact ordered rows imply the same decoded history,
        // without allocating another segment sequence or accrued-value index.
        let expected_rows = rate_storage::encode(&expected.rate_history)
            .map_err(|error| StoreError::Corruption(error.to_string()))?;
        let mut actual = self.env.brn_meta_db.prefix_iter(&self.txn, rate_storage::ROW_PREFIX)
            .map_err(LmdbError::from)?;
        for (expected_key, expected_value) in expected_rows {
            poll("storage:brn-rate")?;
            let (key, value) = actual.next().transpose().map_err(LmdbError::from)?
                .ok_or_else(mismatch)?;
            if key != expected_key.as_slice() || value != expected_value {
                return Err(mismatch());
            }
        }
        if actual.next().transpose().map_err(LmdbError::from)?.is_some() {
            return Err(mismatch());
        }
        Ok(())
    }

    /// Load all BRN state from this view without using the writer cache.
    /// Limits count wallets and their stored key/value bytes, including child rows.
    /// Rate history, other metadata and decoded memory are outside those limits.
    pub fn read_brn_engine(&self, max_rows: usize, max_bytes: usize) -> Result<BrnEngine, StoreError> {
        self.read_brn_snapshot(max_rows, max_bytes).map(|(engine, _, _)| engine)
    }
}

impl crate::read_view::LedgerReadView<'_> {
    pub fn uses_brn_wallet_records(&self) -> Result<bool, StoreError> {
        crate::brn::records::uses_records(self.txn, self.env.brn_meta_db)
    }

    pub fn uses_brn_rate_records(&self) -> Result<bool, StoreError> {
        Ok(self.undo_value_ref(UndoTable::BrnMeta, burst_brn::rate_storage::HEADER)?.is_some())
    }
}
