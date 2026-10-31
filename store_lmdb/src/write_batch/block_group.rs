use super::*;

/// Bounds the durable cut, not the protocol's per-block operation limits.
pub const MAX_BLOCKS: usize = 32;
const MAX_JOURNAL_BYTES: usize = crate::block_undo::MAX_BYTES;

pub(super) struct Budget {
    blocks: usize,
    bytes: usize,
}

impl<'a> LmdbWriteTransaction<'a> {
    pub(crate) fn new_block_group(env: &'a LmdbEnvironment) -> Result<Self, StoreError> {
        let mut transaction = Self::new(env)?;
        transaction.block_group = Some(Budget {
            blocks: 0,
            bytes: 0,
        });
        Ok(transaction)
    }

    pub(super) fn advance_block_group(&mut self) -> Result<(), StoreError> {
        if self.block_group.as_ref().unwrap().blocks == MAX_BLOCKS {
            return Err(StoreError::Backend("block group exceeds item limit".into()));
        }
        // Seal the previous block before opening the next journal. Its writes
        // remain visible to the next recorder, but neither is published yet.
        self.seal_block_undo()?;
        self.block_group.as_mut().unwrap().blocks += 1;
        Ok(())
    }

    pub(super) fn seal_block_undo(&mut self) -> Result<(), StoreError> {
        let Some(undo) = self.undo.take() else {
            return Ok(());
        };
        let (key, bytes) = undo.finish()?;
        if let Some(budget) = self.block_group.as_mut() {
            budget.bytes = budget
                .bytes
                .checked_add(bytes.len())
                .filter(|&bytes| bytes <= MAX_JOURNAL_BYTES)
                .ok_or_else(|| {
                    StoreError::Backend("block group exceeds journal byte limit".into())
                })?;
        }
        self.observe_sealed_journal(&key, &bytes)?;
        self.env
            .meta_db
            .put(&mut self.txn, &key, &bytes)
            .map_err(LmdbError::from)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::meta::MetaStore;

    fn begin(batch: &mut LmdbWriteTransaction<'_>, id: u8) -> Result<(), StoreError> {
        batch.begin_block_undo(
            BlockHash::new([id; 32]),
            WalletAddress::new("brst_group"),
            id as u64,
        )
    }

    #[test]
    fn grouped_journals_preserve_intermediate_values_and_commit_once() {
        let dir = tempfile::tempdir().unwrap();
        let env = LmdbEnvironment::open(dir.path(), crate::REQUIRED_DATABASES, 16 << 20).unwrap();
        let mut observer = env
            .tx_begin_write()
            .unwrap()
            .watch_body_changes(16, 8192)
            .unwrap();
        let generation = env.env().info().last_txn_id;
        let mut batch = env.tx_begin_block_group().unwrap();
        begin(&mut batch, 1).unwrap();
        batch.put_meta("shared", b"first").unwrap();
        begin(&mut batch, 2).unwrap();
        batch.put_meta("shared", b"second").unwrap();
        assert!(env.meta_store().get_meta("shared").is_err());
        batch.commit().unwrap();
        assert_eq!(env.env().info().last_txn_id, generation + 1);
        let mut observed = Vec::new();
        observer
            .visit(&env, |row| {
                observed.push(row.clone());
                Ok(())
            })
            .unwrap();
        assert_eq!(observed.len(), 4);
        for (offset, id, value) in [(0, 1, b"first".as_slice()), (2, 2, b"second".as_slice())] {
            assert_eq!(observed[offset].key, b"shared");
            assert_eq!(observed[offset].after.as_deref(), Some(value));
            let journal = &observed[offset + 1];
            let hash = BlockHash::new([id; 32]);
            assert_eq!(journal.key, crate::block_undo::key(&hash));
            assert!(journal.before.is_none());
            let decoded = BlockUndo::decode(&hash, journal.after.as_deref().unwrap()).unwrap();
            assert_eq!(decoded.changes, vec![observed[offset].clone()]);
        }
        drop(observer);
        drop(env);
        let env = LmdbEnvironment::open(dir.path(), crate::REQUIRED_DATABASES, 16 << 20).unwrap();
        let batch = env.tx_begin_write().unwrap();
        for (id, before, after) in [
            (1, None, b"first".as_slice()),
            (2, Some(b"first".as_slice()), b"second".as_slice()),
        ] {
            let record = batch
                .block_undo(&BlockHash::new([id; 32]))
                .unwrap()
                .unwrap();
            assert_eq!(record.changes.len(), 1);
            assert_eq!(record.changes[0].before.as_deref(), before);
            assert_eq!(record.changes[0].after.as_deref(), Some(after));
        }
        assert_eq!(
            batch.get_meta("shared").unwrap().as_deref(),
            Some(b"second".as_slice())
        );
    }

    #[test]
    fn failed_or_abandoned_group_publishes_neither_data_nor_journals() {
        let dir = tempfile::tempdir().unwrap();
        let env = LmdbEnvironment::open(dir.path(), crate::REQUIRED_DATABASES, 16 << 20).unwrap();
        for duplicate in [false, true] {
            let mut observer = env
                .tx_begin_write()
                .unwrap()
                .watch_body_changes(16, 8192)
                .unwrap();
            let mut batch = env.tx_begin_block_group().unwrap();
            begin(&mut batch, 1).unwrap();
            batch.put_meta("shared", b"first").unwrap();
            begin(&mut batch, 2).unwrap();
            batch.put_meta("shared", b"second").unwrap();
            if duplicate {
                assert!(begin(&mut batch, 1).is_err());
                assert!(batch.commit().is_err());
            } else {
                drop(batch);
            }
            observer
                .visit(&env, |_| panic!("aborted group published observations"))
                .unwrap();
            let batch = env.tx_begin_write().unwrap();
            assert!(batch.get_meta("shared").unwrap().is_none());
            for id in [1, 2] {
                assert!(batch
                    .block_undo(&BlockHash::new([id; 32]))
                    .unwrap()
                    .is_none());
            }
        }
    }

    #[test]
    fn group_limits_and_single_block_commit_guards_abort_the_whole_cut() {
        let dir = tempfile::tempdir().unwrap();
        let env = LmdbEnvironment::open(dir.path(), crate::REQUIRED_DATABASES, 16 << 20).unwrap();
        assert!(env.tx_begin_block_group().unwrap().commit().is_err());
        let mut batch = env.tx_begin_block_group().unwrap();
        for id in 1..=MAX_BLOCKS as u8 {
            begin(&mut batch, id).unwrap();
            batch.put_meta("shared", &[id]).unwrap();
        }
        assert!(begin(&mut batch, MAX_BLOCKS as u8 + 1).is_err());
        assert!(batch.commit().is_err());
        assert!(env.meta_store().get_meta("shared").is_err());
        for elected in [false, true] {
            let mut batch = env.tx_begin_block_group().unwrap();
            begin(&mut batch, 1).unwrap();
            batch.put_meta("shared", b"first").unwrap();
            let hash = BlockHash::new([1; 32]);
            let account = WalletAddress::new("brst_group");
            let result = if elected {
                batch.commit_elected_block(&hash, &account, 1)
            } else {
                batch.commit_block(&hash, &account, 1)
            };
            assert!(result.is_err());
            assert!(env.meta_store().get_meta("shared").is_err());
        }
    }

    #[test]
    fn active_duplicate_and_journal_budget_failure_cannot_publish() {
        let dir = tempfile::tempdir().unwrap();
        let env = LmdbEnvironment::open(dir.path(), crate::REQUIRED_DATABASES, 16 << 20).unwrap();
        let mut batch = env.tx_begin_block_group().unwrap();
        begin(&mut batch, 1).unwrap();
        batch.put_meta("shared", b"first").unwrap();
        assert!(begin(&mut batch, 1).is_err());
        assert!(batch.commit().is_err());
        for fail_at_next_boundary in [false, true] {
            let mut batch = env.tx_begin_block_group().unwrap();
            begin(&mut batch, 1).unwrap();
            batch.put_meta("shared", b"first").unwrap();
            // Exercise the accumulator boundary without a 64 MiB test allocation.
            batch.block_group.as_mut().unwrap().bytes = MAX_JOURNAL_BYTES;
            if fail_at_next_boundary {
                assert!(begin(&mut batch, 2).is_err());
            }
            assert!(batch.commit().is_err());
            let batch = env.tx_begin_write().unwrap();
            assert!(batch.get_meta("shared").unwrap().is_none());
            assert!(batch
                .block_undo(&BlockHash::new([1; 32]))
                .unwrap()
                .is_none());
        }
        let mut batch = env.tx_begin_block_group().unwrap();
        batch.put_meta("unjournaled", b"value").unwrap();
        assert!(begin(&mut batch, 1).is_err());
        assert!(batch.commit().is_err());
    }
}
