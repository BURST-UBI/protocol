use super::{invalid, StoreError, MAX_BYTES};
use crate::confirmed_replay::scratch_records::{ScratchSequence, ScratchSequenceBuilder, SequenceIter};
use serde::{Serialize, de::DeserializeOwned};
use std::path::Path;

const PAGE_ROWS: u64 = 64;

/// Walk rows stored in pages of 64, except for the final partial page.
/// Iteration checks page lengths and the complete row count.
pub(super) struct WalkSequence<T> {
    pages: ScratchSequence<Vec<T>>,
    length: u64,
}

pub(super) struct Builder<T> {
    pages: ScratchSequenceBuilder<Vec<T>>,
    pending: Vec<T>,
    length: u64,
    max_rows: u64,
    bytes: u64,
    max_bytes: u64,
    page_bytes: u64,
    failed: bool,
}

impl<T: Serialize + DeserializeOwned + Clone> Builder<T> {
    pub(super) fn push(&mut self, row: &T) -> Result<(), StoreError> {
        if self.failed { return Err(invalid("walk sequence builder already failed")); }
        self.failed = true;
        if self.length >= self.max_rows { return Err(invalid("walk sequence exceeds row budget")); }
        let length = bincode::serialized_size(row).map_err(invalid)?;
        self.page_bytes = self.page_bytes.checked_add(length)
            .filter(|bytes| *bytes <= MAX_BYTES as u64)
            .ok_or_else(|| invalid("walk sequence page exceeds byte budget"))?;
        let header = if self.pending.is_empty() { 8 } else { 0 };
        self.bytes = self.bytes.checked_add(length).and_then(|bytes| bytes.checked_add(header))
            .filter(|bytes| *bytes <= self.max_bytes)
            .ok_or_else(|| invalid("walk sequence exceeds byte budget"))?;
        self.pending.push(row.clone());
        if self.pending.len() == PAGE_ROWS as usize { self.flush()?; }
        self.length += 1;
        self.failed = false;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), StoreError> {
        if !self.pending.is_empty() {
            self.pages.push(&self.pending)?;
            self.pending.clear();
            self.page_bytes = 8;
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<WalkSequence<T>, StoreError> {
        if self.failed { return Err(invalid("walk sequence builder already failed")); }
        self.flush()?;
        let pages = self.pages.finish()?;
        if pages.len() != self.length.div_ceil(PAGE_ROWS) { return Err(invalid("walk sequence page count differs")); }
        Ok(WalkSequence { pages, length: self.length })
    }
}

impl<T: Serialize + DeserializeOwned + Clone> WalkSequence<T> {
    pub(super) fn builder(directory: &Path, max_rows: usize, max_bytes: u64) -> Result<Builder<T>, StoreError> {
        Ok(Builder { pages: ScratchSequence::builder(directory, max_rows.div_ceil(PAGE_ROWS as usize), max_bytes)?,
            pending: Vec::new(), length: 0, max_rows: max_rows as u64, bytes: 0, max_bytes,
            page_bytes: 8, failed: false })
    }

    #[cfg(test)]
    pub(super) fn get(&self, position: u64) -> Result<T, StoreError> {
        if position >= self.length { return Err(invalid("walk sequence position out of bounds")); }
        let index = position / PAGE_ROWS;
        let page = self.pages.get(index)?;
        if page.len() as u64 != (self.length - index * PAGE_ROWS).min(PAGE_ROWS) {
            return Err(invalid("walk sequence page length differs"));
        }
        page.into_iter().nth((position % PAGE_ROWS) as usize)
            .ok_or_else(|| invalid("walk sequence position is missing"))
    }

    pub(super) fn iter(&self) -> WalkSequenceIter<T> {
        WalkSequenceIter { pages: self.pages.iter(), pending: Vec::new().into_iter(),
            position: 0, length: self.length, done: false,
            #[cfg(test)] pages_read: 0 }
    }

    #[cfg(test)]
    pub(super) fn truncate_for_test(&self) { self.pages.truncate_for_test(); }
}

pub(super) struct WalkSequenceIter<T> {
    pages: SequenceIter<Vec<T>>,
    pending: std::vec::IntoIter<T>,
    position: u64,
    length: u64,
    done: bool,
    #[cfg(test)]
    pages_read: u64,
}

#[cfg(test)]
impl<T> WalkSequenceIter<T> {
    pub(super) fn pages_read(&self) -> u64 { self.pages_read }
}

impl<T: Serialize + DeserializeOwned> Iterator for WalkSequenceIter<T> {
    type Item = Result<T, StoreError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done { return None; }
        if let Some(row) = self.pending.next() { self.position += 1; return Some(Ok(row)); }
        let result = match self.pages.next() {
            Some(Ok(page)) => {
                #[cfg(test)] { self.pages_read += 1; }
                if self.position >= self.length || page.len() as u64 != (self.length - self.position).min(PAGE_ROWS) {
                    Err(invalid("walk sequence traversal page length differs"))
                } else { self.pending = page.into_iter(); return self.next(); }
            }
            Some(Err(error)) => Err(error),
            None if self.position != self.length => Err(invalid("incomplete walk sequence traversal")),
            None => { self.done = true; return None; }
        };
        self.done = true;
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirmed_replay::scratch_file::ScratchLimit;

    #[test]
    fn walk_pages_preserve_positions_pinned_iteration_and_shared_budget() {
        let dir = tempfile::tempdir().unwrap();
        let limit = ScratchLimit::acquire(dir.path(), 64 << 10).unwrap();
        let mut hashes = WalkSequence::builder(dir.path(), 1025, 64 << 10).unwrap();
        let mut ranges = WalkSequence::builder(dir.path(), 1025, 64 << 10).unwrap();
        for n in 0u64..1025 {
            let mut hash = [0u8; 32]; hash[..8].copy_from_slice(&n.to_be_bytes());
            hashes.push(&hash).unwrap();
            ranges.push(&(n, n + 1)).unwrap();
        }
        let hashes = hashes.finish().unwrap();
        let ranges = ranges.finish().unwrap();
        let pages = 1025u64.div_ceil(PAGE_ROWS);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 1025 * (32 + 16) + 2 * (194 * pages - 102));
        for n in 0u64..1025 {
            assert_eq!(&hashes.get(n).unwrap()[..8], &n.to_be_bytes());
            assert_eq!(ranges.get(n).unwrap(), (n, n + 1));
        }
        assert!(hashes.get(1025).is_err());
        assert!(hashes.get(u64::MAX).is_err());
        let iterator = ranges.iter();
        drop(ranges);
        assert_eq!(iterator.collect::<Result<Vec<_>, _>>().unwrap(), (0..1025).map(|n| (n, n + 1)).collect::<Vec<_>>());
        drop(hashes);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
        let empty = WalkSequence::<u64>::builder(dir.path(), 0, 0).unwrap().finish().unwrap();
        assert!(empty.get(0).is_err());
        assert!(empty.iter().next().is_none());
    }

    #[test]
    fn walk_pages_poison_limits_and_reject_missing_extra_or_damaged_pages() {
        let dir = tempfile::tempdir().unwrap();
        for (rows, bytes) in [(0, 1024), (2, 15)] {
            let mut builder = WalkSequence::builder(dir.path(), rows, bytes).unwrap();
            assert!(builder.push(&1u64).is_err());
            assert!(builder.push(&2u64).is_err());
            assert!(builder.finish().is_err());
        }
        let mut builder = WalkSequence::builder(dir.path(), 65, 4096).unwrap();
        for n in 0u64..65 { builder.push(&n).unwrap(); }
        let sequence = builder.finish().unwrap();
        let mut iterator = sequence.iter();
        sequence.truncate_for_test();
        assert!(sequence.get(64).is_err());
        assert!(iterator.next().unwrap().is_err());
        assert!(iterator.next().is_none());
        for (length, pages) in [(1, vec![]), (1, vec![vec![]]), (1, vec![vec![0u64, 1]]),
            (65, vec![vec![0; 64]]), (1, vec![vec![0], vec![1]]), (65, vec![vec![0; 63], vec![1; 2]])] {
            let mut builder = ScratchSequence::builder(dir.path(), 10, 4096).unwrap();
            for page in &pages { builder.push(page).unwrap(); }
            let sequence = WalkSequence { pages: builder.finish().unwrap(), length };
            assert!(sequence.iter().collect::<Result<Vec<_>, _>>().is_err());
        }
    }
}
