//! LMDB implementation of TransactionStore.

use burst_store::transaction::TransactionStore;
use burst_store::StoreError;
use burst_types::{TxHash, WalletAddress};

pub struct LmdbTransactionStore;

impl TransactionStore for LmdbTransactionStore {
    fn put_transaction(&self, _hash: &TxHash, _tx_bytes: &[u8]) -> Result<(), StoreError> { todo!() }
    fn get_transaction(&self, _hash: &TxHash) -> Result<Vec<u8>, StoreError> { todo!() }
    fn exists(&self, _hash: &TxHash) -> Result<bool, StoreError> { todo!() }
    fn get_account_transactions(&self, _address: &WalletAddress) -> Result<Vec<TxHash>, StoreError> { todo!() }
    fn delete_transaction(&self, _hash: &TxHash) -> Result<(), StoreError> { todo!() }
}
