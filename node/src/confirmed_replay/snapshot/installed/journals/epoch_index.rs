use super::*;
use crate::confirmed_replay::scratch_file::ScratchFile as File;
use std::{
    cell::RefCell,
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
};

const ROW_BYTES: u64 = 40;
const RUN_ROWS: usize = 4096;
type Row = [u8; ROW_BYTES as usize];

/// Disk-backed hash-to-height index. Call `verify_with_poll` after the last
/// lookup to check its bytes and ordering before accepting the results.
/// The caller supplies authenticated epochs; this index does not authenticate them.
pub(super) struct EpochIndex {
    file: File,
    rows: u64,
    digest: [u8; 32],
}

struct Run<'a> {
    file: &'a RefCell<File>,
    offset: u64,
    end: u64,
}

impl Read for Run<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let count = (bytes.len() as u64).min(self.end - self.offset) as usize;
        let mut file = self.file.borrow_mut();
        file.seek(SeekFrom::Start(self.offset))?;
        let read = file.read(&mut bytes[..count])?;
        self.offset += read as u64;
        Ok(read)
    }
}

fn next(reader: &mut impl Read) -> Result<Option<Row>, StoreError> {
    let mut row = [0; ROW_BYTES as usize];
    if reader.read(&mut row[..1]).map_err(invalid)? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut row[1..]).map_err(invalid)?;
    Ok(Some(row))
}

fn write_row(
    writer: &mut impl Write,
    digest: &mut Blake2b<U32>,
    row: &Row,
) -> Result<(), StoreError> {
    writer.write_all(row).map_err(invalid)?;
    digest.update(row);
    Ok(())
}

impl EpochIndex {
    #[cfg(test)]
    pub(super) fn build(
        directory: &Path,
        epochs: impl IntoIterator<Item = Result<(BlockHash, u64), StoreError>>,
    ) -> Result<Self, StoreError> {
        Self::build_with_poll(directory, epochs, &|_| Ok(()))
    }

    pub(super) fn build_with_poll(
        directory: &Path,
        epochs: impl IntoIterator<Item = Result<(BlockHash, u64), StoreError>>,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<Self, StoreError> {
        let mut writer = BufWriter::new(File::new_in(directory).map_err(invalid)?);
        let mut digest = Blake2b::<U32>::new();
        let mut rows = 0u64;
        let mut run = Vec::with_capacity(RUN_ROWS);
        for epoch in epochs {
            let (hash, height) = epoch?;
            poll("journal:epoch-input")?;
            let mut row = [0; ROW_BYTES as usize];
            row[..32].copy_from_slice(hash.as_bytes());
            row[32..].copy_from_slice(&height.to_be_bytes());
            run.push(row);
            rows = rows
                .checked_add(1)
                .filter(|n| n.checked_mul(ROW_BYTES).is_some())
                .ok_or_else(|| invalid("epoch index length overflow"))?;
            if run.len() == RUN_ROWS {
                run.sort_unstable();
                for row in run.drain(..) {
                    write_row(&mut writer, &mut digest, &row)?;
                }
            }
        }
        run.sort_unstable();
        for row in run {
            write_row(&mut writer, &mut digest, &row)?;
        }
        let mut index = Self {
            file: writer.into_inner().map_err(invalid)?,
            rows,
            digest: digest.finalize().into(),
        };
        let mut width = RUN_ROWS as u64;
        while width < rows {
            index = index.merge(directory, width, poll)?;
            width = width.saturating_mul(2);
        }
        index.verify_with_poll(poll)?;
        Ok(index)
    }

    fn merge(
        self,
        directory: &Path,
        width: u64,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<Self, StoreError> {
        let source = RefCell::new(self.file);
        let mut writer = BufWriter::new(File::new_in(directory).map_err(invalid)?);
        let mut digest = Blake2b::<U32>::new();
        let mut start = 0;
        while start < self.rows {
            let middle = start.saturating_add(width).min(self.rows);
            let end = middle.saturating_add(width).min(self.rows);
            let reader = |from, to| {
                BufReader::new(Run {
                    file: &source,
                    offset: from * ROW_BYTES,
                    end: to * ROW_BYTES,
                })
            };
            let mut left = reader(start, middle);
            let mut right = reader(middle, end);
            let mut a = next(&mut left)?;
            let mut b = next(&mut right)?;
            while a.is_some() || b.is_some() {
                poll("journal:epoch-merge")?;
                if a.as_ref()
                    .is_some_and(|a| b.as_ref().is_none_or(|b| a <= b))
                {
                    write_row(&mut writer, &mut digest, &a.unwrap())?;
                    a = next(&mut left)?;
                } else {
                    write_row(&mut writer, &mut digest, &b.unwrap())?;
                    b = next(&mut right)?;
                }
            }
            start = end;
        }
        Self {
            file: source.into_inner(),
            rows: self.rows,
            digest: self.digest,
        }
        .scan(false, poll)?;
        Ok(Self {
            file: writer.into_inner().map_err(invalid)?,
            rows: self.rows,
            digest: digest.finalize().into(),
        })
    }

    pub(super) fn height(&mut self, hash: BlockHash) -> Result<Option<u64>, StoreError> {
        let (mut low, mut high) = (0, self.rows);
        while low < high {
            let middle = low + (high - low) / 2;
            self.file
                .seek(SeekFrom::Start(middle * ROW_BYTES))
                .map_err(invalid)?;
            let row = next(&mut self.file)?.ok_or_else(|| invalid("truncated epoch index"))?;
            match row[..32].cmp(hash.as_bytes()) {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => {
                    return Ok(Some(u64::from_be_bytes(
                        row[32..].try_into().map_err(invalid)?,
                    )))
                }
            }
        }
        Ok(None)
    }

    #[cfg(test)]
    pub(super) fn verify(&mut self) -> Result<(), StoreError> {
        self.verify_with_poll(&|_| Ok(()))
    }

    pub(super) fn verify_with_poll(
        &mut self,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        self.scan(true, poll)
    }

    fn scan(
        &mut self,
        sorted: bool,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        if self.file.metadata().map_err(invalid)?.len() != self.rows * ROW_BYTES {
            return Err(invalid("epoch index length changed"));
        }
        self.file.rewind().map_err(invalid)?;
        let mut reader = BufReader::new(&mut self.file);
        let mut digest = Blake2b::<U32>::new();
        let mut previous: Option<[u8; 32]> = None;
        while let Some(row) = next(&mut reader)? {
            poll("journal:epoch-scan")?;
            let hash: [u8; 32] = row[..32].try_into().map_err(invalid)?;
            if sorted && previous.is_some_and(|previous| previous >= hash) {
                return Err(invalid("duplicate or unordered epoch index"));
            }
            previous = Some(hash);
            digest.update(row);
        }
        let digest: [u8; 32] = digest.finalize().into();
        if digest != self.digest {
            return Err(invalid("epoch index bytes changed"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(height: u64) -> BlockHash {
        BlockHash::new(burst_crypto::blake2b_256(&height.to_be_bytes()))
    }

    #[test]
    fn external_epoch_index_crosses_inventory_limit_and_detects_changed_scratch() {
        let directory = tempfile::tempdir().unwrap();
        let count = 131_073u64;
        let mut index = EpochIndex::build(
            directory.path(),
            (0..count).rev().map(|height| Ok((hash(height), height))),
        )
        .unwrap();
        for height in (0..count).step_by(127).chain([count - 1]) {
            assert_eq!(index.height(hash(height)).unwrap(), Some(height));
        }
        assert_eq!(index.height(hash(count)).unwrap(), None);
        index.verify().unwrap();
        index.file.seek(SeekFrom::Start(32)).unwrap();
        index.file.write_all(&u64::MAX.to_be_bytes()).unwrap();
        assert!(index.verify().is_err());
        index.file.set_len(0).unwrap();
        assert!(index.height(hash(1)).is_err());
        assert!(index.verify().is_err());
    }

    #[test]
    fn external_epoch_index_rejects_duplicate_hashes_and_failed_input() {
        let directory = tempfile::tempdir().unwrap();
        let duplicate =
            (0..=RUN_ROWS as u64).map(|height| Ok((hash(height % RUN_ROWS as u64), height)));
        assert!(EpochIndex::build(directory.path(), duplicate).is_err());
        assert!(EpochIndex::build(
            directory.path(),
            [Ok((hash(0), 0)), Err(invalid("missing authority"))]
        )
        .is_err());
        for target in [
            "journal:epoch-input",
            "journal:epoch-merge",
            "journal:epoch-scan",
        ] {
            let cancelled_dir = tempfile::tempdir().unwrap();
            let limit = crate::confirmed_replay::scratch_file::ScratchLimit::acquire(
                cancelled_dir.path(),
                2 << 20,
            )
            .unwrap();
            let polls = std::cell::Cell::new(0);
            let result = EpochIndex::build_with_poll(
                cancelled_dir.path(),
                (0..(RUN_ROWS as u64 + 1)).map(|height| Ok((hash(height), height))),
                &|stage| {
                    if stage == target {
                        polls.set(polls.get() + 1);
                        if polls.get() == 3 {
                            return Err(StoreError::Backend("epoch checkpoint sentinel".into()));
                        }
                    }
                    Ok(())
                },
            );
            assert!(
                matches!(result, Err(ref error) if error.to_string().contains("epoch checkpoint sentinel"))
            );
            assert_eq!(polls.get(), 3, "checkpoint not reached: {target}");
            drop(result);
            assert_eq!(
                limit.used_temporary_bytes().unwrap(),
                0,
                "scratch leaked: {target}"
            );
        }
        let mut empty = EpochIndex::build(directory.path(), std::iter::empty()).unwrap();
        assert_eq!(empty.height(hash(0)).unwrap(), None);
        empty.verify().unwrap();
    }
}
