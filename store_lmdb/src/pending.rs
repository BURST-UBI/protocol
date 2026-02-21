//! LMDB implementation of PendingStore.
//!
//! Key format: `destination.as_str().as_bytes() ++ source_hash.as_bytes()`
//! (binary composite key). All BURST addresses have identical length, so
//! prefix scans for a given destination work correctly.

use std::ops::Bound;
use std::sync::Arc;

use heed::types::Bytes;
use heed::{Database, Env};

use burst_store::pending::{PendingInfo, PendingStore};
use burst_store::StoreError;
use burst_types::{TxHash, WalletAddress};

use crate::block::increment_prefix;
use crate::LmdbError;

pub struct LmdbPendingStore {
    pub(crate) env: Arc<Env>,
    pub(crate) pending_db: Database<Bytes, Bytes>,
}

/// Build the binary composite key `destination_bytes ++ source_hash_bytes`.
fn pending_key(destination: &WalletAddress, source_hash: &TxHash) -> Vec<u8> {
    let dest = destination.as_str().as_bytes();
    let mut key = Vec::with_capacity(dest.len() + 32);
    key.extend_from_slice(dest);
    key.extend_from_slice(source_hash.as_bytes());
    key
}

/// Build the binary composite key from raw bytes (used by WriteBatch).
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
        let prefix = destination.as_str().as_bytes();
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
            let (_key, val) = result.map_err(LmdbError::from)?;
            let info: PendingInfo = bincode::deserialize(val).map_err(LmdbError::from)?;
            results.push(info);
        }
        Ok(results)
    }

    fn pending_count(&self) -> Result<u64, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let count = self.pending_db.len(&rtxn).map_err(LmdbError::from)?;
        Ok(count)
    }
}
