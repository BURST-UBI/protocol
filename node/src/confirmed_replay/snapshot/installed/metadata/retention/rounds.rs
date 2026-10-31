use super::*;
use crate::checkpoint_agreement::retention::{self, RoundJournal};
use burst_types::{BlockHash, NetworkId};

pub(crate) struct RoundJournalReader<'scan, 'db> {
    store: &'scan LmdbStore,
    view: &'scan burst_store_lmdb::LmdbReadTransaction<'db>,
    network: NetworkId,
    genesis: BlockHash,
    after: Option<String>,
    done: bool,
    failed: bool,
}

impl<'scan, 'db> RoundJournalReader<'scan, 'db> {
    pub(super) fn new(
        store: &'scan LmdbStore,
        view: &'scan burst_store_lmdb::LmdbReadTransaction<'db>,
        network: NetworkId,
        genesis: BlockHash,
    ) -> Self {
        Self {
            store,
            view,
            network,
            genesis,
            after: None,
            done: false,
            failed: false,
        }
    }

    /// One decoded round at a time. Key and encoded body count against the page
    /// budget before decoding. On error, discard the scan results; callbacks
    /// already invoked are not undone. Completion alone does not permit pruning.
    pub(crate) fn visit_next_page(
        &mut self,
        max_rows: usize,
        max_bytes: usize,
        mut visit: impl FnMut(RoundJournal<'_>) -> Result<(), StoreError>,
    ) -> Result<ReferencePage, StoreError> {
        if self.failed {
            return Err(invalid("round retention scan already failed"));
        }
        let result = (|| {
            if max_rows == 0
                || max_rows > burst_store_lmdb::retention::MAX_PAGE_ROWS
                || max_bytes == 0
                || max_bytes > burst_store_lmdb::retention::MAX_PAGE_BYTES
            {
                return Err(invalid("invalid round retention page budget"));
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
                "checkpoint_round/",
                self.after.as_deref(),
                max_rows,
                max_bytes,
                |key, bytes| {
                    retention::visit_round(key, bytes, self.network, self.genesis, &mut visit)
                },
            )?;
            if let Some(key) = page.last_key {
                self.after = Some(key);
            }
            self.done = page.done;
            Ok(ReferencePage {
                rows: page.rows,
                bytes: page.bytes,
                done: self.done,
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
