//! Borrowed reads from one LMDB transaction, including a writer's pending changes.
use crate::{LmdbEnvironment, LmdbError, write_batch::CleanStorageCut};
use burst_store::{StoreError, account::AccountInfo};
use heed::RoTxn;

/// Borrows its transaction and environment; exposes no mutation or commit API.
/// A clean cut identifies storage only, never replay or deletion authority.
#[derive(Clone)]
pub struct LedgerReadView<'a> {
    pub(crate) env: &'a LmdbEnvironment,
    pub(crate) txn: &'a RoTxn<'a>,
    cut: Option<CleanStorageCut>,
}

impl<'a> LedgerReadView<'a> {
    pub(crate) fn new(env: &'a LmdbEnvironment, txn: &'a RoTxn<'a>, cut: Option<CleanStorageCut>) -> Self {
        Self { env, txn, cut }
    }

    /// Read up to 257 retained roots of an unfinished historical-head migration.
    pub fn checkpoint_head_staging(&self, id: &crate::checkpoint_heads::NodeHash)
        -> Result<Vec<crate::checkpoint_heads::NodeHash>, StoreError> {
        crate::checkpoint_heads::read_staging(self.env.meta_db, self.env.checkpoint_heads_db, self.txn, id)
    }

    /// Checks only the first durable cleanup entry; does not scan historical nodes.
    pub fn checkpoint_heads_need_cleanup(&self) -> Result<bool, StoreError> {
        Ok(crate::checkpoint_heads::next_unowned(self.env.checkpoint_heads_db, self.txn)?.is_some())
    }

    pub fn clean_storage_cut(&self) -> Option<CleanStorageCut> { self.cut.clone() }

    pub fn storage_directory(&self) -> &'a std::path::Path { self.env.env().path() }

    pub fn visit_indexed_block_window(
        &self, after: Option<burst_types::BlockHash>, max_rows: usize, max_bytes: usize,
        visit: impl FnMut(crate::block::IndexedBlock<'a>) -> Result<(), StoreError>,
    ) -> Result<crate::block::IndexedBlockWindow, StoreError> {
        crate::block::inventory::visit_window(self.txn, self.env.blocks_db,
            self.env.height_db, self.env.block_height_db, after, max_rows, max_bytes, visit)
    }

    /// Visit at most 131,072 metadata rows and 64 MiB of keys plus values.
    /// Errors can follow earlier callbacks; stage callback side effects until success.
    pub fn visit_checkpoint_metadata(&self, prefix: &[u8],
        mut visit: impl FnMut(&[u8], &[u8]) -> Result<(), StoreError>) -> Result<(), StoreError> {
        let db = self.env.meta_db;
        let mut total = 0usize;
        let iter: Box<dyn Iterator<Item = Result<(&[u8], &[u8]), heed::Error>> + '_> = match (!prefix.is_empty()).then_some(prefix) {
            Some(prefix) => Box::new(db.prefix_iter(&self.txn, prefix).map_err(LmdbError::from)?),
            None => Box::new(db.iter(&self.txn).map_err(LmdbError::from)?),
        };
        for (count, row) in iter.enumerate() {
            let (key, value) = row.map_err(LmdbError::from)?;
            total = total
                .checked_add(key.len())
                .and_then(|n| n.checked_add(value.len()))
                .filter(|n| *n <= (64 << 20))
                .ok_or_else(|| StoreError::Corruption("checkpoint scan exceeds byte limit".into()))?;
            if count == 131_072 {
                return Err(StoreError::Corruption("checkpoint scan exceeds row limit".into()));
            }
            visit(key, value)?;
        }
        Ok(())
    }

    /// Check journal encoding only; replay still authenticates its economic claims.
    pub fn verify_checkpoint_undo_encoding(&self) -> Result<(), StoreError> {
        self.verify_checkpoint_undo_encoding_with_poll(|_| Ok(()))
    }

    /// Visit the same bounded encoding scan with caller-owned interruption checks.
    pub fn verify_checkpoint_undo_encoding_with_poll(
        &self, mut checkpoint: impl FnMut(&'static str) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.visit_checkpoint_metadata(b"block_undo/", |key, bytes| {
            checkpoint("storage:undo-row")?;
            let hash = key
                .strip_prefix(crate::block_undo::PREFIX)
                .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
                .map(burst_types::BlockHash::new)
                .ok_or_else(|| StoreError::Corruption("invalid block undo key or version".into()))?;
            crate::block_undo::BlockUndo::decode(&hash, bytes)?;
            Ok(())
        })
    }

    /// Borrow physical rows only. This does not reconstruct virtual undo images.
    pub fn undo_value_ref(&self, table: crate::block_undo::UndoTable, key: &[u8])
        -> Result<Option<&'a [u8]>, StoreError> {
        table.database(self.env).get(self.txn, key).map_err(LmdbError::from).map_err(Into::into)
    }

    /// Borrow BRN wallet rows within cumulative row and key/value byte limits.
    /// Keys are checked for UTF-8 and prefix; values remain undecoded. Stop
    /// consuming on error. These limits do not bound LMDB pages retained by the view.
    pub fn bounded_brn_entries(&self, limit: usize, byte_limit: usize)
        -> Result<impl Iterator<Item = Result<(burst_types::WalletAddress, &'a [u8]), StoreError>> + 'a, StoreError> {
        let mut count = 0usize;
        let mut total = 0usize;
        let rows = self
            .env
            .brn_wallets_db
            .iter(&self.txn)
            .map_err(LmdbError::from)?;
        Ok(rows.map(move |row| {
            let (key, bytes) = row.map_err(LmdbError::from)?;
            total = total
                .checked_add(key.len())
                .and_then(|n| n.checked_add(bytes.len()))
                .ok_or_else(|| StoreError::Corruption("BRN snapshot size overflow".into()))?;
            if count == limit || total > byte_limit {
                return Err(StoreError::Corruption(
                    "BRN snapshot exceeds restoration limit".into(),
                ));
            }
            let address = crate::brn::decode_wallet_address(key)?;
            count += 1;
            Ok((address, bytes))
        }))
        }

    /// Raw node integrity only; historical certificates still require authentication.
    pub fn checkpoint_head_node(&self, hash: &[u8; 32])
        -> Result<Option<crate::checkpoint_heads::HeadNode>, StoreError> {
        crate::checkpoint_heads::read(self.env.checkpoint_heads_db, self.txn, hash)
            .map(|row| row.map(|(_, node)| node))
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.env.meta_db.get(self.txn, key.as_bytes())
            .map(|value| value.map(<[u8]>::to_vec))
            .map_err(|error| StoreError::Backend(error.to_string()))
    }

    pub fn contains_meta(&self, key: &str) -> Result<bool, StoreError> {
        self.env.meta_db.get(self.txn, key.as_bytes()).map(|value| value.is_some())
            .map_err(|error| StoreError::Backend(error.to_string()))
    }

    pub fn account_entries(&self) -> Result<impl Iterator<Item = Result<AccountInfo, StoreError>> + 'a, StoreError> {
        let entries = self.env.accounts_db.iter(self.txn).map_err(LmdbError::from)?;
        Ok(entries.map(|entry| {
            let (key, bytes) = entry.map_err(LmdbError::from)?;
            crate::account::decode_account_entry(key, bytes)
        }))
    }

    /// Keep the existing per-record bound; callers own aggregate work budgets.
    pub fn checkpoint_metadata_entries(&self, prefix: &'a [u8])
        -> Result<impl Iterator<Item = Result<(&'a [u8], &'a [u8]), StoreError>> + 'a, StoreError> {
        let rows = self.env.meta_db.prefix_iter(self.txn, prefix).map_err(LmdbError::from)?;
        Ok(rows.map(|row| {
            let (key, value) = row.map_err(LmdbError::from)?;
            if key.len().checked_add(value.len()).is_none_or(|bytes| bytes > 64 << 20) {
                return Err(StoreError::Corruption("checkpoint metadata row exceeds byte limit".into()));
            }
            Ok((key, value))
        }))
    }

    /// Borrow pending rows with a 64 MiB bound per key/value pair. The caller
    /// decodes each receipt and bounds the total scan.
    pub fn pending_entries(&self)
        -> Result<impl Iterator<Item = Result<(&'a [u8], &'a [u8]), StoreError>> + 'a, StoreError> {
        let rows = self.env.pending_db.iter(self.txn).map_err(LmdbError::from)?;
        Ok(rows.map(|row| {
            let (key, bytes) = row.map_err(LmdbError::from)?;
            if key.len().checked_add(bytes.len()).is_none_or(|size| size > 64 << 20) {
                return Err(StoreError::Corruption("pending row exceeds byte limit".into()));
            }
            Ok((key, bytes))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::{WalletAddress, BlockHash};

    #[test]
    fn pinned_view_keeps_rows_and_identity_while_writer_commits() {
        let directory = tempfile::tempdir().unwrap();
        let env = std::sync::Arc::new(LmdbEnvironment::open(directory.path(), 32, 32 << 20).unwrap());
        let address = WalletAddress::new("brst_read_view_account");
        let mut account = AccountInfo::unopened(address.clone());
        let mut writer = env.tx_begin_write().unwrap();
        writer.put_meta("read_view/a", b"before").unwrap();
        writer.put_account_info(&account, false).unwrap();
        let pending = burst_store::pending::PendingInfo {
            source: address.clone(), amount: 5, timestamp: burst_types::Timestamp::new(1),
            provenance: Vec::new(), return_credit: false,
        };
        writer.put_pending(&address, &[9; 32], &bincode::serialize(&pending).unwrap()).unwrap();
        writer.commit().unwrap();
        let reader = env.tx_begin_read().unwrap();
        let view = env.read_view(&reader).unwrap();
        let before = view.clean_storage_cut().unwrap();
        assert!(before.same_committed_view(&view.clean_storage_cut().unwrap()));
        let source = env.clone();
        let (sent, received) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut writer = source.tx_begin_write().unwrap();
            account.head = BlockHash::new([7; 32]);
            account.block_count = 1;
            writer.put_account_info(&account, false).unwrap();
            writer.put_meta("read_view/a", b"after").unwrap();
            writer.put_meta("read_view/b", b"new").unwrap();
            writer.delete_pending(&account.address, &[9; 32]).unwrap();
            writer.commit().unwrap();
            sent.send(()).unwrap();
        });
        received.recv_timeout(std::time::Duration::from_secs(5)).expect("pinned read blocked the writer");
        thread.join().unwrap();
        assert_eq!(view.get_meta("read_view/a").unwrap(), Some(b"before".to_vec()));
        assert!(!view.contains_meta("read_view/b").unwrap());
        assert_eq!(view.checkpoint_metadata_entries(b"read_view/").unwrap().count(), 1);
        let pending_rows = view.pending_entries().unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(pending_rows.len(), 1);
        assert_eq!(bincode::deserialize::<burst_store::pending::PendingInfo>(pending_rows[0].1).unwrap(), pending);
        assert_eq!(view.account_entries().unwrap().next().unwrap().unwrap().block_count, 0);
        assert!(before.same_committed_view(&view.clean_storage_cut().unwrap()));
        assert!(before.body_dependencies_unchanged(&view.clean_storage_cut().unwrap(), &env).unwrap());
        drop(view); drop(reader);
        let current = env.tx_begin_read().unwrap();
        let view = env.read_view(&current).unwrap();
        assert_eq!(view.get_meta("read_view/a").unwrap(), Some(b"after".to_vec()));
        assert_eq!(view.account_entries().unwrap().next().unwrap().unwrap().block_count, 1);
        assert!(view.pending_entries().unwrap().next().is_none());
        assert!(!before.same_committed_view(&view.clean_storage_cut().unwrap()));
        assert!(!before.body_dependencies_unchanged(&view.clean_storage_cut().unwrap(), &env).unwrap());
    }

    #[test]
    #[cfg(unix)]
    fn pinned_reader_capacity_is_separate_from_observation_budget_and_recovers_after_drop() {
        use std::os::unix::fs::MetadataExt;
        const MAP: usize = 8 << 20;
        const VALUE: usize = 128 << 10;
        const WRITES: usize = 96;
        for pinned in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let env = LmdbEnvironment::open(directory.path(), 32, MAP).unwrap();
            let write = |value: u8| -> Result<(), StoreError> {
                let mut writer = env.tx_begin_write()?;
                writer.put_meta("read_view/mvcc", &vec![value; VALUE])?;
                writer.commit()
            };
            write(0).unwrap();
            let watch = env.tx_begin_write().unwrap().watch_body_changes(65_536, 64 << 20).unwrap();
            let reader = pinned.then(|| env.tx_begin_read().unwrap());
            let measure = |stage: &str| {
                let info = env.env().info();
                let file = std::fs::metadata(directory.path().join("data.mdb")).unwrap();
                eprintln!("MVCC_PROBE pinned={pinned} stage={stage} map={} last_page={} file_bytes={} allocated_bytes={}",
                    info.map_size, info.last_page_number, file.len(), file.blocks() * 512);
                assert_eq!(info.map_size, MAP);
                assert!(file.len() <= MAP as u64);
            };
            measure("before");
            let started = std::time::Instant::now();
            let mut completed = 0;
            let mut capacity = false;
            for n in 1..=WRITES {
                match write(n as u8) {
                    Ok(()) => completed += 1,
                    Err(StoreError::MapFull(message))
                        if message == heed::Error::Mdb(heed::MdbError::MapFull).to_string() => {
                        capacity = true;
                        break;
                    }
                    Err(error) => panic!("unexpected overwrite failure: {error}"),
                }
            }
            eprintln!("MVCC_PROBE pinned={pinned} completed={completed} map_full={capacity} elapsed_us={}", started.elapsed().as_micros());
            assert_eq!(capacity, pinned, "same fixed overwrite workload differs only by reader retention");
            assert!(completed > 0);
            if !pinned { assert_eq!(completed, WRITES); }
            measure("overwritten");
            if let Some(reader) = &reader {
                assert_eq!(env.read_view(reader).unwrap().get_meta("read_view/mvcc").unwrap(), Some(vec![0; VALUE]));
            }
            // The failed transaction rolls back, and observer capacity still
            // covers the current cut: its byte limit did not bound native pages.
            let current = env.tx_begin_write().unwrap().clean_storage_cut().unwrap();
            watch.check_covered_cut(&env, &current).unwrap();
            drop(reader);
            {
                let reader = env.tx_begin_read().unwrap();
                assert_eq!(env.read_view(&reader).unwrap().get_meta("read_view/mvcc").unwrap(),
                    Some(vec![completed as u8; VALUE]));
            }
            drop(watch);
            for n in 0..WRITES { write(255 - n as u8).unwrap(); }
            measure("released-and-reused");
        }

        // A committed-file poll cannot reserve transaction-private dirty pages.
        // The undo record adds another value-sized write during commit itself.
        let directory = tempfile::tempdir().unwrap();
        let env = LmdbEnvironment::open(directory.path(), 32, MAP).unwrap();
        let watch = env.tx_begin_write().unwrap().watch_body_changes(65_536, 64 << 20).unwrap();
        let reader = env.tx_begin_read().unwrap();
        let extent = || env.env().real_disk_size().unwrap();
        let before = extent();
        let hash = burst_types::BlockHash::new([73; 32]);
        let value = vec![73; 3 << 20];
        let mut writer = env.tx_begin_write().unwrap();
        writer.begin_block_undo(hash, burst_types::WalletAddress::new("brst_capacity"), 1).unwrap();
        writer.put_meta("read_view/private_pages", &value).unwrap();
        let private = extent();
        assert_eq!(private, before, "default LMDB keeps this transaction's pages private before commit");
        assert!(env.read_view(&reader).unwrap().get_meta("read_view/private_pages").unwrap().is_none());
        writer.commit().unwrap();
        let committed = extent();
        eprintln!("MVCC_PRIVATE_PROBE before_bytes={before} private_bytes={private} committed_bytes={committed} value_bytes={} map={MAP}", value.len());
        assert!(committed - before > (4 << 20), "one commit exceeds conservative capture headroom despite no precommit extent growth");
        assert_eq!(env.env().info().map_size, MAP);
        assert!(committed <= MAP as u64);
        assert!(env.read_view(&reader).unwrap().get_meta("read_view/private_pages").unwrap().is_none());
        let current = env.tx_begin_write().unwrap();
        assert_eq!(current.get_meta("read_view/private_pages").unwrap().as_deref(), Some(value.as_slice()));
        let journal = current.block_undo(&hash).unwrap().unwrap();
        assert_eq!(journal.changes.len(), 1);
        assert_eq!(journal.changes[0].after.as_deref(), Some(value.as_slice()));
        watch.check_covered_cut(&env, &current.clean_storage_cut().unwrap()).unwrap();
    }

    #[test]
    fn file_growth_ceiling_covers_raw_nested_and_journal_writers() {
        let directory = tempfile::tempdir().unwrap();
        let env = LmdbEnvironment::open(directory.path(), 32, 16 << 20).unwrap();
        let mut owner = env.tx_begin_write().unwrap();
        let limit = owner.limit_file_extent(4 << 20).unwrap();
        let watch = limit.watch();
        assert!(!watch.was_exceeded());
        assert!(owner.limit_file_extent(8 << 20).is_err());
        drop(owner);
        let reader = env.tx_begin_read().unwrap();
        let before = env.env().info().last_txn_id;
        let hash = burst_types::BlockHash::new([74; 32]);
        let value = vec![74; 3 << 20];
        let mut writer = env.tx_begin_write().unwrap();
        let outcome = writer.growth_outcome().unwrap();
        writer.begin_block_undo(hash, burst_types::WalletAddress::new("brst_capacity"), 1).unwrap();
        writer.put_meta("limited", &value).unwrap();
        assert!(matches!(writer.commit(), Err(StoreError::GrowthLimit(_))), "journal sealing must respect the native ceiling");
        assert!(outcome.was_rejected());
        assert!(watch.was_exceeded());
        assert!(!outcome.watch().wait_released(std::time::Duration::ZERO));
        assert_eq!(env.env().info().last_txn_id, before);
        let current = env.tx_begin_write().unwrap();
        assert!(current.get_meta("limited").unwrap().is_none());
        assert!(current.block_undo(&hash).unwrap().is_none());
        drop(current);
        assert!(env.env().real_disk_size().unwrap() <= 4 << 20);

        let mut raw = env.env().write_txn().unwrap();
        assert!(matches!(env.meta_db.put(&mut raw, b"raw", &vec![1; 5 << 20]),
            Err(heed::Error::Mdb(heed::MdbError::GrowthLimit))));
        drop(raw);
        let mut raw = env.env().write_txn().unwrap();
        let parent_outcome = raw.growth_outcome().unwrap();
        let mut nested = env.env().nested_write_txn(&mut raw).unwrap();
        let child_outcome = nested.growth_outcome().unwrap();
        assert!(nested.limit_file_extent(8 << 20).is_err());
        assert!(matches!(env.meta_db.put(&mut nested, b"nested", &vec![2; 5 << 20]),
            Err(heed::Error::Mdb(heed::MdbError::GrowthLimit))));
        drop(nested);
        drop(raw);
        assert!(child_outcome.was_rejected());
        assert!(!parent_outcome.was_rejected(), "nested failure must not identify an unrelated root failure");
        assert!(env.read_view(&reader).unwrap().get_meta("limited").unwrap().is_none());
        drop(reader);
        // Releasing a lease does not retroactively relax an active writer.
        let mut raw = env.env().write_txn().unwrap();
        drop(limit);
        assert!(outcome.watch().wait_released(std::time::Duration::ZERO));
        assert!(matches!(env.meta_db.put(&mut raw, b"active", &vec![3; 5 << 20]),
            Err(heed::Error::Mdb(heed::MdbError::GrowthLimit))));
        drop(raw);
        let mut writer = env.tx_begin_write().unwrap();
        writer.begin_block_undo(hash, burst_types::WalletAddress::new("brst_capacity"), 1).unwrap();
        writer.put_meta("limited", &value).unwrap();
        writer.commit().unwrap();
        assert_eq!(env.env().info().last_txn_id, before + 1);
        assert!(env.env().real_disk_size().unwrap() > 4 << 20);
        assert_eq!(env.env().info().map_size, 16 << 20);
        let mut owner = env.tx_begin_write().unwrap();
        assert!(owner.limit_file_extent(4096).is_err());
        let limit = owner.limit_file_extent(12 << 20).unwrap();
        drop(owner);
        drop(limit);
    }

    #[test]
    fn native_commit_allocation_failure_does_not_publish_the_transaction() {
        let mut commit_limited = false;
        // Locate the boundary between this fixed-size put and native freelist /
        // database metadata commit work without assuming a particular page count.
        for extra_pages in 1..=32 {
            let directory = tempfile::tempdir().unwrap();
            let env = LmdbEnvironment::open(directory.path(), 32, 8 << 20).unwrap();
            let mut seed = env.env().write_txn().unwrap();
            env.meta_db.put(&mut seed, b"native", &[0; 64 << 10]).unwrap();
            let page_size = env.meta_db.stat(&seed).unwrap().page_size as usize;
            seed.commit().unwrap();
            let reader = env.tx_begin_read().unwrap();
            let generation = env.env().info().last_txn_id;
            let ceiling = env.env().real_disk_size().unwrap() as usize + extra_pages * page_size;
            let mut writer = env.env().write_txn().unwrap();
            let limit = writer.limit_file_extent(ceiling).unwrap();
            let outcome = writer.growth_outcome().unwrap();
            match env.meta_db.put(&mut writer, b"native", &[1; 64 << 10]) {
                Err(heed::Error::Mdb(heed::MdbError::GrowthLimit)) => drop(writer),
                Err(error) => panic!("unexpected put error: {error}"),
                Ok(()) => match writer.commit() {
                    Err(heed::Error::Mdb(heed::MdbError::GrowthLimit)) => {
                        commit_limited = true;
                        assert!(outcome.was_rejected(), "commit-time rejection must survive native transaction destruction");
                        assert!(limit.watch().was_exceeded());
                        assert_eq!(env.env().info().last_txn_id, generation);
                        let current = env.env().write_txn().unwrap();
                        assert_eq!(env.meta_db.get(&current, b"native").unwrap(), Some([0; 64 << 10].as_slice()));
                        eprintln!("NATIVE_COMMIT_LIMIT extra_pages={extra_pages} ceiling={ceiling}");
                    }
                    Ok(()) => (),
                    Err(error) => panic!("unexpected commit error: {error}"),
                },
            }
            assert!(env.env().real_disk_size().unwrap() <= ceiling as u64);
            drop(reader);
            drop(limit);
            if commit_limited { break; }
        }
        assert!(commit_limited, "exercise native commit allocation after a successful put");
    }

    #[test]
    fn growth_notifications_keep_transaction_and_lease_identity_after_release() {
        use std::time::Duration;
        let directory = tempfile::tempdir().unwrap();
        let env = LmdbEnvironment::open(directory.path(), 32, 16 << 20).unwrap();
        assert!(env.env().may_limit_file_extent());
        let mut writer = env.env().write_txn().unwrap();
        let limit = writer.limit_file_extent(4 << 20).unwrap();
        assert!(!env.env().may_limit_file_extent());
        let first = writer.growth_outcome().unwrap();
        let retry = writer.reserve_growth_retry().unwrap().unwrap();
        let retry_clone = retry.clone();
        assert!(matches!(env.meta_db.put(&mut writer, b"first", &vec![1; 5 << 20]),
            Err(heed::Error::Mdb(heed::MdbError::GrowthLimit))));
        writer.abort();
        assert!(first.was_rejected());
        assert!(first.watch().was_exceeded());
        assert!(!first.watch().wait_released(Duration::ZERO));

        // Native transaction storage is reused, but rejection belongs to the
        // failed writer rather than every later writer under this lease.
        let writer = env.env().write_txn().unwrap();
        let clean = writer.growth_outcome().unwrap();
        drop(writer);
        assert!(!clean.was_rejected());
        let waiting = first.watch();
        let (started, ready) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            started.send(()).unwrap();
            waiting.wait_released(Duration::from_secs(5))
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(limit);
        assert!(thread.join().unwrap());
        assert!(!env.env().may_limit_file_extent());
        let mut deferred = env.env().write_txn().unwrap();
        assert!(deferred.try_limit_file_extent(8 << 20).unwrap().is_none());
        drop(retry);
        assert!(!env.env().may_limit_file_extent());
        assert!(deferred.try_limit_file_extent(8 << 20).unwrap().is_none());
        drop(deferred);
        drop(retry_clone);
        assert!(env.env().may_limit_file_extent());

        let mut writer = env.env().write_txn().unwrap();
        assert!(writer.growth_outcome().is_none());
        let next = writer.limit_file_extent(8 << 20).unwrap();
        let second = writer.growth_outcome().unwrap();
        assert!(!next.watch().was_exceeded());
        assert!(!second.was_rejected());
        assert!(!second.watch().wait_released(Duration::ZERO));
        assert!(first.watch().wait_released(Duration::ZERO));
        env.meta_db.put(&mut writer, b"small", b"committed").unwrap();
        writer.commit().unwrap();
        assert!(!second.was_rejected());
        assert!(!next.watch().was_exceeded());
        drop(next);
        assert!(env.env().may_limit_file_extent());
        assert!(second.watch().wait_released(Duration::ZERO));
        assert!(first.was_rejected(), "later leases cannot reset an old outcome");
    }

    #[test]
    fn foreign_reader_is_rejected_and_uncommitted_writer_has_no_clean_cut() {
        let directory = tempfile::tempdir().unwrap();
        let other_directory = tempfile::tempdir().unwrap();
        let env = LmdbEnvironment::open(directory.path(), 32, 32 << 20).unwrap();
        let other = LmdbEnvironment::open(other_directory.path(), 32, 32 << 20).unwrap();
        let reader = env.tx_begin_read().unwrap();
        assert!(other.read_view(&reader).is_err());
        let before = env.read_view(&reader).unwrap().clean_storage_cut().unwrap();
        let foreign = other.tx_begin_read().unwrap();
        assert!(!before.same_committed_view(&other.read_view(&foreign).unwrap().clean_storage_cut().unwrap()));
        drop(foreign);
        drop(reader);
        let mut writer = env.tx_begin_write().unwrap();
        assert!(writer.read_view().clean_storage_cut().is_some());
        writer.put_meta("read_view/aborted", b"private").unwrap();
        let view = writer.read_view();
        assert!(view.clean_storage_cut().is_none());
        assert_eq!(view.get_meta("read_view/aborted").unwrap(), Some(b"private".to_vec()));
        drop(view); drop(writer);
        let reader = env.tx_begin_read().unwrap();
        assert!(!env.read_view(&reader).unwrap().contains_meta("read_view/aborted").unwrap());
    }

    #[test]
    fn common_account_reader_preserves_index_validation() {
        let directory = tempfile::tempdir().unwrap();
        let env = LmdbEnvironment::open(directory.path(), 32, 32 << 20).unwrap();
        let mut account = AccountInfo::unopened(WalletAddress::new("brst_expected"));
        account.confirmation_height = 1;
        let mut writer = env.tx_begin_write().unwrap();
        writer.put_account(&account.address, &bincode::serialize(&account).unwrap()).unwrap();
        assert!(writer.account_entries().unwrap().next().unwrap().is_err());
        writer.commit().unwrap();
        let reader = env.tx_begin_read().unwrap();
        assert!(env.read_view(&reader).unwrap().account_entries().unwrap().next().unwrap().is_err());
    }
}
