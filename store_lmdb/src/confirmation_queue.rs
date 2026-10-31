//! Durable handoff from consensus to causal cementation. This queue stores local
//! decisions, not portable quorum certificates; peers cannot enqueue directly.

use crate::{LmdbError, LmdbMetaStore};
use burst_store::StoreError;
use burst_types::BlockHash;

pub(crate) const PREFIX: &[u8] = b"pending_confirmation/v1/";
pub(crate) const CAPACITY: usize = 16_384;

pub(crate) fn key(hash: &BlockHash) -> Vec<u8> {
    [PREFIX, hash.as_bytes()].concat()
}

pub(crate) fn decode(key: &[u8], value: &[u8]) -> Result<BlockHash, StoreError> {
    let bytes: [u8; 32] = key.strip_prefix(crate::confirmation_queue::PREFIX)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| StoreError::Corruption("invalid confirmation queue key".into()))?;
    let hash = BlockHash::new(bytes);
    if hash.is_zero() || value != [1] {
        return Err(StoreError::Corruption("invalid confirmation queue record".into()));
    }
    Ok(hash)
}


impl LmdbMetaStore {
    pub fn confirmation_pending_txn(
        &self,
        txn: &crate::LmdbReadTransaction<'_>,
        hash: &BlockHash,
    ) -> Result<bool, StoreError> {
        match self
            .meta_db
            .get(&txn.txn, &key(hash))
            .map_err(LmdbError::from)?
        {
            Some([1]) => Ok(true),
            None => Ok(false),
            Some(_) => Err(StoreError::Corruption(
                "invalid confirmation queue record".into(),
            )),
        }
    }

    /// Persist a local choice without checking block presence or consensus.
    /// Repeated hashes are idempotent; a new entry scans the capped queue.
    /// Use the writer's `handoff_confirmation` to check block presence atomically.
    pub fn enqueue_confirmation(&self, hash: &BlockHash) -> Result<(), StoreError> {
        if hash.is_zero() {
            return Err(StoreError::Corruption("zero confirmation target".into()));
        }
        let mut txn = self.env.write_txn().map_err(LmdbError::from)?;
        let key = key(hash);
        match self.meta_db.get(&txn, &key).map_err(LmdbError::from)? {
            Some([1]) => {}
            Some(_) => {
                return Err(StoreError::Corruption(
                    "invalid confirmation queue record".into(),
                ))
            }
            None => {
                let mut count = 0;
                for item in self
                    .meta_db
                    .prefix_iter(&txn, PREFIX)
                    .map_err(LmdbError::from)?
                {
                    item.map_err(LmdbError::from)?;
                    count += 1;
                    if count >= CAPACITY {
                        return Err(StoreError::Backend("confirmation queue full".into()));
                    }
                }
                self.meta_db
                    .put(&mut txn, &key, &[1])
                    .map_err(LmdbError::from)?;
            }
        }
        txn.commit().map_err(LmdbError::from)?;
        self.env.force_sync().map_err(LmdbError::from)?;
        Ok(())
    }

    /// Return the complete capped queue in hash order, not arrival order.
    /// Scheduling and rotation belong to the caller. Records survive restart.
    pub fn pending_confirmations(&self) -> Result<Vec<BlockHash>, StoreError> {
        let txn = self.env.read_txn().map_err(LmdbError::from)?;
        self.pending_confirmations_txn(&crate::LmdbReadTransaction::from_ro(txn))
    }

    pub fn pending_confirmations_txn(
        &self,
        txn: &crate::LmdbReadTransaction<'_>,
    ) -> Result<Vec<BlockHash>, StoreError> {
        let mut hashes = Vec::new();
        for item in self
            .meta_db
            .prefix_iter(&txn.txn, PREFIX)
            .map_err(LmdbError::from)?
        {
            let (key, value) = item.map_err(LmdbError::from)?;
            let bytes: [u8; 32] = key[PREFIX.len()..]
                .try_into()
                .map_err(|_| StoreError::Corruption("invalid confirmation queue key".into()))?;
            let hash = BlockHash::new(bytes);
            if hash.is_zero() || value != [1] || hashes.len() >= CAPACITY {
                return Err(StoreError::Corruption(
                    "invalid confirmation queue record or size".into(),
                ));
            }
            hashes.push(hash);
        }
        Ok(hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LmdbStore;

    #[test]
    fn pending_lookup_uses_the_callers_snapshot_and_rejects_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let meta = store.meta_store();
        let hash = BlockHash::new([17; 32]);
        let before = store.tx_begin_read().unwrap();
        assert!(!meta.confirmation_pending_txn(&before, &hash).unwrap());
        assert!(meta.pending_confirmations_txn(&before).unwrap().is_empty());
        meta.enqueue_confirmation(&hash).unwrap();
        assert!(!meta.confirmation_pending_txn(&before, &hash).unwrap());
        assert!(meta.pending_confirmations_txn(&before).unwrap().is_empty());
        drop(before);
        assert_eq!(
            meta.pending_confirmations_txn(&store.tx_begin_read().unwrap())
                .unwrap(),
            vec![hash]
        );
        assert!(meta
            .confirmation_pending_txn(&store.tx_begin_read().unwrap(), &hash)
            .unwrap());
        let mut txn = meta.env.write_txn().unwrap();
        meta.meta_db.put(&mut txn, &key(&hash), &[2]).unwrap();
        txn.commit().unwrap();
        assert!(meta
            .confirmation_pending_txn(&store.tx_begin_read().unwrap(), &hash)
            .is_err());
    }

    #[test]
    fn writer_handoff_checks_presence_and_poisoned_batches_cannot_commit() {
        use burst_store::block::BlockStore;
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let hash = BlockHash::new([77; 32]);
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta("tentative", b"value").unwrap();
        assert!(batch.handoff_confirmation(&hash).is_err());
        assert!(batch.commit().is_err());
        assert!(store
            .tx_begin_write()
            .unwrap()
            .get_meta("tentative")
            .unwrap()
            .is_none());
        assert!(store
            .meta_store()
            .pending_confirmations()
            .unwrap()
            .is_empty());

        let mut batch = store.tx_begin_write().unwrap();
        batch.put_block(&hash, b"block").unwrap();
        batch.handoff_confirmation(&hash).unwrap();
        batch.commit().unwrap();
        assert!(store.block_store().exists(&hash).unwrap());
        assert_eq!(
            store.meta_store().pending_confirmations().unwrap(),
            vec![hash]
        );
    }

    #[test]
    fn capacity_backpressure_preserves_existing_decisions() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let meta = store.meta_store();
        let mut txn = meta.env.write_txn().unwrap();
        for i in 1..=CAPACITY {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&(i as u64).to_be_bytes());
            meta.meta_db
                .put(&mut txn, &key(&BlockHash::new(bytes)), &[1])
                .unwrap();
        }
        txn.commit().unwrap();
        let hashes = meta.pending_confirmations().unwrap();
        assert_eq!(hashes.len(), CAPACITY);
        assert!(meta
            .enqueue_confirmation(&BlockHash::new([255; 32]))
            .is_err());
        meta.enqueue_confirmation(&hashes[0]).unwrap();
        assert_eq!(meta.pending_confirmations().unwrap(), hashes);
    }

    #[test]
    fn corrupt_queue_records_do_not_disappear_silently() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let meta = store.meta_store();
        let hash = BlockHash::new([7; 32]);
        assert!(meta.enqueue_confirmation(&BlockHash::ZERO).is_err());
        let mut txn = meta.env.write_txn().unwrap();
        meta.meta_db.put(&mut txn, &key(&hash), &[99]).unwrap();
        txn.commit().unwrap();
        assert!(meta.pending_confirmations().is_err());
        assert!(meta.enqueue_confirmation(&hash).is_err());
        let mut txn = meta.env.write_txn().unwrap();
        meta.meta_db.delete(&mut txn, &key(&hash)).unwrap();
        meta.meta_db.put(&mut txn, PREFIX, &[1]).unwrap();
        txn.commit().unwrap();
        assert!(meta.pending_confirmations().is_err());
    }

    #[test]
    fn queue_survives_reopen_and_acknowledgment_is_transactional() {
        let dir = tempfile::tempdir().unwrap();
        let hash = BlockHash::new([7; 32]);
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        store.meta_store().enqueue_confirmation(&hash).unwrap();
        store.meta_store().enqueue_confirmation(&hash).unwrap();
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        assert_eq!(
            store.meta_store().pending_confirmations().unwrap(),
            vec![hash]
        );
        {
            let mut batch = store.tx_begin_write().unwrap();
            batch.acknowledge_confirmation(&hash).unwrap();
        }
        assert_eq!(
            store.meta_store().pending_confirmations().unwrap(),
            vec![hash]
        );
        let mut batch = store.tx_begin_write().unwrap();
        batch.acknowledge_confirmation(&hash).unwrap();
        batch.commit().unwrap();
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        assert!(store
            .meta_store()
            .pending_confirmations()
            .unwrap()
            .is_empty());
    }
}
