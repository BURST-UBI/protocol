//! Write batching — groups multiple store operations into a single LMDB write
//! transaction, amortising the cost of the fsync that each commit performs.
//!
//! # Usage
//!
//! ```ignore
//! let mut batch = env.write_batch()?;
//! batch.put_block(&hash, &block_bytes)?;
//! batch.put_frontier(&account, &head)?;
//! batch.put_account_info(&account_info, false)?;
//! batch.commit()?;
//! ```
//!
//! If the batch is dropped without calling [`WriteBatch::commit`], all
//! operations are rolled back (the underlying LMDB transaction is aborted).

use heed::RwTxn;

use burst_store::account::AccountInfo;
use burst_store::StoreError;
use burst_types::{BlockHash, Timestamp, TxHash, WalletAddress};

use crate::environment::LmdbEnvironment;
use crate::LmdbError;

/// A write batch that groups multiple store operations into a single LMDB
/// write transaction, amortising the cost of the fsync.
pub struct WriteBatch<'a> {
    txn: RwTxn<'a>,
    env: &'a LmdbEnvironment,
}

impl<'a> WriteBatch<'a> {
    /// Begin a new write batch.
    pub(crate) fn new(env: &'a LmdbEnvironment) -> Result<Self, StoreError> {
        let txn = env.env().write_txn().map_err(LmdbError::from)?;
        Ok(Self { txn, env })
    }

    // ── Block operations ────────────────────────────────────────────────

    /// Put a block into the batch (raw bytes, keyed by block hash).
    pub fn put_block(&mut self, hash: &BlockHash, block_bytes: &[u8]) -> Result<(), StoreError> {
        self.env
            .blocks_db
            .put(&mut self.txn, hash.as_bytes(), block_bytes)
            .map_err(LmdbError::from)?;
        Ok(())
    }

    /// Put a block and update the height indexes.
    ///
    /// `height` should be the block's sequence number in the account chain
    /// (1-based). Callers can derive this from `AccountInfo.block_count + 1`
    /// for existing accounts, or `1` for the open block. This avoids an
    /// expensive reverse range scan of `height_db` on every block.
    pub fn put_block_with_account(
        &mut self,
        hash: &BlockHash,
        block_bytes: &[u8],
        account: &WalletAddress,
        height: u64,
    ) -> Result<(), StoreError> {
        self.env
            .blocks_db
            .put(&mut self.txn, hash.as_bytes(), block_bytes)
            .map_err(LmdbError::from)?;

        let mut hk = account.as_str().as_bytes().to_vec();
        hk.extend_from_slice(&height.to_be_bytes());
        self.env
            .height_db
            .put(&mut self.txn, &hk, hash.as_bytes())
            .map_err(LmdbError::from)?;

        self.env
            .block_height_db
            .put(&mut self.txn, hash.as_bytes(), &hk)
            .map_err(LmdbError::from)?;

        Ok(())
    }

    /// Delete a block from the store.
    pub fn delete_block(&mut self, hash: &BlockHash) -> Result<(), StoreError> {
        self.env
            .blocks_db
            .delete(&mut self.txn, hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?;
        Ok(())
    }

    // ── Frontier operations ─────────────────────────────────────────────

    /// Put a frontier update into the batch.
    ///
    /// Mirrors [`LmdbFrontierStore::put_frontier`] — stores the 32-byte hash directly.
    pub fn put_frontier(
        &mut self,
        account: &WalletAddress,
        head: &BlockHash,
    ) -> Result<(), StoreError> {
        self.env
            .frontiers_db
            .put(&mut self.txn, account.as_str().as_bytes(), head.as_bytes())
            .map_err(LmdbError::from)?;
        Ok(())
    }

    /// Delete a frontier entry.
    pub fn delete_frontier(&mut self, account: &WalletAddress) -> Result<(), StoreError> {
        self.env
            .frontiers_db
            .delete(&mut self.txn, account.as_str().as_bytes())
            .map_err(LmdbError::from)?;
        Ok(())
    }

    // ── Account operations ──────────────────────────────────────────────

    /// Put an account info into the batch (pre-serialised bytes).
    pub fn put_account(
        &mut self,
        address: &WalletAddress,
        data: &[u8],
    ) -> Result<(), StoreError> {
        self.env
            .accounts_db
            .put(&mut self.txn, address.as_str().as_bytes(), data)
            .map_err(LmdbError::from)?;
        Ok(())
    }

    /// Put an [`AccountInfo`] into the batch, serialising it automatically.
    ///
    /// Maintains the `verified_count` counter in `meta_db` by detecting
    /// state transitions between Verified and non-Verified.
    ///
    /// `was_verified` indicates whether the account was previously in the
    /// `Verified` state, avoiding an extra LMDB read + `bincode::deserialize`
    /// of the old record. Callers already have this from `prev_account`.
    pub fn put_account_info(
        &mut self,
        info: &AccountInfo,
        was_verified: bool,
    ) -> Result<(), StoreError> {
        let bytes = bincode::serialize(info).map_err(LmdbError::from)?;
        self.env
            .accounts_db
            .put(&mut self.txn, info.address.as_str().as_bytes(), &bytes)
            .map_err(LmdbError::from)?;

        let now_verified = info.state == burst_types::WalletState::Verified;
        if was_verified != now_verified {
            let count = self
                .env
                .meta_db
                .get(&self.txn, b"verified_count")
                .map_err(LmdbError::from)?
                .and_then(|b| b.try_into().ok().map(u64::from_be_bytes))
                .unwrap_or(0);
            let new_count = if now_verified {
                count.saturating_add(1)
            } else {
                count.saturating_sub(1)
            };
            self.env
                .meta_db
                .put(&mut self.txn, b"verified_count", &new_count.to_be_bytes())
                .map_err(LmdbError::from)?;
        }
        Ok(())
    }

    // ── Transaction operations ──────────────────────────────────────────

    /// Put a transaction into the batch (raw bytes, keyed by tx hash).
    pub fn put_transaction(
        &mut self,
        hash: &TxHash,
        tx_bytes: &[u8],
    ) -> Result<(), StoreError> {
        self.env
            .transactions_db
            .put(&mut self.txn, hash.as_bytes(), tx_bytes)
            .map_err(LmdbError::from)?;
        Ok(())
    }

    /// Put a transaction and update the per-account index using composite key.
    pub fn put_transaction_with_account(
        &mut self,
        hash: &TxHash,
        tx_bytes: &[u8],
        account: &WalletAddress,
    ) -> Result<(), StoreError> {
        self.env
            .transactions_db
            .put(&mut self.txn, hash.as_bytes(), tx_bytes)
            .map_err(LmdbError::from)?;

        let mut ck = account.as_str().as_bytes().to_vec();
        ck.extend_from_slice(hash.as_bytes());
        self.env
            .account_txs_db
            .put(&mut self.txn, &ck, &[])
            .map_err(LmdbError::from)?;
        Ok(())
    }

    /// Delete a transaction from the store.
    pub fn delete_transaction(&mut self, hash: &TxHash) -> Result<(), StoreError> {
        self.env
            .transactions_db
            .delete(&mut self.txn, hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?;
        Ok(())
    }

    // ── Pending operations ──────────────────────────────────────────────

    /// Put a pending entry into the batch using binary composite key.
    pub fn put_pending(
        &mut self,
        destination: &WalletAddress,
        source_hash_bytes: &[u8; 32],
        data: &[u8],
    ) -> Result<(), StoreError> {
        let key = crate::pending::pending_key_raw(destination, source_hash_bytes);
        self.env
            .pending_db
            .put(&mut self.txn, &key, data)
            .map_err(LmdbError::from)?;
        Ok(())
    }

    /// Delete a pending entry using binary composite key.
    pub fn delete_pending(
        &mut self,
        destination: &WalletAddress,
        source_hash_bytes: &[u8; 32],
    ) -> Result<(), StoreError> {
        let key = crate::pending::pending_key_raw(destination, source_hash_bytes);
        self.env
            .pending_db
            .delete(&mut self.txn, &key)
            .map_err(LmdbError::from)?;
        Ok(())
    }

    // ── Meta operations ─────────────────────────────────────────────────

    /// Put a meta key/value pair into the batch.
    pub fn put_meta(&mut self, key: &str, value: &[u8]) -> Result<(), StoreError> {
        self.env
            .meta_db
            .put(&mut self.txn, key.as_bytes(), value)
            .map_err(LmdbError::from)?;
        Ok(())
    }

    // ── TRST index operations ─────────────────────────────────────────

    /// Record a TRST origin index entry and its reverse mapping.
    pub fn put_origin_index(
        &mut self,
        origin_hash: &TxHash,
        tx_hash: &TxHash,
    ) -> Result<(), StoreError> {
        let mut key = [0u8; 64];
        key[..32].copy_from_slice(origin_hash.as_bytes());
        key[32..].copy_from_slice(tx_hash.as_bytes());
        self.env
            .trst_origin_db
            .put(&mut self.txn, &key[..], &[])
            .map_err(LmdbError::from)?;

        let mut rev_val = match self
            .env
            .trst_reverse_db
            .get(&self.txn, tx_hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?
        {
            Some(bytes) => bytes.to_vec(),
            None => vec![0u8; 40],
        };
        if rev_val.len() >= 32 {
            rev_val[..32].copy_from_slice(origin_hash.as_bytes());
        }
        self.env
            .trst_reverse_db
            .put(&mut self.txn, tx_hash.as_bytes().as_slice(), &rev_val)
            .map_err(LmdbError::from)?;
        Ok(())
    }

    /// Record a TRST expiry index entry and update the reverse mapping.
    pub fn put_expiry_index(
        &mut self,
        expiry: Timestamp,
        tx_hash: &TxHash,
    ) -> Result<(), StoreError> {
        let mut key = [0u8; 40];
        key[..8].copy_from_slice(&expiry.as_secs().to_be_bytes());
        key[8..].copy_from_slice(tx_hash.as_bytes());
        self.env
            .trst_expiry_db
            .put(&mut self.txn, &key[..], &[])
            .map_err(LmdbError::from)?;

        let mut rev_val = match self
            .env
            .trst_reverse_db
            .get(&self.txn, tx_hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?
        {
            Some(bytes) => bytes.to_vec(),
            None => vec![0u8; 40],
        };
        if rev_val.len() >= 40 {
            rev_val[32..40].copy_from_slice(&expiry.as_secs().to_be_bytes());
        }
        self.env
            .trst_reverse_db
            .put(&mut self.txn, tx_hash.as_bytes().as_slice(), &rev_val)
            .map_err(LmdbError::from)?;
        Ok(())
    }

    // ── Commit / rollback ───────────────────────────────────────────────

    /// Commit all batched operations in a single write transaction.
    ///
    /// This is the only fsync in the entire batch.
    pub fn commit(self) -> Result<(), StoreError> {
        self.txn.commit().map_err(LmdbError::from)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LmdbEnvironment;
    use burst_store::block::BlockStore;
    use burst_store::frontier::FrontierStore;

    /// Helper: open a temporary LMDB environment.
    fn temp_env() -> (tempfile::TempDir, LmdbEnvironment) {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let env = LmdbEnvironment::open(dir.path(), 30, 10 * 1024 * 1024)
            .expect("failed to open env");
        (dir, env)
    }

    #[test]
    fn batch_put_block_and_frontier_committed() {
        let (_dir, env) = temp_env();

        let hash = BlockHash::new([1u8; 32]);
        let account = WalletAddress::new(
            "brst_1genesis1ive1111111111111111111111111111111111111111111111111111111",
        );
        let block_bytes = b"fake-block-data";

        // Write via batch
        let mut batch = env.write_batch().expect("write_batch");
        batch.put_block(&hash, block_bytes).expect("put_block");
        batch.put_frontier(&account, &hash).expect("put_frontier");
        batch.commit().expect("commit");

        // Verify block is readable
        let block_store = env.block_store();
        let stored = block_store.get_block(&hash).expect("get_block");
        assert_eq!(stored, block_bytes);

        // Verify frontier is readable
        let frontier_store = env.frontier_store();
        let head = frontier_store.get_frontier(&account).expect("get_frontier");
        assert_eq!(head, hash);
    }

    #[test]
    fn dropped_batch_does_not_persist() {
        let (_dir, env) = temp_env();

        let hash = BlockHash::new([2u8; 32]);
        let block_bytes = b"should-not-persist";

        // Start a batch but drop it without committing
        {
            let mut batch = env.write_batch().expect("write_batch");
            batch.put_block(&hash, block_bytes).expect("put_block");
            // batch is dropped here — implicit rollback
        }

        // Block should NOT be in the store
        let block_store = env.block_store();
        let result = block_store.get_block(&hash);
        assert!(result.is_err(), "dropped batch should not persist");
    }

    #[test]
    fn batch_multiple_blocks() {
        let (_dir, env) = temp_env();

        let mut batch = env.write_batch().expect("write_batch");

        let hashes: Vec<BlockHash> = (0..10)
            .map(|i| {
                let mut bytes = [0u8; 32];
                bytes[0] = i;
                BlockHash::new(bytes)
            })
            .collect();

        for (i, hash) in hashes.iter().enumerate() {
            let data = format!("block-{i}");
            batch
                .put_block(hash, data.as_bytes())
                .expect("put_block");
        }

        batch.commit().expect("commit");

        // Verify all blocks are readable
        let block_store = env.block_store();
        for (i, hash) in hashes.iter().enumerate() {
            let stored = block_store.get_block(hash).expect("get_block");
            assert_eq!(stored, format!("block-{i}").as_bytes());
        }
    }

    #[test]
    fn batch_put_account() {
        let (_dir, env) = temp_env();

        let address = WalletAddress::new(
            "brst_1genesis1ive1111111111111111111111111111111111111111111111111111111",
        );
        let data = b"account-info-bytes";

        let mut batch = env.write_batch().expect("write_batch");
        batch.put_account(&address, data).expect("put_account");
        batch.commit().expect("commit");

        // Verify via a raw read transaction
        let rtxn = env.env().read_txn().expect("read_txn");
        let stored = env
            .accounts_db
            .get(&rtxn, address.as_str().as_bytes())
            .expect("get")
            .expect("account should exist");
        assert_eq!(stored, data);
    }

    #[test]
    fn batch_put_account_info() {
        let (_dir, env) = temp_env();

        let info = AccountInfo {
            address: WalletAddress::new(
                "brst_1genesis1ive1111111111111111111111111111111111111111111111111111111",
            ),
            state: burst_types::WalletState::Unverified,
            verified_at: None,
            head: BlockHash::new([0u8; 32]),
            block_count: 1,
            confirmation_height: 0,
            representative: WalletAddress::new(
                "brst_1genesis1ive1111111111111111111111111111111111111111111111111111111",
            ),
            total_brn_burned: 0,
            trst_balance: 0,
            total_brn_staked: 0,
            expired_trst: 0,
            revoked_trst: 0,
            epoch: 0,
        };

        let mut batch = env.write_batch().expect("write_batch");
        batch.put_account_info(&info, false).expect("put_account_info");
        batch.commit().expect("commit");

        // Verify via the account store
        use burst_store::account::AccountStore;
        let store = env.account_store();
        let loaded = store.get_account(&info.address).expect("get_account");
        assert_eq!(loaded.address, info.address);
        assert_eq!(loaded.block_count, 1);
    }

    #[test]
    fn batch_put_pending() {
        let (_dir, env) = temp_env();

        let dest = WalletAddress::new(
            "brst_1genesis1ive1111111111111111111111111111111111111111111111111111111",
        );
        let source_hash = [0xABu8; 32];
        let data = b"pending-info-bytes";

        let mut batch = env.write_batch().expect("write_batch");
        batch
            .put_pending(&dest, &source_hash, data)
            .expect("put_pending");
        batch.commit().expect("commit");

        // Verify via raw read using the same binary composite key
        let key = crate::pending::pending_key_raw(&dest, &source_hash);
        let rtxn = env.env().read_txn().expect("read_txn");
        let stored = env
            .pending_db
            .get(&rtxn, &key)
            .expect("get")
            .expect("pending should exist");
        assert_eq!(stored, data);
    }

    #[test]
    fn batch_delete_block() {
        let (_dir, env) = temp_env();

        let hash = BlockHash::new([3u8; 32]);
        let block_bytes = b"to-be-deleted";

        // First commit the block
        let mut batch = env.write_batch().expect("write_batch");
        batch.put_block(&hash, block_bytes).expect("put_block");
        batch.commit().expect("commit");

        // Delete in a new batch
        let mut batch = env.write_batch().expect("write_batch");
        batch.delete_block(&hash).expect("delete_block");
        batch.commit().expect("commit");

        let block_store = env.block_store();
        assert!(block_store.get_block(&hash).is_err());
    }

    #[test]
    fn batch_put_meta() {
        let (_dir, env) = temp_env();

        let mut batch = env.write_batch().expect("write_batch");
        batch
            .put_meta("schema_version", b"42")
            .expect("put_meta");
        batch.commit().expect("commit");

        let rtxn = env.env().read_txn().expect("read_txn");
        let stored = env
            .meta_db
            .get(&rtxn, "schema_version".as_bytes())
            .expect("get")
            .expect("meta should exist");
        assert_eq!(stored, b"42");
    }
}
