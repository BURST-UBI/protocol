//! Inline or paged storage of encoded payment-authority records.
//! Page commitments detect storage inconsistency; authority authentication is
//! performed by the payment_authority reader and startup replay.
use crate::paged_metadata::{self, Location, Reader};
use burst_store::{meta::{CERTIFIED_PAYMENT_AUTHORITY_KEY, CERTIFIED_PAYMENT_AUTHORITY_PAGE_PREFIX}, StoreError};
use burst_store_lmdb::{LmdbStore, LmdbWriteTransaction};
use std::io::Read;

const MAX_BYTES: usize = 64 << 20;
struct AuthorityLocation;
impl Location for AuthorityLocation {
    fn descriptor_key(&self) -> &str { CERTIFIED_PAYMENT_AUTHORITY_KEY }
    fn page_key(&self, index: usize) -> String {
        format!("{CERTIFIED_PAYMENT_AUTHORITY_PAGE_PREFIX}{index:08x}")
    }
}
fn invalid(error: impl ToString) -> StoreError { StoreError::Corruption(error.to_string()) }

/// Collect the complete record, limited to 64 MiB of encoded data. The descriptor
/// and every page must come from the same pinned transaction; this returns a
/// contiguous buffer rather than streaming authority decoding.
pub(crate) fn read_at(
    mut get: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
) -> Result<Option<Vec<u8>>, StoreError> {
    let Some(stored) = get(CERTIFIED_PAYMENT_AUTHORITY_KEY)? else { return Ok(None); };
    if !paged_metadata::is_paged(&stored)? {
        if stored.len() > MAX_BYTES { return Err(invalid("installed authority exceeds byte limit")); }
        return Ok(Some(stored));
    }
    let mut descriptor = Some(stored);
    let mut reader = Reader::open_at_bounded(AuthorityLocation, |key| {
        if key == CERTIFIED_PAYMENT_AUTHORITY_KEY { Ok(descriptor.take()) } else { get(key) }
    }, MAX_BYTES)?.ok_or_else(|| invalid("installed authority disappeared"))?;
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(invalid)?;
    reader.commitment()?;
    Ok(Some(bytes))
}

pub(crate) fn read(store: &LmdbStore) -> Result<Option<Vec<u8>>, StoreError> {
    let txn = store.tx_begin_read()?;
    let meta = store.meta_store();
    read_at(|key| meta.get_meta_txn(&txn, key))
}

/// Stage the record in the caller's transaction; abort on error and commit on
/// success. Reads the old record in full to skip equal bytes. Equal inline bytes
/// remain inline; this helper does not force a representation change.
pub(crate) fn write(batch: &mut LmdbWriteTransaction<'_>, bytes: &[u8]) -> Result<(), StoreError> {
    if bytes.is_empty() || bytes.len() > MAX_BYTES { return Err(invalid("invalid installed authority size")); }
    if read_at(|key| batch.get_meta(key))?.as_deref() == Some(bytes) { return Ok(()); }
    if bytes.len() <= paged_metadata::PAGE_BYTES {
        paged_metadata::delete_at(AuthorityLocation, batch, MAX_BYTES)?;
        batch.put_meta(CERTIFIED_PAYMENT_AUTHORITY_KEY, bytes)
    } else {
        paged_metadata::write_bytes_at(AuthorityLocation, batch, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paged_authority_preserves_logical_bytes_abort_pinned_reads_and_shrink() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let bytes = vec![7; 9001];
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(CERTIFIED_PAYMENT_AUTHORITY_KEY, &bytes).unwrap();
        batch.commit().unwrap();
        assert_eq!(read(&store).unwrap(), Some(bytes.clone()));
        // Force the paged form to exercise pinned reads across a later shrink.
        let mut batch = store.tx_begin_write().unwrap();
        paged_metadata::write_bytes_at(AuthorityLocation, &mut batch, &bytes).unwrap();
        batch.commit().unwrap();
        let pinned = store.tx_begin_read().unwrap();
        let meta = store.meta_store();
        let descriptor = meta.get_meta_txn(&pinned, CERTIFIED_PAYMENT_AUTHORITY_KEY).unwrap().unwrap();
        assert!(paged_metadata::is_paged(&descriptor).unwrap());
        assert_eq!(read_at(|key| meta.get_meta_txn(&pinned, key)).unwrap(), Some(bytes.clone()));
        {
            let mut batch = store.tx_begin_write().unwrap();
            batch.delete_meta(&AuthorityLocation.page_key(1)).unwrap();
            assert!(read_at(|key| batch.get_meta(key)).is_err());
        }
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(read_at(|key| batch.get_meta(key)).unwrap(), Some(bytes.clone()));
        write(&mut batch, b"small").unwrap();
        assert!(batch.checkpoint_metadata_entries(CERTIFIED_PAYMENT_AUTHORITY_PAGE_PREFIX.as_bytes()).unwrap().next().is_none());
        batch.commit().unwrap();
        assert_eq!(read_at(|key| meta.get_meta_txn(&pinned, key)).unwrap(), Some(bytes));
        drop(pinned);
        assert_eq!(read(&store).unwrap(), Some(b"small".to_vec()));
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        assert_eq!(read(&store).unwrap(), Some(b"small".to_vec()));
    }

    #[test]
    fn authority_descriptor_limits_and_page_hashes_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        write(&mut batch, &vec![9; 9001]).unwrap();
        let descriptor = batch.get_meta(CERTIFIED_PAYMENT_AUTHORITY_KEY).unwrap().unwrap();
        let mut oversized = descriptor.clone();
        oversized[16..24].copy_from_slice(&((MAX_BYTES as u64) + 1).to_be_bytes());
        assert!(read_at(|key| {
            assert_eq!(key, CERTIFIED_PAYMENT_AUTHORITY_KEY, "bound must precede page reads");
            Ok(Some(oversized.clone()))
        }).is_err());
        batch.put_meta(&AuthorityLocation.page_key(0), &vec![8; paged_metadata::PAGE_BYTES]).unwrap();
        assert_eq!(batch.get_meta(CERTIFIED_PAYMENT_AUTHORITY_KEY).unwrap(), Some(descriptor));
        assert!(read_at(|key| batch.get_meta(key)).is_err());
        assert!(write(&mut batch, b"replacement").is_err());
    }
}
