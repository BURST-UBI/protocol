use super::*;
use crate::verification_commit::retention::{self, Reference};

/// Stake/outcome roots, dependency links and activation/deadline/default records.
/// Completion excludes other lifecycle state, authority ancestry and provisional
/// undo closure; enumerated references still require authenticated resolution.
pub(crate) struct ReceiptReferenceReader<'scan, 'db> {
    store: &'scan LmdbStore,
    view: &'scan burst_store_lmdb::LmdbReadTransaction<'db>,
    phase: usize,
    after: Option<String>,
    failed: bool,
}

impl<'scan, 'db> ReceiptReferenceReader<'scan, 'db> {
    pub(super) fn new(store: &'scan LmdbStore, view: &'scan burst_store_lmdb::LmdbReadTransaction<'db>) -> Self {
        Self { store, view, phase: 0, after: None, failed: false }
    }

    pub(crate) fn visit_next_page(
        &mut self,
        max_rows: usize,
        max_bytes: usize,
        mut visit: impl FnMut(Reference<'_>) -> Result<(), StoreError>,
    ) -> Result<ReferencePage, StoreError> {
        if self.failed { return Err(invalid("receipt retention scan already failed")); }
        let result = (|| {
            if max_rows == 0 || max_rows > burst_store_lmdb::retention::MAX_PAGE_ROWS
                || max_bytes == 0 || max_bytes > burst_store_lmdb::retention::MAX_PAGE_BYTES {
                return Err(invalid("invalid receipt retention page budget"));
            }
            if self.phase == retention::PREFIXES.len() { return Ok(ReferencePage { rows: 0, bytes: 0, done: true }); }
            let prefix = retention::PREFIXES[self.phase];
            let page = self.store.meta_store().visit_prefix_page_txn(
                self.view, prefix, self.after.as_deref(), max_rows, max_bytes,
                |key, bytes| {
                    retention::visit_row(key, bytes, &mut visit)
                },
            )?;
            if let Some(key) = page.last_key { self.after = Some(key); }
            if page.done { self.phase += 1; self.after = None; }
            Ok(ReferencePage { rows: page.rows, bytes: page.bytes, done: self.phase == retention::PREFIXES.len() })
        })();
        if result.is_err() { self.failed = true; }
        result
    }
}

#[cfg(test)]
mod tests;
