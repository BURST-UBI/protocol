use super::*;
use bincode::Options;
use serde::de::DeserializeOwned;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::sync::Mutex;

#[cfg(test)]
mod tests;

/// Disposable local input. Complete reads check a digest retained in memory;
/// neither the file nor its decoded rows grant protocol authority.
pub(crate) struct ScratchRows<T, const MAX_ROW_BYTES: u64> {
    file: Mutex<std::fs::File>,
    directory: std::path::PathBuf,
    bytes: u64,
    rows: u64,
    digest: Blake2b<U32>,
    row: PhantomData<fn() -> T>,
}

pub(crate) struct ScratchCursor<T, const MAX_ROW_BYTES: u64> {
    reader: BufReader<std::fs::File>,
    bytes: u64,
    remaining: u64,
    consumed: u64,
    expected: [u8; 32],
    digest: Blake2b<U32>,
    finished: bool,
    row: PhantomData<fn() -> T>,
}

impl<T: Serialize + DeserializeOwned, const MAX_ROW_BYTES: u64> ScratchRows<T, MAX_ROW_BYTES> {
    pub(crate) fn new(directory: &std::path::Path) -> Result<Self, StoreError> {
        Ok(Self {
            file: Mutex::new(tempfile::tempfile_in(directory).map_err(invalid)?),
            directory: directory.to_owned(),
            bytes: 0,
            rows: 0,
            digest: Blake2b::new(),
            row: PhantomData,
        })
    }

    pub(super) fn empty_like(&self) -> Result<Self, StoreError> {
        Self::new(&self.directory)
    }

    pub(crate) fn len(&self) -> u64 {
        self.rows
    }

    pub(crate) fn push(&mut self, row: T) -> Result<(), StoreError> {
        let length = bincode::serialized_size(&row).map_err(invalid)?;
        if length > MAX_ROW_BYTES {
            return Err(invalid("checkpoint scratch row exceeds limit"));
        }
        let bytes = bincode::serialize(&row).map_err(invalid)?;
        let end = self
            .bytes
            .checked_add(8)
            .and_then(|n| n.checked_add(length))
            .ok_or_else(|| invalid("checkpoint scratch length overflow"))?;
        let rows = self
            .rows
            .checked_add(1)
            .ok_or_else(|| invalid("checkpoint scratch count overflow"))?;
        let file = self.file.get_mut().map_err(invalid)?;
        if file.metadata().map_err(invalid)?.len() != self.bytes {
            return Err(invalid("checkpoint scratch file changed before append"));
        }
        file.seek(SeekFrom::Start(self.bytes)).map_err(invalid)?;
        file.write_all(&length.to_be_bytes()).map_err(invalid)?;
        file.write_all(&bytes).map_err(invalid)?;
        self.digest.update(length.to_be_bytes());
        self.digest.update(bytes);
        self.bytes = end;
        self.rows = rows;
        Ok(())
    }

    pub(crate) fn visit(
        &self,
        mut visit: impl FnMut(T) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        // Cloned descriptors share their offset. Hold the lock through the read;
        // append and future readers always seek explicitly.
        let file = self.file.lock().map_err(invalid)?;
        let cursor = ScratchCursor::<T, MAX_ROW_BYTES>::new(
            file.try_clone().map_err(invalid)?,
            self.bytes,
            self.rows,
            self.digest.clone().finalize().into(),
        )?;
        for row in cursor {
            visit(row?)?;
        }
        drop(file);
        Ok(())
    }

    pub(crate) fn into_cursor(self) -> Result<ScratchCursor<T, MAX_ROW_BYTES>, StoreError> {
        ScratchCursor::new(
            self.file.into_inner().map_err(invalid)?,
            self.bytes,
            self.rows,
            self.digest.finalize().into(),
        )
    }
}

impl<T: DeserializeOwned, const MAX_ROW_BYTES: u64> ScratchCursor<T, MAX_ROW_BYTES> {
    fn new(
        mut file: std::fs::File,
        bytes: u64,
        rows: u64,
        expected: [u8; 32],
    ) -> Result<Self, StoreError> {
        if file.metadata().map_err(invalid)?.len() != bytes {
            return Err(invalid("checkpoint scratch file length changed"));
        }
        file.seek(SeekFrom::Start(0)).map_err(invalid)?;
        Ok(Self {
            reader: BufReader::new(file),
            bytes,
            remaining: rows,
            consumed: 0,
            expected,
            digest: Blake2b::new(),
            finished: false,
            row: PhantomData,
        })
    }

    fn finish(&mut self) -> Result<(), StoreError> {
        if self.consumed != self.bytes
            || self.reader.get_ref().metadata().map_err(invalid)?.len() != self.bytes
            || <[u8; 32]>::from(self.digest.clone().finalize()) != self.expected
        {
            return Err(invalid("checkpoint scratch file changed"));
        }
        self.finished = true;
        Ok(())
    }

    fn read_next(&mut self) -> Result<Option<T>, StoreError> {
        if self.remaining == 0 {
            self.finish()?;
            return Ok(None);
        }
        let mut frame = [0; 8];
        self.reader.read_exact(&mut frame).map_err(invalid)?;
        let length = u64::from_be_bytes(frame);
        let end = self
            .consumed
            .checked_add(8)
            .and_then(|n| n.checked_add(length))
            .ok_or_else(|| invalid("checkpoint scratch length overflow"))?;
        if length > MAX_ROW_BYTES || end > self.bytes {
            return Err(invalid("invalid checkpoint scratch frame"));
        }
        let mut bytes = vec![0; length as usize];
        self.reader.read_exact(&mut bytes).map_err(invalid)?;
        self.digest.update(frame);
        self.digest.update(&bytes);
        let row = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_ROW_BYTES)
            .reject_trailing_bytes()
            .deserialize(&bytes)
            .map_err(invalid)?;
        self.consumed = end;
        self.remaining -= 1;
        // Verify before returning the final row, including when the caller
        // knows the row count and does not ask for another item.
        if self.remaining == 0 {
            self.finish()?;
        }
        Ok(Some(row))
    }
}

impl<T: DeserializeOwned, const MAX_ROW_BYTES: u64> Iterator for ScratchCursor<T, MAX_ROW_BYTES> {
    type Item = Result<T, StoreError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        match self.read_next() {
            Ok(row) => row.map(Ok),
            Err(error) => {
                self.finished = true;
                Some(Err(error))
            }
        }
    }
}
