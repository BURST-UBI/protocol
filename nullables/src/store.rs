//! Nullable store — in-memory storage for testing.

use burst_store::account::{AccountInfo, AccountStore};
use burst_store::block::BlockStore;
use burst_store::StoreError;
use burst_types::{BlockHash, WalletAddress};
use std::collections::HashMap;

/// An in-memory account store for testing.
pub struct NullStore {
    accounts: HashMap<String, AccountInfo>,
    blocks: HashMap<[u8; 32], Vec<u8>>,
}

impl NullStore {
    pub fn new() -> Self {
        Self {
            accounts: HashMap::new(),
            blocks: HashMap::new(),
        }
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
            .get(address.as_str())
            .cloned()
            .ok_or_else(|| StoreError::NotFound(address.to_string()))
    }

    fn put_account(&self, _info: &AccountInfo) -> Result<(), StoreError> {
        // NullStore is immutable after construction for simplicity.
        // A real test store would use interior mutability.
        todo!("use RefCell for interior mutability in tests")
    }

    fn exists(&self, address: &WalletAddress) -> Result<bool, StoreError> {
        Ok(self.accounts.contains_key(address.as_str()))
    }

    fn account_count(&self) -> Result<u64, StoreError> {
        Ok(self.accounts.len() as u64)
    }

    fn iter_accounts(&self) -> Result<Vec<AccountInfo>, StoreError> {
        Ok(self.accounts.values().cloned().collect())
    }

    fn iter_verified_accounts(&self) -> Result<Vec<AccountInfo>, StoreError> {
        Ok(self.accounts.values()
            .filter(|a| a.state.can_transact())
            .cloned()
            .collect())
    }
}

impl BlockStore for NullStore {
    fn put_block(&self, _hash: &BlockHash, _block_bytes: &[u8]) -> Result<(), StoreError> {
        todo!("use RefCell for interior mutability in tests")
    }

    fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError> {
        self.blocks
            .get(hash.as_bytes())
            .cloned()
            .ok_or_else(|| StoreError::NotFound(format!("{:?}", hash)))
    }

    fn exists(&self, hash: &BlockHash) -> Result<bool, StoreError> {
        Ok(self.blocks.contains_key(hash.as_bytes()))
    }

    fn delete_block(&self, _hash: &BlockHash) -> Result<(), StoreError> {
        todo!("use RefCell for interior mutability in tests")
    }

    fn get_account_blocks(&self, _address: &WalletAddress) -> Result<Vec<BlockHash>, StoreError> {
        todo!("index blocks by account in test store")
    }

    fn block_count(&self) -> Result<u64, StoreError> {
        Ok(self.blocks.len() as u64)
    }
}
