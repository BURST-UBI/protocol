//! LMDB implementation of PeerStore.

use std::sync::Arc;

use heed::types::Bytes;
use heed::{Database, Env};

use burst_store::peer::PeerStore;
use burst_store::StoreError;

use crate::LmdbError;

pub struct LmdbPeerStore {
    pub(crate) env: Arc<Env>,
    pub(crate) peers_db: Database<Bytes, Bytes>,
}

impl PeerStore for LmdbPeerStore {
    fn put_peer(&self, addr: &str, timestamp: u64) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.peers_db
            .put(&mut wtxn, addr.as_bytes(), &timestamp.to_le_bytes())
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_peer(&self, addr: &str) -> Result<Option<u64>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let val = self
            .peers_db
            .get(&rtxn, addr.as_bytes())
            .map_err(LmdbError::from)?;
        match val {
            Some(bytes) if bytes.len() == 8 => {
                let arr: [u8; 8] = bytes.try_into().expect("checked length");
                Ok(Some(u64::from_le_bytes(arr)))
            }
            Some(_) => Ok(None),
            None => Ok(None),
        }
    }

    fn delete_peer(&self, addr: &str) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.peers_db
            .delete(&mut wtxn, addr.as_bytes())
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn iter_peers(&self) -> Result<Vec<(String, u64)>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let iter = self.peers_db.iter(&rtxn).map_err(LmdbError::from)?;
        let mut result = Vec::new();
        for entry in iter {
            let (key, val) = entry.map_err(LmdbError::from)?;
            if let (Ok(addr), true) = (std::str::from_utf8(key), val.len() == 8) {
                let arr: [u8; 8] = val.try_into().expect("checked length");
                result.push((addr.to_string(), u64::from_le_bytes(arr)));
            }
        }
        Ok(result)
    }

    fn purge_older_than(&self, cutoff_secs: u64) -> Result<usize, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let iter = self.peers_db.iter(&rtxn).map_err(LmdbError::from)?;
        let mut to_delete = Vec::new();
        for entry in iter {
            let (key, val) = entry.map_err(LmdbError::from)?;
            if val.len() == 8 {
                let arr: [u8; 8] = val.try_into().expect("checked length");
                let ts = u64::from_le_bytes(arr);
                if ts < cutoff_secs {
                    to_delete.push(key.to_vec());
                }
            }
        }
        drop(rtxn);

        let count = to_delete.len();
        if !to_delete.is_empty() {
            let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
            for key in &to_delete {
                self.peers_db
                    .delete(&mut wtxn, key)
                    .map_err(LmdbError::from)?;
            }
            wtxn.commit().map_err(LmdbError::from)?;
        }
        Ok(count)
    }
}
