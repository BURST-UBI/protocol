use super::*;
use burst_verification::orchestrator::records::{
    normalized, retention::Reference, VerificationRecord,
};

/// Normalized verification state from the already authenticated pinned cut.
/// Page completion is not transitive closure or permission to retire history.
pub(crate) struct VerificationReferenceReader<'scan, 'db> {
    store: &'scan LmdbStore,
    view: &'scan burst_store_lmdb::LmdbReadTransaction<'db>,
    after: Option<String>,
    done: bool,
    failed: bool,
}

impl<'scan, 'db> VerificationReferenceReader<'scan, 'db> {
    pub(super) fn new(
        store: &'scan LmdbStore,
        view: &'scan burst_store_lmdb::LmdbReadTransaction<'db>,
    ) -> Result<Self, StoreError> {
        let meta = store.meta_store();
        if meta
            .get_meta_txn(view, "verification_records_format")?
            .as_deref()
            != Some(&[2])
            || meta.contains_meta_txn(view, "verification_orchestrator")?
            || !meta.contains_meta_txn(view, "verification_state/v1/0")?
        {
            return Err(invalid(
                "retention requires normalized verification state and rules",
            ));
        }
        Ok(Self {
            store,
            view,
            after: None,
            done: false,
            failed: false,
        })
    }

    pub(crate) fn visit_next_page(
        &mut self,
        max_rows: usize,
        max_bytes: usize,
        mut visit: impl FnMut(Reference<'_>) -> Result<(), StoreError>,
    ) -> Result<ReferencePage, StoreError> {
        if self.failed {
            return Err(invalid("verification retention scan already failed"));
        }
        let result = (|| {
            if max_rows == 0
                || max_rows > burst_store_lmdb::retention::MAX_PAGE_ROWS
                || max_bytes == 0
                || max_bytes > burst_store_lmdb::retention::MAX_PAGE_BYTES
            {
                return Err(invalid("invalid verification retention page budget"));
            }
            if self.done {
                return Ok(ReferencePage {
                    rows: 0,
                    bytes: 0,
                    done: true,
                });
            }
            let page = self.store.meta_store().visit_prefix_page_txn(
                self.view,
                "verification_state/",
                self.after.as_deref(),
                max_rows,
                max_bytes,
                |key, bytes| {
                    let mut callback_error = None;
                    let mut forward = |reference: Reference<'_>| {
                        visit(reference).map_err(|error| {
                            callback_error = Some(error);
                            "retention callback stopped".to_string()
                        })
                    };
                    let result = if key.starts_with(normalized::PREFIX) {
                        normalized::visit_row_retention_references(key, bytes, &mut forward)
                    } else if key == "verification_state/v1/0"
                        || key.starts_with("verification_state/v1/2/")
                    {
                        VerificationRecord::from_storage_entry(key, bytes)
                            .and_then(|record| record.visit_retention_references(&mut forward))
                    } else {
                        Err("unsupported or mixed verification retention schema".into())
                    };
                    result.map_err(|reason| callback_error.unwrap_or_else(|| invalid(reason)))
                },
            )?;
            self.after = page.last_key.or_else(|| self.after.take());
            self.done = page.done;
            Ok(ReferencePage {
                rows: page.rows,
                bytes: page.bytes,
                done: page.done,
            })
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}

#[cfg(test)]
mod tests;
