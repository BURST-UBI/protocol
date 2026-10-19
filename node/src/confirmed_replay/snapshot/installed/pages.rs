//! Snapshot pages and their descriptor share the ledger installation transaction.
//! The descriptor commits bytes, not authority: callers still authenticate from genesis.

use super::{invalid, BASE_KEY};
use blake2::{digest::consts::U32, Blake2b, Digest};
use burst_store::StoreError;
use burst_store_lmdb::LmdbWriteTransaction;
use std::io::{self, Read, Write};

const MAGIC: &[u8; 16] = b"BURST-BASE-PAGE\x01";
const PAGE_BYTES: usize = 1024 * 1024;
const PREFIX: &str = "checkpoint_snapshot/pages/v1/";
const DESCRIPTOR_BYTES: usize = 16 + 8 + 32;

fn key(index: usize) -> String {
    format!("{PREFIX}{index:08x}")
}

fn length(descriptor: &[u8]) -> Result<usize, StoreError> {
    if descriptor.len() != DESCRIPTOR_BYTES {
        return Err(invalid("invalid snapshot page descriptor"));
    }
    let size = u64::from_be_bytes(descriptor[16..24].try_into().map_err(invalid)?);
    let size = usize::try_from(size).map_err(invalid)?;
    if size == 0 {
        return Err(invalid("stored snapshot is empty"));
    }
    Ok(size)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Commitment {
    size: usize,
    hash: [u8; 32],
}

impl Commitment {
    /// Recheck every committed page, not only the descriptor, under the writer.
    pub(crate) fn verify(
        &self,
        get: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
    ) -> Result<(), StoreError> {
        let mut reader = Reader::open(get)?.ok_or_else(|| invalid("snapshot base disappeared"))?;
        if reader.commitment != *self {
            return Err(invalid("snapshot base changed during recovery"));
        }
        io::copy(&mut reader, &mut io::sink()).map_err(invalid)?;
        Ok(())
    }
}

/// The getter must use one pinned database transaction throughout this stream.
pub(super) struct Reader<F> {
    get: F,
    commitment: Commitment,
    page: Vec<u8>,
    offset: usize,
    consumed: usize,
    next_page: usize,
    digest: Blake2b<U32>,
    verified: bool,
    failed: bool,
}

impl<F: FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>> Reader<F> {
    pub(super) fn open(mut get: F) -> Result<Option<Self>, StoreError> {
        let Some(descriptor) = get(BASE_KEY)? else {
            return Ok(None);
        };
        let (commitment, page) = if descriptor.starts_with(MAGIC) {
            (
                Commitment {
                    size: length(&descriptor)?,
                    hash: descriptor[24..].try_into().map_err(invalid)?,
                },
                Vec::new(),
            )
        } else {
            if descriptor.is_empty() {
                return Err(invalid("stored snapshot is empty"));
            }
            (
                Commitment {
                    size: descriptor.len(),
                    hash: burst_crypto::blake2b_256(&descriptor),
                },
                descriptor,
            )
        };
        Ok(Some(Self {
            get,
            commitment,
            page,
            offset: 0,
            consumed: 0,
            next_page: 0,
            digest: Blake2b::new(),
            verified: false,
            failed: false,
        }))
    }

    pub(super) fn commitment(&self) -> Result<Commitment, StoreError> {
        if !self.verified {
            return Err(invalid("snapshot stream was not fully verified"));
        }
        Ok(self.commitment)
    }
}

impl<F: FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>> Read for Reader<F> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.failed {
            return Err(io::Error::other("snapshot stream previously failed"));
        }
        if output.is_empty() {
            return Ok(0);
        }
        if self.consumed == self.commitment.size {
            if !self.verified {
                let actual: [u8; 32] = self.digest.clone().finalize().into();
                if actual != self.commitment.hash {
                    self.failed = true;
                    return Err(io::Error::other(
                        "installed snapshot page commitment mismatch",
                    ));
                }
                self.verified = true;
            }
            return Ok(0);
        }
        if self.offset == self.page.len() {
            self.failed = true;
            self.page = (self.get)(&key(self.next_page))
                .map_err(|error| io::Error::other(error.to_string()))?
                .ok_or_else(|| io::Error::other("missing installed snapshot page"))?;
            if self.page.len() != PAGE_BYTES.min(self.commitment.size - self.consumed) {
                return Err(io::Error::other("invalid installed snapshot page length"));
            }
            self.next_page += 1;
            self.offset = 0;
            self.failed = false;
        }
        let count = output.len().min(self.page.len() - self.offset);
        output[..count].copy_from_slice(&self.page[self.offset..self.offset + count]);
        self.digest.update(&output[..count]);
        self.offset += count;
        self.consumed += count;
        Ok(count)
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

pub(super) struct Writer<'a, 'db> {
    batch: &'a mut LmdbWriteTransaction<'db>,
    page: Vec<u8>,
    size: usize,
    next_page: usize,
    old_pages: usize,
    digest: Blake2b<U32>,
    failed: bool,
}

impl<'a, 'db> Writer<'a, 'db> {
    pub(super) fn new(batch: &'a mut LmdbWriteTransaction<'db>) -> Result<Self, StoreError> {
        let previous = batch.get_meta(BASE_KEY)?;
        let old_pages = match previous.as_deref() {
            Some(descriptor) if descriptor.starts_with(MAGIC) => {
                length(descriptor)?.div_ceil(PAGE_BYTES)
            }
            _ => 0,
        };
        Ok(Self {
            batch,
            old_pages,
            page: Vec::with_capacity(PAGE_BYTES),
            size: 0,
            next_page: 0,
            digest: Blake2b::new(),
            failed: false,
        })
    }

    pub(super) fn finish(mut self) -> Result<(), StoreError> {
        if self.failed {
            return Err(invalid("snapshot write previously failed"));
        }
        if self.size == 0 {
            return Err(invalid("empty snapshot stream"));
        }
        if !self.page.is_empty() {
            self.batch.put_meta(&key(self.next_page), &self.page)?;
            self.next_page += 1;
        }
        for index in self.next_page..self.old_pages {
            self.batch.delete_meta(&key(index))?;
        }
        let mut descriptor = MAGIC.to_vec();
        descriptor.extend_from_slice(&(self.size as u64).to_be_bytes());
        descriptor.extend_from_slice(&self.digest.finalize());
        self.batch.put_meta(BASE_KEY, &descriptor)
    }
}

impl Write for Writer<'_, '_> {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
        if self.failed {
            return Err(io::Error::other("snapshot write previously failed"));
        }
        self.failed = true;
        let count = bytes.len();
        let size = self
            .size
            .checked_add(count)
            .ok_or_else(|| io::Error::other("stored snapshot length overflow"))?;
        self.digest.update(bytes);
        while !bytes.is_empty() {
            let take = bytes.len().min(PAGE_BYTES - self.page.len());
            self.page.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.page.len() == PAGE_BYTES {
                self.batch
                    .put_meta(&key(self.next_page), &self.page)
                    .map_err(|error| io::Error::other(error.to_string()))?;
                self.next_page += 1;
                self.page.clear();
            }
        }
        self.size = size;
        self.failed = false;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::meta::MetaStore;
    use burst_store_lmdb::LmdbStore;

    #[test]
    fn stored_pages_cross_old_archive_limit_and_reopen_without_an_admission_budget() {
        let dir = tempfile::tempdir().unwrap();
        let expected = 81 * PAGE_BYTES;
        {
            let store = LmdbStore::open(dir.path(), 32, 128 << 20).unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            let mut writer = Writer::new(&mut batch).unwrap();
            let page = vec![7; PAGE_BYTES];
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
            assert!(writer.page.len() < PAGE_BYTES);
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
        let mut offset = 0;
        let mut buffer = [0; 997];
        loop {
            let count = reader.read(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            assert_eq!(&buffer[..count], &original[offset..offset + count]);
            assert!(reader.page.len() <= PAGE_BYTES);
            offset += count;
        }
        assert_eq!(offset, original.len());
        assert_eq!(reads.get(), 4);
        let commitment = reader.commitment().unwrap();
        assert_eq!(commitment.hash, burst_crypto::blake2b_256(&original));
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
        assert!(
            reader.read(&mut buffer).is_err(),
            "failed streams cannot resume with unchecked bytes"
        );
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
