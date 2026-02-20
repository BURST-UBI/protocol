//! LMDB implementation of MergerGraphStore.
//!
//! `merger_origins_db` and `merger_downstream_db` use composite keys
//! `(parent(32), child(32))` → empty, enabling O(1) put and prefix
//! range-scan for listing.

use std::ops::Bound;
use std::sync::Arc;

use heed::types::Bytes;
use heed::{Database, Env};

use burst_store::merger_graph::MergerGraphStore;
use burst_store::StoreError;
use burst_types::TxHash;

use crate::block::increment_prefix;
use crate::LmdbError;

pub struct LmdbMergerGraphStore {
    pub(crate) env: Arc<Env>,
    pub(crate) merger_origins_db: Database<Bytes, Bytes>,
    pub(crate) merger_downstream_db: Database<Bytes, Bytes>,
    pub(crate) merger_nodes_db: Database<Bytes, Bytes>,
}

/// Build a 64-byte composite key `parent(32) ++ child(32)`.
fn composite_key(parent: &[u8; 32], child: &[u8; 32]) -> [u8; 64] {
    let mut key = [0u8; 64];
    key[..32].copy_from_slice(parent);
    key[32..].copy_from_slice(child);
    key
}

/// Prefix range-scan: collect all 32-byte suffixes whose key starts with `prefix`.
fn range_scan_children(
    db: &Database<Bytes, Bytes>,
    env: &Env,
    prefix: &[u8; 32],
) -> Result<Vec<TxHash>, LmdbError> {
    let rtxn = env.read_txn()?;
    let mut upper = prefix.to_vec();
    increment_prefix(&mut upper);
    let bounds = (
        Bound::Included(prefix.as_ref()),
        Bound::Excluded(upper.as_slice()),
    );
    let iter = db.range(&rtxn, &bounds)?;
    let mut results = Vec::new();
    for result in iter {
        let (key, _) = result?;
        if key.len() == 64 {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&key[32..]);
            results.push(TxHash::new(arr));
        }
    }
    Ok(results)
}

impl MergerGraphStore for LmdbMergerGraphStore {
    fn put_origin_merge(&self, origin: &TxHash, merge_tx: &TxHash) -> Result<(), StoreError> {
        let key = composite_key(origin.as_bytes(), merge_tx.as_bytes());
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.merger_origins_db
            .put(&mut wtxn, &key[..], &[])
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_merges_for_origin(&self, origin: &TxHash) -> Result<Vec<TxHash>, StoreError> {
        range_scan_children(&self.merger_origins_db, &self.env, origin.as_bytes())
            .map_err(StoreError::from)
    }

    fn put_downstream(&self, parent: &TxHash, child: &TxHash) -> Result<(), StoreError> {
        let key = composite_key(parent.as_bytes(), child.as_bytes());
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.merger_downstream_db
            .put(&mut wtxn, &key[..], &[])
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_downstream(&self, parent: &TxHash) -> Result<Vec<TxHash>, StoreError> {
        range_scan_children(&self.merger_downstream_db, &self.env, parent.as_bytes())
            .map_err(StoreError::from)
    }

    fn put_merge_node(&self, merge_tx: &TxHash, data: &[u8]) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.merger_nodes_db
            .put(&mut wtxn, merge_tx.as_bytes().as_slice(), data)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_merge_node(&self, merge_tx: &TxHash) -> Result<Vec<u8>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let val = self
            .merger_nodes_db
            .get(&rtxn, merge_tx.as_bytes().as_slice())
            .map_err(LmdbError::from)?
            .ok_or_else(|| LmdbError::NotFound(format!("merge node {:?}", merge_tx)))?;
        Ok(val.to_vec())
    }
}
