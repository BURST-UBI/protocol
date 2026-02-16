//! LMDB implementation of AccountStore.

use burst_store::account::{AccountInfo, AccountStore};
use burst_store::StoreError;
use burst_types::WalletAddress;

pub struct LmdbAccountStore;

impl AccountStore for LmdbAccountStore {
    fn get_account(&self, _address: &WalletAddress) -> Result<AccountInfo, StoreError> {
        todo!()
    }

    fn put_account(&self, _info: &AccountInfo) -> Result<(), StoreError> {
        todo!()
    }

    fn exists(&self, _address: &WalletAddress) -> Result<bool, StoreError> {
        todo!()
    }

    fn account_count(&self) -> Result<u64, StoreError> {
        todo!()
    }

    fn iter_accounts(&self) -> Result<Vec<AccountInfo>, StoreError> {
        todo!()
    }

    fn iter_verified_accounts(&self) -> Result<Vec<AccountInfo>, StoreError> {
        todo!()
    }
}
