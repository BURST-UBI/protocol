//! LMDB implementation of TransactionStore.
//!
//! Per-account transaction index uses composite key
//! `account_bytes ++ tx_hash(32)` → empty. Listing all transactions for
//! an account collects a prefix range. Account-aware writes and deletes update
//! both tables in one transaction; the plain methods affect only stored bytes.

use crate::shared_env::SharedEnv;
use std::ops::Bound;

use heed::types::Bytes;
use heed::Database;

use burst_store::transaction::TransactionStore;
use burst_store::StoreError;
use burst_types::{TxHash, WalletAddress};

use crate::block::increment_prefix;
use crate::LmdbError;

pub struct LmdbTransactionStore {
    pub(crate) env: SharedEnv,
    pub(crate) transactions_db: Database<Bytes, Bytes>,
    pub(crate) account_txs_db: Database<Bytes, Bytes>,
}

/// Build composite key `account_bytes ++ tx_hash`.
fn account_tx_key(account: &WalletAddress, hash: &TxHash) -> Vec<u8> {
    let account_bytes = account.as_str().as_bytes();
    let mut key = Vec::with_capacity(account_bytes.len() + 32);
    key.extend_from_slice(account_bytes);
    key.extend_from_slice(hash.as_bytes());
    key
}

impl TransactionStore for LmdbTransactionStore {
    fn put_transaction(&self, hash: &TxHash, tx_bytes: &[u8]) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.transactions_db
            .put(&mut wtxn, hash.as_bytes(), tx_bytes)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn put_transaction_with_account(
        &self,
        hash: &TxHash,
        tx_bytes: &[u8],
        account: &WalletAddress,
    ) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.transactions_db
            .put(&mut wtxn, hash.as_bytes(), tx_bytes)
            .map_err(LmdbError::from)?;

        let index_key = account_tx_key(account, hash);
        self.account_txs_db
            .put(&mut wtxn, &index_key, &[])
            .map_err(LmdbError::from)?;

        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_transaction(&self, hash: &TxHash) -> Result<Vec<u8>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let val = self
            .transactions_db
            .get(&rtxn, hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?
            .ok_or_else(|| LmdbError::NotFound(format!("transaction {:?}", hash)))?;
        Ok(val.to_vec())
    }

    fn exists(&self, hash: &TxHash) -> Result<bool, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let exists = self
            .transactions_db
            .get(&rtxn, hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?
            .is_some();
        Ok(exists)
    }

    fn get_account_transactions(&self, address: &WalletAddress) -> Result<Vec<TxHash>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let prefix = address.as_str().as_bytes();
        let mut upper = prefix.to_vec();
        increment_prefix(&mut upper);

        let bounds = (Bound::Included(prefix), Bound::Excluded(upper.as_slice()));
        let iter = self
            .account_txs_db
            .range(&rtxn, &bounds)
            .map_err(LmdbError::from)?;
        let mut hashes = Vec::new();
        let account_len = prefix.len();
        for result in iter {
            let (key, _) = result.map_err(LmdbError::from)?;
            if key.len() == account_len + 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&key[account_len..]);
                hashes.push(TxHash::new(arr));
            }
        }
        Ok(hashes)
    }

    fn delete_transaction(&self, hash: &TxHash) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.transactions_db
            .delete(&mut wtxn, hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn delete_transaction_with_account(
        &self,
        hash: &TxHash,
        account: &WalletAddress,
    ) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.transactions_db
            .delete(&mut wtxn, hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?;
        let index_key = account_tx_key(account, hash);
        self.account_txs_db
            .delete(&mut wtxn, &index_key)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }
}
