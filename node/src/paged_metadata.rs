//! Paged byte streams in caller-owned transactions. A descriptor records a
//! length and digest; it does not establish protocol authority.

pub(crate) mod staged;

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}

/// A caller owns the descriptor key and a disjoint namespace for its pages.
pub(crate) trait Location {
    fn descriptor_key(&self) -> &str;
    fn page_key(&self, index: usize) -> String;
}
use blake2::{digest::consts::U32, Blake2b, Digest};
use burst_store::StoreError;
use burst_store_lmdb::LmdbWriteTransaction;
use std::io::{self, Read, Seek, SeekFrom, Write};

pub(crate) const LEGACY_MAGIC: &[u8; 16] = b"BURST-BASE-PAGE\x01";
pub(crate) const MAGIC: &[u8; 16] = b"BURST-BASE-PAGE\x02";
pub(crate) const LEGACY_PAGE_BYTES: usize = 1024 * 1024;
// Leave room for the native overflow-page header within a 4 KiB LMDB page.
// Unlike a 1 MiB value, this does not require 257 contiguous pages on that host.
pub(crate) const PAGE_BYTES: usize = 4096 - 32;
const DESCRIPTOR_BYTES: usize = 16 + 8 + 32;

#[cfg(test)]
fn report_write(stage: &str, index: usize, bytes: usize) {
    if std::env::var_os("BURST_SNAPSHOT_INSTALL_STAGE_LOG").is_some() {
        eprintln!("snapshot-page stage={stage} index={index} bytes={bytes}");
    }
}

#[cfg(test)]
pub(crate) fn report_metadata_pages(batch: &LmdbWriteTransaction<'_>, stage: &str) {
    if std::env::var_os("BURST_SNAPSHOT_INSTALL_STAGE_LOG").is_none() { return; }
    match batch.metadata_page_stats() {
        Ok(stats) => eprintln!(
            "snapshot-meta-live stage={stage} page_size={} branch={} leaf={} overflow={} entries={}",
            stats.page_size, stats.branch_pages, stats.leaf_pages,
            stats.overflow_pages, stats.entries,
        ),
        Err(error) => eprintln!("snapshot-meta-live stage={stage} unavailable={error}"),
    }
}

fn page_bytes(descriptor: &[u8]) -> Result<usize, StoreError> {
    if descriptor.starts_with(MAGIC) {
        Ok(PAGE_BYTES)
    } else if descriptor.starts_with(LEGACY_MAGIC) {
        Ok(LEGACY_PAGE_BYTES)
    } else if descriptor.starts_with(b"BURST-BASE-PAGE") {
        Err(invalid("unsupported snapshot page descriptor version"))
    } else {
        Ok(0) // Legacy inline archive.
    }
}

pub(crate) fn is_paged(descriptor: &[u8]) -> Result<bool, StoreError> {
    Ok(page_bytes(descriptor)? != 0)
}

/// Descriptor-only page count for bounded retirement planning. This does not
/// verify the stream; callers still need the usual logical commitment check.
pub(crate) fn page_count(descriptor: &[u8], max_bytes: usize) -> Result<usize, StoreError> {
    let stride = page_bytes(descriptor)?;
    let size = if stride == 0 { descriptor.len() } else { length(descriptor)? };
    if size > max_bytes { return Err(invalid("paged metadata exceeds retirement limit")); }
    Ok(if stride == 0 { 0 } else { size.div_ceil(stride) })
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

/// Delete a value and its pages in the caller's transaction. The caller must
/// abort on error, as for Writer. Bounds prevent corrupt lengths driving cleanup.
pub(crate) fn delete_at(
    location: impl Location,
    batch: &mut LmdbWriteTransaction<'_>,
    max_bytes: usize,
) -> Result<(), StoreError> {
    if let Some(descriptor) = batch.get_meta(location.descriptor_key())? {
        let stride = page_bytes(&descriptor)?;
        let size = if stride == 0 { descriptor.len() } else { length(&descriptor)? };
        if size > max_bytes {
            return Err(invalid("paged metadata exceeds deletion byte limit"));
        }
        if stride != 0 {
            for index in 0..size.div_ceil(stride) {
                batch.delete_meta(&location.page_key(index))?;
            }
        }
        batch.delete_meta(location.descriptor_key())?;
    }
    Ok(())
}

pub(crate) fn write_bytes_at(
    location: impl Location, batch: &mut LmdbWriteTransaction<'_>, bytes: &[u8],
) -> Result<(), StoreError> {
    let mut writer = Writer::new_at(location, batch)?;
    writer.write_all(bytes).map_err(|error| {
        let message = error.to_string();
        match error.into_inner().and_then(|error| error.downcast::<StoreError>().ok()) {
            Some(error) => *error,
            None => invalid(message),
        }
    })?;
    writer.finish()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Commitment {
    pub(crate) size: usize,
    pub(crate) hash: [u8; 32],
}

impl Commitment {
    /// Recheck the descriptor and all stream bytes in one pinned transaction.
    /// The getter determines whether this is a read or write transaction.
    pub(crate) fn verify_at(
        &self,
        location: impl Location,
        get: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
    ) -> Result<(), StoreError> {
        let mut reader = Reader::open_at(location, get)?.ok_or_else(|| invalid("snapshot base disappeared"))?;
        if reader.commitment != *self {
            return Err(invalid("snapshot base changed during recovery"));
        }
        io::copy(&mut reader, &mut io::sink()).map_err(invalid)?;
        Ok(())
    }
}

/// The getter must use one pinned database transaction throughout this stream.
/// Read through EOF to verify the digest; reading exactly `size` bytes is not
/// sufficient. Paged values buffer one page; inline values are loaded in full.
pub(crate) struct Reader<F, L> {
    location: L,
    get: F,
    commitment: Commitment,
    page: Vec<u8>,
    page_bytes: usize,
    offset: usize,
    consumed: usize,
    next_page: usize,
    digest: Blake2b<U32>,
    verified: bool,
    failed: bool,
}

impl<F: FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>, L: Location> Reader<F, L> {
    pub(crate) fn open_at(location: L, get: F) -> Result<Option<Self>, StoreError> {
        Self::open_at_bounded(location, get, usize::MAX)
    }

    pub(crate) fn open_at_bounded(location: L, mut get: F, max_bytes: usize) -> Result<Option<Self>, StoreError> {
        let Some(descriptor) = get(location.descriptor_key())? else {
            return Ok(None);
        };
        let page_bytes = page_bytes(&descriptor)?;
        let (commitment, page) = if page_bytes != 0 {
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
        if commitment.size > max_bytes {
            return Err(invalid("paged metadata exceeds logical byte limit"));
        }
        Ok(Some(Self {
            location,
            get,
            commitment,
            page,
            page_bytes,
            offset: 0,
            consumed: 0,
            next_page: 0,
            digest: Blake2b::new(),
            verified: false,
            failed: false,
        }))
    }

    #[cfg(test)]
    pub(crate) fn buffered_bytes(&self) -> usize { self.page.len() }

    pub(crate) fn commitment(&self) -> Result<Commitment, StoreError> {
        if self.failed || !self.verified {
            return Err(invalid("snapshot stream was not fully verified"));
        }
        Ok(self.commitment)
    }
}

impl<F: FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>, L: Location> Read for Reader<F, L> {
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
            self.page = (self.get)(&self.location.page_key(self.next_page))
                .map_err(|error| io::Error::other(error.to_string()))?
                .ok_or_else(|| io::Error::other("missing installed snapshot page"))?;
            if self.page.len() != self.page_bytes.min(self.commitment.size - self.consumed) {
                return Err(io::Error::other("invalid installed snapshot page length"));
            }
            self.next_page += 1;
            self.offset = 0;
            self.failed = false;
        }
        let count = output.len().min(self.page.len() - self.offset);
        output[..count].copy_from_slice(&self.page[self.offset..self.offset + count]);
        if !self.verified {
            self.digest.update(&output[..count]);
        }
        self.offset += count;
        self.consumed += count;
        Ok(count)
    }
}

// Seeking is only valid after a complete sequential commitment check. The pinned
// transaction then guarantees that rereads see the same digest-checked bytes.
impl<F: FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>, L: Location> Seek for Reader<F, L> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        if self.failed || !self.verified {
            return Err(io::Error::other("snapshot seeking requires a verified stream"));
        }
        let position = match from {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::Current(delta) => self.consumed as i128 + i128::from(delta),
            SeekFrom::End(delta) => self.commitment.size as i128 + i128::from(delta),
        };
        if position < 0 || position > self.commitment.size as i128 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "snapshot seek out of bounds"));
        }
        let position = position as usize;
        if position == self.consumed {
            return Ok(position as u64);
        }
        if self.page_bytes != 0 && position < self.commitment.size {
            let index = position / self.page_bytes;
            self.failed = true;
            let page = (self.get)(&self.location.page_key(index))
                .map_err(|error| io::Error::other(error.to_string()))?
                .ok_or_else(|| io::Error::other("missing installed snapshot page"))?;
            if page.len() != self.page_bytes.min(self.commitment.size - index * self.page_bytes) {
                return Err(io::Error::other("invalid installed snapshot page length"));
            }
            self.page = page;
            self.offset = position % self.page_bytes;
            self.next_page = index + 1;
            self.failed = false;
        } else {
            self.offset = if self.page_bytes != 0 { self.page.len() } else { position };
        }
        self.consumed = position;
        Ok(position as u64)
    }
}

/// Stage pages in the supplied transaction. `finish` writes the descriptor and
/// removes obsolete pages; it does not commit. `flush` is a no-op. Abort the
/// transaction after an error or if the writer is dropped before finishing.
/// Buffering is page-sized, but total stream length and transaction work are
/// not capped here.
pub(crate) struct Writer<'a, 'db, L> {
    location: L,
    batch: &'a mut LmdbWriteTransaction<'db>,
    page: Vec<u8>,
    size: usize,
    next_page: usize,
    old_pages: usize,
    digest: Blake2b<U32>,
    failed: bool,
    storage_error: Option<StoreError>,
}

impl<'a, 'db, L: Location> Writer<'a, 'db, L> {
    pub(crate) fn new_at(location: L, batch: &'a mut LmdbWriteTransaction<'db>) -> Result<Self, StoreError> {
        let previous = batch.get_meta(location.descriptor_key())?;
        let old_pages = match previous.as_deref() {
            Some(descriptor) => match page_bytes(descriptor)? {
                0 => 0,
                stride => length(descriptor)?.div_ceil(stride),
            },
            None => 0,
        };
        Ok(Self {
            location,
            batch,
            old_pages,
            page: Vec::with_capacity(PAGE_BYTES),
            size: 0,
            next_page: 0,
            digest: Blake2b::new(),
            failed: false,
            storage_error: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn buffered_bytes(&self) -> usize { self.page.len() }

    /// Recover the typed backend failure when an outer serializer erased its source.
    pub(crate) fn take_storage_error(&mut self) -> Option<StoreError> {
        self.storage_error.take()
    }

    pub(crate) fn finish(mut self) -> Result<(), StoreError> {
        if self.failed {
            return Err(self.storage_error.take()
                .unwrap_or_else(|| invalid("snapshot write previously failed")));
        }
        if self.size == 0 {
            return Err(invalid("empty snapshot stream"));
        }
        if !self.page.is_empty() {
            #[cfg(test)]
            report_write("put-tail", self.next_page, self.page.len());
            self.batch.put_meta(&self.location.page_key(self.next_page), &self.page)?;
            #[cfg(test)]
            report_write("put-tail-done", self.next_page, self.page.len());
            self.next_page += 1;
        }
        for index in self.next_page..self.old_pages {
            #[cfg(test)]
            report_write("delete-obsolete", index, 0);
            self.batch.delete_meta(&self.location.page_key(index))?;
        }
        let mut descriptor = MAGIC.to_vec();
        descriptor.extend_from_slice(&(self.size as u64).to_be_bytes());
        descriptor.extend_from_slice(&self.digest.finalize());
        #[cfg(test)]
        report_write("descriptor", self.next_page, descriptor.len());
        self.batch.put_meta(self.location.descriptor_key(), &descriptor)
    }
}

impl<L: Location> Write for Writer<'_, '_, L> {
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
                #[cfg(test)]
                report_write("put", self.next_page, self.page.len());
                #[cfg(test)]
                if self.next_page % 1024 == 0 {
                    report_metadata_pages(self.batch, self.location.descriptor_key());
                }
                self.batch
                    .put_meta(&self.location.page_key(self.next_page), &self.page)
                    .map_err(|error| {
                        self.storage_error = Some(error.clone());
                        io::Error::other(error)
                    })?;
                #[cfg(test)]
                report_write("put-done", self.next_page, self.page.len());
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
    use burst_store_lmdb::LmdbStore;

    #[derive(Clone, Copy)]
    struct TestLocation(&'static str);
    impl Location for TestLocation {
        fn descriptor_key(&self) -> &str { self.0 }
        fn page_key(&self, index: usize) -> String { format!("{}/pages/{index:08x}", self.0) }
    }

    #[test]
    fn namespaces_keep_independent_bytes_and_commitments_in_one_transaction() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 8 << 20).unwrap();
        let left = TestLocation("test/left");
        let right = TestLocation("test/right");
        let mut batch = store.tx_begin_write().unwrap();
        for (location, byte) in [(left, 3), (right, 7)] {
            let mut writer = Writer::new_at(location, &mut batch).unwrap();
            writer.write_all(&vec![byte; PAGE_BYTES + 17]).unwrap();
            writer.finish().unwrap();
        }
        batch.commit().unwrap();
        let pinned = store.tx_begin_read().unwrap();
        for (location, byte) in [(left, 3), (right, 7)] {
            let mut reader = Reader::open_at_bounded(location,
                |key| store.meta_store().get_meta_txn(&pinned, key), PAGE_BYTES + 17)
                .unwrap().unwrap();
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, vec![byte; PAGE_BYTES + 17]);
            reader.commitment().unwrap().verify_at(location,
                |key| store.meta_store().get_meta_txn(&pinned, key)).unwrap();
        }
    }

    #[test]
    fn logical_limit_rejects_descriptors_before_loading_pages() {
        let mut descriptor = MAGIC.to_vec();
        descriptor.extend_from_slice(&4096u64.to_be_bytes());
        descriptor.extend_from_slice(&[0; 32]);
        let location = TestLocation("test/bounded");
        let calls = std::cell::Cell::new(0);
        assert!(Reader::open_at_bounded(location, |key| {
            assert_eq!(key, location.descriptor_key());
            calls.set(calls.get() + 1);
            Ok(Some(descriptor.clone()))
        }, 4095).is_err());
        assert_eq!(calls.get(), 1);
        assert!(Reader::open_at_bounded(location, |_| Ok(Some(vec![1; 17])), 16).is_err());
    }

    #[test]
    fn capacity_errors_survive_serializer_erasure_and_abort_partial_pages() {
        for growth in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(directory.path(), 32, if growth { 8 << 20 } else { 1 << 20 }).unwrap();
            let location = TestLocation("test/capacity");
            let original = vec![7; PAGE_BYTES + 17];
            let mut batch = store.tx_begin_write().unwrap();
            write_bytes_at(location, &mut batch, &original).unwrap();
            batch.commit().unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            let limit = growth.then(|| batch.limit_file_extent(256 << 10).unwrap());
            let mut writer = Writer::new_at(location, &mut batch).unwrap();
            let error = writer.write_all(&vec![9; 2 << 20]).unwrap_err();
            // The archive layer may retain only text; recover the original type
            // from the writer without interpreting that text as an error code.
            let erased = error.to_string();
            assert!(!erased.is_empty());
            let source = writer.take_storage_error().unwrap();
            assert!(if growth { matches!(source, StoreError::GrowthLimit(_)) }
                else { matches!(source, StoreError::MapFull(_)) });
            assert!(writer.finish().is_err());
            assert!(batch.commit().is_err());
            drop(limit);
            let txn = store.tx_begin_read().unwrap();
            let mut reader = Reader::open_at(location, |key| store.meta_store().get_meta_txn(&txn, key)).unwrap().unwrap();
            let mut actual = Vec::new();
            reader.read_to_end(&mut actual).unwrap();
            assert_eq!(actual, original);
        }
    }
}
