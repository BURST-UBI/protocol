//! LMDB implementation of PendingStore.
//!
//! Key format: `destination.as_str().as_bytes() ++ source_hash.as_bytes()`
//! (binary composite key). All BURST addresses have identical length, so
//! prefix scans for a given destination work correctly.

use crate::shared_env::SharedEnv;
use std::ops::Bound;

use heed::types::Bytes;
use heed::Database;

use burst_store::pending::{PendingInfo, PendingStore};
use burst_store::StoreError;
use burst_types::{TxHash, WalletAddress};

use crate::block::increment_prefix;
use crate::LmdbError;

#[cfg(test)]
mod tests;

pub struct LmdbPendingStore {
    pub(crate) env: SharedEnv,
    pub(crate) pending_db: Database<Bytes, Bytes>,
    pub(crate) pending_sender_db: Database<Bytes, Bytes>,
}

pub(crate) fn sender_key(source: &WalletAddress, pending_key: &[u8]) -> Vec<u8> {
    let mut key = source.as_str().as_bytes().to_vec();
    key.push(0);
    key.extend_from_slice(pending_key);
    key
}

/// A derived index, updated in the primary write transaction, including undo.
pub(crate) fn update_sender_index(
    txn: &mut heed::RwTxn<'_>,
    pending_db: Database<Bytes, Bytes>,
    sender_db: Database<Bytes, Bytes>,
    key: &[u8],
    value: Option<&[u8]>,
) -> Result<(), LmdbError> {
    let before = pending_db
        .get(txn, key)?
        .map(bincode::deserialize::<PendingInfo>)
        .transpose()?;
    let after = value.map(bincode::deserialize::<PendingInfo>).transpose()?;
    if let Some(before) = before {
        sender_db.delete(txn, &sender_key(&before.source, key))?;
    }
    if let Some(after) = after {
        sender_db.put(txn, &sender_key(&after.source, key), key)?;
    }
    Ok(())
}

pub(crate) fn rebuild_sender_index(
    txn: &mut heed::RwTxn<'_>,
    pending_db: Database<Bytes, Bytes>,
    sender_db: Database<Bytes, Bytes>,
) -> Result<(), LmdbError> {
    sender_db.clear(txn)?;
    let mut after: Option<Vec<u8>> = None;
    loop {
        let bounds = (
            after.as_deref().map_or(Bound::Unbounded, Bound::Excluded),
            Bound::<&[u8]>::Unbounded,
        );
        let entries = pending_db
            .range(txn, &bounds)?
            .take(1024)
            .map(|entry| {
                let (key, value) = entry?;
                let pending: PendingInfo = bincode::deserialize(value)?;
                Ok((sender_key(&pending.source, key), key.to_vec()))
            })
            .collect::<Result<Vec<_>, LmdbError>>()?;
        if entries.is_empty() {
            break;
        }
        after = entries.last().map(|(_, key)| key.clone());
        for (index, key) in entries {
            sender_db.put(txn, &index, &key)?;
        }
    }
    Ok(())
}

/// Build the binary composite key `destination_bytes ++ source_hash_bytes`.
fn pending_key(destination: &WalletAddress, source_hash: &TxHash) -> Vec<u8> {
    let dest = destination.as_str().as_bytes();
    let mut key = Vec::with_capacity(dest.len() + 32);
    key.extend_from_slice(dest);
    key.extend_from_slice(source_hash.as_bytes());
    key
}

/// Build the binary composite key from raw bytes (used by LmdbWriteTransaction).
pub(crate) fn pending_key_raw(
    destination: &WalletAddress,
    source_hash_bytes: &[u8; 32],
) -> Vec<u8> {
    let dest = destination.as_str().as_bytes();
    let mut key = Vec::with_capacity(dest.len() + 32);
    key.extend_from_slice(dest);
    key.extend_from_slice(source_hash_bytes);
    key
}

impl PendingStore for LmdbPendingStore {
    fn put_pending(
        &self,
        destination: &WalletAddress,
        source_hash: &TxHash,
        info: &PendingInfo,
    ) -> Result<(), StoreError> {
        let key = pending_key(destination, source_hash);
        let bytes = bincode::serialize(info).map_err(LmdbError::from)?;
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        update_sender_index(
            &mut wtxn,
            self.pending_db,
            self.pending_sender_db,
            &key,
            Some(&bytes),
        )?;
        self.pending_db
            .put(&mut wtxn, &key, &bytes)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_pending(
        &self,
        destination: &WalletAddress,
        source_hash: &TxHash,
    ) -> Result<PendingInfo, StoreError> {
        let key = pending_key(destination, source_hash);
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let val = self
            .pending_db
            .get(&rtxn, &key)
            .map_err(LmdbError::from)?
            .ok_or_else(|| LmdbError::NotFound("pending entry".to_string()))?;
        let info: PendingInfo = bincode::deserialize(val).map_err(LmdbError::from)?;
        Ok(info)
    }

    fn delete_pending(
        &self,
        destination: &WalletAddress,
        source_hash: &TxHash,
    ) -> Result<(), StoreError> {
        let key = pending_key(destination, source_hash);
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        update_sender_index(
            &mut wtxn,
            self.pending_db,
            self.pending_sender_db,
            &key,
            None,
        )?;
        self.pending_db
            .delete(&mut wtxn, &key)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_pending_for_account(
        &self,
        destination: &WalletAddress,
    ) -> Result<Vec<PendingInfo>, StoreError> {
        self.get_pending_for_account_with_hashes(destination)
            .map(|v| v.into_iter().map(|(_, info)| info).collect())
    }

    fn get_pending_for_account_with_hashes(
        &self,
        destination: &WalletAddress,
    ) -> Result<Vec<(TxHash, PendingInfo)>, StoreError> {
        let prefix = destination.as_str().as_bytes();
        let prefix_len = prefix.len();
        let mut upper = prefix.to_vec();
        increment_prefix(&mut upper);

        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let bounds = (Bound::Included(prefix), Bound::Excluded(upper.as_slice()));
        let iter = self
            .pending_db
            .range(&rtxn, &bounds)
            .map_err(LmdbError::from)?;
        let mut results = Vec::new();
        for result in iter {
            let (key, val) = result.map_err(LmdbError::from)?;
            let info: PendingInfo = bincode::deserialize(val).map_err(LmdbError::from)?;
            let source_hash = if key.len() >= prefix_len + 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&key[prefix_len..prefix_len + 32]);
                TxHash::new(arr)
            } else {
                TxHash::new([0u8; 32])
            };
            results.push((source_hash, info));
        }
        Ok(results)
    }

    fn get_pending_from_account(
        &self,
        source: &WalletAddress,
    ) -> Result<Vec<(WalletAddress, TxHash, PendingInfo)>, StoreError> {
        let prefix = sender_key(source, &[]);
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let mut results = Vec::new();
        for entry in self
            .pending_sender_db
            .prefix_iter(&rtxn, &prefix)
            .map_err(LmdbError::from)?
        {
            let (index, key) = entry.map_err(LmdbError::from)?;
            if index != sender_key(source, key) || key.len() <= 32 {
                return Err(StoreError::Corruption(
                    "invalid pending sender index".into(),
                ));
            }
            let bytes = self
                .pending_db
                .get(&rtxn, key)
                .map_err(LmdbError::from)?
                .ok_or_else(|| {
                    StoreError::Corruption("pending sender index has no transfer".into())
                })?;
            let pending: PendingInfo = bincode::deserialize(bytes).map_err(LmdbError::from)?;
            if &pending.source != source {
                return Err(StoreError::Corruption(
                    "pending sender index has wrong owner".into(),
                ));
            }
            if pending.return_credit {
                continue;
            }
            let (destination, hash) = key.split_at(key.len() - 32);
            let destination = std::str::from_utf8(destination)
                .map_err(|_| StoreError::Corruption("invalid pending destination".into()))?;
            results.push((
                WalletAddress::new(destination),
                TxHash::new(hash.try_into().unwrap()),
                pending,
            ));
        }
        Ok(results)
    }

    fn pending_count(&self) -> Result<u64, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let count = self.pending_db.len(&rtxn).map_err(LmdbError::from)?;
        Ok(count)
    }

    fn get_all_pending(&self) -> Result<Vec<(WalletAddress, TxHash, PendingInfo)>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let iter = self.pending_db.iter(&rtxn).map_err(LmdbError::from)?;
        let mut results = Vec::new();
        for result in iter {
            let (key, val) = result.map_err(LmdbError::from)?;
            if key.len() <= 32 {
                continue;
            }
            let (dest_bytes, hash_bytes) = key.split_at(key.len() - 32);
            let destination = match std::str::from_utf8(dest_bytes) {
                Ok(s) => WalletAddress::new(s),
                Err(_) => continue,
            };
            let mut arr = [0u8; 32];
            arr.copy_from_slice(hash_bytes);
            let info: PendingInfo = bincode::deserialize(val).map_err(LmdbError::from)?;
            results.push((destination, TxHash::new(arr), info));
        }
        Ok(results)
    }
}
