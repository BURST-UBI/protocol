use burst_store::{BrnStore, StoreError};
use burst_types::WalletAddress;
use heed::{Database, Env, types::Bytes};
use std::sync::Arc;

pub struct LmdbBrnStore {
    env: Arc<Env>,
    wallets_db: Database<Bytes, Bytes>,
    meta_db: Database<Bytes, Bytes>,
}

impl LmdbBrnStore {
    pub fn new(env: Arc<Env>, wallets_db: Database<Bytes, Bytes>, meta_db: Database<Bytes, Bytes>) -> Self {
        Self { env, wallets_db, meta_db }
    }
}

impl BrnStore for LmdbBrnStore {
    fn get_wallet_state(&self, address: &WalletAddress) -> Result<Option<Vec<u8>>, StoreError> {
        let txn = self.env.read_txn().map_err(|e| StoreError::Backend(e.to_string()))?;
        match self.wallets_db.get(&txn, address.as_str().as_bytes()) {
            Ok(Some(bytes)) => Ok(Some(bytes.to_vec())),
            Ok(None) => Ok(None),
            Err(e) => Err(StoreError::Backend(e.to_string())),
        }
    }

    fn put_wallet_state(&self, address: &WalletAddress, state: &[u8]) -> Result<(), StoreError> {
        let mut txn = self.env.write_txn().map_err(|e| StoreError::Backend(e.to_string()))?;
        self.wallets_db.put(&mut txn, address.as_str().as_bytes(), state)
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        txn.commit().map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(())
    }

    fn delete_wallet_state(&self, address: &WalletAddress) -> Result<(), StoreError> {
        let mut txn = self.env.write_txn().map_err(|e| StoreError::Backend(e.to_string()))?;
        self.wallets_db.delete(&mut txn, address.as_str().as_bytes())
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        txn.commit().map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(())
    }

    fn iter_wallet_states(&self) -> Result<Vec<(WalletAddress, Vec<u8>)>, StoreError> {
        let txn = self.env.read_txn().map_err(|e| StoreError::Backend(e.to_string()))?;
        let mut results = Vec::new();
        let iter = self.wallets_db.iter(&txn).map_err(|e| StoreError::Backend(e.to_string()))?;
        for item in iter {
            let (key, val) = item.map_err(|e| StoreError::Backend(e.to_string()))?;
            let addr_str = std::str::from_utf8(key).map_err(|e| StoreError::Backend(e.to_string()))?;
            results.push((WalletAddress::new(addr_str.to_string()), val.to_vec()));
        }
        Ok(results)
    }

    fn get_meta(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        let txn = self.env.read_txn().map_err(|e| StoreError::Backend(e.to_string()))?;
        match self.meta_db.get(&txn, key) {
            Ok(Some(bytes)) => Ok(Some(bytes.to_vec())),
            Ok(None) => Ok(None),
            Err(e) => Err(StoreError::Backend(e.to_string())),
        }
    }

    fn put_meta(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        let mut txn = self.env.write_txn().map_err(|e| StoreError::Backend(e.to_string()))?;
        self.meta_db.put(&mut txn, key, value)
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        txn.commit().map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(())
    }
}
