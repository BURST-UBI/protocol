use super::{invalid, BlockHash, Observation, StoreError, MAX_BYTES};
use crate::confirmed_replay::scratch_records::{
    RecordsCursor, ScratchRecords, SortedRecordsBuilder,
};
use std::path::Path;

const PAGE_ROWS: usize = 64;
const TARGET_BYTES: u64 = 16 << 10;

// Only private scratch pages use this representation. Observation's durable
// cache encoding remains unchanged; noncanonical account strings round-trip exactly.
#[derive(serde::Serialize, serde::Deserialize)]
enum StoredAccount {
    Key([u8; 32]),
    Raw(burst_types::WalletAddress),
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredObservation {
    hash: BlockHash,
    account: StoredAccount,
    height: u64,
    length: usize,
    digest: [u8; 32],
}

impl From<&Observation> for StoredObservation {
    fn from(row: &Observation) -> Self {
        let account = match burst_crypto::decode_address(row.account.as_str()) {
            Some(key) => StoredAccount::Key(key),
            None => StoredAccount::Raw(row.account.clone()),
        };
        Self {
            hash: row.hash,
            account,
            height: row.height,
            length: row.length,
            digest: row.digest,
        }
    }
}

impl From<StoredObservation> for Observation {
    fn from(row: StoredObservation) -> Self {
        let account = match row.account {
            StoredAccount::Key(key) => burst_crypto::derive_address(&burst_types::PublicKey(key)),
            StoredAccount::Raw(account) => account,
        };
        Self {
            hash: row.hash,
            account,
            height: row.height,
            length: row.length,
            digest: row.digest,
        }
    }
}

/// Immutable observations share one authenticated descriptor per bounded page.
/// The page's last hash is its index key, so successor lookup selects the only
/// page that can contain a requested hash. Durable cache encoding is independent.
pub(super) struct ObservationRows {
    pages: ScratchRecords<Vec<StoredObservation>>,
    #[cfg(test)]
    lookups: std::sync::atomic::AtomicUsize,
}

pub(super) struct ObservationBuilder {
    pages: SortedRecordsBuilder<Vec<StoredObservation>>,
    pending: Vec<StoredObservation>,
    page_bytes: u64,
    total_bytes: u64,
    max_bytes: u64,
    count: usize,
    max_rows: usize,
    previous: Option<BlockHash>,
    failed: bool,
}

impl ObservationBuilder {
    pub(super) fn new(directory: &Path, rows: usize, bytes: u64) -> Result<Self, StoreError> {
        Ok(Self {
            pages: SortedRecordsBuilder::new(directory, rows, bytes)?,
            pending: Vec::new(),
            page_bytes: 8,
            total_bytes: 0,
            max_bytes: bytes,
            count: 0,
            max_rows: rows,
            previous: None,
            failed: false,
        })
    }

    pub(super) fn push(&mut self, key: [u8; 32], row: &Observation) -> Result<(), StoreError> {
        if self.failed {
            return Err(invalid("observation builder already failed"));
        }
        self.failed = true;
        if key != *row.hash.as_bytes()
            || self.previous.is_some_and(|old| old >= row.hash)
            || self.count >= self.max_rows
        {
            return Err(invalid("invalid observation key, order or row budget"));
        }
        let stored = StoredObservation::from(row);
        let length = bincode::serialized_size(&stored).map_err(invalid)?;
        if length > MAX_BYTES as u64 - 8 {
            return Err(invalid("observation exceeds page byte budget"));
        }
        if !self.pending.is_empty()
            && (self.pending.len() == PAGE_ROWS || self.page_bytes + length > TARGET_BYTES)
        {
            self.flush()?;
        }
        self.total_bytes = self
            .total_bytes
            .checked_add(length + if self.pending.is_empty() { 8 } else { 0 })
            .filter(|bytes| *bytes <= self.max_bytes)
            .ok_or_else(|| invalid("observations exceed byte budget"))?;
        self.pending.push(stored);
        self.page_bytes += length;
        self.count += 1;
        self.previous = Some(row.hash);
        self.failed = false;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), StoreError> {
        if let Some(last) = self.pending.last() {
            self.pages.push(*last.hash.as_bytes(), &self.pending)?;
            self.pending.clear();
            self.page_bytes = 8;
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> Result<ObservationRows, StoreError> {
        if self.failed {
            return Err(invalid("observation builder already failed"));
        }
        self.flush()?;
        Ok(ObservationRows {
            pages: self.pages.finish()?,
            #[cfg(test)]
            lookups: Default::default(),
        })
    }
}

/// Shared immutable physical rows for a detached retirement catalogue. This
/// owns neither economic state nor a database reader.
pub(crate) struct BodyObservations {
    rows: std::sync::Arc<ObservationRows>,
    count: u64,
}
impl BodyObservations {
    pub(super) fn new(rows: std::sync::Arc<ObservationRows>, count: u64) -> Self {
        Self { rows, count }
    }
    pub(crate) fn count(&self) -> u64 {
        self.count
    }
    /// Resume after a page's last hash. An arbitrary hash may return a page
    /// containing earlier rows; use the returned page key for the next resume.
    pub(crate) fn pages(
        &self,
        after: Option<BlockHash>,
    ) -> Result<BodyObservationPages, StoreError> {
        let cursor = match after {
            Some(hash) => self.rows.pages.cursor_after(*hash.as_bytes())?,
            None => self.rows.pages.cursor()?,
        };
        Ok(BodyObservationPages {
            cursor,
            previous: None,
            done: false,
            failed: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn fixture(
        directory: &Path,
        rows: impl IntoIterator<Item = (BlockHash, usize)>,
    ) -> Self {
        let mut builder = ObservationBuilder::new(directory, 1_000_000, 128 << 20).unwrap();
        let mut count = 0;
        for (hash, length) in rows {
            let row = Observation {
                hash,
                length,
                account: burst_types::WalletAddress::new("brst_page"),
                height: 1,
                digest: [7; 32],
            };
            builder.push(*hash.as_bytes(), &row).unwrap();
            count += 1;
        }
        Self::new(std::sync::Arc::new(builder.finish().unwrap()), count)
    }
    #[cfg(test)]
    pub(crate) fn truncate_for_test(&self) {
        self.rows.truncate_for_test();
    }
}

pub(crate) struct BodyObservationPages {
    cursor: RecordsCursor<Vec<StoredObservation>>,
    previous: Option<BlockHash>,
    done: bool,
    failed: bool,
}
impl Iterator for BodyObservationPages {
    type Item = Result<([u8; 32], Vec<(BlockHash, u64)>), StoreError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        while !self.done {
            let mut output = None;
            let result = self
                .cursor
                .visit_next::<StoreError>(1, MAX_BYTES as u64, |key, rows| {
                    validate_page(&rows, key, self.previous)?;
                    self.previous = Some(BlockHash::new(key));
                    output = Some((
                        key,
                        rows.into_iter()
                            .map(|row| (row.hash, row.length as u64))
                            .collect(),
                    ));
                    Ok(())
                });
            match result {
                Ok(page) => self.done = page.done,
                Err(error) => {
                    self.failed = true;
                    return Some(Err(error));
                }
            }
            if let Some(output) = output {
                return Some(Ok(output));
            }
        }
        None
    }
}

fn validate_page(
    rows: &[StoredObservation],
    key: [u8; 32],
    previous: Option<BlockHash>,
) -> Result<(), StoreError> {
    if rows.is_empty()
        || rows.len() > PAGE_ROWS
        || rows.last().is_none_or(|last| *last.hash.as_bytes() != key)
        || rows.windows(2).any(|pair| pair[0].hash >= pair[1].hash)
        || previous.is_some_and(|previous| rows[0].hash <= previous)
    {
        return Err(invalid("invalid observation page order or bounds"));
    }
    Ok(())
}

pub(super) struct Observations {
    cursor: RecordsCursor<Vec<StoredObservation>>,
    pending: std::vec::IntoIter<StoredObservation>,
    after: Option<BlockHash>,
    previous_page: Option<BlockHash>,
    done: bool,
    failed: bool,
}

impl Iterator for Observations {
    type Item = Result<Observation, StoreError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        loop {
            for row in self.pending.by_ref() {
                if self.after.is_none_or(|after| row.hash > after) {
                    return Some(Ok(row.into()));
                }
            }
            if self.done {
                return None;
            }
            let result = self
                .cursor
                .visit_next::<StoreError>(1, MAX_BYTES as u64, |key, rows| {
                    validate_page(&rows, key, self.previous_page)?;
                    self.previous_page = Some(BlockHash::new(key));
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

impl ObservationRows {
    pub(super) fn scan(&self, after: Option<BlockHash>) -> Result<Observations, StoreError> {
        let cursor = match after {
            Some(after) => self.pages.cursor_after(*after.as_bytes())?,
            None => self.pages.cursor()?,
        };
        Ok(Observations {
            cursor,
            pending: Vec::new().into_iter(),
            after,
            previous_page: None,
            done: false,
            failed: false,
        })
    }

    pub(super) fn get(&self, key: &[u8; 32]) -> Result<Option<Observation>, StoreError> {
        #[cfg(test)]
        self.lookups
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        Ok(row.filter(|row| row.hash.as_bytes() == key))
    }

    #[cfg(test)]
    pub(super) fn visit(
        &self,
        mut visit: impl FnMut([u8; 32], Observation) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        for row in self.scan(None)? {
            let row = row?;
            visit(*row.hash.as_bytes(), row)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn lookup_count(&self) -> usize {
        self.lookups.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(super) fn truncate_for_test(&self) {
        self.pages.truncate_for_test();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirmed_replay::scratch_file::ScratchLimit;
    use burst_types::WalletAddress;

    fn row(n: u64) -> Observation {
        let mut hash = [0; 32];
        hash[24..].copy_from_slice(&n.to_be_bytes());
        Observation {
            hash: BlockHash::new(hash),
            account: WalletAddress::new("brst_page"),
            height: n + 1,
            length: 37,
            digest: [7; 32],
        }
    }

    #[test]
    fn observation_scratch_compacts_canonical_accounts_without_changing_identity() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = burst_crypto::derive_address(&burst_types::PublicKey([42; 32]));
        let mut bad_checksum = canonical.as_str().to_owned();
        bad_checksum.pop();
        bad_checksum.push('!');
        let accounts = [
            canonical.clone(),
            WalletAddress::new("brst_page"),
            WalletAddress::new(bad_checksum),
            WalletAddress::new("brst_é"),
        ];
        let mut builder = ObservationBuilder::new(dir.path(), 131, 1 << 20).unwrap();
        let expected: Vec<_> = (0..131)
            .map(|n| {
                let mut observation = row(n);
                observation.account = accounts[n as usize % accounts.len()].clone();
                observation
            })
            .collect();
        for observation in &expected {
            builder
                .push(*observation.hash.as_bytes(), observation)
                .unwrap();
        }
        let records = builder.finish().unwrap();
        assert_eq!(
            records
                .scan(None)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            expected
        );
        for observation in &expected {
            assert_eq!(
                records.get(observation.hash.as_bytes()).unwrap(),
                Some(observation.clone())
            );
        }
        let mut observation = row(0);
        observation.account = canonical;
        let original = bincode::serialized_size(&observation).unwrap();
        let compact = bincode::serialized_size(&StoredObservation::from(&observation)).unwrap();
        assert_eq!(original - compact, 37);
        // Reconstruct only returned rows; catalogue scans need no address decoding.
        let bodies = BodyObservations::new(std::sync::Arc::new(records), expected.len() as u64);
        let actual = bodies
            .pages(None)
            .unwrap()
            .flat_map(|page| page.unwrap().1)
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            expected
                .iter()
                .map(|row| (row.hash, row.length as u64))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn observation_pages_preserve_lookup_absence_and_resume_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let expected: Vec<_> = (0..131).map(|n| row(n * 2)).collect();
        let mut builder = ObservationBuilder::new(dir.path(), expected.len(), 1 << 20).unwrap();
        for row in &expected {
            builder.push(*row.hash.as_bytes(), row).unwrap();
        }
        let records = builder.finish().unwrap();
        for n in 0..=263 {
            let key = row(n).hash;
            assert_eq!(
                records.get(key.as_bytes()).unwrap(),
                expected.iter().find(|r| r.hash == key).cloned()
            );
            let actual = records
                .scan(Some(key))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(
                actual,
                expected
                    .iter()
                    .filter(|r| r.hash > key)
                    .cloned()
                    .collect::<Vec<_>>()
            );
        }
        assert_eq!(
            records
                .scan(None)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            expected
        );
        assert!(records.get(&[255; 32]).unwrap().is_none());
        assert!(records
            .scan(Some(BlockHash::new([255; 32])))
            .unwrap()
            .next()
            .is_none());
        let empty = ObservationBuilder::new(dir.path(), 0, 0)
            .unwrap()
            .finish()
            .unwrap();
        assert!(empty.get(&[0; 32]).unwrap().is_none());
        assert!(empty.scan(None).unwrap().next().is_none());
    }

    #[test]
    fn observation_pages_bound_shared_space_and_reject_corrupt_payloads() {
        let dir = tempfile::tempdir().unwrap();
        // Account for page payloads and their shared index.
        let limit = ScratchLimit::acquire(dir.path(), 160 << 10).unwrap();
        let mut builder = ObservationBuilder::new(dir.path(), 1025, 160 << 10).unwrap();
        for n in 0..1025 {
            let row = row(n);
            builder.push(*row.hash.as_bytes(), &row).unwrap();
        }
        let records = builder.finish().unwrap();
        let used = limit.used_temporary_bytes().unwrap();
        let payload = bincode::serialized_size(&StoredObservation::from(&row(0))).unwrap() * 1025;
        let pages = 1025u64.div_ceil(PAGE_ROWS as u64);
        assert_eq!(used, payload + pages * (8 + 186) - 102);
        assert_eq!(
            records.get(row(1024).hash.as_bytes()).unwrap(),
            Some(row(1024))
        );
        // A damaged page must fail both lookup and an already-open scan.
        let mut scan = records.scan(None).unwrap();
        records.truncate_for_test();
        assert!(records.get(row(64).hash.as_bytes()).is_err());
        assert!(scan.next().unwrap().is_err());
        assert!(scan.next().is_none());
        drop(scan);
        drop(records);
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    }

    #[test]
    fn observation_pages_poison_bad_input_and_validate_page_keys() {
        let dir = tempfile::tempdir().unwrap();
        for (first, next, rows) in [(1, 1, 2), (2, 1, 2), (1, 2, 1)] {
            let mut builder = ObservationBuilder::new(dir.path(), rows, 4096).unwrap();
            builder
                .push(*row(first).hash.as_bytes(), &row(first))
                .unwrap();
            assert!(builder
                .push(*row(next).hash.as_bytes(), &row(next))
                .is_err());
            assert!(builder.finish().is_err());
        }
        let mut builder = ObservationBuilder::new(dir.path(), 1, 1).unwrap();
        assert!(builder.push(*row(0).hash.as_bytes(), &row(0)).is_err());
        assert!(
            builder.finish().is_err(),
            "pending page must obey the byte limit on flush"
        );
        let mut builder = ObservationBuilder::new(dir.path(), 1, 4096).unwrap();
        assert!(builder.push(*row(2).hash.as_bytes(), &row(1)).is_err());
        assert!(builder.finish().is_err());
        for page in [
            vec![],
            vec![row(1), row(0)],
            vec![row(0), row(0)],
            vec![row(1)],
            (0..65).map(row).collect(),
        ] {
            let mut pages = SortedRecordsBuilder::new(dir.path(), 1, 1 << 20).unwrap();
            let page: Vec<_> = page.iter().map(StoredObservation::from).collect();
            pages.push(*row(0).hash.as_bytes(), &page).unwrap();
            let records = ObservationRows {
                pages: pages.finish().unwrap(),
                lookups: Default::default(),
            };
            assert!(records.scan(None).unwrap().next().unwrap().is_err());
        }
    }
}
