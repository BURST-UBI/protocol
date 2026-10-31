use super::scratch_index::{FixedScratchCursor, FixedScratchIndex, FixedSortedIndexBuilder};
use crate::confirmed_replay::scratch_file::ScratchFile as File;
use bincode::Options;
use burst_store::StoreError;
use serde::{de::DeserializeOwned, Serialize};
use std::{
    io::Write,
    marker::PhantomData,
    path::Path,
    sync::{Arc, Mutex},
};

#[cfg(test)]
use std::io::{Seek, SeekFrom};

/// Variable-size private rows. Both location and full digest are bound to the
/// key by one authenticated descriptor index. The live store keeps no per-row
/// descriptor table in memory.
/// Forks share append-only storage, but publish separate index roots.
pub(crate) struct ScratchRecords<T> {
    storage: Arc<Mutex<Storage>>,
    index: FixedScratchIndex<48>,
    row: PhantomData<fn() -> T>,
}

/// Traversal pinned to one index root. The cursor retains no decoded rows;
/// callbacks own delivered rows and may retain them.
pub(super) struct RecordsCursor<T> {
    records: ScratchRecords<T>,
    indexes: FixedScratchCursor<48>,
    pending: Option<([u8; 32], [u8; 32], [u8; 32])>,
    indexes_done: bool,
    failed: bool,
}

#[derive(Debug)]
pub(super) struct RecordsPage {
    pub nodes: usize,
    pub rows: usize,
    pub bytes: u64,
    pub done: bool,
}

impl<T: Serialize + DeserializeOwned> RecordsCursor<T> {
    pub(super) fn visit_next<E: From<StoreError>>(
        &mut self,
        max_nodes: usize,
        max_bytes: u64,
        mut visit: impl FnMut([u8; 32], T) -> Result<(), E>,
    ) -> Result<RecordsPage, E> {
        if self.failed {
            return Err(invalid("scratch records traversal already failed").into());
        }
        self.failed = true;
        if max_nodes == 0 || max_bytes == 0 {
            return Err(invalid("invalid scratch records traversal budget").into());
        }
        self.records.check_storage()?;
        let mut page = RecordsPage {
            nodes: 0,
            rows: 0,
            bytes: 0,
            done: false,
        };
        loop {
            if let Some((key, location, digest)) = self.pending {
                let length = u64::from_be_bytes(location[8..16].try_into().map_err(invalid)?);
                if length > max_bytes {
                    return Err(invalid("scratch record exceeds page byte budget").into());
                }
                if length > max_bytes - page.bytes {
                    break;
                }
                let row = self.records.read_indexed(location, digest)?;
                visit(key, row)?;
                self.pending = None;
                page.rows += 1;
                page.bytes += length;
            }
            if self.indexes_done || max_nodes == page.nodes {
                break;
            }
            // One node at a time avoids prefetching an unbounded number of rows
            // before their independently authenticated encoded sizes are known.
            let next = self.indexes.visit_next_with(1, |key, descriptor| {
                let (location, digest) = split_descriptor(descriptor);
                self.pending = Some((key, location, digest));
                Ok::<_, E>(())
            })?;
            page.nodes += next.nodes;
            self.indexes_done = next.done;
        }
        page.done = self.indexes_done && self.pending.is_none();
        self.failed = false;
        Ok(page)
    }
}

/// Bulk construction writes payloads to disk and keeps one descriptor per row
/// in memory for sorting at finish. max_bytes limits encoded payloads, excluding
/// descriptors and index storage. Rows cannot be read or forked before finish.
pub(super) struct ScratchRecordsBuilder<T> {
    records: ScratchRecords<T>,
    directory: std::path::PathBuf,
    entries: Vec<([u8; 32], [u8; 32], [u8; 32])>,
    max_rows: usize,
    max_bytes: u64,
}

impl<T: Serialize + DeserializeOwned> ScratchRecordsBuilder<T> {
    pub(super) fn new(
        directory: &Path,
        max_rows: usize,
        max_bytes: u64,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            records: ScratchRecords::new(directory)?,
            directory: directory.to_path_buf(),
            entries: Vec::new(),
            max_rows,
            max_bytes,
        })
    }

    pub(super) fn push(&mut self, key: [u8; 32], row: &T) -> Result<(), StoreError> {
        if self.entries.len() >= self.max_rows {
            return Err(invalid("bulk scratch records exceed row budget"));
        }
        let mut storage = self.records.storage.lock().map_err(invalid)?;
        storage.check_storage()?;
        let length = bincode::serialized_size(row).map_err(invalid)?;
        let end = storage
            .length
            .checked_add(length)
            .filter(|end| *end <= self.max_bytes)
            .ok_or_else(|| invalid("bulk scratch records exceed byte budget"))?;
        let bytes = bincode::serialize(row).map_err(invalid)?;
        if bytes.len() as u64 != length {
            return Err(invalid("bulk scratch record changed encoded length"));
        }
        let mut location = [0; 32];
        location[..8].copy_from_slice(&storage.length.to_be_bytes());
        location[8..16].copy_from_slice(&length.to_be_bytes());
        storage.failed = true;
        storage.file.write_all(&bytes).map_err(invalid)?;
        storage.length = end;
        storage.max_record_bytes = storage.max_record_bytes.max(length);
        storage.failed = false;
        self.entries
            .push((key, location, burst_crypto::blake2b_256(&bytes)));
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<ScratchRecords<T>, StoreError> {
        self.records.check_storage()?;
        self.entries.sort_unstable_by_key(|entry| entry.0);
        self.records.index = FixedScratchIndex::from_sorted(
            &self.directory,
            self.entries
                .iter()
                .map(|(key, location, digest)| (*key, descriptor(*location, *digest))),
        )?;
        Ok(self.records)
    }
}

/// Strictly ordered input avoids retaining a descriptor for every record.
/// The unfinished index edge is bounded by the fixed 256-bit key width.
pub(super) struct SortedRecordsBuilder<T> {
    records: ScratchRecords<T>,
    index: FixedSortedIndexBuilder<48>,
    count: usize,
    max_rows: usize,
    max_bytes: u64,
    failed: bool,
}

impl<T: Serialize + DeserializeOwned> SortedRecordsBuilder<T> {
    pub(super) fn new(
        directory: &Path,
        max_rows: usize,
        max_bytes: u64,
    ) -> Result<Self, StoreError> {
        Ok(Self {
            records: ScratchRecords::new(directory)?,
            index: FixedSortedIndexBuilder::new(directory)?,
            count: 0,
            max_rows,
            max_bytes,
            failed: false,
        })
    }

    pub(super) fn push(&mut self, key: [u8; 32], row: &T) -> Result<(), StoreError> {
        if self.failed {
            return Err(invalid("ordered record builder already failed"));
        }
        self.failed = true;
        if self.count >= self.max_rows {
            return Err(invalid("ordered records exceed row budget"));
        }
        let mut storage = self.records.storage.lock().map_err(invalid)?;
        storage.check_storage()?;
        let length = bincode::serialized_size(row).map_err(invalid)?;
        let end = storage
            .length
            .checked_add(length)
            .filter(|end| *end <= self.max_bytes)
            .ok_or_else(|| invalid("ordered records exceed byte budget"))?;
        let bytes = bincode::serialize(row).map_err(invalid)?;
        if bytes.len() as u64 != length {
            return Err(invalid("ordered record changed encoded length"));
        }
        let mut location = [0; 32];
        location[..8].copy_from_slice(&storage.length.to_be_bytes());
        location[8..16].copy_from_slice(&length.to_be_bytes());
        storage.failed = true;
        storage.file.write_all(&bytes).map_err(invalid)?;
        storage.length = end;
        storage.max_record_bytes = storage.max_record_bytes.max(length);
        storage.failed = false;
        drop(storage);
        self.index
            .push(key, descriptor(location, burst_crypto::blake2b_256(&bytes)))?;
        self.count += 1;
        self.failed = false;
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<ScratchRecords<T>, StoreError> {
        if self.failed {
            return Err(invalid("ordered record builder already failed"));
        }
        self.records.check_storage()?;
        self.records.index = self.index.finish()?;
        Ok(self.records)
    }
}

struct Storage {
    file: File,
    length: u64,
    max_record_bytes: u64,
    failed: bool,
}

/// Ordered private records using the same authenticated storage as keyed replay.
/// Sequence positions use the disk index; the length advances only after a
/// successful append.
pub(super) struct ScratchSequence<T> {
    records: ScratchRecords<T>,
    length: u64,
}

/// Private append-only construction reuses the ordered record builder instead
/// of copying an index path for every sequence position. Reads begin at finish.
pub(super) struct ScratchSequenceBuilder<T> {
    records: SortedRecordsBuilder<T>,
    length: u64,
}

impl<T: Serialize + DeserializeOwned> ScratchSequenceBuilder<T> {
    pub(super) fn push(&mut self, record: &T) -> Result<(), StoreError> {
        let next = self
            .length
            .checked_add(1)
            .ok_or_else(|| invalid("scratch sequence overflow"))?;
        self.records
            .push(ScratchSequence::<T>::key(self.length), record)?;
        self.length = next;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> u64 {
        self.length
    }

    pub(super) fn finish(self) -> Result<ScratchSequence<T>, StoreError> {
        Ok(ScratchSequence {
            records: self.records.finish()?,
            length: self.length,
        })
    }
}

impl<T: Serialize + DeserializeOwned> ScratchSequence<T> {
    pub(super) fn builder(
        directory: &Path,
        max_rows: usize,
        max_bytes: u64,
    ) -> Result<ScratchSequenceBuilder<T>, StoreError> {
        Ok(ScratchSequenceBuilder {
            records: SortedRecordsBuilder::new(directory, max_rows, max_bytes)?,
            length: 0,
        })
    }

    pub(super) fn new(directory: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            records: ScratchRecords::new(directory)?,
            length: 0,
        })
    }

    pub(super) fn fork(&self) -> Result<Self, StoreError> {
        Ok(Self {
            records: self.records.fork()?,
            length: self.length,
        })
    }

    pub(super) fn check_storage(&self) -> Result<(), StoreError> {
        self.records.check_storage()
    }

    #[cfg(test)]
    pub(super) fn truncate_for_test(&self) {
        self.records.truncate_for_test();
    }

    fn key(index: u64) -> [u8; 32] {
        let mut key = [0; 32];
        key[24..].copy_from_slice(&index.to_be_bytes());
        key
    }

    pub(super) fn push(&mut self, record: &T) -> Result<(), StoreError> {
        let next = self
            .length
            .checked_add(1)
            .ok_or_else(|| invalid("scratch sequence overflow"))?;
        if self.records.put(Self::key(self.length), record)? {
            return Err(invalid("scratch sequence overwrote a record"));
        }
        self.length = next;
        Ok(())
    }

    pub(super) fn len(&self) -> u64 {
        self.length
    }

    pub(super) fn get(&self, index: u64) -> Result<T, StoreError> {
        if index >= self.length {
            return Err(invalid("scratch sequence position out of bounds"));
        }
        self.records
            .get(&Self::key(index))?
            .ok_or_else(|| invalid("missing scratch sequence record"))
    }

    pub(super) fn iter(&self) -> SequenceIter<T> {
        SequenceIter {
            cursor: Some(self.records.cursor()),
            position: 0,
            length: self.length,
        }
    }
}

/// Owns a pinned index root so a resumable consumer need not retain the sequence.
pub(super) struct SequenceIter<T> {
    cursor: Option<Result<RecordsCursor<T>, StoreError>>,
    position: u64,
    length: u64,
}

impl<T: Serialize + DeserializeOwned> Iterator for SequenceIter<T> {
    type Item = Result<T, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        let result = (|| {
            let Some(state) = self.cursor.as_mut() else {
                return Ok(None);
            };
            let scan = match state {
                Ok(scan) => scan,
                Err(error) => return Err(error.clone()),
            };
            loop {
                let mut row = None;
                let page = scan.visit_next::<StoreError>(1, u64::MAX, |key, value| {
                    if self.position >= self.length
                        || key != ScratchSequence::<T>::key(self.position)
                    {
                        return Err(invalid("scratch sequence traversal differs from position"));
                    }
                    self.position += 1;
                    row = Some(value);
                    Ok(())
                })?;
                if page.done && self.position != self.length {
                    return Err(invalid("incomplete scratch sequence traversal"));
                }
                if page.done || row.is_some() {
                    return Ok(row);
                }
            }
        })();
        match result {
            Ok(Some(row)) => Some(Ok(row)),
            Ok(None) => {
                self.cursor = None;
                None
            }
            Err(error) => {
                self.cursor = None;
                Some(Err(error))
            }
        }
    }
}

// Offset and length occupy 16 bytes; the full 32-byte content digest follows.
fn descriptor(location: [u8; 32], digest: [u8; 32]) -> [u8; 48] {
    let mut value = [0; 48];
    value[..16].copy_from_slice(&location[..16]);
    value[16..].copy_from_slice(&digest);
    value
}

fn split_descriptor(value: [u8; 48]) -> ([u8; 32], [u8; 32]) {
    let mut location = [0; 32];
    let mut digest = [0; 32];
    location[..16].copy_from_slice(&value[..16]);
    digest.copy_from_slice(&value[16..]);
    (location, digest)
}

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}

impl<T: Serialize + DeserializeOwned> ScratchRecords<T> {
    pub(crate) fn new(directory: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            storage: Arc::new(Mutex::new(Storage {
                file: File::new_in(directory).map_err(invalid)?,
                length: 0,
                max_record_bytes: 0,
                failed: false,
            })),
            index: FixedScratchIndex::new(directory)?,
            row: PhantomData,
        })
    }

    /// Write payloads, sort up to max_rows descriptors in memory, then build
    /// the index. max_bytes limits payload bytes, not total memory or disk use.
    #[cfg(test)]
    pub(super) fn from_rows(
        directory: &Path,
        rows: impl IntoIterator<Item = ([u8; 32], T)>,
        max_rows: usize,
        max_bytes: u64,
    ) -> Result<Self, StoreError> {
        let mut builder = ScratchRecordsBuilder::new(directory, max_rows, max_bytes)?;
        for (key, row) in rows {
            builder.push(key, &row)?;
        }
        builder.finish()
    }

    pub(super) fn fork(&self) -> Result<Self, StoreError> {
        self.check_storage()?;
        Ok(Self {
            storage: Arc::clone(&self.storage),
            index: self.index.clone(),
            row: PhantomData,
        })
    }

    fn check_storage(&self) -> Result<(), StoreError> {
        self.storage.lock().map_err(invalid)?.check_storage()
    }

    #[cfg(test)]
    pub(super) fn bytes_for_test(&self) -> u64 {
        self.storage.lock().unwrap().length
    }

    #[cfg(test)]
    pub(crate) fn truncate_for_test(&self) {
        self.storage.lock().unwrap().file.set_len(0).unwrap();
    }

    pub(crate) fn get(&self, key: &[u8; 32]) -> Result<Option<T>, StoreError> {
        self.check_storage()?;
        let Some(value) = self.index.get(key)? else {
            return Ok(None);
        };
        let (location, digest) = split_descriptor(value);
        self.read_indexed(location, digest).map(Some)
    }

    fn read_indexed(&self, location: [u8; 32], digest: [u8; 32]) -> Result<T, StoreError> {
        let offset = u64::from_be_bytes(location[..8].try_into().map_err(invalid)?);
        let length = u64::from_be_bytes(location[8..16].try_into().map_err(invalid)?);
        let mut storage = self.storage.lock().map_err(invalid)?;
        storage.check_storage()?;
        if location[16..] != [0; 16]
            || length > storage.max_record_bytes
            || offset
                .checked_add(length)
                .is_none_or(|end| end > storage.length)
        {
            return Err(invalid("invalid replay scratch record location"));
        }
        let mut bytes = vec![0; usize::try_from(length).map_err(invalid)?];
        let file = &mut storage.file;
        file.read_exact_at(&mut bytes, offset).map_err(invalid)?;
        if burst_crypto::blake2b_256(&bytes) != digest {
            return Err(invalid("replay scratch record digest changed"));
        }
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .with_limit(length)
            .deserialize(&bytes)
            .map_err(invalid)
    }

    pub(super) fn visit(
        &self,
        visit: impl FnMut([u8; 32], T) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.visit_with(visit)
    }

    pub(super) fn cursor(&self) -> Result<RecordsCursor<T>, StoreError> {
        Ok(RecordsCursor {
            records: self.fork()?,
            indexes: self.index.cursor()?,
            pending: None,
            indexes_done: false,
            failed: false,
        })
    }

    pub(super) fn cursor_after(&self, after: [u8; 32]) -> Result<RecordsCursor<T>, StoreError> {
        Ok(RecordsCursor {
            records: self.fork()?,
            indexes: self.index.cursor_after(after)?,
            pending: None,
            indexes_done: false,
            failed: false,
        })
    }

    pub(super) fn visit_with<E: From<StoreError>>(
        &self,
        mut visit: impl FnMut([u8; 32], T) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut cursor = self.cursor()?;
        while !cursor.visit_next(1024, u64::MAX, &mut visit)?.done {}
        Ok(())
    }

    /// Append the record and publish its descriptor. Replaced bytes remain until
    /// all storage owners drop. Failure after writing begins disables every fork.
    pub(crate) fn put(&mut self, key: [u8; 32], row: &T) -> Result<bool, StoreError> {
        self.check_storage()?;
        // An update must not conceal corruption in the record it replaces.
        let existing = self.get(&key)?.is_some();
        let prior = self.index.get(&key)?;
        if prior.is_some() != existing {
            return Err(invalid("replay scratch record changed before update"));
        }
        let bytes = bincode::serialize(row).map_err(invalid)?;
        let length = bytes.len() as u64;
        let mut storage = self.storage.lock().map_err(invalid)?;
        storage.check_storage()?;
        let offset = storage.length;
        let end = storage
            .length
            .checked_add(length)
            .ok_or_else(|| invalid("replay scratch record overflow"))?;
        let mut location = [0; 32];
        location[..8].copy_from_slice(&offset.to_be_bytes());
        location[8..16].copy_from_slice(&length.to_be_bytes());
        let mut index = self.index.clone();
        storage.failed = true;
        let file = &mut storage.file;
        file.write_all_at(&bytes, offset).map_err(invalid)?;
        index.insert(key, descriptor(location, burst_crypto::blake2b_256(&bytes)))?;
        self.index = index;
        storage.length = end;
        storage.max_record_bytes = storage.max_record_bytes.max(length);
        storage.failed = false;
        Ok(prior.is_some())
    }
}

impl Storage {
    fn check_storage(&self) -> Result<(), StoreError> {
        if self.failed || self.file.metadata().map_err(invalid)?.len() != self.length {
            return Err(invalid("replay scratch records changed or a write failed"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_records_preserve_replay_order_and_propagate_storage_failure() {
        let directory = tempfile::tempdir().unwrap();
        let mut records = ScratchSequence::new(directory.path()).unwrap();
        assert_eq!(records.iter().count(), 0);
        let expected = [vec![9; 17], vec![1; 3], vec![9; 17]];
        for record in &expected {
            records.push(record).unwrap();
        }
        for _ in 0..2 {
            assert_eq!(
                records.iter().collect::<Result<Vec<_>, _>>().unwrap(),
                expected
            );
        }
        records.records.truncate_for_test();
        assert!(records.iter().next().unwrap().is_err());
        assert!(records.push(&vec![4]).is_err());
        assert_eq!(records.length, 3);
    }

    #[test]
    fn bulk_sequence_preserves_order_forks_bounds_and_shared_budget() {
        use crate::confirmed_replay::scratch_file::ScratchLimit;
        let directory = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(directory.path(), 1 << 20).unwrap();
        let mut builder = ScratchSequence::builder(directory.path(), 4096, 4096 * 8).unwrap();
        for value in 0u64..4096 {
            assert_eq!(builder.len(), value);
            builder.push(&value).unwrap();
        }
        let mut sequence = builder.finish().unwrap();
        assert_eq!(sequence.len(), 4096);
        assert_eq!(
            sequence.iter().collect::<Result<Vec<_>, _>>().unwrap(),
            (0..4096).collect::<Vec<u64>>()
        );
        assert!(sequence.get(4096).is_err());
        let mut fork = sequence.fork().unwrap();
        sequence.push(&9000).unwrap();
        fork.push(&9001).unwrap();
        assert_eq!(sequence.get(4096).unwrap(), 9000);
        assert_eq!(fork.get(4096).unwrap(), 9001);
        drop(sequence);
        fork.truncate_for_test();
        assert!(fork.get(0).is_err());
        drop(fork);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);

        for (rows, bytes) in [(0, 8), (1, 7)] {
            let mut builder = ScratchSequence::builder(directory.path(), rows, bytes).unwrap();
            assert!(builder.push(&1u64).is_err());
            assert_eq!(builder.len(), 0);
            assert!(builder.push(&2u64).is_err());
            assert!(builder.finish().is_err());
            assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
        }
        let empty = ScratchSequence::<u64>::builder(directory.path(), 0, 0)
            .unwrap()
            .finish()
            .unwrap();
        assert_eq!(empty.len(), 0);
        assert!(empty.get(0).is_err());
    }

    #[test]
    fn sequence_iteration_rejects_missing_extra_and_corrupt_rows() {
        let directory = tempfile::tempdir().unwrap();
        for (keys, length) in [(vec![0, 2], 2), (vec![0], 2), (vec![0, 1], 1)] {
            let mut sequence = ScratchSequence::<u64>::new(directory.path()).unwrap();
            for key in keys {
                sequence
                    .records
                    .put(ScratchSequence::<u64>::key(key), &key)
                    .unwrap();
            }
            sequence.length = length;
            let mut rows = sequence.iter();
            assert!(rows.by_ref().any(|row| row.is_err()));
            assert!(
                rows.next().is_none(),
                "failed iteration must stay exhausted"
            );
        }
        let mut sequence = ScratchSequence::new(directory.path()).unwrap();
        sequence.push(&42u64).unwrap();
        let mut rows = sequence.iter();
        sequence.truncate_for_test();
        assert!(rows.next().unwrap().is_err());
        assert!(rows.next().is_none());
        let empty = ScratchSequence::<u64>::new(directory.path()).unwrap();
        assert!(empty.iter().next().is_none());
    }

    #[test]
    fn sequence_forks_share_bytes_and_keep_independent_order() {
        let directory = tempfile::tempdir().unwrap();
        let mut parent = ScratchSequence::new(directory.path()).unwrap();
        for value in 0u64..128 {
            parent.push(&value).unwrap();
        }
        assert_eq!(parent.len(), 128);
        assert_eq!(parent.get(127).unwrap(), 127);
        assert!(parent.get(128).is_err());
        let pinned = parent.iter();
        let bytes = parent.records.bytes_for_test();
        let mut child = parent.fork().unwrap();
        assert!(Arc::ptr_eq(&parent.records.storage, &child.records.storage));
        assert_eq!(parent.records.bytes_for_test(), bytes);
        parent.push(&900).unwrap();
        child.push(&901).unwrap();
        let expected: Vec<_> = (0..128).chain([900]).collect();
        assert_eq!(
            parent.iter().collect::<Result<Vec<_>, _>>().unwrap(),
            expected
        );
        drop(parent);
        assert_eq!(
            pinned.collect::<Result<Vec<_>, _>>().unwrap(),
            (0..128).collect::<Vec<u64>>()
        );
        let expected: Vec<_> = (0..128).chain([901]).collect();
        assert_eq!(
            child.iter().collect::<Result<Vec<_>, _>>().unwrap(),
            expected
        );
        child.truncate_for_test();
        assert!(child.fork().is_err());
        assert!(child.check_storage().is_err());
        assert!(child.get(0).is_err());
    }

    #[test]
    fn forked_records_keep_replacements_and_appends_private() {
        let directory = tempfile::tempdir().unwrap();
        let mut parent = ScratchRecords::<Vec<u8>>::new(directory.path()).unwrap();
        parent.put([1; 32], &vec![1, 2, 3]).unwrap();
        let mut child = parent.fork().unwrap();
        assert!(Arc::ptr_eq(&parent.storage, &child.storage));
        assert!(child.put([1; 32], &vec![4]).unwrap());
        assert!(!child.put([2; 32], &vec![5]).unwrap());
        assert_eq!(parent.get(&[1; 32]).unwrap(), Some(vec![1, 2, 3]));
        assert_eq!(parent.get(&[2; 32]).unwrap(), None);
        parent.put([3; 32], &vec![6]).unwrap();
        assert_eq!(child.get(&[3; 32]).unwrap(), None);
        drop(parent);
        assert_eq!(child.get(&[1; 32]).unwrap(), Some(vec![4]));
        assert_eq!(child.get(&[2; 32]).unwrap(), Some(vec![5]));
    }

    #[test]
    fn concurrent_forks_append_without_copying_or_exposing_sibling_rows() {
        let directory = tempfile::tempdir().unwrap();
        let mut parent = ScratchRecords::<u64>::new(directory.path()).unwrap();
        parent.put([0; 32], &7).unwrap();
        let left = parent.fork().unwrap();
        let right = parent.fork().unwrap();
        let write = |mut branch: ScratchRecords<u64>, value| {
            for key in 1..=64u8 {
                assert!(!branch.put([key; 32], &value).unwrap());
            }
            branch
        };
        let (left, right) = std::thread::scope(|scope| {
            let left = scope.spawn(|| write(left, 11));
            let right = scope.spawn(|| write(right, 22));
            (left.join().unwrap(), right.join().unwrap())
        });
        assert!(Arc::ptr_eq(&parent.storage, &left.storage));
        assert!(Arc::ptr_eq(&left.storage, &right.storage));
        for key in 1..=64u8 {
            assert_eq!(parent.get(&[key; 32]).unwrap(), None);
            assert_eq!(left.get(&[key; 32]).unwrap(), Some(11));
            assert_eq!(right.get(&[key; 32]).unwrap(), Some(22));
        }
        assert_eq!(parent.storage.lock().unwrap().length, 129 * 8);
        parent.truncate_for_test();
        assert!(parent.get(&[0; 32]).is_err());
        assert!(left.get(&[1; 32]).is_err());
        assert!(right.get(&[255; 32]).is_err());
        assert!(parent.fork().is_err());
    }

    #[test]
    fn variable_rows_replace_without_retaining_an_offset_map() {
        let directory = tempfile::tempdir().unwrap();
        let mut records = ScratchRecords::<Vec<u8>>::new(directory.path()).unwrap();
        for index in 0u32..512 {
            let key = burst_crypto::blake2b_256(&index.to_be_bytes());
            assert!(!records
                .put(key, &vec![index as u8; index as usize + 1])
                .unwrap());
        }
        for index in (0u32..512).rev() {
            let key = burst_crypto::blake2b_256(&index.to_be_bytes());
            assert_eq!(
                records.get(&key).unwrap(),
                Some(vec![index as u8; index as usize + 1])
            );
            assert!(records.put(key, &vec![7; 3]).unwrap());
            assert_eq!(records.get(&key).unwrap(), Some(vec![7; 3]));
        }
        assert_eq!(records.get(&[255; 32]).unwrap(), None);
        drop(records);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn damaged_records_cannot_be_overwritten_or_reported_as_missing() {
        let directory = tempfile::tempdir().unwrap();
        let mut records = ScratchRecords::<Vec<u8>>::new(directory.path()).unwrap();
        records.put([1; 32], &vec![1, 2, 3]).unwrap();
        {
            let mut storage = records.storage.lock().unwrap();
            let file = &mut storage.file;
            file.seek(SeekFrom::End(-1)).unwrap();
            file.write_all(&[99]).unwrap();
        }
        assert!(records.get(&[1; 32]).is_err());
        assert!(records.put([1; 32], &vec![4]).is_err());
        assert_eq!(records.get(&[2; 32]).unwrap(), None);
        records.truncate_for_test();
        assert!(records.get(&[2; 32]).is_err());
        assert!(records.put([2; 32], &vec![4]).is_err());
    }

    #[test]
    fn mismatched_record_indexes_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let mut records = ScratchRecords::<u64>::new(directory.path()).unwrap();
        records.put([1; 32], &7).unwrap();
        records.index.insert([2; 32], [9; 48]).unwrap();
        assert!(records.get(&[2; 32]).is_err());
        assert!(records.visit(|_, _| Ok(())).is_err());
        assert!(records.put([2; 32], &8).is_err());
        assert_eq!(records.get(&[1; 32]).unwrap(), Some(7));
    }
}

#[cfg(test)]
mod bulk_tests {
    use super::*;

    #[test]
    fn single_descriptor_tree_preserves_full_digest_with_bounded_storage() {
        let directory = tempfile::tempdir().unwrap();
        let limit =
            crate::confirmed_replay::scratch_file::ScratchLimit::acquire(directory.path(), 1 << 20)
                .unwrap();
        let rows: Vec<_> = (0u64..4096)
            .map(|n| (burst_crypto::blake2b_256(&n.to_be_bytes()), n))
            .collect();
        let mut records =
            ScratchRecords::from_rows(directory.path(), rows.clone(), rows.len(), 4096 * 8)
                .unwrap();
        assert_eq!(
            limit.used_temporary_bytes().unwrap(),
            4096 * (186 + 8) - 102
        );
        for (key, value) in &rows {
            assert_eq!(records.get(key).unwrap(), Some(*value));
        }
        let pinned = records.fork().unwrap();
        records.put(rows[0].0, &9000).unwrap();
        assert_eq!(pinned.get(&rows[0].0).unwrap(), Some(0));
        assert_eq!(records.get(&rows[0].0).unwrap(), Some(9000));
        // Every byte of the digest participates in payload validation.
        for byte in 16..48 {
            let mut corrupt = pinned.fork().unwrap();
            let mut descriptor = corrupt.index.get(&rows[1].0).unwrap().unwrap();
            descriptor[byte] ^= 1;
            corrupt.index.insert(rows[1].0, descriptor).unwrap();
            assert!(corrupt.get(&rows[1].0).is_err());
        }
        records.index.truncate_for_test();
        assert!(records.get(&rows[0].0).is_err());
        assert!(pinned.get(&rows[0].0).is_err());
        drop(records);
        drop(pinned);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    }

    #[test]
    fn bulk_records_preserve_rows_and_updates_and_reject_duplicates_budgets_and_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let rows: Vec<_> = (0u8..64)
            .rev()
            .map(|i| ([i; 32], vec![i; i as usize + 1]))
            .collect();
        let mut records =
            ScratchRecords::from_rows(directory.path(), rows.clone(), 64, 4096).unwrap();
        for (key, value) in &rows {
            assert_eq!(records.get(key).unwrap().as_ref(), Some(value));
        }
        let mut visited = std::collections::BTreeMap::new();
        records
            .visit(|key, value| {
                assert!(visited.insert(key, value).is_none());
                Ok(())
            })
            .unwrap();
        assert_eq!(visited, rows.iter().cloned().collect());
        assert!(records.visit(|_, _| Err(invalid("stop"))).is_err());
        let child = records.fork().unwrap();
        records.put([0; 32], &vec![99]).unwrap();
        assert_eq!(child.get(&[0; 32]).unwrap(), Some(vec![0]));
        assert_eq!(records.get(&[0; 32]).unwrap(), Some(vec![99]));
        assert!(ScratchRecords::from_rows(directory.path(), rows.clone(), 63, 4096).is_err());
        assert!(ScratchRecords::from_rows(directory.path(), rows.clone(), 64, 8).is_err());
        assert!(ScratchRecords::from_rows(
            directory.path(),
            [rows[0].clone(), rows[0].clone()],
            2,
            4096
        )
        .is_err());
        let mut storage = records.storage.lock().unwrap();
        storage.file.rewind().unwrap();
        storage.file.write_all(&[255]).unwrap();
        drop(storage);
        assert!(records.get(&rows[0].0).is_err());
        assert!(child.get(&rows[0].0).is_err());
        assert!(records.visit(|_, _| Ok(())).is_err());
        assert!(child.visit(|_, _| Ok(())).is_err());
    }
}

#[cfg(test)]
mod cursor_tests {
    use super::*;

    fn key(n: u8) -> [u8; 32] {
        [n; 32]
    }

    #[test]
    fn record_pages_bound_nodes_and_bytes_without_rescans_or_fork_leaks() {
        let directory = tempfile::tempdir().unwrap();
        let rows: Vec<_> = (0..19u8)
            .map(|n| (key(n), vec![n; 1 + usize::from(n) * 3]))
            .collect();
        let mut records =
            ScratchRecords::from_rows(directory.path(), rows.clone(), 19, 4096).unwrap();
        let mut cursor = records.cursor().unwrap();
        let mut single = records.cursor().unwrap();
        let mut singles = Vec::new();
        loop {
            let page = single
                .visit_next::<StoreError>(1, 4096, |key, row| {
                    singles.push((key, row));
                    Ok(())
                })
                .unwrap();
            assert!(page.nodes <= 1 && page.rows <= 1);
            if page.done {
                break;
            }
        }
        assert_eq!(singles, rows);
        records.put(key(0), &vec![99; 70]).unwrap();
        records.put(key(99), &vec![99]).unwrap();
        let mut output = Vec::new();
        let mut nodes = 0;
        let mut bytes = 0;
        loop {
            let page = cursor
                .visit_next::<StoreError>(5, 70, |key, row| {
                    output.push((key, row));
                    Ok(())
                })
                .unwrap();
            assert!(page.nodes <= 5 && page.bytes <= 70);
            nodes += page.nodes;
            bytes += page.bytes;
            if page.done {
                break;
            }
            assert!(page.nodes > 0 || page.rows > 0);
        }
        assert_eq!(output, rows);
        assert_eq!(nodes, 2 * rows.len() - 1);
        assert_eq!(
            bytes,
            rows.iter()
                .map(|(_, row)| bincode::serialized_size(row).unwrap())
                .sum::<u64>()
        );
        let end = cursor
            .visit_next::<StoreError>(2, 70, |_, _| panic!("exhausted"))
            .unwrap();
        assert!(end.done && end.nodes == 0 && end.rows == 0 && end.bytes == 0);
    }

    #[test]
    fn record_pages_reject_oversize_callbacks_index_mismatch_and_payload_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let mut records =
            ScratchRecords::from_rows(directory.path(), [(key(1), vec![1u8; 32])], 1, 4096)
                .unwrap();
        let mut cursor = records.cursor().unwrap();
        assert!(cursor
            .visit_next::<StoreError>(2, 8, |_, _| panic!("oversized row decoded"))
            .is_err());
        assert!(cursor
            .visit_next::<StoreError>(2, 4096, |_, _| Ok(()))
            .is_err());
        let mut cursor = records.cursor().unwrap();
        assert!(cursor
            .visit_next::<StoreError>(2, 4096, |_, _| Err(invalid("callback")))
            .is_err());
        assert!(cursor
            .visit_next::<StoreError>(2, 4096, |_, _| Ok(()))
            .is_err());
        let mut corrupt = records.cursor().unwrap();
        {
            let mut storage = records.storage.lock().unwrap();
            storage.file.seek(SeekFrom::Start(8)).unwrap();
            storage.file.write_all(&[255]).unwrap();
        }
        assert!(corrupt
            .visit_next::<StoreError>(2, 4096, |_, _| panic!("corrupt bytes"))
            .is_err());
        assert!(corrupt
            .visit_next::<StoreError>(2, 4096, |_, _| Ok(()))
            .is_err());
        records.index =
            FixedScratchIndex::from_sorted(directory.path(), [(key(2), [3; 48])]).unwrap();
        let mut mismatch = records.cursor().unwrap();
        assert!(mismatch
            .visit_next::<StoreError>(2, 4096, |_, _| panic!("foreign index key"))
            .is_err());
        assert!(mismatch
            .visit_next::<StoreError>(2, 4096, |_, _| Ok(()))
            .is_err());
    }
}

#[cfg(test)]
mod ordered_tests {
    use super::*;

    #[test]
    fn ordered_builder_preserves_rows_and_poisons_invalid_or_incomplete_builds() {
        let directory = tempfile::tempdir().unwrap();
        let rows: Vec<_> = (0..64u8)
            .map(|n| ([n; 32], vec![n; usize::from(n) + 1]))
            .collect();
        let bytes = rows
            .iter()
            .map(|(_, row)| bincode::serialized_size(row).unwrap())
            .sum();
        let mut builder = SortedRecordsBuilder::new(directory.path(), rows.len(), bytes).unwrap();
        for (key, row) in &rows {
            builder.push(*key, row).unwrap();
        }
        let records = builder.finish().unwrap();
        let mut actual = Vec::new();
        records
            .visit(|key, row| {
                actual.push((key, row));
                Ok(())
            })
            .unwrap();
        assert_eq!(actual, rows);
        for (key, row) in &rows {
            assert_eq!(records.get(key).unwrap(), Some(row.clone()));
        }
        for after in [[0; 32], [17; 32], [63; 32], [255; 32]] {
            let mut cursor = records.cursor_after(after).unwrap();
            let mut selected = Vec::new();
            while !cursor
                .visit_next::<StoreError>(7, 80, |key, row| {
                    selected.push((key, row));
                    Ok(())
                })
                .unwrap()
                .done
            {}
            assert_eq!(
                selected,
                rows.iter()
                    .filter(|(key, _)| *key > after)
                    .cloned()
                    .collect::<Vec<_>>()
            );
        }
        for (first, next, limit, byte_limit) in [
            (2, 2, 2, 4096),
            (2, 1, 2, 4096),
            (1, 2, 1, 4096),
            (1, 2, 2, 9),
        ] {
            let mut builder =
                SortedRecordsBuilder::new(directory.path(), limit, byte_limit).unwrap();
            builder.push([first; 32], &vec![1u8]).unwrap();
            assert!(builder.push([next; 32], &vec![2u8]).is_err());
            assert!(builder.push([3; 32], &vec![3u8]).is_err());
            assert!(
                builder.finish().is_err(),
                "failed ordered input must not publish a prefix"
            );
        }
        let empty = SortedRecordsBuilder::<Vec<u8>>::new(directory.path(), 0, 0)
            .unwrap()
            .finish()
            .unwrap();
        empty.visit(|_, _| panic!("empty builder")).unwrap();
        let mut broken = SortedRecordsBuilder::new(directory.path(), 2, 4096).unwrap();
        broken.push([1; 32], &vec![1u8]).unwrap();
        broken.records.truncate_for_test();
        assert!(broken.finish().is_err());
    }
}
