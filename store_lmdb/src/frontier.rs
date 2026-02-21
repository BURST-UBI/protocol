//! LMDB implementation of FrontierStore.

use std::sync::Arc;

use heed::types::Bytes;
use heed::{Database, Env};

use burst_store::frontier::FrontierStore;
use burst_store::StoreError;
use burst_types::{BlockHash, WalletAddress};

use crate::LmdbError;

pub struct LmdbFrontierStore {
    pub(crate) env: Arc<Env>,
    pub(crate) frontiers_db: Database<Bytes, Bytes>,
}

impl FrontierStore for LmdbFrontierStore {
    fn put_frontier(&self, account: &WalletAddress, head: &BlockHash) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.frontiers_db
            .put(&mut wtxn, account.as_str().as_bytes(), head.as_bytes())
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_frontier(&self, account: &WalletAddress) -> Result<BlockHash, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let val = self
            .frontiers_db
            .get(&rtxn, account.as_str().as_bytes())
            .map_err(LmdbError::from)?
            .ok_or_else(|| LmdbError::NotFound(format!("frontier {}", account.as_str())))?;
        let arr: [u8; 32] = val
            .try_into()
            .map_err(|_| LmdbError::Serialization("invalid frontier hash length".into()))?;
        Ok(BlockHash::new(arr))
    }

    fn delete_frontier(&self, account: &WalletAddress) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.frontiers_db
            .delete(&mut wtxn, account.as_str().as_bytes())
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn iter_frontiers(&self) -> Result<Vec<(WalletAddress, BlockHash)>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let mut frontiers = Vec::new();
        let iter = self.frontiers_db.iter(&rtxn).map_err(LmdbError::from)?;
        for result in iter {
            let (key, val) = result.map_err(LmdbError::from)?;
            let key_str =
                std::str::from_utf8(key).map_err(|e| LmdbError::Serialization(e.to_string()))?;
            let address = WalletAddress::new(key_str);
            let arr: [u8; 32] = val
                .try_into()
                .map_err(|_| LmdbError::Serialization("invalid frontier hash length".into()))?;
            frontiers.push((address, BlockHash::new(arr)));
        }
        Ok(frontiers)
    }

    fn frontier_count(&self) -> Result<u64, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let count = self.frontiers_db.len(&rtxn).map_err(LmdbError::from)?;
        Ok(count)
    }
}
