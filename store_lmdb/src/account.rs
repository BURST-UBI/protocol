//! Account rows keyed by address, with a derived membership index and counter.
//! `verified_account_count()` reads one metadata value without scanning accounts.
//! Membership includes both Verified and Challenged wallet states.

use crate::shared_env::SharedEnv;

use heed::types::Bytes;
use heed::Database;

use burst_store::account::{AccountInfo, AccountStore};
use burst_store::StoreError;
use burst_types::WalletAddress;

use crate::LmdbError;

const VERIFIED_COUNT_KEY: &[u8] = b"verified_count";

pub struct LmdbAccountStore {
    pub(crate) env: SharedEnv,
    pub(crate) accounts_db: Database<Bytes, Bytes>,
    pub(crate) verified_accounts_db: Database<Bytes, Bytes>,
    pub(crate) meta_db: Database<Bytes, Bytes>,
}

impl LmdbAccountStore {
    /// Read electorate membership from the caller's multi-table snapshot.
    pub fn all_verified_addresses_txn(
        &self,
        txn: &crate::LmdbReadTransaction<'_>,
    ) -> Result<Vec<WalletAddress>, StoreError> {
        crate::verified_index::addresses(&txn.txn, self.verified_accounts_db)
    }

    /// Stream rows from one read snapshot, checking address/key agreement and
    /// confirmation_height <= block_count. Does not authenticate account history.
    pub fn account_entries<'a>(
        &'a self,
        txn: &'a crate::LmdbReadTransaction<'_>,
    ) -> Result<impl Iterator<Item = Result<AccountInfo, StoreError>> + 'a, StoreError> {
        let rows = self.accounts_db.iter(&txn.txn).map_err(LmdbError::from)?;
        Ok(rows.map(|row| {
            let (key, bytes) = row.map_err(LmdbError::from)?;
            decode_account_entry(key, bytes)
        }))
    }
}

pub(crate) fn decode_account_entry(key: &[u8], bytes: &[u8]) -> Result<AccountInfo, StoreError> {
    let account: AccountInfo = bincode::deserialize(bytes).map_err(LmdbError::from)?;
    if key != account.address.as_str().as_bytes()
        || account.confirmation_height > account.block_count
    {
        return Err(StoreError::Corruption(
            "invalid account index or confirmation height".into(),
        ));
    }
    Ok(account)
}

impl LmdbAccountStore {
    /// Read at most `limit` rows strictly after the cursor in this snapshot.
    /// Checks address/key agreement, but not confirmation height or chain history.
    pub fn accounts_page_txn(
        &self,
        txn: &crate::LmdbReadTransaction<'_>,
        cursor: Option<&WalletAddress>,
        limit: usize,
    ) -> Result<Vec<AccountInfo>, StoreError> {
        use std::ops::Bound;
        let lower = cursor.map_or(Bound::Unbounded, |address| {
            Bound::Excluded(address.as_str().as_bytes())
        });
        let bounds = (lower, Bound::<&[u8]>::Unbounded);
        let mut accounts = Vec::new();
        for entry in self
            .accounts_db
            .range(&txn.txn, &bounds)
            .map_err(LmdbError::from)?
            .take(limit)
        {
            let (key, bytes) = entry.map_err(LmdbError::from)?;
            let account: AccountInfo = bincode::deserialize(bytes).map_err(LmdbError::from)?;
            if key != account.address.as_str().as_bytes() {
                return Err(StoreError::Corruption(
                    "account address index mismatch".into(),
                ));
            }
            accounts.push(account);
        }
        Ok(accounts)
    }

    /// Collect all account values or fail if their count exceeds `limit`.
    /// Only deserializes values; it does not check keys or account invariants.
    /// History readers must reconstruct authority from signed blocks.
    pub fn all_accounts_bounded_txn(
        &self,
        txn: &crate::LmdbReadTransaction<'_>,
        limit: usize,
    ) -> Result<Vec<AccountInfo>, StoreError> {
        let mut accounts = Vec::new();
        for entry in self.accounts_db.iter(&txn.txn).map_err(LmdbError::from)? {
            let (_, bytes) = entry.map_err(LmdbError::from)?;
            if accounts.len() == limit {
                return Err(StoreError::Corruption(
                    "account inventory exceeds replay limit".into(),
                ));
            }
            accounts.push(bincode::deserialize(bytes).map_err(LmdbError::from)?);
        }
        Ok(accounts)
    }

    pub fn get_account_txn(
        &self,
        txn: &crate::LmdbReadTransaction<'_>,
        address: &WalletAddress,
    ) -> Result<AccountInfo, StoreError> {
        let bytes = self
            .accounts_db
            .get(&txn.txn, address.as_str().as_bytes())
            .map_err(LmdbError::from)?
            .ok_or_else(|| StoreError::NotFound(address.to_string()))?;
        bincode::deserialize(bytes)
            .map_err(LmdbError::from)
            .map_err(Into::into)
    }

    /// Read the current verified-account counter from meta_db.
    fn read_verified_count(&self, txn: &heed::RoTxn<'_>) -> Result<u64, StoreError> {
        match self
            .meta_db
            .get(txn, VERIFIED_COUNT_KEY)
            .map_err(LmdbError::from)?
        {
            Some(bytes) => bytes
                .try_into()
                .map(u64::from_be_bytes)
                .map_err(|_| StoreError::Corruption("invalid verified account count".into())),
            None => Err(StoreError::Corruption(
                "verified account count missing".into(),
            )),
        }
    }

    /// Read the current verified-account counter within a write transaction.
    fn read_verified_count_rw(&self, txn: &heed::RwTxn<'_>) -> Result<u64, StoreError> {
        self.read_verified_count(txn)
    }
}

fn is_verified(info: &AccountInfo) -> bool {
    info.state.is_verified()
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

    /// Commit the account, membership index and counter together. Preserves the
    /// larger confirmation height; callers must supply a compatible block count.
    fn put_account(&self, info: &AccountInfo) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        let old = self
            .accounts_db
            .get(&wtxn, info.address.as_str().as_bytes())
            .map_err(LmdbError::from)?
            .map(bincode::deserialize::<AccountInfo>)
            .transpose()
            .map_err(LmdbError::from)?;
        let was_verified = old.as_ref().is_some_and(is_verified);
        let mut info = info.clone();
        if let Some(old) = old {
            info.confirmation_height = info.confirmation_height.max(old.confirmation_height);
        }
        let bytes = bincode::serialize(&info).map_err(LmdbError::from)?;

        crate::verified_index::update(
            &mut wtxn,
            self.accounts_db,
            self.verified_accounts_db,
            info.address.as_str().as_bytes(),
            Some(&bytes),
        )?;

        self.accounts_db
            .put(&mut wtxn, info.address.as_str().as_bytes(), &bytes)
            .map_err(LmdbError::from)?;

        let now_verified = is_verified(&info);
        if was_verified != now_verified {
            let count = self.read_verified_count_rw(&wtxn)?;
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

    fn advance_confirmation_height(
        &self,
        address: &WalletAddress,
        height: u64,
    ) -> Result<(), StoreError> {
        let mut txn = self.env.write_txn().map_err(LmdbError::from)?;
        let key = address.as_str().as_bytes();
        let bytes = self
            .accounts_db
            .get(&txn, key)
            .map_err(LmdbError::from)?
            .ok_or_else(|| StoreError::NotFound(address.to_string()))?;
        let mut account: AccountInfo = bincode::deserialize(bytes).map_err(LmdbError::from)?;
        if height > account.block_count {
            return Err(StoreError::Backend(
                "confirmation exceeds chain height".into(),
            ));
        }
        account.confirmation_height = account.confirmation_height.max(height);
        self.accounts_db
            .put(
                &mut txn,
                key,
                &bincode::serialize(&account).map_err(LmdbError::from)?,
            )
            .map_err(LmdbError::from)?;
        txn.commit().map_err(LmdbError::from)?;
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

    fn all_accounts(&self) -> Result<Vec<AccountInfo>, StoreError> {
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

    fn visit_accounts(
        &self,
        visitor: &mut dyn FnMut(AccountInfo) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let txn = self.env.read_txn().map_err(LmdbError::from)?;
        for row in self.accounts_db.iter(&txn).map_err(LmdbError::from)? {
            let (key, bytes) = row.map_err(LmdbError::from)?;
            visitor(decode_account_entry(key, bytes)?)?;
        }
        Ok(())
    }

    fn all_verified_accounts(&self) -> Result<Vec<AccountInfo>, StoreError> {
        let txn = self.env.read_txn().map_err(LmdbError::from)?;
        crate::verified_index::addresses(&txn, self.verified_accounts_db)?
            .into_iter()
            .map(|address| {
                let key = address.as_str().as_bytes();
                let bytes = self
                    .accounts_db
                    .get(&txn, key)
                    .map_err(LmdbError::from)?
                    .ok_or_else(|| {
                        StoreError::Corruption("verified index references missing account".into())
                    })?;
                let account = decode_account_entry(key, bytes)?;
                if !account.state.is_verified() {
                    return Err(StoreError::Corruption(
                        "verified index references ineligible account".into(),
                    ));
                }
                Ok(account)
            })
            .collect()
    }

    fn all_verified_addresses(&self) -> Result<Vec<WalletAddress>, StoreError> {
        let txn = self.env.read_txn().map_err(LmdbError::from)?;
        crate::verified_index::addresses(&txn, self.verified_accounts_db)
    }

    fn verified_account_count(&self) -> Result<u64, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        self.read_verified_count(&rtxn)
    }

    fn accounts_page(
        &self,
        cursor: Option<&WalletAddress>,
        limit: usize,
    ) -> Result<Vec<AccountInfo>, StoreError> {
        let txn =
            crate::LmdbReadTransaction::from_ro(self.env.read_txn().map_err(LmdbError::from)?);
        self.accounts_page_txn(&txn, cursor, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::meta::MetaStore;
    use burst_types::{BlockHash, Timestamp, WalletState};

    fn account() -> AccountInfo {
        AccountInfo {
            address: WalletAddress::new("brst_verified_counter_fixture"),
            state: WalletState::Verified,
            verified_at: Some(Timestamp::EPOCH),
            head: BlockHash::ZERO,
            block_count: 0,
            confirmation_height: 0,
            representative: WalletAddress::new("brst_rep"),
            total_brn_burned: 0,
            total_brn_staked: 0,
            trst_balance: 0,
            expired_trst: 0,
            revoked_trst: 0,
            epoch: 0,
            verifier_opted_in_at: None,
            orv_evicted: false,
        }
    }

    #[test]
    fn electorate_index_tracks_trait_and_transactional_state_and_abort() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let accounts = store.account_store();
        let mut eligible = account();
        accounts.put_account(&eligible).unwrap();
        let outsider = AccountInfo::unopened(WalletAddress::new("brst_unverified"));
        accounts.put_account(&outsider).unwrap();
        assert_eq!(
            accounts.all_verified_addresses().unwrap(),
            vec![eligible.address.clone()]
        );
        eligible.state = WalletState::Revoked;
        {
            let mut batch = store.tx_begin_write().unwrap();
            batch.put_account_info(&eligible, false).unwrap();
            assert!(batch.verified_accounts().unwrap().is_empty());
        }
        assert_eq!(
            accounts.all_verified_addresses().unwrap(),
            vec![eligible.address.clone()]
        );
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_account_info(&eligible, false).unwrap();
        batch.commit().unwrap();
        assert!(accounts.all_verified_addresses().unwrap().is_empty());
        eligible.state = WalletState::Challenged;
        accounts.put_account(&eligible).unwrap();
        assert_eq!(
            accounts.all_verified_addresses().unwrap(),
            vec![eligible.address.clone()]
        );
        assert_eq!(accounts.all_verified_accounts().unwrap().len(), 1);
        drop(accounts);
        drop(store);
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        assert_eq!(
            store.tx_begin_write().unwrap().verified_accounts().unwrap(),
            vec![eligible.address]
        );
    }

    #[test]
    fn electorate_migration_rebuilds_multiple_batches_and_is_atomic_on_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            crate::LmdbStore::open(dir.path(), crate::REQUIRED_DATABASES, 16 << 20).unwrap();
        let accounts = store.account_store();
        let mut expected = Vec::new();
        for i in 0..600 {
            let mut row = account();
            row.address = WalletAddress::new(format!("brst_migration_{i:04}"));
            row.state = match i % 3 {
                0 => WalletState::Verified,
                1 => WalletState::Challenged,
                _ => WalletState::Revoked,
            };
            if row.state.is_verified() {
                expected.push(row.address.clone());
            }
            accounts.put_account(&row).unwrap();
        }
        let original = account();
        {
            let mut txn = accounts.env.write_txn().unwrap();
            accounts.verified_accounts_db.clear(&mut txn).unwrap();
            accounts
                .accounts_db
                .put(&mut txn, original.address.as_str().as_bytes(), b"bad")
                .unwrap();
            txn.commit().unwrap();
        }
        store
            .meta_store()
            .put_meta("schema_version", &6u32.to_le_bytes())
            .unwrap();
        assert!(crate::LmdbStore::open(dir.path(), crate::REQUIRED_DATABASES, 16 << 20).is_err());
        assert_eq!(store.meta_store().get_schema_version().unwrap(), 6);
        assert!(accounts.all_verified_addresses().unwrap().is_empty());
        {
            let mut txn = accounts.env.write_txn().unwrap();
            accounts
                .accounts_db
                .delete(&mut txn, original.address.as_str().as_bytes())
                .unwrap();
            txn.commit().unwrap();
        }
        drop(accounts);
        drop(store);
        let store =
            crate::LmdbStore::open(dir.path(), crate::REQUIRED_DATABASES, 16 << 20).unwrap();
        assert_eq!(
            store.meta_store().get_schema_version().unwrap(),
            crate::CURRENT_SCHEMA_VERSION
        );
        assert_eq!(
            store.account_store().all_verified_addresses().unwrap(),
            expected
        );
        assert_eq!(
            store.tx_begin_write().unwrap().verified_accounts().unwrap(),
            expected
        );
    }

    #[test]
    fn electorate_index_rejects_bad_markers_and_membership_disagreement() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            crate::LmdbStore::open(dir.path(), crate::REQUIRED_DATABASES, 16 << 20).unwrap();
        let accounts = store.account_store();
        let row = account();
        accounts.put_account(&row).unwrap();
        {
            let mut txn = accounts.env.write_txn().unwrap();
            accounts
                .verified_accounts_db
                .put(&mut txn, row.address.as_str().as_bytes(), &[2])
                .unwrap();
            txn.commit().unwrap();
        }
        assert!(accounts.all_verified_addresses().is_err());
        assert!(accounts.all_verified_accounts().is_err());
        assert!(accounts.put_account(&row).is_err());
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch.put_account_info(&row, true).is_err());
        assert!(batch.commit().is_err());
    }

    #[test]
    fn account_visitor_keeps_snapshot_and_propagates_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let accounts = store.account_store();
        for suffix in ["a", "b"] {
            accounts
                .put_account(&AccountInfo::unopened(WalletAddress::new(format!(
                    "brst_{suffix}"
                ))))
                .unwrap();
        }
        let mut seen = Vec::new();
        accounts
            .visit_accounts(&mut |a| {
                if seen.is_empty() {
                    accounts.put_account(&AccountInfo::unopened(WalletAddress::new("brst_c")))?;
                }
                seen.push(a.address);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            seen,
            vec![WalletAddress::new("brst_a"), WalletAddress::new("brst_b")]
        );
        assert_eq!(accounts.account_count().unwrap(), 3);
        let mut calls = 0;
        assert!(accounts
            .visit_accounts(&mut |_| {
                calls += 1;
                Err(StoreError::Corruption("visitor stop".into()))
            })
            .is_err());
        assert_eq!(calls, 1);
        let mut txn = accounts.env.write_txn().unwrap();
        accounts
            .accounts_db
            .put(&mut txn, b"brst_b", b"corrupt")
            .unwrap();
        txn.commit().unwrap();
        assert!(accounts.visit_accounts(&mut |_| Ok(())).is_err());
    }

    #[test]
    fn paged_account_reader_keeps_one_snapshot_and_an_exclusive_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let accounts = store.account_store();
        for suffix in ["a", "b", "c"] {
            let mut value = account();
            value.address = WalletAddress::new(format!("brst_{suffix}"));
            accounts.put_account(&value).unwrap();
        }
        let snapshot = store.tx_begin_read().unwrap();
        let first = accounts.accounts_page_txn(&snapshot, None, 1).unwrap();
        assert_eq!(first[0].address.as_str(), "brst_a");
        let mut inserted = account();
        inserted.address = WalletAddress::new("brst_ab");
        accounts.put_account(&inserted).unwrap();
        let streamed = accounts
            .account_entries(&snapshot)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            streamed
                .iter()
                .map(|row| row.address.as_str())
                .collect::<Vec<_>>(),
            vec!["brst_a", "brst_b", "brst_c"]
        );
        let rest = accounts
            .accounts_page_txn(&snapshot, Some(&first[0].address), 2)
            .unwrap();
        assert_eq!(
            rest.iter().map(|a| a.address.as_str()).collect::<Vec<_>>(),
            vec!["brst_b", "brst_c"]
        );
        assert!(accounts
            .accounts_page_txn(&snapshot, Some(&rest[1].address), 1)
            .unwrap()
            .is_empty());
        assert!(accounts
            .accounts_page_txn(&snapshot, None, 0)
            .unwrap()
            .is_empty());
        drop(snapshot);
        assert_eq!(
            accounts.accounts_page(Some(&first[0].address), 1).unwrap()[0].address,
            inserted.address
        );
    }

    #[test]
    fn challenge_keeps_human_in_electorate_until_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let mut account = account();
        store.account_store().put_account(&account).unwrap();
        account.state = WalletState::Challenged;
        store.account_store().put_account(&account).unwrap();
        assert_eq!(store.account_store().verified_account_count().unwrap(), 1);
        assert_eq!(
            store.account_store().all_verified_accounts().unwrap().len(),
            1
        );
        account.state = WalletState::Revoked;
        for commit in [false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            batch.put_account_info(&account, true).unwrap();
            assert_eq!(batch.verified_account_count().unwrap(), 0);
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
            }
            assert_eq!(
                store.account_store().verified_account_count().unwrap(),
                u64::from(!commit)
            );
        }
    }

    #[test]
    fn old_counter_is_rebuilt_but_corrupt_current_counter_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let mut account = account();
        account.state = WalletState::Challenged;
        store.account_store().put_account(&account).unwrap();
        store
            .meta_store()
            .put_meta("verified_count", &0u64.to_be_bytes())
            .unwrap();
        store
            .meta_store()
            .put_meta("verified_count_version", &[1])
            .unwrap();
        drop(store);
        let store = crate::LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        assert_eq!(store.account_store().verified_account_count().unwrap(), 1);
        store
            .meta_store()
            .put_meta("verified_count", &[99])
            .unwrap();
        assert!(store.account_store().verified_account_count().is_err());
        account.state = WalletState::Revoked;
        assert!(store.account_store().put_account(&account).is_err());
        assert_eq!(
            store
                .account_store()
                .get_account(&account.address)
                .unwrap()
                .state,
            WalletState::Challenged
        );
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch.put_account_info(&account, true).is_err());
        drop(batch);
        assert_eq!(
            store
                .account_store()
                .get_account(&account.address)
                .unwrap()
                .state,
            WalletState::Challenged
        );
    }
}
