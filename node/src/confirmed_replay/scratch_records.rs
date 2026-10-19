use super::scratch_index::ScratchIndex;
use bincode::Options;
use burst_store::StoreError;
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    marker::PhantomData,
    path::Path,
    sync::{Arc, Mutex},
};

/// Variable-size private rows. Both location and full digest are bound to the
/// key by authenticated indexes; no per-row offsets or digests stay in RAM.
/// Forks share append-only storage, but publish separate index roots.
pub(super) struct ScratchRecords<T> {
    storage: Arc<Mutex<Storage>>,
    locations: ScratchIndex,
    digests: ScratchIndex,
    row: PhantomData<fn() -> T>,
}

struct Storage {
    file: File,
    length: u64,
    largest: u64,
    failed: bool,
}

/// Ordered private records using the same authenticated storage as keyed replay.
/// Only the length stays in memory; failed appends never advance it.
pub(super) struct ScratchSequence<T> {
    records: ScratchRecords<T>,
    length: u64,
}

impl<T: Serialize + DeserializeOwned> ScratchSequence<T> {
    pub(super) fn new(directory: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            records: ScratchRecords::new(directory)?,
            length: 0,
        })
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

    pub(super) fn iter(&self) -> impl Iterator<Item = Result<T, StoreError>> + '_ {
        (0..self.length).map(|index| {
            self.records
                .get(&Self::key(index))?
                .ok_or_else(|| invalid("missing scratch sequence record"))
        })
    }
}

fn invalid(error: impl ToString) -> StoreError {
    StoreError::Corruption(error.to_string())
}

impl<T: Serialize + DeserializeOwned> ScratchRecords<T> {
    pub(super) fn new(directory: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            storage: Arc::new(Mutex::new(Storage {
                file: tempfile::tempfile_in(directory).map_err(invalid)?,
                length: 0,
                largest: 0,
                failed: false,
            })),
            locations: ScratchIndex::new(directory)?,
            digests: ScratchIndex::new(directory)?,
            row: PhantomData,
        })
    }

    pub(super) fn fork(&self) -> Result<Self, StoreError> {
        self.ready()?;
        Ok(Self {
            storage: Arc::clone(&self.storage),
            locations: self.locations.clone(),
            digests: self.digests.clone(),
            row: PhantomData,
        })
    }

    fn ready(&self) -> Result<(), StoreError> {
        self.storage.lock().map_err(invalid)?.ready()
    }

    #[cfg(test)]
    pub(super) fn bytes_for_test(&self) -> u64 {
        self.storage.lock().unwrap().length
    }

    #[cfg(test)]
    pub(super) fn truncate_for_test(&self) {
        self.storage.lock().unwrap().file.set_len(0).unwrap();
    }

    pub(super) fn get(&self, key: &[u8; 32]) -> Result<Option<T>, StoreError> {
        self.ready()?;
        let location = self.locations.get(key)?;
        let digest = self.digests.get(key)?;
        let (location, digest) = match (location, digest) {
            (None, None) => return Ok(None),
            (Some(location), Some(digest)) => (location, digest),
            _ => return Err(invalid("incomplete replay scratch record index")),
        };
        let offset = u64::from_be_bytes(location[..8].try_into().map_err(invalid)?);
        let length = u64::from_be_bytes(location[8..16].try_into().map_err(invalid)?);
        let mut storage = self.storage.lock().map_err(invalid)?;
        storage.ready()?;
        if location[16..] != [0; 16]
            || length > storage.largest
            || offset
                .checked_add(length)
                .is_none_or(|end| end > storage.length)
        {
            return Err(invalid("invalid replay scratch record location"));
        }
        let mut bytes = vec![0; usize::try_from(length).map_err(invalid)?];
        let file = &mut storage.file;
        file.seek(SeekFrom::Start(offset)).map_err(invalid)?;
        file.read_exact(&mut bytes).map_err(invalid)?;
        if burst_crypto::blake2b_256(&bytes) != digest {
            return Err(invalid("replay scratch record digest changed"));
        }
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .with_limit(length)
            .deserialize(&bytes)
            .map(Some)
            .map_err(invalid)
    }

    pub(super) fn visit(
        &self,
        mut visit: impl FnMut([u8; 32], T) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.ready()?;
        self.locations.visit(|key, _| {
            let row = self
                .get(&key)?
                .ok_or_else(|| invalid("missing replay scratch record"))?;
            visit(key, row)
        })
    }

    /// Publish both indexes only after the complete record has been written.
    pub(super) fn put(&mut self, key: [u8; 32], row: &T) -> Result<bool, StoreError> {
        self.ready()?;
        // An update must not conceal corruption in the record it replaces.
        let existing = self.get(&key)?.is_some();
        let prior = self.locations.get(&key)?;
        if prior.is_some() != self.digests.get(&key)?.is_some() {
            return Err(invalid("incomplete replay scratch record index"));
        }
        if prior.is_some() != existing {
            return Err(invalid("replay scratch record changed before update"));
        }
        let bytes = bincode::serialize(row).map_err(invalid)?;
        let length = bytes.len() as u64;
        let mut storage = self.storage.lock().map_err(invalid)?;
        storage.ready()?;
        let offset = storage.length;
        let end = storage
            .length
            .checked_add(length)
            .ok_or_else(|| invalid("replay scratch record overflow"))?;
        let mut location = [0; 32];
        location[..8].copy_from_slice(&offset.to_be_bytes());
        location[8..16].copy_from_slice(&length.to_be_bytes());
        let mut locations = self.locations.clone();
        let mut digests = self.digests.clone();
        storage.failed = true;
        let file = &mut storage.file;
        file.seek(SeekFrom::Start(offset)).map_err(invalid)?;
        file.write_all(&bytes).map_err(invalid)?;
        locations.insert(key, location)?;
        digests.insert(key, burst_crypto::blake2b_256(&bytes))?;
        self.locations = locations;
        self.digests = digests;
        storage.length = end;
        storage.largest = storage.largest.max(length);
        storage.failed = false;
        Ok(prior.is_some())
    }
}

impl Storage {
    fn ready(&self) -> Result<(), StoreError> {
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
        records.digests.insert([2; 32], [9; 32]).unwrap();
        assert!(records.get(&[2; 32]).is_err());
        assert!(records.put([2; 32], &8).is_err());
        assert_eq!(records.get(&[1; 32]).unwrap(), Some(7));
    }
}
