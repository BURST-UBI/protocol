//! LMDB implementation of TrstIndexStore.
//!
//! Three databases:
//! - `trst_origin_db`: composite key `(origin_hash(32), tx_hash(32))` → empty.
//!   Enables O(1) put/delete and prefix range-scan for all tokens sharing an origin.
//! - `trst_expiry_db`: binary key `expiry_be_u64(8) ++ tx_hash(32)` → empty.
//!   Big-endian u64 sorts lexicographically by time, enabling efficient range scans.
//! - `trst_reverse_db`: `tx_hash(32)` → `origin_hash(32) ++ expiry_be_u64(8)`.
//!   Enables O(1) `delete_token`.

use crate::shared_env::SharedEnv;
use std::ops::Bound;

use heed::types::Bytes;
use heed::Database;

use burst_store::trst_index::TrstIndexStore;
use burst_store::StoreError;
use burst_types::{Timestamp, TxHash};

use crate::block::increment_prefix;
use crate::LmdbError;

pub struct LmdbTrstIndexStore {
    pub(crate) env: SharedEnv,
    pub(crate) trst_origin_db: Database<Bytes, Bytes>,
    pub(crate) trst_expiry_db: Database<Bytes, Bytes>,
    /// Reverse index: tx_hash(32) → origin_hash(32) + expiry_be(8).
    pub(crate) trst_reverse_db: Database<Bytes, Bytes>,
}

/// Build the 64-byte composite key `origin_hash ++ tx_hash` for `trst_origin_db`.
fn origin_composite_key(origin_hash: &TxHash, tx_hash: &TxHash) -> [u8; 64] {
    let mut key = [0u8; 64];
    key[..32].copy_from_slice(origin_hash.as_bytes());
    key[32..].copy_from_slice(tx_hash.as_bytes());
    key
}

/// Build the 40-byte binary key `expiry_be_u64 ++ tx_hash` for `trst_expiry_db`.
fn expiry_binary_key(expiry: Timestamp, tx_hash: &TxHash) -> [u8; 40] {
    let mut key = [0u8; 40];
    key[..8].copy_from_slice(&expiry.as_secs().to_be_bytes());
    key[8..].copy_from_slice(tx_hash.as_bytes());
    key
}

impl LmdbTrstIndexStore {
    /// Remove only derived expiry lookup rows, using their actual stored keys.
    /// Origins, reverse mappings, token portfolios and ledger bodies stay intact.
    pub fn clean_expiry_before(
        &self,
        cutoff: Timestamp,
        limit: usize,
    ) -> Result<usize, StoreError> {
        if limit == 0 || limit > 4096 {
            return Err(StoreError::Corruption(
                "invalid expiry cleanup batch limit".into(),
            ));
        }
        let mut txn = self.env.write_txn().map_err(LmdbError::from)?;
        let upper = expiry_binary_key(cutoff, &TxHash::ZERO);
        let bounds = (Bound::<&[u8]>::Unbounded, Bound::Excluded(upper.as_ref()));
        let keys = self
            .trst_expiry_db
            .range(&txn, &bounds)
            .map_err(LmdbError::from)?
            .take(limit)
            .map(|row| {
                let (key, value) = row.map_err(LmdbError::from)?;
                if key.len() != 40 || !value.is_empty() {
                    return Err(StoreError::Corruption("invalid expiry index row".into()));
                }
                Ok(key.to_vec())
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        for key in &keys {
            self.trst_expiry_db
                .delete(&mut txn, key)
                .map_err(LmdbError::from)?;
        }
        txn.commit().map_err(LmdbError::from)?;
        Ok(keys.len())
    }
}

impl TrstIndexStore for LmdbTrstIndexStore {
    fn put_origin_index(&self, origin_hash: &TxHash, tx_hash: &TxHash) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;

        let key = origin_composite_key(origin_hash, tx_hash);
        self.trst_origin_db
            .put(&mut wtxn, &key[..], &[])
            .map_err(LmdbError::from)?;

        // Update reverse index: store origin for this tx_hash.
        let mut rev_val = match self
            .trst_reverse_db
            .get(&wtxn, tx_hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?
        {
            Some(bytes) => bytes.to_vec(),
            None => vec![0u8; 40],
        };
        if rev_val.len() >= 32 {
            rev_val[..32].copy_from_slice(origin_hash.as_bytes());
        }
        self.trst_reverse_db
            .put(&mut wtxn, tx_hash.as_bytes().as_slice(), &rev_val)
            .map_err(LmdbError::from)?;

        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_by_origin(&self, origin_hash: &TxHash) -> Result<Vec<TxHash>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let prefix = origin_hash.as_bytes();
        let mut upper = prefix.to_vec();
        increment_prefix(&mut upper);

        let bounds = (
            Bound::Included(prefix.as_ref()),
            Bound::Excluded(upper.as_slice()),
        );
        let iter = self
            .trst_origin_db
            .range(&rtxn, &bounds)
            .map_err(LmdbError::from)?;
        let mut results = Vec::new();
        for result in iter {
            let (key, _) = result.map_err(LmdbError::from)?;
            if key.len() == 64 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&key[32..]);
                results.push(TxHash::new(arr));
            }
        }
        Ok(results)
    }

    fn delete_origin_index(&self, origin_hash: &TxHash) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        let prefix = origin_hash.as_bytes();
        let mut upper = prefix.to_vec();
        increment_prefix(&mut upper);

        let bounds = (
            Bound::Included(prefix.as_ref()),
            Bound::Excluded(upper.as_slice()),
        );
        let keys: Vec<Vec<u8>> = self
            .trst_origin_db
            .range(&wtxn, &bounds)
            .map_err(LmdbError::from)?
            .filter_map(|r| r.ok().map(|(k, _)| k.to_vec()))
            .collect();
        for k in keys {
            self.trst_origin_db
                .delete(&mut wtxn, &k)
                .map_err(LmdbError::from)?;
        }
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn put_expiry_index(&self, expiry: Timestamp, tx_hash: &TxHash) -> Result<(), StoreError> {
        let key = expiry_binary_key(expiry, tx_hash);
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.trst_expiry_db
            .put(&mut wtxn, &key[..], &[])
            .map_err(LmdbError::from)?;

        // Update reverse index: store expiry for this tx_hash.
        let mut rev_val = match self
            .trst_reverse_db
            .get(&wtxn, tx_hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?
        {
            Some(bytes) => bytes.to_vec(),
            None => vec![0u8; 40],
        };
        if rev_val.len() >= 40 {
            rev_val[32..40].copy_from_slice(&expiry.as_secs().to_be_bytes());
        }
        self.trst_reverse_db
            .put(&mut wtxn, tx_hash.as_bytes().as_slice(), &rev_val)
            .map_err(LmdbError::from)?;

        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_expired_before(&self, cutoff: Timestamp) -> Result<Vec<TxHash>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let upper = expiry_binary_key(cutoff, &TxHash::new([0u8; 32]));
        let bounds = (Bound::<&[u8]>::Unbounded, Bound::Excluded(upper.as_ref()));
        let iter = self
            .trst_expiry_db
            .range(&rtxn, &bounds)
            .map_err(LmdbError::from)?;
        let mut results = Vec::new();
        for result in iter {
            let (key, _) = result.map_err(LmdbError::from)?;
            if key.len() == 40 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&key[8..]);
                results.push(TxHash::new(arr));
            }
        }
        Ok(results)
    }

    fn delete_expiry_index(&self, expiry: Timestamp, tx_hash: &TxHash) -> Result<(), StoreError> {
        let key = expiry_binary_key(expiry, tx_hash);
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.trst_expiry_db
            .delete(&mut wtxn, &key[..])
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn delete_token(&self, tx_hash: &TxHash) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;

        let rev_val = self
            .trst_reverse_db
            .get(&wtxn, tx_hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?;

        if let Some(bytes) = rev_val {
            if bytes.len() >= 40 {
                let mut origin_arr = [0u8; 32];
                origin_arr.copy_from_slice(&bytes[..32]);
                let origin_hash = TxHash::new(origin_arr);

                let expiry_secs = u64::from_be_bytes(bytes[32..40].try_into().unwrap());

                // Delete from expiry DB — O(1).
                if expiry_secs > 0 {
                    let exp_key = expiry_binary_key(Timestamp::new(expiry_secs), tx_hash);
                    let _ = self
                        .trst_expiry_db
                        .delete(&mut wtxn, &exp_key[..])
                        .map_err(LmdbError::from)?;
                }

                // Delete from origin DB — O(1) with composite key.
                let all_zero = origin_arr.iter().all(|&b| b == 0);
                if !all_zero {
                    let ck = origin_composite_key(&origin_hash, tx_hash);
                    let _ = self
                        .trst_origin_db
                        .delete(&mut wtxn, &ck[..])
                        .map_err(LmdbError::from)?;
                }
            }
        }

        let _ = self
            .trst_reverse_db
            .delete(&mut wtxn, tx_hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?;

        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    use crate::LmdbStore;

    #[test]
    fn expiry_cleanup_is_bounded_exact_and_preserves_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let index = store.trst_index_store();
        let origin = TxHash::new([99; 32]);
        for n in 1..=4 {
            let token = TxHash::new([n; 32]);
            index.put_origin_index(&origin, &token).unwrap();
            index
                .put_expiry_index(Timestamp::new(n as u64 * 10), &token)
                .unwrap();
        }
        let before = {
            let txn = index.env.read_txn().unwrap();
            index
                .trst_reverse_db
                .iter(&txn)
                .unwrap()
                .map(|row| {
                    let (key, value) = row.unwrap();
                    (key.to_vec(), value.to_vec())
                })
                .collect::<Vec<_>>()
        };
        assert!(index.clean_expiry_before(Timestamp::new(30), 0).is_err());
        assert!(index.clean_expiry_before(Timestamp::new(30), 4097).is_err());
        assert_eq!(index.clean_expiry_before(Timestamp::new(30), 1).unwrap(), 1);
        assert_eq!(
            index.get_expired_before(Timestamp::new(30)).unwrap(),
            vec![TxHash::new([2; 32])]
        );
        assert_eq!(index.clean_expiry_before(Timestamp::new(30), 8).unwrap(), 1);
        assert_eq!(index.clean_expiry_before(Timestamp::new(30), 8).unwrap(), 0);
        assert_eq!(
            index.get_expired_before(Timestamp::new(41)).unwrap(),
            vec![TxHash::new([3; 32]), TxHash::new([4; 32])]
        );
        assert_eq!(index.get_by_origin(&origin).unwrap().len(), 4);
        let txn = index.env.read_txn().unwrap();
        for (key, value) in before {
            assert_eq!(
                index.trst_reverse_db.get(&txn, &key).unwrap(),
                Some(value.as_slice())
            );
        }
    }

    #[test]
    fn expiry_cleanup_aborts_on_a_damaged_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let index = store.trst_index_store();
        index
            .put_expiry_index(Timestamp::new(1), &TxHash::new([1; 32]))
            .unwrap();
        let bad = expiry_binary_key(Timestamp::new(2), &TxHash::new([2; 32]));
        let mut txn = index.env.write_txn().unwrap();
        index.trst_expiry_db.put(&mut txn, &bad, b"bad").unwrap();
        txn.commit().unwrap();
        assert!(index.clean_expiry_before(Timestamp::new(3), 8).is_err());
        let txn = index.env.read_txn().unwrap();
        assert_eq!(index.trst_expiry_db.len(&txn).unwrap(), 2);
        assert_eq!(
            index.trst_expiry_db.get(&txn, &bad).unwrap(),
            Some(b"bad".as_slice())
        );
    }
}
