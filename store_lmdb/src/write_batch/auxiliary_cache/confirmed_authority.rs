//! Account-row persistence with an atomic legacy conversion and shared delta discovery.
use super::*;
use crate::confirmed_authority::{Authority, Cached, Snapshot, HEADER, KEY, MAX_BYTES, ROW_PREFIX};
use burst_utils::SharedMap;

fn invalid(message: &str) -> StoreError { StoreError::Corruption(message.into()) }
fn size<T: serde::Serialize>(value: &T) -> Result<u64, StoreError> {
    bincode::serialized_size(value).map_err(|e| StoreError::Serialization(e.to_string()))
}
fn row_bytes(key: &String, row: &Authority) -> Result<u64, StoreError> {
    size(key)?.checked_add(size(row)?).ok_or_else(|| invalid("authority size overflow"))
}
fn header(bytes: &[u8]) -> Result<(BlockHash, u64, u64), StoreError> {
    bincode::DefaultOptions::new().with_fixint_encoding().with_limit(48)
        .reject_trailing_bytes().deserialize(bytes).map_err(|e| StoreError::Serialization(e.to_string()))
}
fn load(env: &LmdbEnvironment, txn: &heed::RoTxn<'_>) -> Result<Cached, StoreError> {
    let legacy = env.meta_db.get(txn, KEY.as_bytes()).map_err(LmdbError::from)?;
    let manifest = env.meta_db.get(txn, HEADER.as_bytes()).map_err(LmdbError::from)?;
    let mut rows = env.meta_db.prefix_iter(txn, ROW_PREFIX.as_bytes()).map_err(LmdbError::from)?;
    let Some(manifest) = manifest else {
        if rows.next().transpose().map_err(LmdbError::from)?.is_some() {
            return Err(invalid("orphaned confirmed authority rows"));
        }
        let bytes = legacy.ok_or_else(|| invalid("ledger lacks confirmed authority history"))?;
        return Ok(Cached { snapshot: Snapshot::decode(bytes)?, logical_bytes: bytes.len() as u64, uses_row_storage: false });
    };
    if legacy.is_some() { return Err(invalid("mixed confirmed authority formats")); }
    let (genesis, count, expected_bytes) = header(manifest)?;
    if genesis.is_zero() || count == 0 || count > 1_000_000 || expected_bytes > MAX_BYTES {
        return Err(invalid("invalid confirmed authority header"));
    }
    let mut accounts = SharedMap::default();
    let mut logical_bytes = 40u64; // Existing v1 genesis and map length encoding.
    for item in rows {
        let (key, bytes) = item.map_err(LmdbError::from)?;
        let key = std::str::from_utf8(&key[ROW_PREFIX.len()..]).map_err(|_| invalid("invalid authority row key"))?;
        let row: Authority = bincode::DefaultOptions::new().with_fixint_encoding().with_limit(MAX_BYTES)
            .reject_trailing_bytes().deserialize(bytes).map_err(|e| StoreError::Serialization(e.to_string()))?;
        if row.address.as_str() != key || accounts.len() as u64 >= count { return Err(invalid("invalid authority row identity/count")); }
        let key = key.to_owned();
        logical_bytes = logical_bytes.checked_add(row_bytes(&key, &row)?).filter(|n| *n <= MAX_BYTES)
            .ok_or_else(|| invalid("confirmed authority exceeds byte limit"))?;
        accounts.insert(key, row);
    }
    if accounts.len() as u64 != count || logical_bytes != expected_bytes { return Err(invalid("authority row manifest mismatch")); }
    Ok(Cached { snapshot: Snapshot { genesis, accounts }, logical_bytes, uses_row_storage: true })
}

impl LmdbEnvironment {
    pub fn read_confirmed_authority_snapshot(&self) -> Result<Snapshot, StoreError> {
        let view = self.tx_begin_read()?;
        self.read_confirmed_authority_view(&view)
    }

    pub(super) fn read_confirmed_authority_view(&self, view: &LmdbReadTransaction<'_>) -> Result<Snapshot, StoreError> {
        if let Some(generation) = view.generation {
            let slot = self.auxiliary_cache.lock().unwrap();
            if let Some((_, cache)) = slot.as_ref().filter(|(id, _)| *id == generation) {
                if let Some(cached) = &cache.confirmed_authority {
                    return Ok(cached.snapshot.clone());
                }
            }
        }
        let cached = load(self, &view.txn)?;
        let snapshot = cached.snapshot.clone();
        self.publish_read_auxiliary(view.generation, |cache| cache.confirmed_authority = Some(cached));
        Ok(snapshot)
    }
}

impl LmdbWriteTransaction<'_> {
    pub fn has_confirmed_authority_storage(&self) -> Result<bool, StoreError> {
        Ok(self.env.meta_db.prefix_iter(&self.txn, b"confirmed_authority/")
            .map_err(LmdbError::from)?.next().transpose().map_err(LmdbError::from)?.is_some())
    }

    pub fn read_confirmed_authority(&self) -> Result<Snapshot, StoreError> {
        if let Some(cached) = &self.auxiliary_cache.borrow().confirmed_authority { return Ok(cached.snapshot.clone()); }
        let cached = load(self.env, &self.txn)?;
        let snapshot = cached.snapshot.clone();
        self.auxiliary_cache.borrow_mut().confirmed_authority = Some(cached);
        Ok(snapshot)
    }

    /// Stage a caller-authenticated authority snapshot. Storage checks cover
    /// identities, counts and encoding limits; they do not verify its history.
    /// Any error prevents this transaction from committing.
    pub fn write_confirmed_authority(&mut self, snapshot: Snapshot) -> Result<(), StoreError> {
        let result = self.write_confirmed_authority_rows(snapshot);
        if result.is_err() { self.failed = true; }
        result
    }

    fn retain_confirmed_authority_rows(
        &mut self,
        keep: impl Fn(&[u8]) -> bool,
    ) -> Result<(), StoreError> {
        use std::ops::Bound;
        let mut cursor: Option<Vec<u8>> = None;
        let mut logical_bytes = 40u64;
        let mut count = 0usize;
        loop {
            let bounds = (
                cursor.as_deref().map_or(Bound::Included(ROW_PREFIX.as_bytes()), Bound::Excluded),
                Bound::Unbounded,
            );
            let keys = self.env.meta_db.range(&self.txn, &bounds).map_err(LmdbError::from)?
                .take_while(|row| row.as_ref().map_or(true, |(key, _)| key.starts_with(ROW_PREFIX.as_bytes())))
                .take(256)
                .map(|row| {
                    let (key, value) = row.map_err(LmdbError::from)?;
                    logical_bytes = logical_bytes.checked_add((key.len() - ROW_PREFIX.len()) as u64 + 8)
                        .and_then(|n| n.checked_add(value.len() as u64)).filter(|n| *n <= MAX_BYTES)
                        .ok_or_else(|| invalid("authority row replacement exceeds byte limit"))?;
                    count += 1;
                    if count > 1_000_000 { return Err(invalid("authority row replacement exceeds count limit")); }
                    Ok((key.to_vec(), keep(&key[ROW_PREFIX.len()..])))
                }).collect::<Result<Vec<_>, StoreError>>()?;
            let Some((last, _)) = keys.last() else { break; };
            cursor = Some(last.clone());
            for (key, retained) in keys {
                if !retained { self.delete_raw(UndoTable::Meta, &key)?; }
            }
        }
        Ok(())
    }

    fn write_confirmed_authority_rows(&mut self, snapshot: Snapshot) -> Result<(), StoreError> {
        if self.undo.is_some() { return Err(invalid("authority projection cannot join provisional undo")); }
        if snapshot.genesis.is_zero() || snapshot.accounts.is_empty() || snapshot.accounts.len() > 1_000_000 {
            return Err(invalid("invalid confirmed authority projection"));
        }
        let previous = self.auxiliary_cache.borrow().confirmed_authority.clone();
        if previous.as_ref().is_some_and(|old| old.uses_row_storage && old.snapshot.genesis == snapshot.genesis
            && old.snapshot.accounts.shares_storage_with(&snapshot.accounts)) { return Ok(()); }
        let delta = previous.as_ref().filter(|old| old.uses_row_storage);
        let keys: Vec<_> = match delta {
            Some(old) => snapshot.accounts.changed_keys(&old.snapshot.accounts).cloned().collect(),
            None => snapshot.accounts.keys().cloned().collect(),
        };
        let mut logical_bytes = delta.map_or(40, |old| old.logical_bytes);
        let mut encoded = Vec::new();
        let mut legacy_key = false;
        for key in &keys {
            if let Some(old) = delta.and_then(|old| old.snapshot.accounts.get(key)) {
                logical_bytes = logical_bytes.checked_sub(row_bytes(key, old)?).ok_or_else(|| invalid("authority size underflow"))?;
            }
            let bytes = if let Some(row) = snapshot.accounts.get(key) {
                if row.address.as_str() != key { return Err(invalid("authority account key mismatch")); }
                logical_bytes = logical_bytes.checked_add(row_bytes(key, row)?).ok_or_else(|| invalid("authority size overflow"))?;
                legacy_key |= ROW_PREFIX.len() + key.len() > 511;
                Some(bincode::serialize(row).map_err(|e| StoreError::Serialization(e.to_string()))?)
            } else { None };
            encoded.push((format!("{ROW_PREFIX}{key}"), bytes));
        }
        if logical_bytes > MAX_BYTES { return Err(invalid("confirmed authority exceeds byte limit")); }
        // Older aggregate snapshots accepted arbitrary-length addresses. Preserve
        // that compatibility without trying to insert an oversized LMDB key.
        if legacy_key {
            let bytes = bincode::serialize(&snapshot).map_err(|e| StoreError::Serialization(e.to_string()))?;
            Snapshot::decode(&bytes)?;
            self.retain_confirmed_authority_rows(|_| false)?;
            self.delete_meta(HEADER)?;
            self.put_meta(KEY, &bytes)?;
            self.auxiliary_cache.get_mut().confirmed_authority = Some(Cached { snapshot, logical_bytes: bytes.len() as u64, uses_row_storage: false });
            return Ok(());
        }
        if delta.is_none() {
            // Without a cached baseline, check and encode every target account.
            // Preserve matching keys so put_meta can skip byte-identical rows.
            self.retain_confirmed_authority_rows(|key| {
                std::str::from_utf8(key).is_ok_and(|key| snapshot.accounts.contains_key(&key.to_owned()))
            })?;
        }
        for (key, bytes) in encoded {
            match bytes { Some(bytes) => self.put_meta(&key, &bytes)?, None => self.delete_meta(&key)? }
        }
        self.delete_meta(KEY)?;
        let bytes = bincode::serialize(&(snapshot.genesis, snapshot.accounts.len() as u64, logical_bytes))
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        self.put_meta(HEADER, &bytes)?;
        self.auxiliary_cache.get_mut().confirmed_authority = Some(Cached { snapshot, logical_bytes, uses_row_storage: true });
        Ok(())
    }
}
