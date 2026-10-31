use super::*;
use crate::block_undo::BlockUndo;

impl LmdbWriteTransaction<'_> {
    /// Storage mechanics only. The caller must derive every record from
    /// authenticated replay and stage matching economics in this transaction.
    /// Partial inventories and journals touching signing promises are rejected.
    pub fn replace_checkpoint_undo(&mut self, records: &[BlockUndo]) -> Result<(), StoreError> {
        self.replace_checkpoint_undo_stream(records.iter().cloned().map(Ok))
    }

    /// Replace journals one record at a time, checking complete provisional-height
    /// coverage, encoding and primary block/index insertions. The caller authenticates
    /// economic before/after images through replay. Limits are 131,072 journals and
    /// 64 MiB of serialized journals plus keys; seen block hashes remain in memory.
    /// Any source or validation error prevents committing partially staged journals.
    pub fn replace_checkpoint_undo_stream(
        &mut self,
        records: impl IntoIterator<Item = Result<BlockUndo, StoreError>>,
    ) -> Result<(), StoreError> {
        let result = (|| {
            if self.failed || self.undo.is_some() {
                return Err(invalid("undo reconstruction requires a projection writer"));
            }
            self.verify_checkpoint_undo_encoding()?;
            let mut required = 0u64;
            for account in self.account_entries()? {
                let account = account?;
                required = required
                    .checked_add(
                        account
                            .block_count
                            .checked_sub(account.confirmation_height)
                            .ok_or_else(|| invalid("confirmation exceeds account height"))?,
                    )
                    .filter(|count| *count <= MAX_ROWS as u64)
                    .ok_or_else(|| invalid("provisional journal inventory exceeds limit"))?;
            }
            let mut seen = HashSet::new();
            let mut bytes = 0u64;
            self.clear_checkpoint_rows(UndoTable::Meta, Some(crate::block_undo::PREFIX))?;
            for record in records {
                let record = record?;
                if seen.len() as u64 >= required {
                    return Err(invalid("reconstructed undo inventory is excessive"));
                }
                let account = self.get_account(&record.account)?;
                if record.height <= account.confirmation_height
                    || record.height > account.block_count
                    || !seen.insert(record.block)
                    || self.block_at_height(&record.account, record.height)? != Some(record.block)
                    || self.block_height(&record.account, &record.block)? != record.height
                {
                    return Err(invalid(
                        "reconstructed undo differs from provisional history",
                    ));
                }
                bytes = bytes
                    .checked_add(bincode::serialized_size(&record).map_err(LmdbError::from)?)
                    .and_then(|size| {
                        size.checked_add(crate::block_undo::key(&record.block).len() as u64)
                    })
                    .filter(|size| *size <= MAX_BYTES as u64)
                    .ok_or_else(|| invalid("reconstructed undo exceeds byte limit"))?;
                let value = bincode::serialize(&record).map_err(LmdbError::from)?;
                BlockUndo::decode(&record.block, &value)?;
                let mut height_key = record.account.as_str().as_bytes().to_vec();
                height_key.extend_from_slice(&record.height.to_be_bytes());
                for (table, key, after) in [
                    (
                        UndoTable::Blocks,
                        record.block.as_bytes().to_vec(),
                        self.get_block(&record.block)?,
                    ),
                    (
                        UndoTable::Heights,
                        height_key.clone(),
                        record.block.as_bytes().to_vec(),
                    ),
                    (
                        UndoTable::BlockHeights,
                        record.block.as_bytes().to_vec(),
                        height_key,
                    ),
                ] {
                    if !record.changes.iter().any(|change| {
                        change.table == table
                            && change.key == key
                            && change.before.is_none()
                            && change.after.as_ref() == Some(&after)
                    }) {
                        return Err(invalid(
                            "reconstructed undo omits its primary ledger insertion",
                        ));
                    }
                }
                self.put_raw(
                    UndoTable::Meta,
                    &crate::block_undo::key(&record.block),
                    &value,
                )?;
            }
            if seen.len() as u64 != required {
                return Err(invalid("reconstructed undo inventory is incomplete"));
            }
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{block_undo::UndoChange, LmdbStore};

    #[test]
    fn recovered_undo_requires_complete_inventory_and_aborts_invalid_replacements() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let address = WalletAddress::new("brst_undo_reconstruction");
        let first = BlockHash::new([1; 32]);
        let second = BlockHash::new([2; 32]);
        let mut account = AccountInfo::unopened(address.clone());
        account.head = first;
        account.block_count = 1;
        account.confirmation_height = 1;
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_block_with_account(&first, b"first", &address, 1)
            .unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch.put_frontier(&address, &first).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.begin_block_undo(second, address.clone(), 2).unwrap();
        batch
            .put_block_with_account(&second, b"second", &address, 2)
            .unwrap();
        account.head = second;
        account.block_count = 2;
        batch.put_account_info(&account, false).unwrap();
        batch.put_frontier(&address, &second).unwrap();
        batch.commit().unwrap();
        let original = store
            .tx_begin_write()
            .unwrap()
            .block_undo(&second)
            .unwrap()
            .unwrap();
        for case in 0..5 {
            let mut record = original.clone();
            match case {
                1 => record.block = first,
                2 => record
                    .changes
                    .retain(|change| change.table != UndoTable::Blocks),
                3 => {
                    record.changes.push(UndoChange {
                        table: UndoTable::Meta,
                        key: b"snapshot_attestation/v1/forbidden".to_vec(),
                        before: None,
                        after: Some(vec![1]),
                    });
                    record
                        .changes
                        .sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
                }
                _ => (),
            }
            let records = match case {
                0 => vec![],
                4 => vec![record.clone(), record],
                _ => vec![record],
            };
            let mut batch = store.tx_begin_write().unwrap();
            assert!(
                batch.replace_checkpoint_undo(&records).is_err(),
                "case {case}"
            );
            assert!(batch.commit().is_err());
            assert_eq!(
                store.tx_begin_write().unwrap().block_undo(&second).unwrap(),
                Some(original.clone())
            );
        }
        let mut batch = store.tx_begin_write().unwrap();
        let interrupted = [
            Ok(original.clone()),
            Err(invalid("staged journal read failed")),
        ];
        assert!(batch.replace_checkpoint_undo_stream(interrupted).is_err());
        assert!(batch.commit().is_err());
        assert_eq!(
            store.tx_begin_write().unwrap().block_undo(&second).unwrap(),
            Some(original.clone())
        );
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .replace_checkpoint_undo_stream([Ok(original)])
            .unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.rewind_block(&second).unwrap();
        assert_eq!(batch.get_account(&address).unwrap().head, first);
        assert_eq!(batch.get_account(&address).unwrap().confirmation_height, 1);
    }
}
