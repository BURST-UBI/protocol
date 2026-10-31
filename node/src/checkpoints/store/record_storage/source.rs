//! Final source revalidation for staged legacy-record migration.
//! Mutation coverage proves local identity, never certificate authority.
use super::{invalid, is_record_key, RecordLocation};
use burst_store::StoreError;
use burst_store_lmdb::{block_undo::UndoTable, retention::BodyChangeWatch, LmdbStore, LmdbWriteTransaction};
use crate::paged_metadata::Commitment;

pub(super) struct VerifiedSource {
    key: String,
    pages: String,
    watch: BodyChangeWatch,
    protected: Vec<String>,
}

impl VerifiedSource {
    #[cfg(test)]
    pub(super) fn capture(store: &LmdbStore, key: &str, expected: Commitment)
        -> Result<Self, StoreError> {
        Self::capture_with(store, key, expected, Vec::new(), |_| Ok(()))
    }

    #[cfg(test)]
    pub(super) fn capture_with(store: &LmdbStore, key: &str, expected: Commitment,
        protected: Vec<String>,
        validate: impl FnOnce(&burst_store_lmdb::LmdbReadTransaction<'_>) -> Result<(), StoreError>,
    ) -> Result<Self, StoreError> {
        Self::capture_with_poll(store, key, expected, protected, &|| Ok(()), validate)
    }

    /// After staging, verify source bytes and dependencies in one observed snapshot.
    /// Logical decoding runs without a writer. `protected` lists metadata prefixes
    /// read by `validate`; changes to them invalidate publication.
    pub(super) fn capture_with_poll(store: &LmdbStore, key: &str, expected: Commitment,
        protected: Vec<String>, poll: &dyn Fn() -> Result<(), StoreError>,
        validate: impl FnOnce(&burst_store_lmdb::LmdbReadTransaction<'_>) -> Result<(), StoreError>,
    ) -> Result<Self, StoreError> {
        poll()?;
        if !is_record_key(key) { return Err(invalid("migration source is not a record")); }
        if protected.len() > 4 || protected.iter().any(|key| key.is_empty() || key.len() > 256) {
            return Err(invalid("migration dependency coverage exceeds bound"));
        }
        let batch = store.tx_begin_write()?;
        let watch = batch.watch_body_changes(1024, 2 << 20)?;
        let txn = store.tx_begin_read()?;
        let cut = store.read_view(&txn)?.clean_storage_cut()
            .ok_or_else(|| invalid("migration source has no clean cut"))?;
        if !batch.matches_clean_storage_cut(&cut) { return Err(invalid("migration source cut changed")); }
        watch.check_unconsumed_since(store, &cut)?;
        drop(batch);
        let descriptor = store.meta_store().get_meta_txn(&txn, key)?
            .ok_or_else(|| StoreError::StaleRead("migration source disappeared".into()))?;
        if super::shared::is_shared(&descriptor)? {
            return Err(StoreError::StaleRead("migration source is already shared".into()));
        }
        let actual = super::inspect_stored(key, &descriptor,
            |name| { poll()?; store.meta_store().get_meta_txn(&txn, name) },
            |hash| { poll()?; store.read_view(&txn)?.checkpoint_head_node(hash) }, expected.size)?;
        poll()?;
        if actual.size != expected.size || actual.hash != expected.hash {
            return Err(StoreError::StaleRead("migration source bytes changed".into()));
        }
        validate(&txn)?;
        Ok(Self { key: key.to_owned(), pages: RecordLocation::new(key).page_prefix, watch, protected })
    }

    /// Consume before editing an untouched publication writer. Holding that
    /// writer prevents another commit between reconciliation and publication.
    pub(super) fn verify(mut self, store: &LmdbStore, batch: &LmdbWriteTransaction<'_>)
        -> Result<(), StoreError> {
        let current = batch.clean_storage_cut()
            .ok_or_else(|| invalid("migration publication requires a clean writer"))?;
        self.watch.check_covered_cut(store, &current)?;
        let key = self.key.as_bytes();
        let pages = self.pages.as_bytes();
        let reconciled = self.watch.visit(store, |change| {
            if change.table == UndoTable::Meta
                && (change.key == key || change.key.starts_with(pages)
                    || self.protected.iter().any(|prefix| change.key.starts_with(prefix.as_bytes()))) {
                return Err(StoreError::StaleRead("migration source mutated after verification".into()));
            }
            Ok(())
        })?;
        if !batch.matches_clean_storage_cut(&reconciled) {
            return Err(StoreError::StaleRead("migration source coverage changed".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paged_metadata::{Location, PAGE_BYTES};

    fn seed(store: &LmdbStore, key: &str) -> Commitment {
        let bytes = vec![7; PAGE_BYTES * 3];
        let mut batch = store.tx_begin_write().unwrap();
        super::super::put(&mut batch, key, &bytes).unwrap();
        batch.commit().unwrap();
        Commitment { size: bytes.len(), hash: burst_crypto::blake2b_256(&bytes) }
    }

    #[test]
    fn source_guard_allows_unrelated_commits_but_rejects_page_changes_and_aba() {
        for mutation in 0..3 {
            let dir = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
            let key = "checkpoint_record_archive/v1/dev/source/1";
            let expected = seed(&store, key);
            let guard = VerifiedSource::capture(&store, key, expected).unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            if mutation == 0 { batch.put_meta("unrelated", b"changed").unwrap(); }
            else {
                let page = RecordLocation::new(key).page_key(1);
                let original = batch.get_meta(&page).unwrap().unwrap();
                batch.put_meta(&page, b"corrupt").unwrap();
                if mutation == 2 { batch.put_meta(&page, &original).unwrap(); }
            }
            batch.commit().unwrap();
            let batch = store.tx_begin_write().unwrap();
            assert_eq!(guard.verify(&store, &batch).is_ok(), mutation == 0);
        }
    }

    #[test]
    fn source_guard_rejects_dirty_writer_and_observer_overflow() {
        for overflow in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
            let key = "checkpoint_record_archive/v1/dev/source/1";
            let expected = seed(&store, key);
            let guard = VerifiedSource::capture(&store, key, expected).unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            let value = if overflow { vec![1; 3 << 20] } else { b"dirty".to_vec() };
            batch.put_meta("unrelated", &value).unwrap();
            if overflow {
                batch.commit().unwrap();
                let batch = store.tx_begin_write().unwrap();
                assert!(guard.verify(&store, &batch).is_err());
            } else { assert!(guard.verify(&store, &batch).is_err()); }
        }
    }
}
