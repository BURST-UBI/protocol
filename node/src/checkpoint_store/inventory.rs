//! Enumerate stored records without treating a numeric height bound as a key scan.
use super::*;
use std::collections::BTreeMap;

pub(super) fn read(
    store: &LmdbStore,
    txn: &burst_store_lmdb::LmdbReadTransaction<'_>,
    prefix: &str,
    tip: u64,
) -> Result<BTreeMap<u64, Vec<u8>>, StoreError> {
    read_map(store, txn, prefix, tip, |bytes| bytes.to_vec())
}

pub(super) fn fingerprints(
    store: &LmdbStore,
    txn: &burst_store_lmdb::LmdbReadTransaction<'_>,
    prefix: &str,
    tip: u64,
) -> Result<BTreeMap<u64, burst_types::BlockHash>, StoreError> {
    read_map(store, txn, prefix, tip, |bytes| {
        burst_types::BlockHash::new(burst_crypto::blake2b_256(bytes))
    })
}

fn read_map<T>(
    store: &LmdbStore,
    txn: &burst_store_lmdb::LmdbReadTransaction<'_>,
    prefix: &str,
    tip: u64,
    mut map: impl FnMut(&[u8]) -> T,
) -> Result<BTreeMap<u64, T>, StoreError> {
    let prefix = format!("{prefix}/");
    let mut records = BTreeMap::new();
    store.meta_store().visit_prefix_bounded_txn(
        txn,
        &prefix,
        MAX_CHECKPOINTS as usize + 1,
        MAX_CHAIN_BYTES,
        |key, bytes| {
            let suffix = key
                .strip_prefix(&prefix)
                .ok_or_else(|| invalid("invalid checkpoint key"))?;
            if suffix != "head" {
                let height: u64 = suffix.parse().map_err(invalid)?;
                if height == 0 || suffix != height.to_string() {
                    return Err(invalid("invalid checkpoint record key"));
                }
                if height > tip {
                    return Err(invalid("checkpoint record exists beyond its chain head"));
                }
                if records.len() >= MAX_CHECKPOINTS as usize {
                    return Err(invalid("checkpoint inventory requires incremental replay"));
                }
                records.insert(height, map(bytes));
            }
            Ok(())
        },
    )?;
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::meta::MetaStore;

    #[test]
    fn inventory_pages_preserve_numeric_order_and_reject_hidden_records() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let prefix = "checkpoint_chain/v1/dev/test";
        for height in 1u64..=33 {
            store
                .meta_store()
                .put_meta(&format!("{prefix}/{height}"), &height.to_be_bytes())
                .unwrap();
        }
        let txn = store.tx_begin_read().unwrap();
        let rows = read(&store, &txn, prefix, 33).unwrap();
        assert_eq!(
            rows.keys().copied().collect::<Vec<_>>(),
            (1..=33).collect::<Vec<_>>()
        );
        drop(txn);
        for suffix in ["129", "999999", "01", "0", "bogus"] {
            let txn = store.tx_begin_read().unwrap();
            let key = format!("{prefix}/{suffix}");
            store.meta_store().put_meta(&key, b"hidden").unwrap();
            assert_eq!(read(&store, &txn, prefix, 33).unwrap().len(), 33);
            drop(txn);
            assert!(
                read(&store, &store.tx_begin_read().unwrap(), prefix, 33).is_err(),
                "{suffix}"
            );
            store.meta_store().delete_meta(&key).unwrap();
        }
    }

    #[test]
    fn inventory_bounds_record_count_not_numeric_height() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let prefix = "checkpoint_chain/v1/dev/test";
        for height in 1000u64..1128 {
            store
                .meta_store()
                .put_meta(&format!("{prefix}/{height}"), b"record")
                .unwrap();
        }
        assert_eq!(
            read(&store, &store.tx_begin_read().unwrap(), prefix, 1127)
                .unwrap()
                .len(),
            128
        );
        store
            .meta_store()
            .put_meta(&format!("{prefix}/1128"), b"record")
            .unwrap();
        assert!(read(&store, &store.tx_begin_read().unwrap(), prefix, 1128).is_err());
    }

    #[test]
    fn loader_rejects_orphan_records_outside_the_old_height_scan() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let params = ProtocolParams::default();
        let anchor = ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &params).unwrap();
        store
            .meta_store()
            .put_meta(&format!("{}/999", super::super::prefix(&anchor)), b"orphan")
            .unwrap();
        assert!(chain::load(&store, NetworkId::Dev, &params).is_err());
    }
}
