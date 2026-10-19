//! Validated fork payloads kept separately from the applied ledger.
use crate::{LmdbError, LmdbMetaStore};
use burst_store::StoreError;
use burst_types::BlockHash;

const PREFIX: &[u8] = b"fork_candidate/v1/";
const MAX_CONTENT_BYTES: usize = 512 * 1024;
pub(crate) fn content_key(hash: &BlockHash) -> Vec<u8> {
    [b"fork_content/v1/".as_slice(), hash.as_bytes()].concat()
}
pub(crate) fn key(hash: &BlockHash) -> Vec<u8> {
    [PREFIX, hash.as_bytes()].concat()
}

impl LmdbMetaStore {
    /// Re-admission input after restart, never proof that an election won.
    pub fn fork_candidates(&self) -> Result<Vec<(BlockHash, Vec<u8>)>, StoreError> {
        let txn = self.env.read_txn().map_err(LmdbError::from)?;
        let mut candidates = Vec::new();
        for row in self
            .meta_db
            .prefix_iter(&txn, PREFIX)
            .map_err(LmdbError::from)?
        {
            let (key, bytes) = row.map_err(LmdbError::from)?;
            let hash = BlockHash::new(
                key[PREFIX.len()..]
                    .try_into()
                    .map_err(|_| StoreError::Corruption("invalid fork payload key".into()))?,
            );
            if hash.is_zero()
                || bytes.is_empty()
                || bytes.len() > 128 * 1024
                || candidates.len() >= 256
            {
                return Err(StoreError::Corruption("invalid fork payload cache".into()));
            }
            candidates.push((hash, bytes.to_vec()));
        }
        Ok(candidates)
    }

    /// The node must validate against the candidate's predecessor before calling.
    /// No eviction: a full cache applies backpressure instead of losing evidence
    /// for a signed vote. Candidates do not appear in ledger/height indexes.
    pub fn put_fork_candidate(&self, hash: &BlockHash, bytes: &[u8]) -> Result<(), StoreError> {
        self.put_fork_candidate_with_content(hash, bytes, None)
    }

    /// Both immutable records commit together. Content authenticity belongs to
    /// the caller; this cache is bounded storage, never consensus authority.
    pub fn put_fork_candidate_with_content(
        &self,
        hash: &BlockHash,
        bytes: &[u8],
        content: Option<&[u8]>,
    ) -> Result<(), StoreError> {
        if hash.is_zero() || bytes.is_empty() || bytes.len() > 128 * 1024 {
            return Err(StoreError::Corruption(
                "invalid fork payload size or hash".into(),
            ));
        }
        if content.is_some_and(|bytes| bytes.is_empty() || bytes.len() > MAX_CONTENT_BYTES) {
            return Err(StoreError::Corruption("invalid fork content size".into()));
        }
        let mut txn = self.env.write_txn().map_err(LmdbError::from)?;
        let key = key(hash);
        if let Some(existing) = self.meta_db.get(&txn, &key).map_err(LmdbError::from)? {
            if existing != bytes
                || self
                    .meta_db
                    .get(&txn, &content_key(hash))
                    .map_err(LmdbError::from)?
                    != content
            {
                return Err(StoreError::Corruption("fork payload changed".into()));
            }
            return Ok(());
        }
        if self
            .meta_db
            .get(&txn, &content_key(hash))
            .map_err(LmdbError::from)?
            .is_some()
        {
            return Err(StoreError::Corruption("orphan fork content".into()));
        }
        for (count, row) in self
            .meta_db
            .prefix_iter(&txn, PREFIX)
            .map_err(LmdbError::from)?
            .enumerate()
        {
            row.map_err(LmdbError::from)?;
            if count >= 255 {
                return Err(StoreError::Backend("fork payload cache full".into()));
            }
        }
        self.meta_db
            .put(&mut txn, &key, bytes)
            .map_err(LmdbError::from)?;
        if let Some(content) = content {
            self.meta_db
                .put(&mut txn, &content_key(hash), content)
                .map_err(LmdbError::from)?;
        }
        txn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    pub fn fork_candidate(&self, hash: &BlockHash) -> Result<Option<Vec<u8>>, StoreError> {
        let txn = self.env.read_txn().map_err(LmdbError::from)?;
        self.fork_candidate_txn(&crate::LmdbReadTransaction { txn }, hash)
    }

    pub fn fork_candidate_txn(
        &self,
        txn: &crate::LmdbReadTransaction<'_>,
        hash: &BlockHash,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let value = self
            .meta_db
            .get(&txn.txn, &key(hash))
            .map_err(LmdbError::from)?;
        match value {
            Some(bytes) if bytes.is_empty() || bytes.len() > 128 * 1024 => {
                Err(StoreError::Corruption("invalid fork payload".into()))
            }
            _ => Ok(value.map(<[u8]>::to_vec)),
        }
    }

    pub fn fork_content(&self, hash: &BlockHash) -> Result<Option<Vec<u8>>, StoreError> {
        let txn = self.env.read_txn().map_err(LmdbError::from)?;
        self.fork_content_txn(&crate::LmdbReadTransaction { txn }, hash)
    }

    pub fn fork_content_txn(
        &self,
        txn: &crate::LmdbReadTransaction<'_>,
        hash: &BlockHash,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let content = self
            .meta_db
            .get(&txn.txn, &content_key(hash))
            .map_err(LmdbError::from)?;
        if content.is_some_and(|bytes| bytes.is_empty() || bytes.len() > MAX_CONTENT_BYTES)
            || (content.is_some()
                && self
                    .meta_db
                    .get(&txn.txn, &key(hash))
                    .map_err(LmdbError::from)?
                    .is_none())
        {
            return Err(StoreError::Corruption("invalid fork content cache".into()));
        }
        Ok(content.map(<[u8]>::to_vec))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LmdbStore;
    use burst_store::block::BlockStore;

    #[test]
    fn candidate_content_is_atomic_immutable_and_recoverable() {
        let dir = tempfile::tempdir().unwrap();
        let hash = BlockHash::new([4; 32]);
        let store = LmdbStore::open(dir.path(), 32, 64 << 20).unwrap();
        let meta = store.meta_store();
        assert!(meta
            .put_fork_candidate_with_content(&hash, b"block", Some(&vec![0; MAX_CONTENT_BYTES + 1]))
            .is_err());
        assert_eq!(meta.fork_candidate(&hash).unwrap(), None);
        assert_eq!(meta.fork_content(&hash).unwrap(), None);
        meta.put_fork_candidate_with_content(&hash, b"block", Some(b"content"))
            .unwrap();
        assert!(meta
            .put_fork_candidate_with_content(&hash, b"block", Some(b"changed"))
            .is_err());
        assert!(meta.put_fork_candidate(&hash, b"block").is_err());
        drop(meta);
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 64 << 20).unwrap();
        assert_eq!(
            store.meta_store().fork_candidate(&hash).unwrap(),
            Some(b"block".to_vec())
        );
        assert_eq!(
            store.meta_store().fork_content(&hash).unwrap(),
            Some(b"content".to_vec())
        );
    }

    #[test]
    fn bounded_immutable_candidates_survive_reopen_without_becoming_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 64 * 1024 * 1024).unwrap();
        let hash = |n: u16| {
            let mut bytes = [0; 32];
            bytes[..2].copy_from_slice(&n.to_be_bytes());
            BlockHash::new(bytes)
        };
        assert!(store
            .meta_store()
            .put_fork_candidate(&BlockHash::ZERO, b"a")
            .is_err());
        assert!(store
            .meta_store()
            .put_fork_candidate(&hash(1), &[])
            .is_err());
        assert!(store
            .meta_store()
            .put_fork_candidate(&hash(1), &vec![0; 128 * 1024 + 1])
            .is_err());
        for n in 1..=256 {
            store
                .meta_store()
                .put_fork_candidate(&hash(n), b"candidate")
                .unwrap();
        }
        assert!(store
            .meta_store()
            .put_fork_candidate(&hash(257), b"overflow")
            .is_err());
        store
            .meta_store()
            .put_fork_candidate(&hash(1), b"candidate")
            .unwrap();
        assert!(store
            .meta_store()
            .put_fork_candidate(&hash(1), b"changed")
            .is_err());
        assert!(!store.block_store().exists(&hash(1)).unwrap());
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 64 * 1024 * 1024).unwrap();
        assert_eq!(
            store.meta_store().fork_candidate(&hash(1)).unwrap(),
            Some(b"candidate".to_vec())
        );
        assert_eq!(store.meta_store().fork_candidate(&hash(257)).unwrap(), None);
        assert_eq!(store.meta_store().fork_candidates().unwrap().len(), 256);
    }
}
