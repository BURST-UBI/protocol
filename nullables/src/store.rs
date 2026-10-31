//! In-memory account and block maps for tests that need no persistence.

use burst_store::account::{AccountInfo, AccountStore};
use burst_store::block::BlockStore;
use burst_store::StoreError;
use burst_types::{BlockHash, WalletAddress};
use std::collections::HashMap;
use std::sync::Mutex;

/// Partial AccountStore/BlockStore implementation. Each map has its own mutex;
/// writes across maps are not transactional. Height lookups always return None.
/// Block insertion/deletion does not maintain the separate account-chain list.
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

    /// Append to the fixture's account-chain list, without storing block bytes or
    /// updating the account record.
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

    fn advance_confirmation_height(
        &self,
        address: &WalletAddress,
        height: u64,
    ) -> Result<(), StoreError> {
        let mut accounts = self.accounts.lock().unwrap();
        let account = accounts
            .get_mut(address.as_str())
            .ok_or_else(|| StoreError::NotFound(address.to_string()))?;
        if height > account.block_count {
            return Err(StoreError::Backend(
                "confirmation exceeds chain height".into(),
            ));
        }
        account.confirmation_height = account.confirmation_height.max(height);
        Ok(())
    }

    fn account_count(&self) -> Result<u64, StoreError> {
        Ok(self.accounts.lock().unwrap().len() as u64)
    }

    fn all_accounts(&self) -> Result<Vec<AccountInfo>, StoreError> {
        Ok(self.accounts.lock().unwrap().values().cloned().collect())
    }

    fn all_verified_accounts(&self) -> Result<Vec<AccountInfo>, StoreError> {
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
            verifier_opted_in_at: None,
            orv_evicted: false,
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
