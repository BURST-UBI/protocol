//! Structurally checked confirmation hashes cached by storage generation.
//! Queue membership records a local handoff, not a verified quorum certificate.
use super::*;
use burst_utils::SharedSet;
use std::ops::Bound;

use crate::confirmation_queue::decode;

fn load(env: &LmdbEnvironment, txn: &heed::RoTxn<'_>) -> Result<SharedSet<BlockHash>, StoreError> {
    let mut queue = SharedSet::default();
    for item in env.meta_db.prefix_iter(txn, crate::confirmation_queue::PREFIX).map_err(LmdbError::from)? {
        let (key, value) = item.map_err(LmdbError::from)?;
        if queue.len() >= crate::confirmation_queue::CAPACITY {
            return Err(StoreError::Corruption("confirmation queue exceeds capacity".into()));
        }
        queue.insert(decode(key, value)?);
    }
    Ok(queue)
}

impl AuxiliaryCache {
    pub(crate) fn update_confirmations(&mut self, key: &[u8], value: Option<&[u8]>) {
        if !key.starts_with(crate::confirmation_queue::PREFIX) { return; }
        let Some(queue) = self.confirmations.as_mut() else { return; };
        let Ok(hash) = decode(key, value.unwrap_or(&[1])) else {
            self.confirmations = None;
            return;
        };
        if value.is_some() { queue.insert(hash); } else { queue.remove(&hash); }
        if queue.len() > crate::confirmation_queue::CAPACITY { self.confirmations = None; }
    }
}

fn select(queue: &SharedSet<BlockHash>, after: Option<BlockHash>, limit: usize) -> Vec<BlockHash> {
    let lower = after.map_or(Bound::Unbounded, Bound::Excluded);
    let upper = after.map_or(Bound::Unbounded, Bound::Included);
    let mut batch: Vec<_> = queue.range((lower, Bound::Unbounded)).take(limit).copied().collect();
    if after.is_some() && batch.len() < limit {
        batch.extend(queue.range((Bound::Unbounded, upper)).take(limit - batch.len()).copied());
    }
    batch
}

impl LmdbWriteTransaction<'_> {
    /// Select from this writer's validated queue, including handoffs committed
    /// while acquisition waited. Rotation and caps match the pinned read path.
    pub fn pending_confirmation_batch(&self, after: Option<BlockHash>, limit: usize) -> Result<Vec<BlockHash>, StoreError> {
        if limit > crate::confirmation_queue::CAPACITY {
            return Err(StoreError::Serialization("confirmation batch exceeds capacity".into()));
        }
        self.confirmation_queue_len()?;
        let cache = self.auxiliary_cache.borrow();
        Ok(select(cache.confirmations.as_ref().expect("validated queue cached"), after, limit))
    }

    pub(crate) fn confirmation_queue_len(&self) -> Result<usize, StoreError> {
        if let Some(queue) = &self.auxiliary_cache.borrow().confirmations { return Ok(queue.len()); }
        let queue = load(self.env, &self.txn)?;
        let len = queue.len();
        self.auxiliary_cache.borrow_mut().confirmations = Some(queue);
        Ok(len)
    }
}

impl LmdbEnvironment {
    /// Select in hash order after the cursor, wrapping once to the beginning.
    /// Copy only the requested batch from a generation-matched cache; otherwise
    /// read and structurally validate the entire capped queue first.
    pub fn pending_confirmation_batch(&self, after: Option<BlockHash>, limit: usize) -> Result<Vec<BlockHash>, StoreError> {
        if limit > crate::confirmation_queue::CAPACITY {
            return Err(StoreError::Serialization("confirmation batch exceeds capacity".into()));
        }
        let view = self.tx_begin_read()?;
        let cached = view.generation.and_then(|generation| {
            self.auxiliary_cache.lock().unwrap().as_ref()
                .filter(|(id, _)| *id == generation)
                .and_then(|(_, cache)| cache.confirmations.clone())
        });
        let queue = match cached {
            Some(queue) => queue,
            None => {
                let queue = load(self, &view.txn)?;
                if let Some(generation) = view.generation {
                    let mut slot = self.auxiliary_cache.lock().unwrap();
                    if slot.as_ref().is_none_or(|(id, _)| *id <= generation) {
                        if slot.as_ref().is_none_or(|(id, _)| *id != generation) {
                            *slot = Some((generation, AuxiliaryCache::default()));
                        }
                        slot.as_mut().unwrap().1.confirmations = Some(queue.clone());
                    }
                }
                queue
            }
        };
        Ok(select(&queue, after, limit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hash(i: u64) -> BlockHash {
        let mut bytes = [0; 32]; bytes[..8].copy_from_slice(&i.to_be_bytes()); BlockHash::new(bytes)
    }

    #[test]
    fn pinned_queue_reader_cannot_publish_under_newer_environment_identity() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let txn = store.env().read_txn().unwrap();
        let pinned_id = txn.id();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let mut writer = store.tx_begin_write().unwrap();
                writer.put_block(&hash(1), b"block").unwrap();
                writer.handoff_confirmation(&hash(1)).unwrap();
                writer.commit().unwrap();
            }).join().unwrap();
        });
        // Model an old reader while environment observations both report the
        // newer cut. The snapshot ID, not agreement between those observations,
        // must prevent publishing its empty queue over the committed handoff.
        let before = store.env().info().last_txn_id;
        let view = LmdbReadTransaction::from_pinned(&store, txn);
        let after = store.env().info().last_txn_id;
        assert_eq!(before, after);
        assert!(pinned_id < before);
        assert_eq!(view.generation(), Some(pinned_id));
        let old_queue = load(&store, &view.txn).unwrap();
        assert!(old_queue.is_empty());
        store.publish_read_auxiliary(view.generation, |cache| cache.confirmations = Some(old_queue));
        drop(view);
        assert_eq!(store.pending_confirmation_batch(None, 256).unwrap(), vec![hash(1)]);
        let writer = store.tx_begin_write().unwrap();
        assert_eq!(writer.pending_confirmation_batch(None, 256).unwrap(), vec![hash(1)]);
        assert!(writer.confirmation_pending(&hash(1)).unwrap());
    }

    #[test]
    fn confirmation_batches_rotate_update_and_reject_corrupt_cuts() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        for i in 1..=600 {
            batch.put_block(&hash(i), b"block").unwrap();
            batch.handoff_confirmation(&hash(i)).unwrap();
        }
        batch.commit().unwrap();
        assert_eq!(store.pending_confirmation_batch(None, 256).unwrap(), (1..=256).map(hash).collect::<Vec<_>>());
        assert_eq!(store.pending_confirmation_batch(Some(hash(590)), 20).unwrap(), (591..=600).chain(1..=10).map(hash).collect::<Vec<_>>());
        assert_eq!(store.pending_confirmation_batch(Some(hash(999)), 2).unwrap(), vec![hash(1), hash(2)]);
        assert!(store.pending_confirmation_batch(None, 0).unwrap().is_empty());
        for (after, limit) in [(None, 256), (Some(hash(590)), 20), (Some(hash(999)), 2), (None, 0)] {
            let expected = store.pending_confirmation_batch(after, limit).unwrap();
            let batch = store.tx_begin_write().unwrap();
            assert_eq!(batch.pending_confirmation_batch(after, limit).unwrap(), expected);
            assert!(batch.pending_confirmation_batch(after, crate::confirmation_queue::CAPACITY + 1).is_err());
        }
        let mut batch = store.tx_begin_write().unwrap();
        batch.acknowledge_confirmation(&hash(1)).unwrap();
        assert_eq!(batch.confirmation_queue_len().unwrap(), 599);
        drop(batch);
        assert_eq!(store.pending_confirmation_batch(None, 1).unwrap(), vec![hash(1)]);
        let mut batch = store.tx_begin_write().unwrap();
        batch.acknowledge_confirmation(&hash(1)).unwrap();
        batch.commit().unwrap();
        assert_eq!(store.pending_confirmation_batch(None, 1).unwrap(), vec![hash(2)]);
        assert_eq!(store.pending_confirmation_batch(Some(hash(599)), 1000).unwrap().len(), 599);
        // An untracked corruption beyond the requested window must force a
        // complete validation, not disappear behind an apparently valid page.
        let mut txn = store.env().write_txn().unwrap();
        store.meta_db.put(&mut txn, &crate::confirmation_queue::key(&hash(600)), &[9]).unwrap();
        txn.commit().unwrap();
        assert!(store.pending_confirmation_batch(None, 1).is_err());
        assert!(store.tx_begin_write().unwrap().pending_confirmation_batch(None, 1).is_err());
        let mut batch = store.tx_begin_write().unwrap();
        batch.acknowledge_confirmation(&hash(600)).unwrap();
        batch.commit().unwrap();
        assert_eq!(store.pending_confirmation_batch(None, 1).unwrap(), vec![hash(2)]);
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_raw(UndoTable::Meta, &crate::confirmation_queue::key(&hash(599)), &[9]).unwrap();
        batch.commit().unwrap();
        assert!(store.pending_confirmation_batch(None, 1).is_err());
        assert!(store.tx_begin_write().unwrap().pending_confirmation_batch(None, 1).is_err());
    }
    #[test]
    fn concurrent_handoffs_and_vote_reservations_keep_queue_views_current() {
        use burst_store::MetaStore;
        use std::sync::Barrier;
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let epoch = hash(900);
        store.meta_store().put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, epoch.as_bytes()).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        for i in 1..=64 { batch.put_block(&hash(i), b"block").unwrap(); }
        batch.commit().unwrap();
        assert!(store.pending_confirmation_batch(None, 256).unwrap().is_empty());
        let start = Barrier::new(3);
        std::thread::scope(|scope| {
            let handoffs = scope.spawn(|| {
                start.wait();
                for i in 1..=64 {
                    let mut batch = store.tx_begin_write().unwrap();
                    batch.handoff_confirmation(&hash(i)).unwrap();
                    if i > 1 { batch.acknowledge_confirmation(&hash(i - 1)).unwrap(); }
                    batch.commit().unwrap();
                    std::thread::yield_now();
                }
            });
            let votes = scope.spawn(|| {
                start.wait();
                let rep = burst_types::WalletAddress::new("brst_queue_concurrent");
                for _ in 0..64 {
                    store.meta_store().reserve_consensus_vote(&epoch, &rep, &rep,
                        &BlockHash::ZERO, &hash(1), false, 0).unwrap();
                    std::thread::yield_now();
                }
            });
            start.wait();
            for _ in 0..256 {
                // Pin the committed cut with the writer lock. Cache publication
                // from an earlier commit may still race these reads.
                let pinned = store.tx_begin_write().unwrap();
                let expected: Vec<_> = load(&store, &pinned.txn).unwrap().iter().copied().collect();
                assert_eq!(store.pending_confirmation_batch(None, 256).unwrap(), expected);
                assert_eq!(store.pending_confirmation_batch(Some(hash(999)), 256).unwrap(), expected);
                drop(pinned);
                std::thread::yield_now();
            }
            handoffs.join().unwrap();
            votes.join().unwrap();
        });
        assert_eq!(store.pending_confirmation_batch(None, 256).unwrap(), vec![hash(64)]);
        assert!(store.meta_store().confirmation_pending_txn(&store.tx_begin_read().unwrap(), &hash(64)).unwrap());
    }

    #[test]
    fn confirmation_cache_capacity_reopen_and_checkpoint_clear() {
        use burst_store::MetaStore;
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let capacity = crate::confirmation_queue::CAPACITY;
        let mut txn = store.env().write_txn().unwrap();
        for i in 1..=capacity as u64 {
            store.meta_db.put(&mut txn, &crate::confirmation_queue::key(&hash(i)), &[1]).unwrap();
        }
        txn.commit().unwrap();
        assert_eq!(store.pending_confirmation_batch(None, capacity).unwrap().len(), capacity);
        let extra = hash(capacity as u64 + 1);
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_block(&extra, b"block").unwrap();
        assert!(batch.handoff_confirmation(&extra).is_err());
        assert!(batch.commit().is_err());
        assert_eq!(store.pending_confirmation_batch(None, capacity).unwrap().len(), capacity);
        // Removing and adding within one writer reuses the updated count.
        let mut batch = store.tx_begin_write().unwrap();
        batch.acknowledge_confirmation(&hash(1)).unwrap();
        batch.put_block(&extra, b"block").unwrap();
        batch.handoff_confirmation(&extra).unwrap();
        batch.commit().unwrap();
        assert_eq!(store.pending_confirmation_batch(Some(hash(capacity as u64)), 2).unwrap(), vec![extra, hash(2)]);
        drop(store);
        let store = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        assert_eq!(store.pending_confirmation_batch(Some(hash(capacity as u64)), 2).unwrap(), vec![extra, hash(2)]);
        let schema = store.meta_store().get_meta("schema_version").unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.clear_checkpoint_rows(UndoTable::Meta, None).unwrap();
        batch.put_meta("schema_version", &schema).unwrap();
        batch.commit().unwrap();
        assert!(store.pending_confirmation_batch(None, 1).unwrap().is_empty());
        // A malformed key cannot be hidden outside a requested range.
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_raw(UndoTable::Meta, crate::confirmation_queue::PREFIX, &[1]).unwrap();
        batch.commit().unwrap();
        assert!(store.pending_confirmation_batch(Some(extra), 1).is_err());
    }

}
