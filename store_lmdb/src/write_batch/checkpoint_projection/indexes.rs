use super::*;
use bincode::Options;
use burst_store::pending::PendingInfo;

impl LmdbWriteTransaction<'_> {
    /// Validate the bounded local journal inventory, not its economic claims.
    /// Before-images still need authenticated replay before startup trusts them.
    pub fn verify_checkpoint_undo_encoding(&self) -> Result<(), StoreError> {
        self.read_view().verify_checkpoint_undo_encoding()
    }

    /// Replace origin, expiry and reverse indexes from caller-authenticated mints.
    /// Each mint indexes itself as its origin. Requires a clean writer and at most
    /// 131,072 unique nonzero mint IDs. Any error prevents commit.
    pub fn replace_checkpoint_token_indexes(
        &mut self,
        mints: &[(TxHash, Timestamp)],
    ) -> Result<(), StoreError> {
        let result = (|| {
            self.require_clean_checkpoint_boundary()?;
            let mut seen = HashSet::new();
            if mints.len() > MAX_ROWS
                || mints
                    .iter()
                    .any(|(token, _)| token.is_zero() || !seen.insert(*token))
            {
                return Err(invalid("invalid recovered mint inventory"));
            }
            for table in [
                UndoTable::TrstOrigins,
                UndoTable::TrstExpiry,
                UndoTable::TrstReverse,
            ] {
                self.clear_checkpoint_rows(table, None)?;
            }
            for (token, expiry) in mints {
                self.put_origin_index(token, token)?;
                self.put_expiry_index(*expiry, token)?;
            }
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Check frontiers against account heads and pending sender/amount indexes
    /// against pending rows. This scans all accounts and pending rows without
    /// repairing them. It does not check other indexes or authenticate balances
    /// and provenance.
    pub fn verify_checkpoint_lookup_indexes(&self) -> Result<(), StoreError> {
        self.read_view().verify_checkpoint_lookup_indexes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LmdbStore;

    #[test]
    fn undo_encoding_stream_matches_collecting_oracle_and_never_repairs() {
        use crate::block_undo::{BlockUndo, UndoChange};
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        for n in 1..=3 {
            let hash = BlockHash::new([n; 32]);
            let undo = BlockUndo {
                block: hash,
                account: WalletAddress::new("brst_owner"),
                height: n as u64,
                changes: vec![UndoChange {
                    table: UndoTable::Meta,
                    key: b"test_value".to_vec(),
                    before: None,
                    after: Some(vec![n]),
                }],
            };
            batch
                .put_raw(
                    UndoTable::Meta,
                    &crate::block_undo::key(&hash),
                    &bincode::serialize(&undo).unwrap(),
                )
                .unwrap();
        }
        let baseline = batch.checkpoint_metadata(b"block_undo/").unwrap();
        for (key, bytes) in &baseline {
            let hash = BlockHash::new(
                key.strip_prefix(crate::block_undo::PREFIX)
                    .unwrap()
                    .try_into()
                    .unwrap(),
            );
            BlockUndo::decode(&hash, bytes).unwrap();
        }
        let mut polls = 0;
        assert!(batch
            .read_view()
            .verify_checkpoint_undo_encoding_with_poll(|stage| {
                assert_eq!(stage, "storage:undo-row");
                polls += 1;
                if polls == 2 {
                    return Err(StoreError::Backend("undo checkpoint sentinel".into()));
                }
                Ok(())
            })
            .unwrap_err()
            .to_string()
            .contains("undo checkpoint sentinel"));
        assert_eq!(polls, 2);
        assert_eq!(batch.checkpoint_metadata(b"block_undo/").unwrap(), baseline);
        batch.verify_checkpoint_undo_encoding().unwrap();
        for (key, bytes) in [
            (b"block_undo/v2/unknown".to_vec(), baseline[0].1.clone()),
            (
                crate::block_undo::key(&BlockHash::new([1; 32])),
                baseline[1].1.clone(),
            ),
            (crate::block_undo::key(&BlockHash::new([1; 32])), {
                let mut bytes = baseline[0].1.clone();
                bytes.push(0);
                bytes
            }),
        ] {
            batch.put_raw(UndoTable::Meta, &key, &bytes).unwrap();
            let before = batch.checkpoint_metadata(b"block_undo/").unwrap();
            assert!(batch.verify_checkpoint_undo_encoding().is_err());
            assert_eq!(batch.checkpoint_metadata(b"block_undo/").unwrap(), before);
            batch.delete_raw(UndoTable::Meta, &key).unwrap();
            for (key, bytes) in &baseline {
                batch.put_raw(UndoTable::Meta, key, bytes).unwrap();
            }
            batch.verify_checkpoint_undo_encoding().unwrap();
        }
    }

    #[test]
    fn checkpoint_pending_validation_streams_beyond_old_inventory_limit() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 256 << 20).unwrap();
        let destination = WalletAddress::new("brst_pending_inventory");
        let info = PendingInfo {
            source: WalletAddress::new("brst_pending_source"),
            amount: 7,
            timestamp: Timestamp::new(1),
            provenance: Vec::new(),
            return_credit: true,
        };
        let bytes = bincode::serialize(&info).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        for index in 0..=MAX_ROWS {
            let mut hash = [0; 32];
            hash[..8].copy_from_slice(&(index as u64).to_be_bytes());
            batch.put_pending(&destination, &hash, &bytes).unwrap();
        }
        batch.verify_checkpoint_lookup_indexes().unwrap();
        assert_eq!(batch.pending_entries().unwrap().count(), MAX_ROWS + 1);
        batch
            .env
            .pending_sender_db
            .put(&mut batch.txn, b"extra", b"orphan")
            .unwrap();
        assert!(batch.verify_checkpoint_lookup_indexes().is_err());
        drop(batch);
        assert_eq!(
            store
                .tx_begin_write()
                .unwrap()
                .pending_entries()
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn checkpoint_pending_validation_rejects_malformed_values_without_repair() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .env
            .pending_db
            .put(&mut batch.txn, b"invalid", &[0xff])
            .unwrap();
        assert!(batch.verify_checkpoint_lookup_indexes().is_err());
        assert_eq!(
            batch.pending_entries().unwrap().next().unwrap().unwrap().1,
            &[0xff]
        );
    }

    #[test]
    fn checkpoint_frontier_validation_streams_beyond_old_inventory_limit() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 256 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        for index in 0..=MAX_ROWS {
            let mut account = burst_store::account::AccountInfo::unopened(WalletAddress::new(
                format!("brst_inventory_{index:08}"),
            ));
            // Include unopened identities; they must not require a frontier.
            if index % 2 == 0 {
                account.block_count = 1;
                account.confirmation_height = 1;
                account.head = BlockHash::new([1; 32]);
                batch.put_frontier(&account.address, &account.head).unwrap();
            }
            batch.put_account_info(&account, false).unwrap();
        }
        assert!(batch.bounded_accounts(MAX_ROWS).is_err());
        batch.verify_checkpoint_lookup_indexes().unwrap();
        batch.replace_checkpoint_undo(&[]).unwrap();
        batch
            .put_frontier(
                &WalletAddress::new("brst_inventory_00000001"),
                &BlockHash::new([1; 32]),
            )
            .unwrap();
        assert!(batch.verify_checkpoint_lookup_indexes().is_err());
    }

    #[test]
    fn snapshot_signing_promise_cannot_join_or_be_erased_by_block_undo() {
        use burst_store::meta::MetaStore;
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let key = "snapshot_attestation/v1/dev/genesis/epoch/signer";
        store.meta_store().put_meta(key, b"signed promise").unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .begin_block_undo(BlockHash::new([1; 32]), WalletAddress::new("brst_test"), 1)
            .unwrap();
        assert!(batch.put_meta(key, b"replacement promise").is_err());
        assert!(batch.commit().is_err());
        assert_eq!(store.meta_store().get_meta(key).unwrap(), b"signed promise");
        let batch = store.tx_begin_write().unwrap();
        assert!(batch
            .block_undo(&BlockHash::new([1; 32]))
            .unwrap()
            .is_none());
        batch.verify_checkpoint_undo_encoding().unwrap();
    }

    #[test]
    fn recovered_token_indexes_are_atomic_and_invalid_inputs_poison_the_writer() {
        use burst_store::trst_index::TrstIndexStore;
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let old = TxHash::new([1; 32]);
        let new = TxHash::new([2; 32]);
        let index = store.trst_index_store();
        index.put_origin_index(&old, &old).unwrap();
        index.put_expiry_index(Timestamp::new(4), &old).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .replace_checkpoint_token_indexes(&[(new, Timestamp::new(9))])
            .unwrap();
        assert!(batch
            .undo_value(UndoTable::TrstReverse, old.as_bytes())
            .unwrap()
            .is_none());
        let reverse = batch
            .undo_value(UndoTable::TrstReverse, new.as_bytes())
            .unwrap()
            .unwrap();
        assert_eq!(&reverse[..32], new.as_bytes());
        assert_eq!(&reverse[32..], &9u64.to_be_bytes());
        drop(batch);
        assert_eq!(index.get_by_origin(&old).unwrap(), vec![old]);
        assert!(index.get_by_origin(&new).unwrap().is_empty());
        for mints in [
            vec![(TxHash::ZERO, Timestamp::new(9))],
            vec![(new, Timestamp::new(9)); 2],
        ] {
            let mut batch = store.tx_begin_write().unwrap();
            assert!(batch.replace_checkpoint_token_indexes(&mints).is_err());
            assert!(batch.commit().is_err());
            assert_eq!(index.get_by_origin(&old).unwrap(), vec![old]);
        }
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .replace_checkpoint_token_indexes(&[(new, Timestamp::new(9))])
            .unwrap();
        batch.commit().unwrap();
        assert!(index.get_by_origin(&old).unwrap().is_empty());
        assert_eq!(index.get_by_origin(&new).unwrap(), vec![new]);
    }

    #[test]
    fn checkpoint_lookup_indexes_reject_corruption_without_repair() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let address = WalletAddress::new("brst_index_account");
        let mut account = AccountInfo::unopened(address.clone());
        account.block_count = 1;
        account.head = BlockHash::new([1; 32]);
        let info = PendingInfo {
            source: address.clone(),
            amount: 7,
            timestamp: Timestamp::new(1),
            provenance: Vec::new(),
            return_credit: false,
        };
        let key = crate::pending::pending_key_raw(&address, &[2; 32]);
        let sender = crate::pending::sender_key(&address, &key);
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch.put_frontier(&address, &account.head).unwrap();
        batch
            .put_pending(&address, &[2; 32], &bincode::serialize(&info).unwrap())
            .unwrap();
        for target in ["storage:lookup-account", "storage:lookup-pending"] {
            let mut reached = false;
            assert!(batch
                .read_view()
                .verify_checkpoint_lookup_indexes_with_poll(|stage| {
                    if stage == target {
                        reached = true;
                        return Err(StoreError::Backend("lookup checkpoint sentinel".into()));
                    }
                    Ok(())
                })
                .unwrap_err()
                .to_string()
                .contains("lookup checkpoint sentinel"));
            assert!(reached, "checkpoint not reached: {target}");
        }
        batch.verify_checkpoint_lookup_indexes().unwrap();
        batch.commit().unwrap();
        for case in 0..7 {
            let mut batch = store.tx_begin_write().unwrap();
            match case {
                0 => {
                    batch
                        .env
                        .frontiers_db
                        .delete(&mut batch.txn, address.as_str().as_bytes())
                        .unwrap();
                }
                1 => {
                    batch
                        .put_frontier(&address, &BlockHash::new([3; 32]))
                        .unwrap();
                }
                2 => {
                    batch
                        .put_frontier(&WalletAddress::new("brst_extra"), &account.head)
                        .unwrap();
                }
                3 => {
                    batch
                        .env
                        .pending_sender_db
                        .delete(&mut batch.txn, &sender)
                        .unwrap();
                }
                4 => {
                    batch
                        .env
                        .pending_sender_db
                        .put(&mut batch.txn, &sender, b"wrong")
                        .unwrap();
                }
                5 => {
                    batch
                        .env
                        .pending_sender_db
                        .put(&mut batch.txn, b"extra", &key)
                        .unwrap();
                }
                6 => {
                    batch
                        .env
                        .pending_sender_db
                        .delete(&mut batch.txn, &sender)
                        .unwrap();
                    batch
                        .env
                        .pending_sender_db
                        .put(&mut batch.txn, b"wrong sender", &key)
                        .unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                batch.verify_checkpoint_lookup_indexes().is_err(),
                "case {case}"
            );
            assert!(
                batch.verify_checkpoint_lookup_indexes().is_err(),
                "repair in case {case}"
            );
            drop(batch);
            store
                .tx_begin_write()
                .unwrap()
                .verify_checkpoint_lookup_indexes()
                .unwrap();
        }
    }
}

impl crate::read_view::LedgerReadView<'_> {
    pub fn verify_checkpoint_lookup_indexes(&self) -> Result<(), StoreError> {
        self.verify_checkpoint_lookup_indexes_with_poll(|_| Ok(()))
    }

    /// Scan accounts and pending rows, polling once per primary row.
    /// There is no aggregate row or byte limit. Pending decoding and index lookups
    /// between polls are synchronous; this is not a hard deadline.
    pub fn verify_checkpoint_lookup_indexes_with_poll(
        &self,
        mut checkpoint: impl FnMut(&'static str) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let mut expected_frontiers = 0u64;
        for account in self.account_entries()? {
            let account = account?;
            checkpoint("storage:lookup-account")?;
            if account.block_count == 0 {
                continue;
            }
            expected_frontiers += 1;
            if self
                .env
                .frontiers_db
                .get(&self.txn, account.address.as_str().as_bytes())
                .map_err(LmdbError::from)?
                != Some(account.head.as_bytes().as_slice())
            {
                return Err(invalid("frontier index differs from account heads"));
            }
        }
        if self
            .env
            .frontiers_db
            .len(&self.txn)
            .map_err(LmdbError::from)?
            != expected_frontiers
        {
            return Err(invalid("frontier index differs from account heads"));
        }
        let mut count = 0u64;
        for row in self.pending_entries()? {
            let (key, bytes) = row?;
            checkpoint("storage:lookup-pending")?;
            count += 1;
            let info: PendingInfo = bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .with_limit(bytes.len() as u64)
                .reject_trailing_bytes()
                .deserialize(bytes)
                .map_err(LmdbError::from)?;
            let amount_index = crate::pending::amount_index::key(key, info.amount)?;
            if self
                .env
                .pending_amount_db
                .get(&self.txn, &amount_index)
                .map_err(LmdbError::from)?
                != Some(key)
            {
                return Err(invalid(
                    "pending amount index differs from pending transfer",
                ));
            }
            let index = crate::pending::sender_key(&info.source, key);
            if self
                .env
                .pending_sender_db
                .get(&self.txn, &index)
                .map_err(LmdbError::from)?
                != Some(key)
            {
                return Err(invalid(
                    "pending sender index differs from pending transfer",
                ));
            }
        }
        if self
            .env
            .pending_amount_db
            .len(&self.txn)
            .map_err(LmdbError::from)?
            != count
        {
            return Err(invalid(
                "pending amount index inventory differs from pending transfers",
            ));
        }
        if self
            .env
            .pending_sender_db
            .len(&self.txn)
            .map_err(LmdbError::from)?
            != count
        {
            return Err(invalid(
                "pending sender index inventory differs from pending transfers",
            ));
        }
        Ok(())
    }
}
