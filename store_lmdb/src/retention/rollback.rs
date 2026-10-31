use super::*;
use crate::block_undo::BlockUndo;
use burst_types::TxHash;

/// Local rollback evidence, not authenticated economic state. Journal consumers
/// need complete before/after images; counters must retain absence versus zero.
/// These records alone never authorize replay, signing, or history retirement.
#[derive(Debug)]
pub enum RollbackRecord<'a> {
    Journal(&'a BlockUndo),
    GovernanceRevision { proposal: TxHash, value: u128 },
}

/// Decode one local rollback row with the same checks as the pinned reader.
/// These bytes are observations, never authenticated replay or rollback authority.
pub fn visit_rollback_row(key: &[u8], bytes: &[u8],
    mut visit: impl FnMut(RollbackRecord<'_>) -> Result<(), StoreError>) -> Result<(), StoreError> {
    if key.len().checked_add(bytes.len()).is_none_or(|n| n > MAX_PAGE_BYTES) {
        return Err(invalid("rollback row exceeds byte budget"));
    }
    if key.starts_with(b"block_undo/") {
        let hash = key.strip_prefix(crate::block_undo::PREFIX)
            .and_then(|key| <[u8; 32]>::try_from(key).ok()).map(BlockHash::new)
            .ok_or_else(|| invalid("unsupported rollback journal key"))?;
        let record = BlockUndo::decode(&hash, bytes)?;
        visit(RollbackRecord::Journal(&record))?;
    } else {
        let suffix = key.strip_prefix(b"governance_revision/v1/")
            .ok_or_else(|| invalid("unsupported local revision key"))?;
        let mut hash = [0; 32];
        if suffix.len() != 64 { return Err(invalid("invalid local revision proposal length")); }
        for (byte, pair) in hash.iter_mut().zip(suffix.chunks_exact(2)) {
            *byte = u8::from_str_radix(std::str::from_utf8(pair).map_err(invalid)?, 16).map_err(invalid)?;
        }
        let proposal = TxHash::new(hash);
        if suffix != proposal.to_string().as_bytes() {
            return Err(invalid("noncanonical local revision key"));
        }
        let value = u128::from_be_bytes(bytes.try_into().map_err(|_| invalid("invalid local revision value"))?);
        visit(RollbackRecord::GovernanceRevision { proposal, value })?;
    }
    Ok(())
}

pub struct RollbackReader<'view, 'db> {
    source: &'view TrstReferenceReader<'db>,
    phase: usize,
    after: Option<Vec<u8>>,
    failed: bool,
}

impl<'db> TrstReferenceReader<'db> {
    pub fn rollback_records(&self) -> Result<RollbackReader<'_, 'db>, StoreError> {
        if self.failed { return Err(invalid("retention scan already failed")); }
        Ok(RollbackReader { source: self, phase: 0, after: None, failed: false })
    }
}

impl RollbackReader<'_, '_> {
    /// Keeps only a cursor and one decoded journal, not an inventory of images.
    /// On error discard all emitted records; this reader cannot later complete.
    pub fn visit_next_page(
        &mut self, max_rows: usize, max_bytes: usize,
        mut visit: impl FnMut(RollbackRecord<'_>) -> Result<(), StoreError>,
    ) -> Result<ReferencePage, StoreError> {
        if self.failed { return Err(invalid("rollback retention scan already failed")); }
        let result = self.visit_page(max_rows, max_bytes, &mut visit);
        if result.is_err() { self.failed = true; }
        result
    }

    fn visit_page(
        &mut self, max_rows: usize, max_bytes: usize,
        visit: &mut impl FnMut(RollbackRecord<'_>) -> Result<(), StoreError>,
    ) -> Result<ReferencePage, StoreError> {
        if max_rows == 0 || max_rows > MAX_PAGE_ROWS || max_bytes == 0 || max_bytes > MAX_PAGE_BYTES {
            return Err(invalid("invalid rollback retention page budget"));
        }
        let mut page = ReferencePage { rows: 0, bytes: 0, done: false };
        while self.phase < 2 {
            let prefix = [b"block_undo/".as_slice(), b"governance_revision/"][self.phase];
            let start = self.after.clone();
            let lower = start.as_deref().map_or(Bound::Included(prefix), Bound::Excluded);
            let mut rows = self.source.store.meta_db.range(&self.source.view.txn,
                &(lower, Bound::<&[u8]>::Unbounded)).map_err(LmdbError::from)?;
            loop {
                if page.rows == max_rows { return Ok(page); }
                let Some(row) = rows.next() else { break; };
                let (key, bytes) = row.map_err(LmdbError::from)?;
                if !key.starts_with(prefix) { break; }
                let size = key.len().checked_add(bytes.len()).ok_or_else(|| invalid("rollback row size overflow"))?;
                if size > max_bytes { return Err(invalid("rollback row exceeds byte budget")); }
                if size > max_bytes - page.bytes { return Ok(page); }
                visit_rollback_row(key, bytes, &mut *visit)?;
                self.after = Some(key.to_vec());
                page.rows += 1;
                page.bytes += size;
            }
            self.phase += 1;
            self.after = None;
        }
        page.done = true;
        Ok(page)
    }
}

#[cfg(test)]
mod tests;
