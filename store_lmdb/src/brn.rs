use crate::shared_env::SharedEnv;
use burst_store::brn::{BrnSnapshot, BrnWalletEntries};
use burst_store::{BrnStore, StoreError};
use burst_types::WalletAddress;
use heed::{types::Bytes, Database};

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
    fn get_meta(&self, key: &[u8]) -> Result<Option<&[u8]>, StoreError> {
        self.meta_db
            .get(&self.txn, key)
            .map_err(|e| StoreError::Backend(e.to_string()))
    }

    fn wallet_entries(&self) -> Result<BrnWalletEntries<'_>, StoreError> {
        let rows = self
            .wallets_db
            .iter(&self.txn)
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(Box::new(rows.map(|row| {
            let (key, bytes) = row.map_err(|e| StoreError::Backend(e.to_string()))?;
            Ok((decode_wallet_address(key)?, bytes))
        })))
    }
}

impl LmdbBrnStore {
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
            Ok(Some(bytes)) => Ok(Some(bytes.to_vec())),
            Ok(None) => Ok(None),
            Err(e) => Err(StoreError::Backend(e.to_string())),
        }
    }

    fn put_wallet_state(&self, address: &WalletAddress, state: &[u8]) -> Result<(), StoreError> {
        let mut txn = self
            .env
            .write_txn()
            .map_err(|e| StoreError::Backend(e.to_string()))?;
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
            vec![(first.clone(), b"before".as_slice())]
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
                (first.clone(), b"after".as_slice()),
                (second.clone(), b"after".as_slice())
            ]
        );
        drop(snapshot);
        assert_eq!(
            brn.iter_wallet_states().unwrap(),
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
            assert!(brn.iter_wallet_states().is_err());
            let mut txn = brn.env.write_txn().unwrap();
            brn.wallets_db.delete(&mut txn, key).unwrap();
            txn.commit().unwrap();
        }
    }
}
