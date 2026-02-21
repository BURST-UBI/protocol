//! LMDB implementation of AccountStore — binary serialized, byte-keyed.
//!
//! Maintains a `verified_count` counter in `meta_db` so that
//! `verified_account_count()` is O(1) instead of a full table scan.

use std::sync::Arc;

use heed::types::Bytes;
use heed::{Database, Env};

use burst_store::account::{AccountInfo, AccountStore};
use burst_store::StoreError;
use burst_types::WalletAddress;

use crate::LmdbError;

const VERIFIED_COUNT_KEY: &[u8] = b"verified_count";

pub struct LmdbAccountStore {
    pub(crate) env: Arc<Env>,
    pub(crate) accounts_db: Database<Bytes, Bytes>,
    pub(crate) meta_db: Database<Bytes, Bytes>,
}

impl LmdbAccountStore {
    /// Read the current verified-account counter from meta_db.
    fn read_verified_count(&self, txn: &heed::RoTxn<'_>) -> u64 {
        self.meta_db
            .get(txn, VERIFIED_COUNT_KEY)
            .ok()
            .flatten()
            .and_then(|b| b.try_into().ok().map(u64::from_be_bytes))
            .unwrap_or(0)
    }

    /// Read the current verified-account counter within a write transaction.
    fn read_verified_count_rw(&self, txn: &heed::RwTxn<'_>) -> u64 {
        self.meta_db
            .get(txn, VERIFIED_COUNT_KEY)
            .ok()
            .flatten()
            .and_then(|b| b.try_into().ok().map(u64::from_be_bytes))
            .unwrap_or(0)
    }
}

fn is_verified(info: &AccountInfo) -> bool {
    info.state == burst_types::WalletState::Verified
}

impl AccountStore for LmdbAccountStore {
    fn get_account(&self, address: &WalletAddress) -> Result<AccountInfo, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let val = self
            .accounts_db
            .get(&rtxn, address.as_str().as_bytes())
            .map_err(LmdbError::from)?
            .ok_or_else(|| LmdbError::NotFound(format!("account {}", address.as_str())))?;
        let info: AccountInfo = bincode::deserialize(val).map_err(LmdbError::from)?;
        Ok(info)
    }

    fn put_account(&self, info: &AccountInfo) -> Result<(), StoreError> {
        let bytes = bincode::serialize(info).map_err(LmdbError::from)?;
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;

        let was_verified = self
            .accounts_db
            .get(&wtxn, info.address.as_str().as_bytes())
            .map_err(LmdbError::from)?
            .and_then(|old| bincode::deserialize::<AccountInfo>(old).ok())
            .is_some_and(|old| is_verified(&old));

        self.accounts_db
            .put(&mut wtxn, info.address.as_str().as_bytes(), &bytes)
            .map_err(LmdbError::from)?;

        let now_verified = is_verified(info);
        if was_verified != now_verified {
            let count = self.read_verified_count_rw(&wtxn);
            let new_count = if now_verified {
                count.saturating_add(1)
            } else {
                count.saturating_sub(1)
            };
            self.meta_db
                .put(&mut wtxn, VERIFIED_COUNT_KEY, &new_count.to_be_bytes())
                .map_err(LmdbError::from)?;
        }

        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn exists(&self, address: &WalletAddress) -> Result<bool, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let exists = self
            .accounts_db
            .get(&rtxn, address.as_str().as_bytes())
            .map_err(LmdbError::from)?
            .is_some();
        Ok(exists)
    }

    fn account_count(&self) -> Result<u64, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let count = self.accounts_db.len(&rtxn).map_err(LmdbError::from)?;
        Ok(count)
    }

    fn iter_accounts(&self) -> Result<Vec<AccountInfo>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let mut accounts = Vec::new();
        let iter = self.accounts_db.iter(&rtxn).map_err(LmdbError::from)?;
        for result in iter {
            let (_key, val) = result.map_err(LmdbError::from)?;
            let info: AccountInfo = bincode::deserialize(val).map_err(LmdbError::from)?;
            accounts.push(info);
        }
        Ok(accounts)
    }

    fn iter_verified_accounts(&self) -> Result<Vec<AccountInfo>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let mut accounts = Vec::new();
        let iter = self.accounts_db.iter(&rtxn).map_err(LmdbError::from)?;
        for result in iter {
            let (_key, val) = result.map_err(LmdbError::from)?;
            let info: AccountInfo = bincode::deserialize(val).map_err(LmdbError::from)?;
            if is_verified(&info) {
                accounts.push(info);
            }
        }
        Ok(accounts)
    }

    fn verified_account_count(&self) -> Result<u64, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        Ok(self.read_verified_count(&rtxn))
    }

    fn iter_accounts_paged(
        &self,
        cursor: Option<&WalletAddress>,
        limit: usize,
    ) -> Result<Vec<AccountInfo>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let mut accounts = Vec::with_capacity(limit);

        match cursor {
            Some(addr) => {
                use std::ops::Bound;
                let key = addr.as_str().as_bytes();
                let bounds = (Bound::Excluded(key), Bound::<&[u8]>::Unbounded);
                let iter = self
                    .accounts_db
                    .range(&rtxn, &bounds)
                    .map_err(LmdbError::from)?;
                for result in iter {
                    if accounts.len() >= limit {
                        break;
                    }
                    let (_k, v) = result.map_err(LmdbError::from)?;
                    let info: AccountInfo = bincode::deserialize(v).map_err(LmdbError::from)?;
                    accounts.push(info);
                }
            }
            None => {
                let iter = self.accounts_db.iter(&rtxn).map_err(LmdbError::from)?;
                for result in iter {
                    if accounts.len() >= limit {
                        break;
                    }
                    let (_k, v) = result.map_err(LmdbError::from)?;
                    let info: AccountInfo = bincode::deserialize(v).map_err(LmdbError::from)?;
                    accounts.push(info);
                }
            }
        }

        Ok(accounts)
    }
}
