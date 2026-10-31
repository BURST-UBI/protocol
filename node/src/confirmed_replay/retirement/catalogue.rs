use burst_store::StoreError;
use burst_types::BlockHash;

fn invalid(reason: impl ToString) -> StoreError {
    StoreError::Corruption(reason.to_string())
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub(crate) body_bytes: u64,
    pub(crate) eligible: bool,
    pub(crate) in_snapshot_prefix: bool,
}

impl Candidate {
    pub(crate) fn encode(&self) -> [u8; 32] {
        let mut value = [0; 32];
        value[..8].copy_from_slice(&self.body_bytes.to_be_bytes());
        value[8] = u8::from(self.eligible) | (u8::from(self.in_snapshot_prefix) << 1);
        value
    }
    pub(crate) fn decode(value: [u8; 32]) -> Result<Self, StoreError> {
        if value[8] & !3 != 0 || value[9..].iter().any(|byte| *byte != 0) || value[8] == 1 {
            return Err(invalid("invalid retirement candidate flags"));
        }
        Ok(Self {
            body_bytes: u64::from_be_bytes(value[..8].try_into().map_err(invalid)?),
            eligible: value[8] & 1 != 0,
            in_snapshot_prefix: value[8] & 2 != 0,
        })
    }
}

#[cfg(test)]
use crate::confirmed_replay::scratch_records::RecordsCursor;
use crate::confirmed_replay::scratch_records::{ScratchRecords, SortedRecordsBuilder};
use std::path::Path;

#[cfg(test)]
type Entry = ([u8; 32], [u8; 32]);
#[cfg(test)]
const PAGE_ROWS: usize = 64;
#[cfg(test)]
const PAGE_BYTES: u64 = 8 + PAGE_ROWS as u64 * 64;

// Immutable candidate pages are indexed by their last hash.
mod observed;
use observed::{ObservedCatalogue, ObservedRows};

pub(crate) enum CandidateRows {
    #[cfg(test)]
    Owned(ScratchRecords<Vec<Entry>>),
    Observed(ObservedCatalogue),
}
#[cfg(test)]
pub(crate) struct CandidateBuilder {
    pages: SortedRecordsBuilder<Vec<Entry>>,
    pending: Vec<Entry>,
    previous: Option<[u8; 32]>,
    count: usize,
    max_rows: usize,
    max_bytes: u64,
    failed: bool,
}
#[cfg(test)]
impl CandidateBuilder {
    pub(crate) fn new(
        directory: &Path,
        max_rows: usize,
        max_bytes: u64,
    ) -> Result<Self, StoreError> {
        // max_bytes charges ten logical bytes per candidate. ScratchFile
        // accounts for actual payload and index extents separately.
        let rows = (max_rows as u64).min(max_bytes / 10);
        let payload = rows
            .checked_mul(64)
            .and_then(|bytes| {
                rows.div_ceil(PAGE_ROWS as u64)
                    .checked_mul(8)
                    .and_then(|headers| bytes.checked_add(headers))
            })
            .ok_or_else(|| invalid("retirement catalogue size overflow"))?;
        Ok(Self {
            pages: SortedRecordsBuilder::new(directory, max_rows.div_ceil(PAGE_ROWS), payload)?,
            pending: Vec::new(),
            previous: None,
            count: 0,
            max_rows,
            max_bytes,
            failed: false,
        })
    }
    pub(crate) fn push(&mut self, key: [u8; 32], candidate: &Candidate) -> Result<(), StoreError> {
        if self.failed {
            return Err(invalid("retirement catalogue builder already failed"));
        }
        self.failed = true;
        let count = self
            .count
            .checked_add(1)
            .filter(|count| *count <= self.max_rows)
            .ok_or_else(|| invalid("retirement catalogue row budget exceeded"))?;
        if (count as u64)
            .checked_mul(10)
            .is_none_or(|bytes| bytes > self.max_bytes)
        {
            return Err(invalid("retirement catalogue byte budget exceeded"));
        }
        if self.previous.is_some_and(|previous| previous >= key) {
            return Err(invalid("unordered retirement catalogue"));
        }
        let value = candidate.encode();
        Candidate::decode(value)?;
        self.pending.push((key, value));
        if self.pending.len() == PAGE_ROWS {
            self.flush()?;
        }
        self.previous = Some(key);
        self.count = count;
        self.failed = false;
        Ok(())
    }
    fn flush(&mut self) -> Result<(), StoreError> {
        if let Some((key, _)) = self.pending.last() {
            self.pages.push(*key, &self.pending)?;
            self.pending.clear();
        }
        Ok(())
    }
    pub(crate) fn finish(mut self) -> Result<CandidateRows, StoreError> {
        if self.failed {
            return Err(invalid("retirement catalogue builder already failed"));
        }
        self.flush()?;
        Ok(CandidateRows::Owned(self.pages.finish()?))
    }
}

#[cfg(test)]
pub(crate) struct OwnedRows {
    cursor: RecordsCursor<Vec<Entry>>,
    pending: std::vec::IntoIter<Entry>,
    after: Option<[u8; 32]>,
    previous_page: Option<[u8; 32]>,
    done: bool,
    failed: bool,
}
#[cfg(test)]
impl Iterator for OwnedRows {
    type Item = Result<(BlockHash, Candidate), StoreError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        loop {
            for (key, value) in self.pending.by_ref() {
                if self.after.is_none_or(|after| key > after) {
                    return Some(Candidate::decode(value).map(|row| (BlockHash::new(key), row)));
                }
            }
            if self.done {
                return None;
            }
            let result = self
                .cursor
                .visit_next::<StoreError>(1, PAGE_BYTES, |key, rows| {
                    if rows.is_empty()
                        || rows.len() > PAGE_ROWS
                        || rows.last().is_none_or(|last| last.0 != key)
                        || rows.windows(2).any(|pair| pair[0].0 >= pair[1].0)
                        || self
                            .previous_page
                            .is_some_and(|previous| rows[0].0 <= previous)
                    {
                        return Err(invalid("invalid retirement catalogue page"));
                    }
                    // Validate even entries skipped by an exclusive resume boundary.
                    for (_, value) in &rows {
                        Candidate::decode(*value)?;
                    }
                    self.previous_page = Some(key);
                    self.pending = rows.into_iter();
                    Ok(())
                });
            match result {
                Ok(page) => self.done = page.done,
                Err(error) => {
                    self.failed = true;
                    return Some(Err(error));
                }
            }
        }
    }
}
pub(crate) enum Rows {
    #[cfg(test)]
    Owned(OwnedRows),
    Observed(ObservedRows),
}
impl Iterator for Rows {
    type Item = Result<(BlockHash, Candidate), StoreError>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            #[cfg(test)]
            Self::Owned(rows) => rows.next(),
            Self::Observed(rows) => rows.next(),
        }
    }
}

impl CandidateRows {
    pub(crate) fn scan(&self, after: Option<BlockHash>) -> Result<Rows, StoreError> {
        match self {
            Self::Observed(rows) => rows.scan(after).map(Rows::Observed),
            #[cfg(test)]
            Self::Owned(pages) => {
                let after = after.map(|hash| *hash.as_bytes());
                let cursor = match after {
                    Some(key) => pages.cursor_after(key)?,
                    None => pages.cursor()?,
                };
                Ok(Rows::Owned(OwnedRows {
                    cursor,
                    pending: Vec::new().into_iter(),
                    after,
                    previous_page: None,
                    done: false,
                    failed: false,
                }))
            }
        }
    }
    pub(crate) fn get(&self, key: &[u8; 32]) -> Result<Option<Candidate>, StoreError> {
        let mut before = *key;
        let after = match before.iter().rposition(|byte| *byte != 0) {
            Some(position) => {
                before[position] -= 1;
                before[position + 1..].fill(255);
                Some(BlockHash::new(before))
            }
            None => None,
        };
        let row = self.scan(after)?.next().transpose()?;
        Ok(row
            .filter(|(hash, _)| hash.as_bytes() == key)
            .map(|(_, row)| row))
    }
    #[cfg(test)]
    pub(crate) fn truncate_for_test(&self) {
        match self {
            Self::Owned(pages) => pages.truncate_for_test(),
            Self::Observed(rows) => rows.truncate_for_test(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(n: u64) -> [u8; 32] {
        let mut key = [0; 32];
        key[24..].copy_from_slice(&n.to_be_bytes());
        key
    }
    fn value() -> [u8; 32] {
        Candidate {
            body_bytes: 17,
            eligible: true,
            in_snapshot_prefix: true,
        }
        .encode()
    }

    #[test]
    fn pages_preserve_missing_keys_and_exclusive_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = CandidateBuilder::new(dir.path(), 131, 1310).unwrap();
        for n in 0..131 {
            builder
                .push(key(n * 2), &Candidate::decode(value()).unwrap())
                .unwrap();
        }
        let rows = builder.finish().unwrap();
        for n in 0..=263 {
            assert_eq!(rows.get(&key(n)).unwrap().is_some(), n <= 260 && n % 2 == 0);
            let actual = rows
                .scan(Some(BlockHash::new(key(n))))
                .unwrap()
                .map(|row| *row.unwrap().0.as_bytes())
                .collect::<Vec<_>>();
            assert_eq!(
                actual,
                (0..131)
                    .map(|n| key(n * 2))
                    .filter(|key| *key > self::key(n))
                    .collect::<Vec<_>>()
            );
        }
        assert!(rows.get(&[255; 32]).unwrap().is_none());
        assert!(rows
            .scan(Some(BlockHash::new([255; 32])))
            .unwrap()
            .next()
            .is_none());
        let empty = CandidateBuilder::new(dir.path(), 0, 0)
            .unwrap()
            .finish()
            .unwrap();
        assert!(empty.scan(None).unwrap().next().is_none());
        assert!(empty.get(&key(0)).unwrap().is_none());
    }

    #[test]
    fn malformed_pages_fail_before_emitting_or_skipping_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let mut bad = value();
        bad[8] = 1;
        for page in [
            vec![],
            vec![(key(1), value())],
            vec![(key(1), value()), (key(0), value())],
            vec![(key(0), value()), (key(0), value())],
            vec![(key(0), bad)],
            (0..65).map(|n| (key(n), value())).collect(),
        ] {
            let mut pages = SortedRecordsBuilder::new(dir.path(), 1, 1 << 20).unwrap();
            pages.push(key(0), &page).unwrap();
            let rows = CandidateRows::Owned(pages.finish().unwrap());
            let mut scan = rows.scan(None).unwrap();
            assert!(scan.next().unwrap().is_err());
            assert!(scan.next().is_none());
        }
        let mut pages = SortedRecordsBuilder::new(dir.path(), 1, 1 << 20).unwrap();
        pages
            .push(key(2), &vec![(key(0), bad), (key(2), value())])
            .unwrap();
        let rows = CandidateRows::Owned(pages.finish().unwrap());
        assert!(rows
            .scan(Some(BlockHash::new(key(1))))
            .unwrap()
            .next()
            .unwrap()
            .is_err());
        let mut descending = CandidateBuilder::new(dir.path(), 2, 20).unwrap();
        descending
            .push(key(2), &Candidate::decode(value()).unwrap())
            .unwrap();
        assert!(descending
            .push(key(1), &Candidate::decode(value()).unwrap())
            .is_err());
        assert!(descending.finish().is_err());
    }
}
