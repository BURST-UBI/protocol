use super::*;
use crate::confirmed_replay::snapshot::reconciliation::promises;

const PREFIX: &str = "consensus_final/v2/";

/// Body dependencies only, after complete journal authentication at this view.
/// This is not a journal validator: closed epochs are skipped by prefix, while
/// the original records remain durable anti-equivocation evidence.
pub(super) struct PaymentBodyReader<'scan, 'db> {
    store: &'scan LmdbStore,
    view: &'scan burst_store_lmdb::LmdbReadTransaction<'db>,
    after: Option<String>,
    active: Option<(BlockHash, Option<String>)>,
    done: bool,
    failed: bool,
}

impl<'scan, 'db> PaymentBodyReader<'scan, 'db> {
    pub(super) fn new(
        store: &'scan LmdbStore,
        view: &'scan burst_store_lmdb::LmdbReadTransaction<'db>,
    ) -> Self {
        Self { store, view, after: None, active: None, done: false, failed: false }
    }

    pub(super) fn visit_next_page(
        &mut self,
        max_rows: usize,
        max_bytes: usize,
        mut visit: impl FnMut(BlockHash) -> Result<(), StoreError>,
    ) -> Result<ReferencePage, StoreError> {
        if self.failed { return Err(invalid("payment body scan already failed")); }
        self.failed = true;
        if max_rows == 0 || max_rows > burst_store_lmdb::retention::MAX_PAGE_ROWS
            || max_bytes == 0 || max_bytes > burst_store_lmdb::retention::MAX_PAGE_BYTES {
            return Err(invalid("invalid payment body page budget"));
        }
        let meta = self.store.meta_store();
        let page = if self.done {
            ReferencePage { rows: 0, bytes: 0, done: true }
        } else if let Some((epoch, after)) = &mut self.active {
            let prefix = format!("{PREFIX}{epoch}/");
            let page = meta.visit_prefix_page_txn(
                self.view, &prefix, after.as_deref(), max_rows, max_bytes,
                |key, bytes| match promises::decode(key, bytes)? {
                    PaymentPromise::Final { epoch: decoded, block, .. } if decoded == *epoch => visit(block),
                    _ => Err(invalid("payment body row differs from epoch")),
                },
            )?;
            *after = page.last_key;
            if page.done {
                self.after = Some(epoch_end(*epoch));
                self.active = None;
            }
            ReferencePage { rows: page.rows, bytes: page.bytes, done: false }
        } else {
            // Charge one discovery read per epoch without emitting a body.
            // An open epoch reads that row again when scanning its body references.
            let mut epoch = None;
            let page = meta.visit_prefix_page_txn(
                self.view, PREFIX, self.after.as_deref(), 1, max_bytes,
                |key, bytes| match promises::decode(key, bytes)? {
                    PaymentPromise::Final { epoch: found, .. } => { epoch = Some(found); Ok(()) }
                    _ => Err(invalid("non-final payment body row")),
                },
            )?;
            if let Some(epoch) = epoch {
                if meta.contains_meta_txn(self.view, &burst_store::meta::closed_payment_epoch_key(&epoch))? {
                    self.after = Some(epoch_end(epoch));
                } else {
                    self.active = Some((epoch, None));
                }
            } else {
                self.done = true;
            }
            ReferencePage { rows: page.rows, bytes: page.bytes, done: self.done }
        };
        self.failed = false;
        Ok(page)
    }
}

fn epoch_end(epoch: BlockHash) -> String {
    // Canonical epochs have exactly 64 hex digits. '0' sorts after the '/' of
    // every row in this epoch and before every later epoch's fixed-width ID.
    format!("{PREFIX}{epoch}0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payment_body_pages_skip_closed_history_and_preserve_pinned_live_promises() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let signer = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[0;32]).public);
        let epochs = [BlockHash::new([1;32]), BlockHash::new([2;32]), BlockHash::new([3;32])];
        let live = BlockHash::new([44;32]);
        let mut batch = store.tx_begin_write().unwrap();
        for epoch in [epochs[0], epochs[2]] {
            // The reader's production caller has already authenticated these
            // closure markers. This fixture tests traversal cost, not authority.
            let mut closed = BlockHash::new([4;32]).as_bytes().to_vec();
            closed.extend_from_slice(epoch.as_bytes());
            batch.put_meta(&burst_store::meta::closed_payment_epoch_key(&epoch), &closed).unwrap();
            for number in 0..10_000u64 {
                let mut root = [0;32]; root[..8].copy_from_slice(&number.to_be_bytes());
                batch.put_meta(&format!("{PREFIX}{epoch}/{signer}/{signer}/{}", BlockHash::new(root)),
                    BlockHash::new([77;32]).as_bytes()).unwrap();
            }
        }
        let key = format!("{PREFIX}{}/{signer}/{signer}/{}", epochs[1], BlockHash::new([9;32]));
        batch.put_meta(&key, live.as_bytes()).unwrap();
        batch.commit().unwrap();
        let view = store.tx_begin_read().unwrap();
        let mut reader = PaymentBodyReader::new(&store, &view);
        let mut later = store.tx_begin_write().unwrap();
        later.put_meta(&key, BlockHash::new([55;32]).as_bytes()).unwrap();
        later.put_meta(&burst_store::meta::closed_payment_epoch_key(&epochs[1]), &[0;64]).unwrap();
        later.commit().unwrap();
        let mut bodies = Vec::new();
        let mut examined = 0;
        let mut done = false;
        for _ in 0..10 {
            let page = reader.visit_next_page(1, 4096, |body| { bodies.push(body); Ok(()) }).unwrap();
            assert!(page.rows <= 1 && page.bytes <= 4096);
            examined += page.rows;
            if page.done { done = true; break; }
        }
        assert!(done);
        assert_eq!(bodies, [live]);
        assert_eq!(examined, 4, "one discovery per epoch plus one live body; closed rows are not scanned");
        assert!(reader.visit_next_page(1,4096, |_| panic!("completed")).unwrap().done);
    }

    #[test]
    fn payment_body_pages_poison_on_bad_budgets_rows_and_callback_failure() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let signer = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[0;32]).public);
        let epoch = BlockHash::new([2;32]);
        let key = format!("{PREFIX}{epoch}/{signer}/{signer}/{}", BlockHash::new([9;32]));
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(&key, BlockHash::new([44;32]).as_bytes()).unwrap();
        batch.commit().unwrap();
        let view = store.tx_begin_read().unwrap();
        for (rows, bytes) in [(0,4096),(101,4096),(1,0),(1,1),(1,(64<<20)+1)] {
            let mut reader = PaymentBodyReader::new(&store,&view);
            assert!(reader.visit_next_page(rows,bytes, |_| Ok(())).is_err());
            assert!(reader.visit_next_page(1,4096, |_| Ok(())).is_err());
        }
        let mut reader = PaymentBodyReader::new(&store,&view);
        reader.visit_next_page(1,4096, |_| panic!("discovery")).unwrap();
        assert!(reader.visit_next_page(1,4096, |_| Err(invalid("stop"))).is_err());
        assert!(reader.visit_next_page(1,4096, |_| Ok(())).is_err());
        drop(reader); drop(view);
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(&key, &[0;32]).unwrap(); batch.commit().unwrap();
        let view = store.tx_begin_read().unwrap();
        let mut reader = PaymentBodyReader::new(&store,&view);
        assert!(reader.visit_next_page(1,4096, |_| Ok(())).is_err());
        assert!(reader.visit_next_page(1,4096, |_| Ok(())).is_err());
    }
}
