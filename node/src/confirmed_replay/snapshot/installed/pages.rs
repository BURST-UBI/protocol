//! Snapshot-specific keys for the shared atomic metadata pager.
#[cfg(test)]
use super::invalid;
use super::BASE_KEY;
use crate::paged_metadata;
use burst_store::StoreError;
use burst_store_lmdb::LmdbWriteTransaction;
pub(crate) use paged_metadata::Commitment;
#[cfg(test)]
use paged_metadata::{LEGACY_MAGIC, LEGACY_PAGE_BYTES, MAGIC, PAGE_BYTES};
#[cfg(test)]
use std::io::{self, Read, Seek, SeekFrom, Write};

const PREFIX: &str = "checkpoint_snapshot/pages/v1/";

pub(crate) struct SnapshotLocation;
impl paged_metadata::Location for SnapshotLocation {
    fn descriptor_key(&self) -> &str {
        BASE_KEY
    }
    fn page_key(&self, index: usize) -> String {
        key(index)
    }
}
fn key(index: usize) -> String {
    format!("{PREFIX}{index:08x}")
}

pub(super) type Reader<F> = paged_metadata::Reader<F, SnapshotLocation>;
pub(super) type Writer<'a, 'db> = paged_metadata::Writer<'a, 'db, SnapshotLocation>;

impl<F: FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>> Reader<F> {
    pub(super) fn open(get: F) -> Result<Option<Self>, StoreError> {
        Self::open_at(SnapshotLocation, get)
    }
}
impl<'a, 'db> Writer<'a, 'db> {
    pub(super) fn new(batch: &'a mut LmdbWriteTransaction<'db>) -> Result<Self, StoreError> {
        Self::new_at(SnapshotLocation, batch)
    }
}
impl Commitment {
    pub(crate) fn verify(
        &self,
        get: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
    ) -> Result<(), StoreError> {
        self.verify_at(SnapshotLocation, get)
    }
}

/// Byte-level fixture/export access; production recovery consumes Reader directly.
#[cfg(test)]
pub(crate) fn read(
    get: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
) -> Result<Option<Vec<u8>>, StoreError> {
    let Some(mut reader) = Reader::open(get)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(invalid)?;
    Ok(Some(bytes))
}

#[cfg(test)]
pub(super) fn write(batch: &mut LmdbWriteTransaction<'_>, bytes: &[u8]) -> Result<(), StoreError> {
    let mut writer = Writer::new(batch)?;
    writer.write_all(bytes).map_err(invalid)?;
    writer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::meta::MetaStore;
    use burst_store_lmdb::LmdbStore;

    #[test]
    fn stored_pages_cross_old_archive_limit_and_reopen_without_an_admission_budget() {
        let dir = tempfile::tempdir().unwrap();
        let expected = 81 * LEGACY_PAGE_BYTES;
        {
            let store = LmdbStore::open(dir.path(), 32, 128 << 20).unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            let mut writer = Writer::new(&mut batch).unwrap();
            let page = vec![7; LEGACY_PAGE_BYTES];
            for _ in 0..81 {
                writer.write_all(&page).unwrap();
            }
            writer.finish().unwrap();
            batch.commit().unwrap();
            store.env().force_sync().unwrap();
        }
        let store = LmdbStore::open(dir.path(), 32, 128 << 20).unwrap();
        let txn = store.tx_begin_read().unwrap();
        let mut reader = Reader::open(|name| store.meta_store().get_meta_txn(&txn, name))
            .unwrap()
            .unwrap();
        assert_eq!(
            io::copy(&mut reader, &mut io::sink()).unwrap(),
            expected as u64
        );
        assert_eq!(reader.commitment().unwrap().size, expected);
    }

    #[test]
    fn streams_fragmented_pages_and_rechecks_content_without_trusting_the_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let original: Vec<_> = (0..PAGE_BYTES * 3 + 29).map(|i| (i % 251) as u8).collect();
        let mut batch = store.tx_begin_write().unwrap();
        let mut writer = Writer::new(&mut batch).unwrap();
        for part in original.chunks(7919) {
            writer.write_all(part).unwrap();
            assert!(writer.buffered_bytes() < PAGE_BYTES);
        }
        writer.finish().unwrap();
        batch.commit().unwrap();
        let txn = store.tx_begin_read().unwrap();
        let reads = std::cell::Cell::new(0);
        let mut reader = Reader::open(|name| {
            if name != BASE_KEY {
                reads.set(reads.get() + 1);
            }
            store.meta_store().get_meta_txn(&txn, name)
        })
        .unwrap()
        .unwrap();
        assert_eq!(reads.get(), 0, "opening a stream must not load its pages");
        assert!(reader.commitment().is_err());
        assert!(reader.rewind().is_err());
        let mut offset = 0;
        let mut buffer = [0; 997];
        loop {
            let count = reader.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            assert_eq!(&buffer[..count], &original[offset..offset + count]);
            assert!(reader.buffered_bytes() <= PAGE_BYTES);
            offset += count;
        }
        assert_eq!(offset, original.len());
        assert_eq!(reads.get(), 4);
        let commitment = reader.commitment().unwrap();
        assert_eq!(commitment.hash, burst_crypto::blake2b_256(&original));
        for start in [0, PAGE_BYTES - 7, PAGE_BYTES, original.len() - 9] {
            reader.seek(SeekFrom::Start(start as u64)).unwrap();
            let mut bytes = [0; 9];
            reader.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, &original[start..start + 9]);
        }
        assert!(reader.seek(SeekFrom::End(1)).is_err());
        assert!(reader.seek(SeekFrom::Start(u64::MAX)).is_err());
        reader.seek(SeekFrom::End(-9)).unwrap();
        let mut suffix = Vec::new();
        reader.read_to_end(&mut suffix).unwrap();
        assert_eq!(suffix, original[original.len() - 9..]);
        assert_eq!(reader.commitment().unwrap().hash, commitment.hash);
        drop(reader);
        drop(txn);
        let mut batch = store.tx_begin_write().unwrap();
        commitment.verify(|name| batch.get_meta(name)).unwrap();
        let descriptor = batch.get_meta(BASE_KEY).unwrap();
        batch.put_meta(&key(1), &vec![0; PAGE_BYTES]).unwrap();
        assert_eq!(batch.get_meta(BASE_KEY).unwrap(), descriptor);
        assert!(commitment.verify(|name| batch.get_meta(name)).is_err());
        batch.put_meta(&key(1), b"short").unwrap();
        let mut reader = Reader::open(|name| batch.get_meta(name)).unwrap().unwrap();
        assert!(io::copy(&mut reader, &mut io::sink()).is_err());
        assert!(reader.rewind().is_err());
        assert!(
            reader.read(&mut buffer).is_err(),
            "failed streams cannot resume with unchecked bytes"
        );
    }

    #[test]
    fn verified_inline_archive_can_rewind_and_seek_without_page_lookups() {
        let original = b"legacy inline snapshot";
        let mut reader = Reader::open(|name| {
            assert_eq!(name, BASE_KEY);
            Ok(Some(original.to_vec()))
        })
        .unwrap()
        .unwrap();
        assert!(reader.seek(SeekFrom::End(0)).is_err());
        io::copy(&mut reader, &mut io::sink()).unwrap();
        reader.rewind().unwrap();
        assert!(reader.seek(SeekFrom::Current(-1)).is_err());
        reader.seek(SeekFrom::Current(7)).unwrap();
        let mut suffix = Vec::new();
        reader.read_to_end(&mut suffix).unwrap();
        assert_eq!(suffix, &original[7..]);
        reader.commitment().unwrap();
    }

    #[test]
    fn legacy_pages_migrate_atomically_preserving_commitment_and_pinned_reads() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let original: Vec<_> = (0..LEGACY_PAGE_BYTES * 2 + 17)
            .map(|i| (i % 251) as u8)
            .collect();
        let mut descriptor = LEGACY_MAGIC.to_vec();
        descriptor.extend_from_slice(&(original.len() as u64).to_be_bytes());
        descriptor.extend_from_slice(&burst_crypto::blake2b_256(&original));
        let mut batch = store.tx_begin_write().unwrap();
        for (index, page) in original.chunks(LEGACY_PAGE_BYTES).enumerate() {
            batch.put_meta(&key(index), page).unwrap();
        }
        batch.put_meta(BASE_KEY, &descriptor).unwrap();
        batch.commit().unwrap();
        let pinned = store.tx_begin_read().unwrap();
        let mut reader = Reader::open(|name| store.meta_store().get_meta_txn(&pinned, name))
            .unwrap()
            .unwrap();
        io::copy(&mut reader, &mut io::sink()).unwrap();
        let commitment = reader.commitment().unwrap();
        reader
            .seek(SeekFrom::Start((LEGACY_PAGE_BYTES - 7) as u64))
            .unwrap();
        let mut boundary = [0; 19];
        reader.read_exact(&mut boundary).unwrap();
        assert_eq!(
            &boundary,
            &original[LEGACY_PAGE_BYTES - 7..LEGACY_PAGE_BYTES + 12]
        );
        {
            let mut batch = store.tx_begin_write().unwrap();
            write(&mut batch, &original).unwrap();
            commitment.verify(|name| batch.get_meta(name)).unwrap();
            assert!(batch
                .get_meta(BASE_KEY)
                .unwrap()
                .unwrap()
                .starts_with(MAGIC));
        }
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.get_meta(BASE_KEY).unwrap(), Some(descriptor));
        write(&mut batch, &original).unwrap();
        batch.commit().unwrap();
        reader.rewind().unwrap();
        let mut old_bytes = Vec::new();
        reader.read_to_end(&mut old_bytes).unwrap();
        assert_eq!(old_bytes, original);
        drop(reader);
        drop(pinned);
        let mut batch = store.tx_begin_write().unwrap();
        commitment.verify(|name| batch.get_meta(name)).unwrap();
        let replacement = vec![9; PAGE_BYTES + 3];
        write(&mut batch, &replacement).unwrap();
        assert!(batch.get_meta(&key(2)).unwrap().is_none());
        assert!(batch
            .get_meta(&key(original.len().div_ceil(PAGE_BYTES) - 1))
            .unwrap()
            .is_none());
        batch.commit().unwrap();
        store.env().force_sync().unwrap();
        drop(store);
        let reopened = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        assert_eq!(
            super::super::read_stored_base_bytes(&reopened).unwrap(),
            Some(replacement)
        );
    }

    #[test]
    fn page_descriptors_reject_unknown_versions_and_malformed_lengths() {
        for magic in [LEGACY_MAGIC, MAGIC, b"BURST-BASE-PAGE\x03"] {
            for size in [0u64, 1] {
                let mut descriptor = magic.to_vec();
                descriptor.extend_from_slice(&size.to_be_bytes());
                // Deliberately omit the hash: both known versions must reject it.
                assert!(Reader::open(|_| Ok(Some(descriptor.clone()))).is_err());
                descriptor.extend_from_slice(&[0; 32]);
                if size == 0 || magic != LEGACY_MAGIC && magic != MAGIC {
                    assert!(Reader::open(|_| Ok(Some(descriptor.clone()))).is_err());
                }
            }
        }
    }

    #[test]
    fn replacement_is_atomic_and_removes_only_obsolete_pages() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let original = vec![7; PAGE_BYTES * 2 + 17];
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta("unrelated", b"preserved").unwrap();
        write(&mut batch, &original).unwrap();
        assert_eq!(
            read(|key| batch.get_meta(key)).unwrap(),
            Some(original.clone())
        );
        batch.commit().unwrap();
        let replacement = vec![8; PAGE_BYTES + 3];
        {
            let mut batch = store.tx_begin_write().unwrap();
            write(&mut batch, &replacement).unwrap();
            assert_eq!(
                read(|key| batch.get_meta(key)).unwrap(),
                Some(replacement.clone())
            );
            // Dropping the writer models interruption before the commit boundary.
        }
        assert_eq!(
            super::super::read_stored_base_bytes(&store).unwrap(),
            Some(original.clone())
        );
        let pinned = store.tx_begin_read().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        write(&mut batch, &replacement).unwrap();
        batch.commit().unwrap();
        let mut pinned_reader = Reader::open(|name| store.meta_store().get_meta_txn(&pinned, name))
            .unwrap()
            .unwrap();
        io::copy(&mut pinned_reader, &mut io::sink()).unwrap();
        pinned_reader
            .seek(SeekFrom::Start(PAGE_BYTES as u64))
            .unwrap();
        let mut old_tail = Vec::new();
        pinned_reader.read_to_end(&mut old_tail).unwrap();
        assert_eq!(old_tail, original[PAGE_BYTES..]);
        drop(pinned_reader);
        assert_eq!(
            read(|key| store.meta_store().get_meta_txn(&pinned, key)).unwrap(),
            Some(original)
        );
        drop(pinned);
        assert_eq!(
            super::super::read_stored_base_bytes(&store).unwrap(),
            Some(replacement)
        );
        assert!(store.meta_store().get_meta(&key(2)).is_err());
        assert_eq!(
            store.meta_store().get_meta("unrelated").unwrap(),
            b"preserved".to_vec()
        );
        store
            .meta_store()
            .put_meta(&key(0), &vec![9; PAGE_BYTES])
            .unwrap();
        assert!(super::super::read_stored_base_bytes(&store).is_err());
        store.meta_store().delete_meta(&key(0)).unwrap();
        assert!(super::super::read_stored_base_bytes(&store).is_err());
    }
}
