use super::*;
use burst_governance::{signing_delegation::scoped, record_references::Reference};
use burst_types::{NetworkId, BlockHash};

/// Scoped grants, tombstones and discovery links at the recovered cut. Legacy
/// delegation graphs and transitive resolution remain separate obligations.
pub(crate) struct DelegationReferenceReader<'scan, 'db> {
    store: &'scan LmdbStore,
    view: &'scan burst_store_lmdb::LmdbReadTransaction<'db>,
    network: NetworkId,
    genesis: BlockHash,
    after: Option<String>,
    done: bool,
    failed: bool,
}

impl<'scan, 'db> DelegationReferenceReader<'scan, 'db> {
    pub(super) fn new(store: &'scan LmdbStore, view: &'scan burst_store_lmdb::LmdbReadTransaction<'db>, network: NetworkId, genesis: BlockHash) -> Result<Self, StoreError> {
        if genesis.is_zero() { return Err(invalid("zero delegation retention genesis")); }
        Ok(Self { store, view, network, genesis, after: None, done: false, failed: false })
    }

    pub(crate) fn visit_next_page(
        &mut self, max_rows: usize, max_bytes: usize,
        mut visit: impl FnMut(Reference<'_>) -> Result<(), StoreError>,
    ) -> Result<ReferencePage, StoreError> {
        if self.failed { return Err(invalid("delegation retention scan already failed")); }
        let result = (|| {
            if max_rows == 0 || max_rows > burst_store_lmdb::retention::MAX_PAGE_ROWS
                || max_bytes == 0 || max_bytes > burst_store_lmdb::retention::MAX_PAGE_BYTES {
                return Err(invalid("invalid delegation retention page budget"));
            }
            if self.done { return Ok(ReferencePage { rows: 0, bytes: 0, done: true }); }
            let page = self.store.meta_store().visit_prefix_page_txn(
                self.view, "key_delegation/", self.after.as_deref(), max_rows, max_bytes,
                |key, bytes| {
                    let mut callback_error = None;
                    let mut forward = |reference: Reference<'_>| visit(reference).map_err(|error| {
                        callback_error = Some(error); "retention callback stopped".to_string()
                    });
                    let result = scoped::retention::visit_references(self.network, self.genesis, key, bytes, &mut forward);
                    result.map_err(|reason| callback_error.unwrap_or_else(|| invalid(reason)))
                },
            )?;
            self.after = page.last_key.or_else(|| self.after.take());
            self.done = page.done;
            Ok(ReferencePage { rows: page.rows, bytes: page.bytes, done: page.done })
        })();
        if result.is_err() { self.failed = true; }
        result
    }
}

#[cfg(test)]
mod tests;
