use super::*;
use burst_store::account::AccountInfo;

/// The complete account frontier and local work waiting for cementation. Queue
/// entries are observations, not portable finality proofs or admitted blocks.
#[derive(Debug)]
pub enum AccountWorkRecord<'a> {
    Account(&'a AccountInfo),
    PendingConfirmation(BlockHash),
}

/// Decode a local confirmation handoff without treating it as finality proof.
pub fn pending_confirmation_reference(key: &[u8], bytes: &[u8]) -> Result<BlockHash, StoreError> {
    crate::confirmation_queue::decode(key, bytes)
}

/// Page account records and queued confirmations from the parent reader's
/// snapshot. A failed page permanently invalidates this reader; discard partial
/// callback results rather than treating them as a complete inventory.
pub struct AccountWorkReader<'view, 'db> {
    source: &'view TrstReferenceReader<'db>,
    phase: usize,
    after: Option<Vec<u8>>,
    confirmations: usize,
    failed: bool,
}

impl<'db> TrstReferenceReader<'db> {
    pub fn account_work(&self) -> Result<AccountWorkReader<'_, 'db>, StoreError> {
        if self.failed {
            return Err(invalid("retention scan already failed"));
        }
        Ok(AccountWorkReader {
            source: self,
            phase: 0,
            after: None,
            confirmations: 0,
            failed: false,
        })
    }
}
impl AccountWorkReader<'_, '_> {
    pub fn visit_next_page(
        &mut self,
        max_rows: usize,
        max_bytes: usize,
        mut visit: impl FnMut(AccountWorkRecord<'_>) -> Result<(), StoreError>,
    ) -> Result<ReferencePage, StoreError> {
        if self.failed {
            return Err(invalid("account/work retention scan already failed"));
        }
        let result = self.visit_page(max_rows, max_bytes, &mut visit);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn visit_page(
        &mut self,
        max_rows: usize,
        max_bytes: usize,
        visit: &mut impl FnMut(AccountWorkRecord<'_>) -> Result<(), StoreError>,
    ) -> Result<ReferencePage, StoreError> {
        if max_rows == 0 || max_rows > MAX_PAGE_ROWS || max_bytes == 0 || max_bytes > MAX_PAGE_BYTES
        {
            return Err(invalid("invalid account/work retention budget"));
        }
        let mut page = ReferencePage {
            rows: 0,
            bytes: 0,
            done: false,
        };
        while self.phase < 2 {
            let (db, prefix) = if self.phase == 0 {
                (self.source.store.accounts_db, &b""[..])
            } else {
                (self.source.store.meta_db, &b"pending_confirmation/"[..])
            };
            let start = self.after.clone();
            let lower = start.as_deref().map_or(
                if prefix.is_empty() {
                    Bound::Unbounded
                } else {
                    Bound::Included(prefix)
                },
                Bound::Excluded,
            );
            let mut rows = db
                .range(&self.source.view.txn, &(lower, Bound::<&[u8]>::Unbounded))
                .map_err(LmdbError::from)?;
            loop {
                if page.rows == max_rows {
                    return Ok(page);
                }
                let Some(row) = rows.next() else { break };
                let (key, bytes) = row.map_err(LmdbError::from)?;
                if !key.starts_with(prefix) {
                    break;
                }
                let size = key
                    .len()
                    .checked_add(bytes.len())
                    .ok_or_else(|| invalid("account/work row size overflow"))?;
                if size > max_bytes {
                    return Err(invalid("account/work row exceeds byte budget"));
                }
                if size > max_bytes - page.bytes {
                    return Ok(page);
                }
                if self.phase == 0 {
                    let account = crate::account::decode_account_entry(key, bytes)?;
                    visit(AccountWorkRecord::Account(&account))?;
                } else {
                    if self.confirmations >= crate::confirmation_queue::CAPACITY {
                        return Err(invalid("confirmation queue exceeds capacity"));
                    }
                    visit(AccountWorkRecord::PendingConfirmation(
                        pending_confirmation_reference(key, bytes)?,
                    ))?;
                    self.confirmations += 1;
                }
                page.rows += 1;
                page.bytes += size;
                self.after = Some(key.to_vec());
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
