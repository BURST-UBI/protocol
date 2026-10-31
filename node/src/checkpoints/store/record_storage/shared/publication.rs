//! Verified, constant-page publication of a completed legacy-record migration.
use super::{invalid, staging, Envelope, Header, Prepared};
use crate::checkpoint_store::record_storage::{retirement, source::VerifiedSource};
use crate::paged_metadata::{staged, Commitment};
use burst_store::StoreError;
use burst_store_lmdb::{checkpoint_heads::NodeHash, LmdbStore};

pub(super) struct Publication {
    key: String,
    id: NodeHash,
    header: Header,
    guard: VerifiedSource,
    envelope_state: String,
}

/// One abandoned-job cleanup batch. A concurrent shared replacement owns the
/// final envelope namespace; preserve it and retire only staging's unused tail.
pub(super) fn abandon(store: &LmdbStore, key: &str, id: &NodeHash,
    envelope_state: &str) -> Result<bool, StoreError> {
    if !super::super::is_record_key(key) { return Err(invalid("invalid abandoned migration key")); }
    #[cfg(test)] let mut timing = super::WriteProbe::new("abandon");
        let mut batch = store.tx_begin_write()?;
        #[cfg(test)] timing.acquired();
    let live = match batch.get_meta(key)? {
        Some(bytes) => super::is_shared(&bytes)?,
        None => false,
    };
    let envelope = Envelope::new(key);
    let done = if live {
        staged::discard_preserving_live(&envelope, envelope_state, &mut batch, super::MAX_RECORD_BYTES as usize)?
    } else {
        staged::discard(&envelope, envelope_state, &mut batch, super::MAX_RECORD_BYTES as usize)?
    };
    staging::discard(&mut batch, id)?;
    batch.commit()?;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint_store::record_storage::{self, RecordLocation};
    use crate::paged_metadata::{self, Location};

    const KEY: &str = "checkpoint_record_archive/v1/dev/publication/1";
    const STATE: &str = "checkpoint_heads/envelope-staging/test";
    const ID: NodeHash = [17; 32];

    #[test]
    fn abandoned_job_does_not_delete_a_newer_shared_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let original = super::super::tests::record(true);
        stage(&store, &original);
        let newer = super::super::tests::record(false);
        let mut batch = store.tx_begin_write().unwrap();
        record_storage::put(&mut batch, KEY, &newer).unwrap(); batch.commit().unwrap();
        while !abandon(&store, KEY, &ID, STATE).unwrap() {}
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch.read_view().checkpoint_head_staging(&ID).unwrap().is_empty());
        assert!(batch.get_meta(STATE).unwrap().is_none());
        batch.reclaim_checkpoint_heads(256).unwrap();
        assert_eq!(record_storage::get_batch(&batch, KEY).unwrap().unwrap(), newer);
        batch.commit().unwrap();
    }

    fn stage(store: &LmdbStore, bytes: &[u8]) {
        let prepared = super::super::prepare(bytes).unwrap().unwrap();
        let hash = burst_crypto::blake2b_256(bytes);
        let mut batch = store.tx_begin_write().unwrap();
        paged_metadata::write_bytes_at(RecordLocation::new(KEY), &mut batch, bytes).unwrap();
        batch.commit().unwrap();
        for (index, rows) in prepared.heads.chunks(staging::ROWS_PER_BATCH).enumerate() {
            let mut batch = store.tx_begin_write().unwrap();
            staging::advance(&mut batch, &ID, hash, prepared.heads.len(), index * staging::ROWS_PER_BATCH, rows).unwrap();
            batch.commit().unwrap();
        }
        let envelope = [&bytes[..prepared.prefix], &bytes[prepared.end..]].concat();
        let expected = Commitment { size: envelope.len(), hash: burst_crypto::blake2b_256(&envelope) };
        let budget = staged::PAGES_PER_BATCH * paged_metadata::PAGE_BYTES;
        for (index, chunk) in envelope.chunks(budget).enumerate() {
            let mut batch = store.tx_begin_write().unwrap();
            staged::append(&Envelope::new(KEY), STATE, &mut batch, expected, bytes.len(), index * budget, chunk).unwrap();
            batch.commit().unwrap();
        }
    }

    #[test]
    fn publication_transfers_ownership_and_preserves_original_pinned_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let bytes = super::super::tests::record(true);
        stage(&store, &bytes);
        let prepared = super::super::prepare(&bytes).unwrap().unwrap();
        let publication = Publication::verify(&store, KEY, ID, &prepared, STATE.into()).unwrap();
        // The watcher also opens a reader during commit reconciliation. Keep
        // the independent pinned reader on its own thread, as in live callers.
        std::thread::scope(|scope| {
            let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
            let (done_tx, done_rx) = std::sync::mpsc::sync_channel(0);
            let reader_store = &store;
            let original = &bytes;
            let reader = scope.spawn(move || {
                let pinned = reader_store.tx_begin_read().unwrap();
                ready_tx.send(()).unwrap();
                done_rx.recv().unwrap();
                assert_eq!(record_storage::get_txn(&reader_store.meta_store(), &pinned, KEY).unwrap().unwrap(), *original);
            });
            ready_rx.recv().unwrap();
            publication.commit(&store).unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            assert!(super::super::is_shared(&batch.get_meta(KEY).unwrap().unwrap()).unwrap());
            assert!(batch.read_view().checkpoint_head_staging(&ID).unwrap().is_empty());
            assert!(batch.get_meta(STATE).unwrap().is_none());
            assert_eq!(batch.reclaim_checkpoint_heads(256).unwrap().nodes, 0);
            assert_eq!(record_storage::get_batch(&batch, KEY).unwrap().unwrap(), bytes);
            batch.commit().unwrap();
            loop {
                let mut batch = store.tx_begin_write().unwrap();
                let done = retirement::step(&mut batch, KEY).unwrap(); batch.commit().unwrap();
                if done { break; }
            }
            done_tx.send(()).unwrap();
            reader.join().unwrap();
        });
    }

    #[test]
    fn publication_rejects_staged_envelope_aba_without_replacing_source() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let bytes = super::super::tests::record(true);
        stage(&store, &bytes);
        let prepared = super::super::prepare(&bytes).unwrap().unwrap();
        let publication = Publication::verify(&store, KEY, ID, &prepared, STATE.into()).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let page = Envelope::new(KEY).page_key(0);
        let original = batch.get_meta(&page).unwrap().unwrap();
        batch.put_meta(&page, b"changed").unwrap(); batch.put_meta(&page, &original).unwrap();
        batch.commit().unwrap();
        assert!(publication.commit(&store).is_err());
        let batch = store.tx_begin_write().unwrap();
        assert!(!super::super::is_shared(&batch.get_meta(KEY).unwrap().unwrap()).unwrap());
        assert_eq!(record_storage::get_batch(&batch, KEY).unwrap().unwrap(), bytes);
        assert!(!batch.read_view().checkpoint_head_staging(&ID).unwrap().is_empty());
    }
}

impl Publication {
    #[cfg(test)]
    pub(super) fn verify(store: &LmdbStore, key: &str, id: NodeHash,
        prepared: &Prepared<'_>, envelope_state: String) -> Result<Self, StoreError> {
        Self::verify_with_poll(store, key, id, prepared, envelope_state, &|| Ok(()))
    }

    /// Head and envelope staging must already be complete. Reconstruct the record
    /// on a pinned reader without a writer, verifying bytes rather than consensus.
    pub(super) fn verify_with_poll(store: &LmdbStore, key: &str, id: NodeHash,
        prepared: &Prepared<'_>, envelope_state: String, poll: &dyn Fn() -> Result<(), StoreError>) -> Result<Self, StoreError> {
        poll()?;
        let source = Commitment { size: prepared.bytes.len(), hash: burst_crypto::blake2b_256(&prepared.bytes) };
        let count = prepared.heads.len();
        let mut header = None;
        let envelope = Envelope::new(key);
        let guard = VerifiedSource::capture_with_poll(store, key, source,
            vec![envelope.descriptor.clone(), envelope.pages.clone(), envelope_state.clone()], poll, |txn| {
                let view = store.read_view(txn)?;
                let (offset, root) = staging::progress(&view, &id, source.hash, count)?;
                if offset != count { return Err(invalid("head staging is incomplete")); }
                let root = root.ok_or_else(|| invalid("head staging root missing"))?;
                let proposed = Header { size: source.size, hash: source.hash, root, count,
                    prefix: prepared.prefix, suffix: prepared.bytes.len() - prepared.end };
                // Stream verification checks envelope pages, canonical tree, and
                // exact original logical commitment in this observed snapshot.
                let actual = super::visit(key, &proposed.encode(), |name| { poll()?; view.get_meta(name) },
                    |hash| { poll()?; view.checkpoint_head_node(hash) }, source.size, |_| poll())?;
                if actual != source { return Err(invalid("staged reconstruction differs from source")); }
                let mut digest = burst_crypto::Blake2b256Hasher::default();
                digest.update(&prepared.bytes[..prepared.prefix]);
                digest.update(&prepared.bytes[prepared.end..]);
                let envelope_commitment = Commitment { size: proposed.prefix + proposed.suffix, hash: digest.finalize() };
                if staged::progress(&view, &envelope_state, envelope_commitment, source.size)? != envelope_commitment.size {
                    return Err(invalid("envelope staging is incomplete"));
                }
                header = Some(proposed); Ok(())
            })?;
        Ok(Self { key: key.to_owned(), id, header: header.ok_or_else(|| invalid("missing verified header"))?,
            guard, envelope_state })
    }

    /// Own the transaction so every error aborts all publication changes.
    /// Only root pins, descriptors and cursors change; no old pages are scanned.
    pub(super) fn commit(self, store: &LmdbStore) -> Result<(), StoreError> {
        #[cfg(test)] let mut timing = super::WriteProbe::new("publish");
        let mut batch = store.tx_begin_write()?;
        #[cfg(test)] timing.acquired();
        self.guard.verify(store, &batch)?;
        let (offset, root) = staging::progress(&batch.read_view(), &self.id, self.header.hash, self.header.count)?;
        if offset != self.header.count || root != Some(self.header.root) {
            return Err(invalid("staged root changed before publication"));
        }
        let old = batch.get_meta(&self.key)?.ok_or_else(|| invalid("migration source disappeared"))?;
        retirement::schedule(&mut batch, &self.key, &old)?;
        batch.retain_checkpoint_head(&self.header.root)?;
        batch.put_meta(&self.key, &self.header.encode())?;
        staging::discard(&mut batch, &self.id)?;
        batch.delete_meta(&self.envelope_state)?;
        batch.commit()
    }
}
