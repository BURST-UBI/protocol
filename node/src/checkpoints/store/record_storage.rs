//! Logical checkpoint bytes, independent of inline, paged or shared-head storage.
//! Decoding storage never authenticates a certificate or its history.
use super::{invalid, MAX_RECORD_BYTES};
use crate::paged_metadata::{Commitment, Location, Reader};
use burst_store::StoreError;
use burst_store_lmdb::checkpoint_heads::{HeadNode, NodeHash};
use std::borrow::Cow;
use std::io::Read;
pub(crate) mod maintenance;
mod retirement;
mod shared;
mod source;

fn is_record_key(key: &str) -> bool {
    (key.starts_with("checkpoint_chain/v1/") || key.starts_with("checkpoint_record_archive/v1/"))
        && key
            .rsplit('/')
            .next()
            .is_some_and(|height| height.parse::<u64>().is_ok())
}

/// Other checkpoint metadata (tips, beacons and witness rows) keeps its encoding.
pub(crate) fn get(
    key: &str,
    mut get_raw: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
    get_node: impl FnMut(&NodeHash) -> Result<Option<HeadNode>, StoreError>,
) -> Result<Option<Vec<u8>>, StoreError> {
    let Some(stored) = get_raw(key)? else {
        return Ok(None);
    };
    if !is_record_key(key) {
        return Ok(Some(stored));
    }
    Ok(Some(
        decode_stored(key, &stored, get_raw, get_node, MAX_RECORD_BYTES as usize)?.into_owned(),
    ))
}

pub(crate) fn get_batch(
    batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
    key: &str,
) -> Result<Option<Vec<u8>>, StoreError> {
    get(
        key,
        |name| batch.get_meta(name),
        |hash| batch.read_view().checkpoint_head_node(hash),
    )
}

pub(crate) fn get_txn(
    meta: &burst_store_lmdb::LmdbMetaStore,
    txn: &burst_store_lmdb::LmdbReadTransaction<'_>,
    key: &str,
) -> Result<Option<Vec<u8>>, StoreError> {
    get(
        key,
        |name| meta.get_meta_txn(txn, name),
        |hash| meta.checkpoint_head_node_txn(txn, hash),
    )
}

/// Compare logical bytes without assembling a second complete record. Paged
/// and shared records are checked to the end even after a mismatch, so a
/// replacement cannot silently repair a corrupt archived dependency.
pub(super) fn matches_batch(
    batch: &burst_store_lmdb::LmdbWriteTransaction<'_>,
    key: &str,
    expected: &[u8],
) -> Result<Option<bool>, StoreError> {
    let Some(stored) = batch.get_meta(key)? else {
        return Ok(None);
    };
    if is_record_key(key) && shared::is_shared(&stored)? {
        let mut offset = 0usize;
        let mut equal = true;
        let commitment = shared::visit(
            key,
            &stored,
            |name| batch.get_meta(name),
            |hash| batch.read_view().checkpoint_head_node(hash),
            MAX_RECORD_BYTES as usize,
            |bytes| {
                let end = offset + bytes.len();
                equal &= expected.get(offset..end) == Some(bytes);
                offset = end;
                Ok(())
            },
        )?;
        return Ok(Some(equal && commitment.size == expected.len()));
    }
    if !is_record_key(key) || !crate::paged_metadata::is_paged(&stored)? {
        if is_record_key(key) && stored.len() as u64 > MAX_RECORD_BYTES {
            return Err(invalid("checkpoint record exceeds logical byte limit"));
        }
        return Ok(Some(stored == expected));
    }
    let mut first = Some(stored);
    let mut reader = Reader::open_at_bounded(
        RecordLocation::new(key),
        |name| {
            if name == key {
                Ok(first.take())
            } else {
                batch.get_meta(name)
            }
        },
        MAX_RECORD_BYTES as usize,
    )?
    .ok_or_else(|| invalid("checkpoint record disappeared"))?;
    let mut buffer = [0; 16 << 10];
    let mut offset = 0usize;
    let mut equal = true;
    loop {
        let count = reader.read(&mut buffer).map_err(invalid)?;
        if count == 0 {
            break;
        }
        let end = offset
            .checked_add(count)
            .ok_or_else(|| invalid("checkpoint comparison size overflow"))?;
        equal &= expected.get(offset..end) == Some(&buffer[..count]);
        offset = end;
    }
    reader.commitment()?;
    Ok(Some(equal && offset == expected.len()))
}

/// Keep canonical record bytes unchanged; only their local representation varies.
/// All page writes and replacement cleanup belong to the caller's writer.
pub(super) fn put(
    batch: &mut burst_store_lmdb::LmdbWriteTransaction<'_>,
    key: &str,
    bytes: &[u8],
) -> Result<(), StoreError> {
    if !is_record_key(key) {
        return batch.put_meta(key, bytes);
    }
    if bytes.is_empty() || bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid("invalid checkpoint record size"));
    }
    if matches_batch(batch, key, bytes)? == Some(true) {
        return Ok(());
    }
    if let Some(prepared) = shared::prepare(bytes)? {
        return publish_shared(batch, key, prepared);
    }
    if let Some(stored) = batch.get_meta(key)? {
        if shared::is_shared(&stored)? {
            shared::remove(batch, key, &stored)?;
        }
    }
    if bytes.len() <= crate::paged_metadata::PAGE_BYTES {
        crate::paged_metadata::delete_at(
            RecordLocation::new(key),
            batch,
            MAX_RECORD_BYTES as usize,
        )?;
        batch.put_meta(key, bytes)
    } else {
        crate::paged_metadata::write_bytes_at(RecordLocation::new(key), batch, bytes)
    }
}

pub(super) fn delete(
    batch: &mut burst_store_lmdb::LmdbWriteTransaction<'_>,
    key: &str,
) -> Result<(), StoreError> {
    if !is_record_key(key) {
        return batch.delete_meta(key);
    }
    if let Some(stored) = batch.get_meta(key)? {
        if shared::is_shared(&stored)? {
            inspect_stored(
                key,
                &stored,
                |name| batch.get_meta(name),
                |hash| batch.read_view().checkpoint_head_node(hash),
                MAX_RECORD_BYTES as usize,
            )?;
            shared::remove(batch, key, &stored)?;
            batch.reclaim_checkpoint_heads(64)?;
            return Ok(());
        }
    }
    crate::paged_metadata::delete_at(RecordLocation::new(key), batch, MAX_RECORD_BYTES as usize)
}

fn publish_shared(
    batch: &mut burst_store_lmdb::LmdbWriteTransaction<'_>,
    key: &str,
    prepared: shared::Prepared<'_>,
) -> Result<(), StoreError> {
    let root = prepared.build_root(batch)?;
    if let Some(stored) = batch.get_meta(key)? {
        if shared::is_shared(&stored)? {
            shared::remove(batch, key, &stored)?;
        } else {
            crate::paged_metadata::delete_at(
                RecordLocation::new(key),
                batch,
                MAX_RECORD_BYTES as usize,
            )?;
        }
    }
    prepared.publish(batch, key, root)?;
    batch.reclaim_checkpoint_heads(64)?;
    Ok(())
}

/// Convert one supported record to shared storage within the caller's transaction.
/// This test helper reads and rewrites the complete record.
#[cfg(test)]
pub(super) fn normalize(
    batch: &mut burst_store_lmdb::LmdbWriteTransaction<'_>,
    key: &str,
) -> Result<bool, StoreError> {
    if !is_record_key(key) {
        return Ok(false);
    }
    let Some(stored) = batch.get_meta(key)? else {
        return Ok(false);
    };
    let bytes = decode_stored(
        key,
        &stored,
        |name| batch.get_meta(name),
        |hash| batch.read_view().checkpoint_head_node(hash),
        MAX_RECORD_BYTES as usize,
    )?
    .into_owned();
    if shared::is_shared(&stored)? {
        return Ok(false);
    }
    let Some(prepared) = shared::prepare(&bytes)? else {
        return Ok(false);
    };
    publish_shared(batch, key, prepared)?;
    Ok(true)
}

struct RecordLocation<'a> {
    key: &'a str,
    page_prefix: String,
}

impl<'a> RecordLocation<'a> {
    fn new(key: &'a str) -> Self {
        let identity = burst_types::BlockHash::new(burst_crypto::blake2b_256(key.as_bytes()));
        Self {
            key,
            page_prefix: format!("checkpoint_chain/pages/v1/{identity}/"),
        }
    }
}

impl Location for RecordLocation<'_> {
    fn descriptor_key(&self) -> &str {
        self.key
    }
    fn page_key(&self, index: usize) -> String {
        format!("{}{index:08x}", self.page_prefix)
    }
}

/// The descriptor, pages and shared nodes must come from the same transaction.
/// Charge the reconstructed length to the caller's aggregate budget as well.
pub(super) fn decode_stored<'a>(
    key: &str,
    descriptor: &'a [u8],
    mut get: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
    get_node: impl FnMut(&NodeHash) -> Result<Option<HeadNode>, StoreError>,
    max_bytes: usize,
) -> Result<Cow<'a, [u8]>, StoreError> {
    let max_bytes = max_bytes.min(MAX_RECORD_BYTES as usize);
    if shared::is_shared(descriptor)? {
        let mut bytes = Vec::new();
        shared::visit(key, descriptor, get, get_node, max_bytes, |part| {
            bytes.extend_from_slice(part);
            Ok(())
        })?;
        return Ok(Cow::Owned(bytes));
    }
    if !crate::paged_metadata::is_paged(descriptor)? {
        if descriptor.len() > max_bytes {
            return Err(invalid("checkpoint record exceeds logical byte limit"));
        }
        return Ok(Cow::Borrowed(descriptor));
    }
    let mut first = Some(descriptor.to_vec());
    let mut reader = Reader::open_at_bounded(
        RecordLocation::new(key),
        |name| {
            if name == key {
                Ok(first.take())
            } else {
                get(name)
            }
        },
        max_bytes,
    )?
    .ok_or_else(|| invalid("checkpoint record disappeared"))?;
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(invalid)?;
    reader.commitment()?;
    Ok(Cow::Owned(bytes))
}

/// Return the logical length and hash without assembling a complete record.
/// Paged and shared representations are verified through the end of the stream.
pub(super) fn inspect_stored(
    key: &str,
    descriptor: &[u8],
    mut get: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
    get_node: impl FnMut(&NodeHash) -> Result<Option<HeadNode>, StoreError>,
    max_bytes: usize,
) -> Result<Commitment, StoreError> {
    let max_bytes = max_bytes.min(MAX_RECORD_BYTES as usize);
    if shared::is_shared(descriptor)? {
        return shared::visit(key, descriptor, get, get_node, max_bytes, |_| Ok(()));
    }
    if !crate::paged_metadata::is_paged(descriptor)? {
        if descriptor.len() > max_bytes {
            return Err(invalid("checkpoint record exceeds logical byte limit"));
        }
        return Ok(Commitment {
            size: descriptor.len(),
            hash: burst_crypto::blake2b_256(descriptor),
        });
    }
    let mut first = Some(descriptor.to_vec());
    let mut reader = Reader::open_at_bounded(
        RecordLocation::new(key),
        |name| {
            if name == key {
                Ok(first.take())
            } else {
                get(name)
            }
        },
        max_bytes,
    )?
    .ok_or_else(|| invalid("checkpoint record disappeared"))?;
    std::io::copy(&mut reader, &mut std::io::sink()).map_err(invalid)?;
    reader.commitment()
}

pub(super) fn validated_size(
    key: &str,
    descriptor: &[u8],
    get: impl FnMut(&str) -> Result<Option<Vec<u8>>, StoreError>,
    get_node: impl FnMut(&NodeHash) -> Result<Option<HeadNode>, StoreError>,
    max_bytes: usize,
) -> Result<usize, StoreError> {
    if shared::is_shared(descriptor)? || crate::paged_metadata::is_paged(descriptor)? {
        Ok(inspect_stored(key, descriptor, get, get_node, max_bytes)?.size)
    } else if descriptor.len() <= max_bytes.min(MAX_RECORD_BYTES as usize) {
        // Inline bytes already provide their logical length.
        Ok(descriptor.len())
    } else {
        Err(invalid("checkpoint record exceeds logical byte limit"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paged_metadata::Writer;
    use burst_store_lmdb::LmdbStore;
    use std::io::Write;

    #[test]
    fn paged_and_inline_records_have_identical_logical_bytes_and_hashes() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 8 << 20).unwrap();
        let key = "checkpoint_chain/v1/dev/test/1";
        let bytes = vec![9; 9001];
        let mut batch = store.tx_begin_write().unwrap();
        let mut writer = Writer::new_at(RecordLocation::new(key), &mut batch).unwrap();
        writer.write_all(&bytes).unwrap();
        writer.finish().unwrap();
        batch.commit().unwrap();
        let txn = store.tx_begin_read().unwrap();
        let descriptor = store.meta_store().get_meta_txn(&txn, key).unwrap().unwrap();
        let paged = decode_stored(
            key,
            &descriptor,
            |name| store.meta_store().get_meta_txn(&txn, name),
            |_| Ok(None),
            bytes.len(),
        )
        .unwrap();
        let inline = decode_stored(
            key,
            &bytes,
            |_| panic!("inline record needs no pages"),
            |_| Ok(None),
            bytes.len(),
        )
        .unwrap();
        assert_eq!(paged, inline);
        assert_eq!(
            burst_crypto::blake2b_256(&paged),
            burst_crypto::blake2b_256(&bytes)
        );
        let info = inspect_stored(
            key,
            &descriptor,
            |name| store.meta_store().get_meta_txn(&txn, name),
            |_| Ok(None),
            bytes.len(),
        )
        .unwrap();
        assert_eq!(info.size, bytes.len());
        assert_eq!(info.hash, burst_crypto::blake2b_256(&bytes));
        assert_eq!(
            inspect_stored(
                key,
                &bytes,
                |_| panic!("inline needs no pages"),
                |_| Ok(None),
                bytes.len()
            )
            .unwrap()
            .hash,
            info.hash
        );
        assert_eq!(
            validated_size(
                key,
                &descriptor,
                |name| store.meta_store().get_meta_txn(&txn, name),
                |_| Ok(None),
                bytes.len()
            )
            .unwrap(),
            bytes.len()
        );
        assert!(inspect_stored(
            key,
            &descriptor,
            |_| panic!("reject budget before pages"),
            |_| Ok(None),
            bytes.len() - 1
        )
        .is_err());
        assert!(validated_size(
            key,
            &descriptor,
            |_| panic!("reject budget before pages"),
            |_| Ok(None),
            bytes.len() - 1
        )
        .is_err());
        assert!(inspect_stored(key, &descriptor, |_| Ok(None), |_| Ok(None), bytes.len()).is_err());
        let prefix = "checkpoint_chain/v1/dev/test";
        let fingerprint = burst_types::BlockHash::new(info.hash);
        assert_eq!(
            super::super::inventory::fingerprints(&store, &txn, prefix, 1).unwrap()[&1],
            fingerprint
        );

        assert!(decode_stored(
            key,
            &descriptor,
            |_| panic!("budget must reject before pages"),
            |_| Ok(None),
            bytes.len() - 1
        )
        .is_err());
        assert!(decode_stored(key, &descriptor, |_| Ok(None), |_| Ok(None), bytes.len()).is_err());
        assert_eq!(
            get_txn(&store.meta_store(), &txn, key).unwrap().unwrap(),
            bytes
        );
        {
            let batch = store.tx_begin_write().unwrap();
            assert_eq!(matches_batch(&batch, key, &bytes).unwrap(), Some(true));
            for candidate in [
                vec![],
                vec![8; bytes.len()],
                bytes[..bytes.len() - 1].to_vec(),
                [bytes.as_slice(), &[9]].concat(),
            ] {
                assert_eq!(matches_batch(&batch, key, &candidate).unwrap(), Some(false));
            }
            assert_eq!(
                matches_batch(&batch, "checkpoint_chain/v1/dev/test/99", &bytes).unwrap(),
                None
            );
        }
        let mut aborted = store.tx_begin_write().unwrap();
        aborted
            .put_meta(&RecordLocation::new(key).page_key(0), &[0; 4064])
            .unwrap();
        assert!(get_batch(&aborted, key).is_err());
        assert_eq!(
            get_txn(&store.meta_store(), &txn, key).unwrap().unwrap(),
            bytes
        );
        drop(aborted);
        // A good descriptor cannot hide a damaged or missing final page.
        let last = RecordLocation::new(key)
            .page_key(bytes.len().div_ceil(crate::paged_metadata::PAGE_BYTES) - 1);
        for missing in [false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            if missing {
                batch.delete_meta(&last).unwrap();
            } else {
                batch
                    .put_meta(
                        &last,
                        &vec![8; bytes.len() % crate::paged_metadata::PAGE_BYTES],
                    )
                    .unwrap();
            }
            assert!(validated_size(
                key,
                &descriptor,
                |name| batch.get_meta(name),
                |_| Ok(None),
                bytes.len()
            )
            .is_err());
            // An early mismatch must not bypass a damaged or missing final page.
            let replacement = vec![3; bytes.len()];
            assert!(matches_batch(&batch, key, &replacement).is_err());
            assert!(put(&mut batch, key, &replacement).is_err());
            batch.commit().unwrap();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let changed = store.tx_begin_read().unwrap();
                    assert!(super::super::inventory::fingerprints(&store, &changed, prefix, 1).is_err());
                    assert!(super::super::inventory::read(&store, &changed, prefix, 1).is_err());
                }).join().unwrap();
            });
            assert_eq!(
                super::super::inventory::fingerprints(&store, &txn, prefix, 1).unwrap()[&1],
                fingerprint
            );
        }
    }

    #[test]
    fn inventory_charges_logical_bytes_instead_of_small_descriptors() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 128 << 20).unwrap();
        let prefix = "checkpoint_chain/v1/dev/test";
        let bytes = vec![4; 14 << 20];
        let mut batch = store.tx_begin_write().unwrap();
        for height in 1..=5 {
            let key = format!("{prefix}/{height}");
            let mut writer = Writer::new_at(RecordLocation::new(&key), &mut batch).unwrap();
            writer.write_all(&bytes).unwrap();
            writer.finish().unwrap();
        }
        batch
            .put_meta(&format!("{prefix}/head"), &5u64.to_be_bytes())
            .unwrap();
        batch.commit().unwrap();
        let txn = store.tx_begin_read().unwrap();
        assert!(super::super::inventory::read(&store, &txn, prefix, 5).is_err());
        assert!(super::super::inventory::fingerprints(&store, &txn, prefix, 5).is_err());
    }

    #[test]
    fn archive_move_abort_shrink_and_delete_preserve_pinned_record_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 8 << 20).unwrap();
        let active = "checkpoint_chain/v1/dev/test/1";
        let archive = "checkpoint_record_archive/v1/dev/test/1";
        let bytes = vec![6; crate::paged_metadata::PAGE_BYTES + 17];
        let mut batch = store.tx_begin_write().unwrap();
        put(&mut batch, active, &bytes).unwrap();
        batch.commit().unwrap();
        let pinned = store.tx_begin_read().unwrap();
        {
            let mut aborted = store.tx_begin_write().unwrap();
            put(&mut aborted, archive, &bytes).unwrap();
            delete(&mut aborted, active).unwrap();
            assert!(get_batch(&aborted, active).unwrap().is_none());
            assert_eq!(get_batch(&aborted, archive).unwrap().unwrap(), bytes);
        }
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(get_batch(&batch, active).unwrap().unwrap(), bytes);
        assert!(get_batch(&batch, archive).unwrap().is_none());
        put(&mut batch, archive, &bytes).unwrap();
        delete(&mut batch, active).unwrap();
        for index in 0..2 {
            assert!(batch
                .get_meta(&RecordLocation::new(active).page_key(index))
                .unwrap()
                .is_none());
        }
        batch.commit().unwrap();
        assert_eq!(
            get_txn(&store.meta_store(), &pinned, active)
                .unwrap()
                .unwrap(),
            bytes
        );
        assert!(get_txn(&store.meta_store(), &pinned, archive)
            .unwrap()
            .is_none());
        drop(pinned);
        let mut batch = store.tx_begin_write().unwrap();
        put(&mut batch, archive, b"small").unwrap();
        for index in 0..2 {
            assert!(batch
                .get_meta(&RecordLocation::new(archive).page_key(index))
                .unwrap()
                .is_none());
        }
        assert_eq!(get_batch(&batch, archive).unwrap().unwrap(), b"small");
        assert_eq!(
            matches_batch(&batch, archive, b"small").unwrap(),
            Some(true)
        );
        assert_eq!(
            matches_batch(&batch, archive, b"other").unwrap(),
            Some(false)
        );
        delete(&mut batch, archive).unwrap();
        batch.commit().unwrap();
        let reopened = store.tx_begin_read().unwrap();
        assert!(get_txn(&store.meta_store(), &reopened, archive)
            .unwrap()
            .is_none());
    }
}

#[cfg(test)]
mod history_reuse;
