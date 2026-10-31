//! Read references from TRST holdings, wallet records, merger records and pending
//! transfers in one database snapshot. Related readers cover BRN wallets, account
//! state and rollback records. Callers must authenticate the snapshot and resolve
//! its dependencies before deciding which historical records can be deleted.
use crate::{LmdbEnvironment, LmdbError, LmdbReadTransaction};
use burst_store::StoreError;
use burst_trst::{
    engine::{holding_rows, WalletRecord},
    merger_graph::GraphRecord,
    TrstEngine,
};
use burst_types::{BlockHash, TrstReference};
use std::ops::{Bound, ControlFlow};

pub(crate) mod changes;
pub use changes::{BodyChangeCommit, BodyChangeWatch};
mod sequence_commits;
pub(crate) use sequence_commits::SequenceCommits;
mod account_work;
mod brn;
mod rollback;
pub use account_work::{pending_confirmation_reference, AccountWorkReader, AccountWorkRecord};
pub use brn::BrnWalletReferenceReader;
pub use rollback::{visit_rollback_row, RollbackReader, RollbackRecord};

pub const MAX_PAGE_ROWS: usize = 100;
pub const MAX_PAGE_BYTES: usize = 64 << 20;

#[derive(Debug, PartialEq, Eq)]
pub struct ReferencePage {
    /// Stored rows consumed, not the number of references emitted.
    pub rows: usize,
    /// Stored key/value bytes consumed, excluding decoded allocations.
    pub bytes: usize,
    /// All phases reached their end. A short page alone does not imply completion.
    pub done: bool,
}

/// The view cannot change between pages. References may repeat and are borrowed
/// only for the callback. On any error, discard accumulated results: no later
/// call can report completion for that failed scan.
pub struct TrstReferenceReader<'a> {
    store: &'a LmdbEnvironment,
    view: RetentionView<'a>,
    phase: usize,
    after: Option<Vec<u8>>,
    failed: bool,
}

enum RetentionView<'a> {
    Owned(LmdbReadTransaction<'a>),
    Borrowed(&'a LmdbReadTransaction<'a>),
}

impl<'a> std::ops::Deref for RetentionView<'a> {
    type Target = LmdbReadTransaction<'a>;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Owned(view) => view,
            Self::Borrowed(view) => view,
        }
    }
}

fn invalid(message: impl ToString) -> StoreError {
    StoreError::Corruption(message.to_string())
}

impl LmdbEnvironment {
    pub fn trst_reference_reader(&self) -> Result<TrstReferenceReader<'_>, StoreError> {
        self.trst_reference_reader_from(RetentionView::Owned(self.tx_begin_read()?))
    }

    /// Start a fresh traversal of the caller's existing snapshot, without opening
    /// another native transaction or consuming any other traversal's cursor.
    pub fn trst_reference_reader_view<'a>(
        &'a self,
        view: &'a LmdbReadTransaction<'a>,
    ) -> Result<TrstReferenceReader<'a>, StoreError> {
        self.trst_reference_reader_from(RetentionView::Borrowed(view))
    }

    fn trst_reference_reader_from<'a>(
        &'a self,
        view: RetentionView<'a>,
    ) -> Result<TrstReferenceReader<'a>, StoreError> {
        if !view.belongs_to(self) {
            return Err(invalid("retention reader belongs to another environment"));
        }
        let meta = self.meta_db;
        let get = |key: &str| meta.get(&view.txn, key.as_bytes()).map_err(LmdbError::from);
        if get("trst_state_format")? != Some(&[2][..])
            || get("trst_graph_format")? != Some(&[1][..])
        {
            return Err(invalid(
                "retention scan requires normalized TRST and graph records",
            ));
        }
        if get(TrstEngine::meta_key())?.is_some() || get("merger_graph")?.is_some() {
            return Err(invalid("retention scan rejects mixed TRST layouts"));
        }
        for kind in ["0/", "1/"] {
            let prefix = format!("{}{kind}", WalletRecord::STORAGE_PREFIX);
            if meta
                .prefix_iter(&view.txn, prefix.as_bytes())
                .map_err(LmdbError::from)?
                .next()
                .transpose()
                .map_err(LmdbError::from)?
                .is_some()
            {
                return Err(invalid("retention scan rejects positional holdings"));
            }
        }
        Ok(TrstReferenceReader {
            store: self,
            view,
            phase: 0,
            after: None,
            failed: false,
        })
    }
}

impl TrstReferenceReader<'_> {
    /// Storage identity only, not proof that every dependency was traversed.
    /// Unknown generations and another environment instance fail closed.
    pub(crate) fn check_cut(
        &self,
        store: &LmdbEnvironment,
        generation: usize,
    ) -> Result<(), StoreError> {
        if self.failed
            || !std::ptr::eq(self.store, store)
            || self.view.generation() != Some(generation)
        {
            return Err(invalid(
                "retention cut is stale, failed, or belongs to another environment",
            ));
        }
        Ok(())
    }

    pub(crate) fn check_body_cut(
        &self,
        store: &LmdbEnvironment,
        generation: usize,
    ) -> Result<(), StoreError> {
        if self.failed || !std::ptr::eq(self.store, store) {
            return Err(invalid(
                "retention cut failed or belongs to another environment",
            ));
        }
        let observed = self
            .view
            .generation()
            .ok_or_else(|| invalid("unknown retention cut"))?;
        if observed == generation {
            return Ok(());
        }
        let commits = store.sequence_commits.lock().map_err(invalid)?;
        if !commits.covers(observed, generation) {
            return Err(invalid(
                "body retention dependencies changed or commit coverage is missing",
            ));
        }
        Ok(())
    }

    /// Consumers must read and authenticate their cut from this same view.
    pub fn view(&self) -> &LmdbReadTransaction<'_> {
        &self.view
    }

    pub fn visit_next_page(
        &mut self,
        max_rows: usize,
        max_bytes: usize,
        mut visit: impl FnMut(TrstReference<'_>) -> Result<(), StoreError>,
    ) -> Result<ReferencePage, StoreError> {
        if self.failed {
            return Err(invalid("retention scan already failed"));
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
        visit: &mut impl FnMut(TrstReference<'_>) -> Result<(), StoreError>,
    ) -> Result<ReferencePage, StoreError> {
        if max_rows == 0 || max_rows > MAX_PAGE_ROWS || max_bytes == 0 || max_bytes > MAX_PAGE_BYTES
        {
            return Err(invalid("invalid retention page budget"));
        }
        let mut result = ReferencePage {
            rows: 0,
            bytes: 0,
            done: false,
        };
        while self.phase < 4 {
            let (db, prefix) = match self.phase {
                0 => (self.store.meta_db, holding_rows::PREFIX.as_bytes()),
                1 => (self.store.meta_db, WalletRecord::STORAGE_PREFIX.as_bytes()),
                2 => (self.store.meta_db, GraphRecord::STORAGE_PREFIX.as_bytes()),
                _ => (self.store.pending_db, &b""[..]),
            };
            let start = self.after.clone();
            let lower = match start.as_deref() {
                Some(key) => Bound::Excluded(key),
                None if prefix.is_empty() => Bound::Unbounded,
                None => Bound::Included(prefix),
            };
            let mut rows = db
                .range(&self.view.txn, &(lower, Bound::<&[u8]>::Unbounded))
                .map_err(LmdbError::from)?;
            loop {
                if result.rows == max_rows {
                    return Ok(result);
                }
                let Some(entry) = rows.next() else {
                    break;
                };
                let (key, bytes) = entry.map_err(LmdbError::from)?;
                if !key.starts_with(prefix) {
                    break;
                }
                let size = key
                    .len()
                    .checked_add(bytes.len())
                    .ok_or_else(|| invalid("retention row size overflow"))?;
                if size > max_bytes {
                    return Err(invalid("retention row exceeds byte budget"));
                }
                if size > max_bytes - result.bytes {
                    return Ok(result);
                }
                let progress = match self.phase {
                    0 => {
                        let key = std::str::from_utf8(key).map_err(invalid)?;
                        let row =
                            holding_rows::Row::from_storage_entry(key, bytes).map_err(invalid)?;
                        row.visit_retention_references(|r| {
                            visit(r).map_or_else(ControlFlow::Break, |_| ControlFlow::Continue(()))
                        })
                    }
                    1 => {
                        let key = std::str::from_utf8(key).map_err(invalid)?;
                        let row = WalletRecord::from_storage_entry(key, bytes).map_err(invalid)?;
                        row.visit_retention_references(|r| {
                            visit(r).map_or_else(ControlFlow::Break, |_| ControlFlow::Continue(()))
                        })
                    }
                    2 => {
                        let key = std::str::from_utf8(key).map_err(invalid)?;
                        let row = GraphRecord::from_storage_entry(key, bytes).map_err(invalid)?;
                        row.visit_retention_references(|r| {
                            visit(r).map_or_else(ControlFlow::Break, |_| ControlFlow::Continue(()))
                        })
                    }
                    _ => {
                        let (destination, source, pending) =
                            crate::pending::decode_pending_entry_bounded(key, bytes)?;
                        pending.visit_retention_references(
                            &destination,
                            BlockHash::new(*source.as_bytes()),
                            |r| {
                                visit(r)
                                    .map_or_else(ControlFlow::Break, |_| ControlFlow::Continue(()))
                            },
                        )
                    }
                };
                if let ControlFlow::Break(error) = progress {
                    return Err(error);
                }
                self.after = Some(key.to_vec());
                result.rows += 1;
                result.bytes += size;
            }
            self.phase += 1;
            self.after = None;
        }
        result.done = true;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
