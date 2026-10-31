//! Transaction storage trait.

use crate::StoreError;
use burst_types::{TxHash, WalletAddress};

/// Trait for transaction index storage.
pub trait TransactionStore {
    /// Store a transaction (serialized bytes keyed by hash).
    fn put_transaction(&self, hash: &TxHash, tx_bytes: &[u8]) -> Result<(), StoreError>;

    /// Store a transaction and update the per-account transaction index.
    fn put_transaction_with_account(
        &self,
        hash: &TxHash,
        tx_bytes: &[u8],
        account: &WalletAddress,
    ) -> Result<(), StoreError>;

    /// Retrieve a transaction by hash.
    fn get_transaction(&self, hash: &TxHash) -> Result<Vec<u8>, StoreError>;

    /// Check if a transaction exists.
    fn exists(&self, hash: &TxHash) -> Result<bool, StoreError>;

    /// Collect hashes explicitly indexed under this account. The store does not
    /// infer sender or recipient relationships from transaction bytes.
    fn get_account_transactions(&self, address: &WalletAddress) -> Result<Vec<TxHash>, StoreError>;

    /// Delete a transaction (for pruning expired/revoked TRST history).
    fn delete_transaction(&self, hash: &TxHash) -> Result<(), StoreError>;

    /// Delete a transaction, optionally maintaining its account index.
    /// The default ignores the account and deletes only the transaction bytes.
    /// Backends must override this method to remove both entries atomically.
    fn delete_transaction_with_account(
        &self,
        hash: &TxHash,
        _account: &WalletAddress,
    ) -> Result<(), StoreError> {
        self.delete_transaction(hash)
    }
}
