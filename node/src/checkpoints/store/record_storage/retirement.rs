//! Deferred cleanup of the legacy record page namespace after migration.
//! A later paged replacement can reuse page keys: preserve its entire live range.
use super::{invalid, is_record_key, shared, RecordLocation, MAX_RECORD_BYTES};
use crate::paged_metadata::{page_count, Location, PAGE_BYTES};
use burst_store::StoreError;
use burst_store_lmdb::LmdbWriteTransaction;

const PAGES_PER_BATCH: usize = crate::paged_metadata::staged::PAGES_PER_BATCH;
const MAGIC: &[u8; 8] = b"HDRETIR1";

fn queue_key(key: &str) -> String {
    let id = burst_types::BlockHash::new(burst_crypto::blake2b_256(key.as_bytes()));
    format!("checkpoint_heads/retirement/v1/{id}")
}

fn encode(pages: usize, next: usize) -> Vec<u8> {
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&(pages as u64).to_be_bytes());
    bytes.extend_from_slice(&(next as u64).to_be_bytes());
    bytes
}

fn decode(bytes: &[u8]) -> Result<(usize, usize), StoreError> {
    if bytes.len() != 24 || &bytes[..8] != MAGIC {
        return Err(invalid("invalid record retirement cursor"));
    }
    let pages =
        usize::try_from(u64::from_be_bytes(bytes[8..16].try_into().unwrap())).map_err(invalid)?;
    let next =
        usize::try_from(u64::from_be_bytes(bytes[16..24].try_into().unwrap())).map_err(invalid)?;
    if pages == 0 || pages > (MAX_RECORD_BYTES as usize).div_ceil(PAGE_BYTES) || next >= pages {
        return Err(invalid("record retirement exceeds page bounds"));
    }
    Ok((pages, next))
}

/// Call only after source verification, in the same transaction that replaces
/// its live descriptor. Abort on error. Existing pending cleanup is coalesced.
pub(super) fn schedule(
    batch: &mut LmdbWriteTransaction<'_>,
    key: &str,
    old_descriptor: &[u8],
) -> Result<(), StoreError> {
    if !is_record_key(key)
        || shared::is_shared(old_descriptor)?
        || batch.get_meta(key)?.as_deref() != Some(old_descriptor)
    {
        return Err(invalid("record retirement source changed"));
    }
    let pages = page_count(old_descriptor, MAX_RECORD_BYTES as usize)?;
    if pages == 0 {
        return Ok(());
    }
    let queue = queue_key(key);
    let pages = match batch.get_meta(&queue)? {
        Some(bytes) => pages.max(decode(&bytes)?.0),
        None => pages,
    };
    // Reset when another migration retires newly reused low-index pages.
    batch.put_meta(&queue, &encode(pages, 0))
}

/// Examine at most sixteen page indices. A newer paged value owns all indices
/// below its count, regardless of page size or whether their bytes changed.
pub(super) fn step(batch: &mut LmdbWriteTransaction<'_>, key: &str) -> Result<bool, StoreError> {
    if !is_record_key(key) {
        return Err(invalid("invalid record retirement key"));
    }
    let queue = queue_key(key);
    let Some(bytes) = batch.get_meta(&queue)? else {
        return Ok(true);
    };
    let (pages, next) = decode(&bytes)?;
    let protected = match batch.get_meta(key)? {
        Some(bytes) if !shared::is_shared(&bytes)? => {
            page_count(&bytes, MAX_RECORD_BYTES as usize)?
        }
        _ => 0,
    };
    let end = pages.min(next + PAGES_PER_BATCH);
    let location = RecordLocation::new(key);
    for index in next..end {
        if index >= protected {
            batch.delete_meta(&location.page_key(index))?;
        }
    }
    if end == pages {
        batch.delete_meta(&queue)?;
        Ok(true)
    } else {
        batch.put_meta(&queue, &encode(pages, end))?;
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store_lmdb::LmdbStore;

    #[test]
    fn retirement_resumes_and_preserves_reused_pages_and_pinned_readers() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let key = "checkpoint_record_archive/v1/dev/retirement/1";
        let original = vec![7; PAGE_BYTES * 33 + 17];
        let newer = vec![9; PAGE_BYTES * 20 + 1];
        let mut batch = store.tx_begin_write().unwrap();
        super::super::put(&mut batch, key, &original).unwrap();
        batch.commit().unwrap();
        let pinned = store.tx_begin_read().unwrap();
        {
            let mut aborted = store.tx_begin_write().unwrap();
            let old = aborted.get_meta(key).unwrap().unwrap();
            schedule(&mut aborted, key, &old).unwrap();
            aborted.put_meta(key, b"replacement").unwrap();
        }
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch.get_meta(&queue_key(key)).unwrap().is_none());
        let old = batch.get_meta(key).unwrap().unwrap();
        schedule(&mut batch, key, &old).unwrap();
        batch.put_meta(key, b"replacement").unwrap();
        batch.commit().unwrap();
        // A replacement reuses the same page namespace before cleanup runs.
        let mut batch = store.tx_begin_write().unwrap();
        super::super::put(&mut batch, key, &newer).unwrap();
        batch.commit().unwrap();
        for done in [false, false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            assert_eq!(step(&mut batch, key).unwrap(), done);
            assert_eq!(
                super::super::get_batch(&batch, key).unwrap().unwrap(),
                newer
            );
            batch.commit().unwrap();
        }
        assert_eq!(
            super::super::get_txn(&store.meta_store(), &pinned, key)
                .unwrap()
                .unwrap(),
            original
        );
        drop(pinned);
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(
            super::super::get_batch(&batch, key).unwrap().unwrap(),
            newer
        );
        for index in 21..34 {
            assert!(batch
                .get_meta(&RecordLocation::new(key).page_key(index))
                .unwrap()
                .is_none());
        }
    }

    #[test]
    fn malformed_retirement_cursor_cannot_delete_pages() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let key = "checkpoint_record_archive/v1/dev/retirement/1";
        for cursor in [encode(usize::MAX, 0), encode(1, 1), b"invalid".to_vec()] {
            let mut batch = store.tx_begin_write().unwrap();
            batch.put_meta(&queue_key(key), &cursor).unwrap();
            batch
                .put_meta(&RecordLocation::new(key).page_key(0), b"retain")
                .unwrap();
            assert!(step(&mut batch, key).is_err());
            assert_eq!(
                batch
                    .get_meta(&RecordLocation::new(key).page_key(0))
                    .unwrap()
                    .unwrap(),
                b"retain"
            );
        }
    }
}
