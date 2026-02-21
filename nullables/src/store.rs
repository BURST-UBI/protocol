//! Nullable store — thread-safe in-memory storage for testing.

use burst_store::account::{AccountInfo, AccountStore};
use burst_store::block::BlockStore;
use burst_store::delegation::{DelegationRecord, DelegationStore};
use burst_store::StoreError;
use burst_types::{BlockHash, WalletAddress};
use std::collections::HashMap;
use std::sync::Mutex;

/// An in-memory account + block store for testing.
/// Thread-safe for use with tokio's multi-threaded runtime.
pub struct NullStore {
    accounts: Mutex<HashMap<String, AccountInfo>>,
    blocks: Mutex<HashMap<[u8; 32], Vec<u8>>>,
    account_blocks: Mutex<HashMap<String, Vec<BlockHash>>>,
}

impl NullStore {
    pub fn new() -> Self {
        Self {
            accounts: Mutex::new(HashMap::new()),
            blocks: Mutex::new(HashMap::new()),
            account_blocks: Mutex::new(HashMap::new()),
        }
    }

    /// Add a block hash to an account's chain.
    pub fn add_account_block(&self, address: &WalletAddress, hash: BlockHash) {
        self.account_blocks
            .lock()
            .unwrap()
            .entry(address.to_string())
            .or_default()
            .push(hash);
    }
}

impl Default for NullStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AccountStore for NullStore {
    fn get_account(&self, address: &WalletAddress) -> Result<AccountInfo, StoreError> {
        self.accounts
            .lock()
            .unwrap()
            .get(address.as_str())
            .cloned()
            .ok_or_else(|| StoreError::NotFound(address.to_string()))
    }

    fn put_account(&self, info: &AccountInfo) -> Result<(), StoreError> {
        self.accounts
            .lock()
            .unwrap()
            .insert(info.address.to_string(), info.clone());
        Ok(())
    }

    fn exists(&self, address: &WalletAddress) -> Result<bool, StoreError> {
        Ok(self.accounts.lock().unwrap().contains_key(address.as_str()))
    }

    fn account_count(&self) -> Result<u64, StoreError> {
        Ok(self.accounts.lock().unwrap().len() as u64)
    }

    fn iter_accounts(&self) -> Result<Vec<AccountInfo>, StoreError> {
        Ok(self.accounts.lock().unwrap().values().cloned().collect())
    }

    fn iter_verified_accounts(&self) -> Result<Vec<AccountInfo>, StoreError> {
        Ok(self
            .accounts
            .lock()
            .unwrap()
            .values()
            .filter(|a| a.state.can_transact())
            .cloned()
            .collect())
    }
}

impl BlockStore for NullStore {
    fn put_block(&self, hash: &BlockHash, block_bytes: &[u8]) -> Result<(), StoreError> {
        self.blocks
            .lock()
            .unwrap()
            .insert(*hash.as_bytes(), block_bytes.to_vec());
        Ok(())
    }

    fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError> {
        self.blocks
            .lock()
            .unwrap()
            .get(hash.as_bytes())
            .cloned()
            .ok_or_else(|| StoreError::NotFound(format!("{:?}", hash)))
    }

    fn exists(&self, hash: &BlockHash) -> Result<bool, StoreError> {
        Ok(self.blocks.lock().unwrap().contains_key(hash.as_bytes()))
    }

    fn delete_block(&self, hash: &BlockHash) -> Result<(), StoreError> {
        self.blocks.lock().unwrap().remove(hash.as_bytes());
        Ok(())
    }

    fn get_account_blocks(&self, address: &WalletAddress) -> Result<Vec<BlockHash>, StoreError> {
        Ok(self
            .account_blocks
            .lock()
            .unwrap()
            .get(address.as_str())
            .cloned()
            .unwrap_or_default())
    }

    fn block_count(&self) -> Result<u64, StoreError> {
        Ok(self.blocks.lock().unwrap().len() as u64)
    }

    fn block_at_height(
        &self,
        _account: &WalletAddress,
        _height: u64,
    ) -> Result<Option<BlockHash>, StoreError> {
        Ok(None)
    }

    fn height_of_block(&self, _block_hash: &BlockHash) -> Result<Option<u64>, StoreError> {
        Ok(None)
    }
}

/// An in-memory delegation store for testing.
pub struct NullDelegationStore {
    by_delegator: Mutex<HashMap<String, DelegationRecord>>,
    pubkey_index: Mutex<HashMap<[u8; 32], String>>,
}

impl NullDelegationStore {
    pub fn new() -> Self {
        Self {
            by_delegator: Mutex::new(HashMap::new()),
            pubkey_index: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for NullDelegationStore {
    fn default() -> Self {
        Self::new()
    }
}

impl DelegationStore for NullDelegationStore {
    fn put_delegation(&self, record: &DelegationRecord) -> Result<(), StoreError> {
        let key = record.delegator.to_string();
        self.pubkey_index
            .lock()
            .unwrap()
            .insert(record.delegation_public_key, key.clone());
        self.by_delegator
            .lock()
            .unwrap()
            .insert(key, record.clone());
        Ok(())
    }

    fn get_delegation_by_delegator(
        &self,
        delegator: &WalletAddress,
    ) -> Result<Option<DelegationRecord>, StoreError> {
        Ok(self
            .by_delegator
            .lock()
            .unwrap()
            .get(delegator.as_str())
            .cloned())
    }

    fn get_delegation_by_pubkey(
        &self,
        pubkey: &[u8; 32],
    ) -> Result<Option<DelegationRecord>, StoreError> {
        let index = self.pubkey_index.lock().unwrap();
        match index.get(pubkey) {
            Some(delegator_key) => {
                let records = self.by_delegator.lock().unwrap();
                Ok(records.get(delegator_key).cloned())
            }
            None => Ok(None),
        }
    }

    fn revoke_delegation(&self, delegator: &WalletAddress) -> Result<(), StoreError> {
        let mut records = self.by_delegator.lock().unwrap();
        if let Some(record) = records.get_mut(delegator.as_str()) {
            record.revoked = true;
            Ok(())
        } else {
            Err(StoreError::NotFound(delegator.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{Timestamp, WalletState};

    fn test_address() -> WalletAddress {
        WalletAddress::new("brst_test_111".to_string())
    }

    fn test_account_info(addr: &WalletAddress) -> AccountInfo {
        AccountInfo {
            address: addr.clone(),
            state: WalletState::Verified,
            verified_at: Some(Timestamp::new(1000)),
            head: BlockHash::ZERO,
            block_count: 0,
            confirmation_height: 0,
            representative: addr.clone(),
            total_brn_burned: 0,
            trst_balance: 0,
            total_brn_staked: 0,
            expired_trst: 0,
            revoked_trst: 0,
            epoch: 0,
        }
    }

    #[test]
    fn test_put_get_account() {
        let store = NullStore::new();
        let addr = test_address();
        let info = test_account_info(&addr);
        store.put_account(&info).unwrap();
        let retrieved = store.get_account(&addr).unwrap();
        assert_eq!(retrieved.address.as_str(), addr.as_str());
    }

    #[test]
    fn test_account_not_found() {
        let store = NullStore::new();
        let addr = WalletAddress::new("brst_nonexistent".to_string());
        assert!(store.get_account(&addr).is_err());
    }

    #[test]
    fn test_put_get_block() {
        let store = NullStore::new();
        let hash = BlockHash::new([42u8; 32]);
        store.put_block(&hash, b"block_data").unwrap();
        assert_eq!(store.get_block(&hash).unwrap(), b"block_data");
    }

    #[test]
    fn test_delete_block() {
        let store = NullStore::new();
        let hash = BlockHash::new([42u8; 32]);
        store.put_block(&hash, b"data").unwrap();
        store.delete_block(&hash).unwrap();
        assert!(store.get_block(&hash).is_err());
    }
}
