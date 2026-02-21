//! LMDB implementation of BlockStore.
//!
//! Account block lists are stored via the `height_db` composite-key index
//! `(account_bytes ++ height_be)` → `block_hash`. There is no separate flat
//! list; `get_account_blocks` performs a prefix range-scan on `height_db`.

use std::ops::Bound;
use std::sync::Arc;

use heed::types::Bytes;
use heed::{Database, Env};

use burst_store::block::BlockStore;
use burst_store::StoreError;
use burst_types::{BlockHash, WalletAddress};

use crate::LmdbError;

pub struct LmdbBlockStore {
    pub(crate) env: Arc<Env>,
    pub(crate) blocks_db: Database<Bytes, Bytes>,
    /// Forward height index: `(account_bytes ++ height_be_bytes)` → `block_hash_bytes`.
    pub(crate) height_db: Database<Bytes, Bytes>,
    /// Reverse height index: `block_hash` → `height_be_bytes`.
    pub(crate) block_height_db: Database<Bytes, Bytes>,
}

/// Build the composite key `account_bytes ++ height_be` used in `height_db`.
fn height_key(account: &WalletAddress, height: u64) -> Vec<u8> {
    let acct = account.as_str().as_bytes();
    let mut key = Vec::with_capacity(acct.len() + 8);
    key.extend_from_slice(acct);
    key.extend_from_slice(&height.to_be_bytes());
    key
}

/// Find the next height for an account by scanning for the last entry in
/// `height_db` whose key starts with `account_bytes`.
fn next_height_rw(
    height_db: &Database<Bytes, Bytes>,
    txn: &heed::RwTxn<'_>,
    account: &WalletAddress,
) -> Result<u64, LmdbError> {
    let prefix = account.as_str().as_bytes();
    let mut upper = prefix.to_vec();
    increment_prefix(&mut upper);

    let bounds = (
        Bound::Included(prefix),
        Bound::Excluded(upper.as_slice()),
    );
    let mut iter = height_db.rev_range(txn, &bounds)?;
    match iter.next() {
        Some(Ok((key, _))) => {
            if key.len() >= 8 {
                let h = u64::from_be_bytes(key[key.len() - 8..].try_into().unwrap());
                Ok(h + 1)
            } else {
                Ok(1)
            }
        }
        _ => Ok(1),
    }
}

/// Increment a byte string to form the exclusive upper bound for a prefix scan.
pub(crate) fn increment_prefix(prefix: &mut Vec<u8>) {
    for byte in prefix.iter_mut().rev() {
        if *byte < 0xFF {
            *byte += 1;
            return;
        }
        *byte = 0;
    }
    prefix.push(0);
}

impl BlockStore for LmdbBlockStore {
    fn put_block(&self, hash: &BlockHash, block_bytes: &[u8]) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.blocks_db
            .put(&mut wtxn, hash.as_bytes(), block_bytes)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn put_block_with_account(
        &self,
        hash: &BlockHash,
        block_bytes: &[u8],
        account: &WalletAddress,
    ) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.blocks_db
            .put(&mut wtxn, hash.as_bytes(), block_bytes)
            .map_err(LmdbError::from)?;

        let height = next_height_rw(&self.height_db, &wtxn, account)?;

        let hk = height_key(account, height);
        self.height_db
            .put(&mut wtxn, &hk, hash.as_bytes())
            .map_err(LmdbError::from)?;

        self.block_height_db
            .put(&mut wtxn, hash.as_bytes(), &hk)
            .map_err(LmdbError::from)?;

        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_block(&self, hash: &BlockHash) -> Result<Vec<u8>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let val = self
            .blocks_db
            .get(&rtxn, hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?
            .ok_or_else(|| LmdbError::NotFound(format!("block {:?}", hash)))?;
        Ok(val.to_vec())
    }

    fn exists(&self, hash: &BlockHash) -> Result<bool, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let exists = self
            .blocks_db
            .get(&rtxn, hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?
            .is_some();
        Ok(exists)
    }

    fn delete_block(&self, hash: &BlockHash) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        if let Some(height_key_bytes) = self
            .block_height_db
            .get(&wtxn, hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?
        {
            let hk = height_key_bytes.to_vec();
            self.height_db
                .delete(&mut wtxn, &hk)
                .map_err(LmdbError::from)?;
            self.block_height_db
                .delete(&mut wtxn, hash.as_bytes().as_slice())
                .map_err(LmdbError::from)?;
        }
        self.blocks_db
            .delete(&mut wtxn, hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_account_blocks(&self, address: &WalletAddress) -> Result<Vec<BlockHash>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let prefix = address.as_str().as_bytes();
        let mut upper = prefix.to_vec();
        increment_prefix(&mut upper);

        let bounds = (
            Bound::Included(prefix),
            Bound::Excluded(upper.as_slice()),
        );
        let iter = self
            .height_db
            .range(&rtxn, &bounds)
            .map_err(LmdbError::from)?;
        let mut hashes = Vec::new();
        for result in iter {
            let (_key, val) = result.map_err(LmdbError::from)?;
            if val.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(val);
                hashes.push(BlockHash::new(arr));
            }
        }
        Ok(hashes)
    }

    fn block_count(&self) -> Result<u64, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let count = self.blocks_db.len(&rtxn).map_err(LmdbError::from)?;
        Ok(count)
    }

    fn block_at_height(
        &self,
        account: &WalletAddress,
        height: u64,
    ) -> Result<Option<BlockHash>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let key = height_key(account, height);
        match self.height_db.get(&rtxn, &key).map_err(LmdbError::from)? {
            Some(bytes) => {
                let arr: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| LmdbError::NotFound("invalid hash length in height_db".into()))?;
                Ok(Some(BlockHash::new(arr)))
            }
            None => Ok(None),
        }
    }

    fn height_of_block(&self, block_hash: &BlockHash) -> Result<Option<u64>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        match self
            .block_height_db
            .get(&rtxn, block_hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?
        {
            Some(bytes) if bytes.len() >= 8 => {
                let arr: [u8; 8] = bytes[bytes.len() - 8..].try_into().unwrap();
                Ok(Some(u64::from_be_bytes(arr)))
            }
            Some(_) => Ok(None),
            None => Ok(None),
        }
    }
}
