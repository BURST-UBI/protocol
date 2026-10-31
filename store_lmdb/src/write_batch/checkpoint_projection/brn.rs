use super::*;
use burst_brn::{wallet_states::WalletStates, wallet_storage};
use std::ops::Bound;

impl LmdbWriteTransaction<'_> {
    /// Replace wallet rows from caller-authenticated state, preserving the storage format.
    /// Scan old keys in batches of 256, then encode every supplied wallet. Old values
    /// are not decoded, and identical rows are not rewritten. Any error prevents commit.
    pub(super) fn reconcile_checkpoint_brn_wallets(
        &mut self, wallets: &WalletStates,
    ) -> Result<(), StoreError> {
        let result = (|| {
            if self.failed || self.undo.is_some() {
                return Err(invalid("BRN wallet projection requires a projection writer"));
            }
            let uses_row_storage = match self.env.brn_meta_db.get(&self.txn, wallet_storage::FORMAT_KEY)
                .map_err(LmdbError::from)? {
                Some([1]) => true,
                None => false,
                Some(_) => return Err(invalid("invalid BRN wallet record format")),
            };
            self.invalidate_body_changes();
            self.auxiliary_cache.get_mut().invalidate_projection();
            self.auxiliary_cache.get_mut().clear_brn(UndoTable::BrnWallets);
            self.retain_brn_projection_rows(UndoTable::BrnWallets, b"", |key| {
                crate::brn::decode_wallet_address(key).ok()
                    .is_some_and(|address| wallets.contains_key(&address))
            })?;
            self.retain_brn_projection_rows(UndoTable::BrnMeta, wallet_storage::ROW_PREFIX, |key| {
                if !uses_row_storage { return false; }
                let Ok((owner, relative)) = wallet_storage::split_row_key(key) else { return false; };
                relative != wallet_storage::HEADER
                    && crate::brn::decode_wallet_address(owner).ok()
                        .and_then(|owner| wallets.get(&owner))
                        .is_some_and(|state| wallet_storage::contains_row(state, relative))
            })?;
            for (address, state) in wallets {
                if uses_row_storage {
                    let prefix = wallet_storage::row_prefix(address)
                        .map_err(|error| StoreError::Serialization(error.to_string()))?;
                    let mut storage_error = None;
                    wallet_storage::visit_delta(None, Some(state), |relative, _, bytes| {
                        let bytes = bytes.expect("complete wallet projection emits only values");
                        let result = if relative == wallet_storage::HEADER {
                            self.put_raw(UndoTable::BrnWallets, address.as_str().as_bytes(), &bytes)
                        } else {
                            self.put_raw(UndoTable::BrnMeta, &[prefix.as_slice(), relative].concat(), &bytes)
                        };
                        result.map_err(|error| {
                            let message = error.to_string();
                            storage_error = Some(error);
                            burst_brn::BrnError::Other(message)
                        })
                    }).map_err(|error| storage_error.unwrap_or_else(|| StoreError::Serialization(error.to_string())))?;
                } else {
                    let bytes = bincode::serialize(state)
                        .map_err(|error| StoreError::Serialization(error.to_string()))?;
                    self.put_raw(UndoTable::BrnWallets, address.as_str().as_bytes(), &bytes)?;
                }
            }
            Ok(())
        })();
        if result.is_err() { self.failed = true; }
        result
    }

    fn retain_brn_projection_rows(
        &mut self, table: UndoTable, prefix: &[u8], keep: impl Fn(&[u8]) -> bool,
    ) -> Result<(), StoreError> {
        let db = table.database(self.env);
        let mut cursor: Option<Vec<u8>> = None;
        loop {
            let start = if prefix.is_empty() { Bound::Unbounded } else { Bound::Included(prefix) };
            let bounds = (cursor.as_deref().map_or(start, Bound::Excluded),
                Bound::<&[u8]>::Unbounded);
            let rows = db.range(&self.txn, &bounds).map_err(LmdbError::from)?
                .take_while(|row| match row { Ok((key, _)) => key.starts_with(prefix), Err(_) => true })
                .take(256).map(|row| {
                    let (key, _) = row.map_err(LmdbError::from)?;
                    Ok((key.to_vec(), keep(key)))
                }).collect::<Result<Vec<_>, StoreError>>()?;
            if rows.is_empty() { break; }
            cursor = rows.last().map(|(key, _)| key.clone());
            for (key, retained) in rows {
                if !retained { self.delete_raw(table, &key)?; }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_brn::BrnWalletState;
    use burst_store::BrnStore;
    use burst_types::Timestamp;

    fn contents(store: &crate::LmdbStore) -> Vec<Vec<(Vec<u8>, Vec<u8>)>> {
        let txn = store.env().read_txn().unwrap();
        [store.brn_wallets_db, store.brn_meta_db].into_iter().map(|db| {
            db.iter(&txn).unwrap().map(|row| {
                let (key, value) = row.unwrap(); (key.to_vec(), value.to_vec())
            }).collect()
        }).collect()
    }

    #[test]
    fn replacement_repairs_both_formats_atomically_and_preserves_unchanged_pages() {
        for uses_row_storage in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let store = crate::LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
            let address = |n| WalletAddress::new(format!("brst_projection_{n:04}"));
            let mut wallets = WalletStates::default();
            let mut batch = store.tx_begin_write().unwrap();
            if uses_row_storage { batch.put_brn_meta(wallet_storage::FORMAT_KEY, &[1]).unwrap(); }
            for n in 0..600 {
                let mut wallet = BrnWalletState::new(Timestamp::new(n + 1));
                wallet.paused_intervals.push((Timestamp::new(700), Timestamp::new(800)));
                wallet.record_unproven_release(BlockHash::new([7; 32]), 3, Timestamp::new(900)).unwrap();
                batch.put_brn_wallet_state(&address(n), &wallet).unwrap();
                wallets.insert(address(n), wallet);
            }
            batch.commit().unwrap();
            wallets.remove(&address(0));
            wallets.get_mut(&address(1)).unwrap().total_burned = 8;
            wallets.get_mut(&address(2)).unwrap().paused_intervals.clear();
            wallets.get_mut(&address(3)).unwrap().pending_releases = Default::default();
            let mut batch = store.tx_begin_write().unwrap();
            batch.put_raw(UndoTable::BrnWallets, address(4).as_str().as_bytes(), b"damaged").unwrap();
            batch.delete_raw(UndoTable::BrnWallets, address(5).as_str().as_bytes()).unwrap();
            batch.put_raw(UndoTable::BrnWallets, b"malformed", b"stale").unwrap();
            batch.put_brn_meta(wallet_storage::ROW_PREFIX, b"malformed child").unwrap();
            let prefix = wallet_storage::row_prefix(&address(1)).unwrap();
            batch.put_brn_meta(&[prefix.as_slice(), &[0]].concat(), b"misplaced header").unwrap();
            batch.put_brn_meta(&[prefix.as_slice(), &[9]].concat(), b"unknown child").unwrap();
            let orphan = wallet_storage::row_prefix(&WalletAddress::new("brst_orphan")).unwrap();
            batch.put_brn_meta(&[orphan.as_slice(), &[9]].concat(), b"orphan").unwrap();
            batch.put_brn_meta(b"unrelated", b"keep").unwrap();
            batch.commit().unwrap();
            let before = contents(&store);
            for commit in [false, true] {
                let mut batch = store.tx_begin_write().unwrap();
                batch.reconcile_checkpoint_brn_wallets(&wallets).unwrap();
                if commit { batch.commit().unwrap(); } else {
                    drop(batch);
                    assert_eq!(contents(&store), before);
                }
            }
            let actual = store.brn_store().all_wallet_states().unwrap();
            assert_eq!(actual.len(), wallets.len());
            for (address, bytes) in actual {
                assert_eq!(bytes, bincode::serialize(wallets.get(&address).unwrap()).unwrap());
            }
            assert_eq!(store.brn_store().get_meta(b"unrelated").unwrap().as_deref(), Some(&b"keep"[..]));
            let before = contents(&store);
            let id = store.env().read_txn().unwrap().id();
            let mut batch = store.tx_begin_write().unwrap();
            batch.reconcile_checkpoint_brn_wallets(&wallets).unwrap();
            batch.commit().unwrap();
            assert_eq!(store.env().read_txn().unwrap().id(), id);
            store.force_sync().unwrap();
            drop(store);
            let store = crate::LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
            assert_eq!(contents(&store), before);
        }
    }
}
