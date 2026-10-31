//! BRN storage with pinned snapshot reads and individually committed trait writes.
//! Record-backed wallets are decoded and re-encoded for the aggregate-byte API;
//! the engine loader can read their headers and child rows directly.
use crate::shared_env::SharedEnv;
use burst_store::brn::{BrnSnapshot, BrnWalletEntries, BrnMetaEntries, BrnWalletHeaders};
use burst_store::{BrnStore, StoreError};
use burst_types::WalletAddress;
use heed::{types::Bytes, Database};
pub(crate) mod records;

pub struct LmdbBrnStore {
    env: SharedEnv,
    wallets_db: Database<Bytes, Bytes>,
    meta_db: Database<Bytes, Bytes>,
}

struct LmdbBrnSnapshot<'a> {
    txn: heed::RoTxn<'a>,
    wallets_db: Database<Bytes, Bytes>,
    meta_db: Database<Bytes, Bytes>,
}

pub(crate) fn decode_wallet_address(key: &[u8]) -> Result<WalletAddress, StoreError> {
    let address = std::str::from_utf8(key).map_err(|e| StoreError::Corruption(e.to_string()))?;
    if !address.starts_with("brst_") {
        return Err(StoreError::Corruption("invalid BRN wallet key".into()));
    }
    Ok(WalletAddress::new(address))
}

impl BrnSnapshot for LmdbBrnSnapshot<'_> {
    fn wallet_record_headers(&self) -> Result<Option<BrnWalletHeaders<'_>>, StoreError> {
        if !records::validate_inventory(&self.txn, self.wallets_db, self.meta_db, usize::MAX)? {
            return Ok(None);
        }
        let rows = self.wallets_db.iter(&self.txn)
            .map_err(|error| StoreError::Backend(error.to_string()))?;
        Ok(Some(Box::new(rows.map(|row| {
            let (key, header) = row.map_err(|error| StoreError::Backend(error.to_string()))?;
            Ok((decode_wallet_address(key)?, header))
        }))))
    }

    fn get_meta(&self, key: &[u8]) -> Result<Option<&[u8]>, StoreError> {
        self.meta_db
            .get(&self.txn, key)
            .map_err(|e| StoreError::Backend(e.to_string()))
    }

    fn meta_entries(&self, prefix: &[u8]) -> Result<BrnMetaEntries<'_>, StoreError> {
        let rows = self.meta_db.prefix_iter(&self.txn, prefix)
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(Box::new(rows.map(|row| row.map_err(|e| StoreError::Backend(e.to_string())))))
    }

    fn wallet_entries(&self) -> Result<BrnWalletEntries<'_>, StoreError> {
        let normalized = records::validate_inventory(&self.txn, self.wallets_db, self.meta_db, usize::MAX)?;
        let rows = self
            .wallets_db
            .iter(&self.txn)
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(Box::new(rows.map(move |row| {
            let (key, bytes) = row.map_err(|e| StoreError::Backend(e.to_string()))?;
            let address = decode_wallet_address(key)?;
            let bytes = if normalized {
                let state = records::decode_wallet(&self.txn, self.meta_db, &address, Some(bytes), usize::MAX)?.0
                    .ok_or_else(|| StoreError::Corruption("missing BRN wallet header".into()))?;
                std::borrow::Cow::Owned(bincode::serialize(&state).map_err(|error| StoreError::Serialization(error.to_string()))?)
            } else { std::borrow::Cow::Borrowed(bytes) };
            Ok((address, bytes))
        })))
    }
}

impl LmdbBrnStore {
    /// Test presence in the supplied transaction without copying the stored value.
    pub fn contains_meta_txn(
        &self, txn: &crate::LmdbReadTransaction<'_>, key: &[u8],
    ) -> Result<bool, StoreError> {
        self.meta_db.get(&txn.txn, key).map(|value| value.is_some())
            .map_err(|error| StoreError::Backend(error.to_string()))
    }

    pub fn get_meta_txn(
        &self,
        txn: &crate::LmdbReadTransaction<'_>,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, StoreError> {
        self.meta_db.get(&txn.txn, key)
            .map(|bytes| bytes.map(<[u8]>::to_vec))
            .map_err(|error| StoreError::Backend(error.to_string()))
    }

    pub(crate) fn new(
        env: SharedEnv,
        wallets_db: Database<Bytes, Bytes>,
        meta_db: Database<Bytes, Bytes>,
    ) -> Self {
        Self {
            env,
            wallets_db,
            meta_db,
        }
    }
}

impl BrnStore for LmdbBrnStore {
    fn read_snapshot(&self) -> Result<Box<dyn BrnSnapshot + '_>, StoreError> {
        Ok(Box::new(LmdbBrnSnapshot {
            txn: self
                .env
                .read_txn()
                .map_err(|e| StoreError::Backend(e.to_string()))?,
            wallets_db: self.wallets_db,
            meta_db: self.meta_db,
        }))
    }

    fn get_wallet_state(&self, address: &WalletAddress) -> Result<Option<Vec<u8>>, StoreError> {
        let txn = self
            .env
            .read_txn()
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        match self.wallets_db.get(&txn, address.as_str().as_bytes()) {
            Ok(bytes) => {
                if records::uses_records(&txn, self.meta_db)? {
                    records::decode_wallet(&txn, self.meta_db, address, bytes, usize::MAX)?.0
                        .map(|state| bincode::serialize(&state).map_err(|error| StoreError::Serialization(error.to_string()))).transpose()
                } else { Ok(bytes.map(<[u8]>::to_vec)) }
            }
            Err(e) => Err(StoreError::Backend(e.to_string())),
        }
    }

    fn put_wallet_state(&self, address: &WalletAddress, state: &[u8]) -> Result<(), StoreError> {
        let mut txn = self
            .env
            .write_txn()
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        if records::uses_records(&txn, self.meta_db)? {
            let state = burst_brn::BrnWalletState::from_persisted_bytes(state)
                .map_err(|error| StoreError::Serialization(error.to_string()))?;
            records::replace_wallet(&mut txn, self.wallets_db, self.meta_db, address, Some(&state))?;
            txn.commit().map_err(|error| StoreError::Backend(error.to_string()))?;
            return Ok(());
        }
        self.wallets_db
            .put(&mut txn, address.as_str().as_bytes(), state)
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        txn.commit()
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(())
    }

    fn delete_wallet_state(&self, address: &WalletAddress) -> Result<(), StoreError> {
        let mut txn = self
            .env
            .write_txn()
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        if records::uses_records(&txn, self.meta_db)? {
            records::replace_wallet(&mut txn, self.wallets_db, self.meta_db, address, None)?;
            txn.commit().map_err(|error| StoreError::Backend(error.to_string()))?;
            return Ok(());
        }
        self.wallets_db
            .delete(&mut txn, address.as_str().as_bytes())
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        txn.commit()
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(())
    }

    fn get_meta(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        let txn = self
            .env
            .read_txn()
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        match self.meta_db.get(&txn, key) {
            Ok(Some(bytes)) => Ok(Some(bytes.to_vec())),
            Ok(None) => Ok(None),
            Err(e) => Err(StoreError::Backend(e.to_string())),
        }
    }

    fn put_meta(&self, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        let mut txn = self
            .env
            .write_txn()
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        self.meta_db
            .put(&mut txn, key, value)
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        txn.commit()
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LmdbStore;

    #[test]
    fn engine_loader_uses_pinned_records_without_canonical_wallet_round_trip() {
        use burst_brn::{BrnEngine, BrnWalletState, RateHistory};
        use burst_types::{BlockHash, Timestamp};
        use std::cell::Cell;
        struct View<'a> {
            inner: &'a dyn BrnSnapshot,
            normalized: bool,
            canonical_calls: Cell<usize>,
        }
        impl BrnSnapshot for View<'_> {
            fn get_meta(&self, key: &[u8]) -> Result<Option<&[u8]>, StoreError> { self.inner.get_meta(key) }
            fn meta_entries(&self, prefix: &[u8]) -> Result<BrnMetaEntries<'_>, StoreError> { self.inner.meta_entries(prefix) }
            fn wallet_record_headers(&self) -> Result<Option<BrnWalletHeaders<'_>>, StoreError> { self.inner.wallet_record_headers() }
            fn wallet_entries(&self) -> Result<BrnWalletEntries<'_>, StoreError> {
                assert!(!self.normalized, "engine reentered the canonical encoding adapter");
                self.canonical_calls.set(self.canonical_calls.get() + 1);
                self.inner.wallet_entries()
            }
        }
        for normalized in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
            let address = WalletAddress::new("brst_direct_records");
            let mut wallet = BrnWalletState::new(Timestamp::EPOCH);
            for id in 1u64..=1024 {
                let mut hash = [0; 32];
                hash[..8].copy_from_slice(&id.to_be_bytes());
                wallet.record_release(BlockHash::new(hash), id as u128,
                    &[BlockHash::new(hash), BlockHash::new([99; 32])], Timestamp::new(10)).unwrap();
            }
            wallet.paused_intervals.push((Timestamp::new(1), Timestamp::new(2)));
            let mut batch = store.tx_begin_write().unwrap();
            if normalized { assert!(batch.initialize_brn_wallet_records_if_empty().unwrap()); }
            batch.put_brn_meta(b"next_stake_id", &7u64.to_be_bytes()).unwrap();
            batch.persist_brn_rate_history(&RateHistory::new(13, Timestamp::EPOCH)).unwrap();
            batch.put_brn_wallet_state(&address, &wallet).unwrap();
            batch.commit().unwrap();
            let brn = store.brn_store();
            let snapshot = brn.read_snapshot().unwrap();
            let view = View { inner: snapshot.as_ref(), normalized, canonical_calls: Cell::new(0) };
            let before = BrnEngine::load_from_snapshot(&view).unwrap();
            assert_eq!(before.get_wallet(&address), Some(&wallet));
            assert_eq!(before.next_stake_id(), 7);
            assert_eq!(view.canonical_calls.get(), usize::from(!normalized));
            let mut changed = wallet.clone();
            changed.total_burned = 9;
            let mut batch = store.tx_begin_write().unwrap();
            batch.put_brn_wallet_state(&address, &changed).unwrap();
            batch.put_brn_meta(b"next_stake_id", &8u64.to_be_bytes()).unwrap();
            batch.commit().unwrap();
            let pinned = BrnEngine::load_from_snapshot(&view).unwrap();
            assert_eq!(pinned.get_wallet(&address), Some(&wallet));
            assert_eq!(pinned.next_stake_id(), 7);
            assert_eq!(snapshot.wallet_entries().unwrap().next().unwrap().unwrap().1.as_ref(),
                bincode::serialize(&wallet).unwrap());
            drop(snapshot);
            let current = BrnEngine::load_from_store(&brn).unwrap();
            assert_eq!(current.get_wallet(&address), Some(&changed));
            assert_eq!(current.next_stake_id(), 8);
            for key in [b"next_stake_id".as_slice(), burst_brn::rate_storage::HEADER] {
                let mut batch = store.tx_begin_write().unwrap();
                batch.delete_brn_meta(key).unwrap();
                batch.commit().unwrap();
                assert!(BrnEngine::load_from_store(&brn).is_err());
                let mut batch = store.tx_begin_write().unwrap();
                batch.put_brn_meta(b"next_stake_id", &8u64.to_be_bytes()).unwrap();
                batch.put_brn_meta(burst_brn::rate_storage::HEADER, &1u64.to_be_bytes()).unwrap();
                batch.commit().unwrap();
            }
            if normalized {
                let mut batch = store.tx_begin_write().unwrap();
                batch.put_checkpoint_brn_wallet_header(&address, b"bad").unwrap();
                batch.commit().unwrap();
                assert!(BrnEngine::load_from_store(&brn).is_err());
            }
        }
    }

    #[test]
    fn brn_snapshot_keeps_metadata_and_borrowed_rows_in_one_read_view() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let brn = store.brn_store();
        let first = WalletAddress::new("brst_first");
        let second = WalletAddress::new("brst_second");
        let write = |version: &[u8], extra: bool| {
            let mut txn = brn.env.write_txn().unwrap();
            brn.meta_db
                .put(&mut txn, b"next_stake_id", version)
                .unwrap();
            brn.meta_db.put(&mut txn, b"rate_history", version).unwrap();
            brn.wallets_db
                .put(&mut txn, first.as_str().as_bytes(), version)
                .unwrap();
            if extra {
                brn.wallets_db
                    .put(&mut txn, second.as_str().as_bytes(), version)
                    .unwrap();
            }
            txn.commit().unwrap();
        };
        write(b"before", false);
        let snapshot = brn.read_snapshot().unwrap();
        let id = snapshot.get_meta(b"next_stake_id").unwrap().unwrap();
        write(b"after", true);
        assert_eq!(id, b"before");
        assert_eq!(
            snapshot.get_meta(b"rate_history").unwrap(),
            Some(b"before".as_slice())
        );
        assert_eq!(snapshot.get_meta(b"missing").unwrap(), None);
        assert_eq!(
            snapshot
                .wallet_entries()
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            vec![(first.clone(), std::borrow::Cow::Borrowed(b"before".as_slice()))]
        );
        drop(snapshot);
        let snapshot = brn.read_snapshot().unwrap();
        assert_eq!(
            snapshot.get_meta(b"next_stake_id").unwrap(),
            Some(b"after".as_slice())
        );
        assert_eq!(
            snapshot.get_meta(b"rate_history").unwrap(),
            Some(b"after".as_slice())
        );
        assert_eq!(
            snapshot
                .wallet_entries()
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            vec![
                (first.clone(), std::borrow::Cow::Borrowed(b"after".as_slice())),
                (second.clone(), std::borrow::Cow::Borrowed(b"after".as_slice()))
            ]
        );
        drop(snapshot);
        assert_eq!(
            brn.all_wallet_states().unwrap(),
            vec![(first, b"after".to_vec()), (second, b"after".to_vec())]
        );
    }

    #[test]
    fn brn_snapshot_reports_malformed_keys_without_panicking() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let brn = store.brn_store();
        assert!(brn
            .read_snapshot()
            .unwrap()
            .wallet_entries()
            .unwrap()
            .next()
            .is_none());
        for key in [b"invalid".as_slice(), &[255]] {
            let mut txn = brn.env.write_txn().unwrap();
            brn.wallets_db.put(&mut txn, key, b"wallet").unwrap();
            txn.commit().unwrap();
            {
                let snapshot = brn.read_snapshot().unwrap();
                let error = snapshot
                    .wallet_entries()
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap_err();
                assert!(matches!(error, StoreError::Corruption(_)));
            }
            assert!(brn.all_wallet_states().is_err());
            let mut txn = brn.env.write_txn().unwrap();
            brn.wallets_db.delete(&mut txn, key).unwrap();
            txn.commit().unwrap();
        }
    }
}
