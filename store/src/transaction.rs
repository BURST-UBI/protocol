//! Transaction storage trait.

use crate::StoreError;
use burst_types::{TxHash, WalletAddress};

/// Trait for transaction index storage.
pub trait TransactionStore {
    /// Store a transaction (serialized bytes keyed by hash).
    fn put_transaction(&self, hash: &TxHash, tx_bytes: &[u8]) -> Result<(), StoreError>;

    /// Retrieve a transaction by hash.
    fn get_transaction(&self, hash: &TxHash) -> Result<Vec<u8>, StoreError>;

    /// Check if a transaction exists.
    fn exists(&self, hash: &TxHash) -> Result<bool, StoreError>;

    /// Get all transaction hashes involving an account (as sender or receiver).
    fn get_account_transactions(&self, address: &WalletAddress) -> Result<Vec<TxHash>, StoreError>;

    /// Delete a transaction (for pruning expired/revoked TRST history).
    fn delete_transaction(&self, hash: &TxHash) -> Result<(), StoreError>;
}
