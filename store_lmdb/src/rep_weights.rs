//! LMDB implementation of RepWeightStore.

use std::sync::Arc;

use heed::types::Bytes;
use heed::{Database, Env};

use burst_store::rep_weights::RepWeightStore;
use burst_store::StoreError;
use burst_types::WalletAddress;

use crate::LmdbError;

pub struct LmdbRepWeightStore {
    pub(crate) env: Arc<Env>,
    pub(crate) rep_weights_db: Database<Bytes, Bytes>,
    pub(crate) online_weight_db: Database<Bytes, Bytes>,
}

impl RepWeightStore for LmdbRepWeightStore {
    fn put_rep_weight(&self, rep: &WalletAddress, weight: u128) -> Result<(), StoreError> {
        let key = rep.as_str().as_bytes();
        let val = weight.to_be_bytes();
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.rep_weights_db
            .put(&mut wtxn, key, &val)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_rep_weight(&self, rep: &WalletAddress) -> Result<Option<u128>, StoreError> {
        let key = rep.as_str().as_bytes();
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        match self.rep_weights_db.get(&rtxn, key).map_err(LmdbError::from)? {
            Some(bytes) => {
                if bytes.len() != 16 {
                    return Err(StoreError::Serialization(
                        "invalid rep weight bytes length".into(),
                    ));
                }
                let mut buf = [0u8; 16];
                buf.copy_from_slice(bytes);
                Ok(Some(u128::from_be_bytes(buf)))
            }
            None => Ok(None),
        }
    }

    fn delete_rep_weight(&self, rep: &WalletAddress) -> Result<(), StoreError> {
        let key = rep.as_str().as_bytes();
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.rep_weights_db
            .delete(&mut wtxn, key)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn iter_rep_weights(&self) -> Result<Vec<(WalletAddress, u128)>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let iter = self.rep_weights_db.iter(&rtxn).map_err(LmdbError::from)?;
        let mut results = Vec::new();
        for entry in iter {
            let (key, val) = entry.map_err(LmdbError::from)?;
            let addr_str = std::str::from_utf8(key)
                .map_err(|e| LmdbError::Serialization(e.to_string()))?;
            if val.len() != 16 {
                continue;
            }
            let mut buf = [0u8; 16];
            buf.copy_from_slice(val);
            results.push((WalletAddress::new(addr_str), u128::from_be_bytes(buf)));
        }
        Ok(results)
    }

    fn put_online_weight_sample(&self, timestamp: u64, weight: u128) -> Result<(), StoreError> {
        let key = timestamp.to_be_bytes();
        let val = weight.to_be_bytes();
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.online_weight_db
            .put(&mut wtxn, &key, &val)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_online_weight_samples(&self, limit: usize) -> Result<Vec<(u64, u128)>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let iter = self
            .online_weight_db
            .rev_iter(&rtxn)
            .map_err(LmdbError::from)?;
        let mut results = Vec::new();
        for entry in iter {
            if results.len() >= limit {
                break;
            }
            let (key, val) = entry.map_err(LmdbError::from)?;
            if key.len() != 8 || val.len() != 16 {
                continue;
            }
            let mut ts_buf = [0u8; 8];
            ts_buf.copy_from_slice(key);
            let mut wt_buf = [0u8; 16];
            wt_buf.copy_from_slice(val);
            results.push((u64::from_be_bytes(ts_buf), u128::from_be_bytes(wt_buf)));
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::rep_weights::RepWeightStore;

    fn open_test_env() -> crate::LmdbEnvironment {
        let dir = tempfile::tempdir().unwrap();
        crate::LmdbEnvironment::open(dir.path(), 30, 1 << 20).unwrap()
    }

    #[test]
    fn put_and_get_rep_weight() {
        let env = open_test_env();
        let store = env.rep_weight_store();
        let rep = WalletAddress::new("brst_test_rep_alice");

        assert_eq!(store.get_rep_weight(&rep).unwrap(), None);

        store.put_rep_weight(&rep, 42_000).unwrap();
        assert_eq!(store.get_rep_weight(&rep).unwrap(), Some(42_000));
    }

    #[test]
    fn delete_rep_weight() {
        let env = open_test_env();
        let store = env.rep_weight_store();
        let rep = WalletAddress::new("brst_test_rep_bob");

        store.put_rep_weight(&rep, 100).unwrap();
        assert_eq!(store.get_rep_weight(&rep).unwrap(), Some(100));

        store.delete_rep_weight(&rep).unwrap();
        assert_eq!(store.get_rep_weight(&rep).unwrap(), None);
    }

    #[test]
    fn iter_rep_weights_returns_all() {
        let env = open_test_env();
        let store = env.rep_weight_store();

        store.put_rep_weight(&WalletAddress::new("brst_alice"), 1000).unwrap();
        store.put_rep_weight(&WalletAddress::new("brst_bob"), 2000).unwrap();
        store.put_rep_weight(&WalletAddress::new("brst_carol"), 3000).unwrap();

        let all = store.iter_rep_weights().unwrap();
        assert_eq!(all.len(), 3);

        let total: u128 = all.iter().map(|(_, w)| w).sum();
        assert_eq!(total, 6000);
    }

    #[test]
    fn overwrite_rep_weight() {
        let env = open_test_env();
        let store = env.rep_weight_store();
        let rep = WalletAddress::new("brst_test_rep");

        store.put_rep_weight(&rep, 100).unwrap();
        store.put_rep_weight(&rep, 200).unwrap();
        assert_eq!(store.get_rep_weight(&rep).unwrap(), Some(200));
    }

    #[test]
    fn online_weight_samples_put_and_get() {
        let env = open_test_env();
        let store = env.rep_weight_store();

        store.put_online_weight_sample(1000, 500_000).unwrap();
        store.put_online_weight_sample(1020, 600_000).unwrap();
        store.put_online_weight_sample(1040, 700_000).unwrap();

        let samples = store.get_online_weight_samples(10).unwrap();
        assert_eq!(samples.len(), 3);
        // Newest first (descending timestamp)
        assert_eq!(samples[0], (1040, 700_000));
        assert_eq!(samples[1], (1020, 600_000));
        assert_eq!(samples[2], (1000, 500_000));
    }

    #[test]
    fn online_weight_samples_limited() {
        let env = open_test_env();
        let store = env.rep_weight_store();

        for ts in 0..20 {
            store.put_online_weight_sample(ts * 20, ts as u128 * 1000).unwrap();
        }

        let samples = store.get_online_weight_samples(5).unwrap();
        assert_eq!(samples.len(), 5);
        assert_eq!(samples[0].0, 380); // newest
    }

    #[test]
    fn empty_store_returns_none_and_empty() {
        let env = open_test_env();
        let store = env.rep_weight_store();

        assert_eq!(store.get_rep_weight(&WalletAddress::new("brst_nobody")).unwrap(), None);
        assert!(store.iter_rep_weights().unwrap().is_empty());
        assert!(store.get_online_weight_samples(10).unwrap().is_empty());
    }
}
