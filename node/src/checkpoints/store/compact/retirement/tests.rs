use super::*;

const MIB: usize = 1 << 20;
const ROWS: [(u64, usize); 6] = [
    (1, MIB),
    (2, MIB),
    (10, 15 * MIB),
    (11, 15 * MIB),
    (12, 15 * MIB),
    (13, 15 * MIB),
];

fn anchor() -> CheckpointAnchor {
    ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &ProtocolParams::default()).unwrap()
}

// Opaque payloads isolate size limits, archival and transaction rollback.
// They do not exercise certificate or economic validation.
fn seed(directory: &std::path::Path) {
    let store = LmdbStore::open(directory, 32, 256 * MIB).unwrap();
    let prefix = prefix(&anchor());
    let mut batch = store.tx_begin_write().unwrap();
    for (height, bytes) in ROWS {
        batch
            .put_meta(&format!("{prefix}/{height}"), &vec![height as u8; bytes])
            .unwrap();
    }
    batch
        .put_meta(&format!("{prefix}/head"), &13u64.to_be_bytes())
        .unwrap();
    batch.put_meta("unrelated-preference", b"keep").unwrap();
    batch.commit().unwrap();
    store.force_sync().unwrap();
}

fn stage_successor(batch: &mut LmdbWriteTransaction<'_>) {
    let prefix = prefix(&anchor());
    record_storage::put(batch, &format!("{prefix}/14"), &vec![14; 16 * MIB])
        .unwrap();
    batch
        .put_meta(&format!("{prefix}/head"), &14u64.to_be_bytes())
        .unwrap();
}

fn verify(store: &LmdbStore, committed: bool) {
    let anchor = anchor();
    let prefix = prefix(&anchor);
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(
        decode_tip(
            record_storage::get_batch(&batch, &format!("{prefix}/head"))
                .unwrap()
                .as_deref()
        )
        .unwrap(),
        if committed { 14 } else { 13 }
    );
    for (height, size) in ROWS {
        let active = record_storage::get_batch(&batch, &format!("{prefix}/{height}")).unwrap();
        let archived = record_storage::get_batch(&batch, &archived_record_key(&anchor, height))
            .unwrap();
        let bytes = if committed && height <= 10 {
            assert!(active.is_none());
            archived.unwrap()
        } else {
            assert!(archived.is_none());
            active.unwrap()
        };
        assert_eq!(bytes.len(), size);
        assert!(bytes.iter().all(|byte| *byte == height as u8));
    }
    let next = record_storage::get_batch(&batch, &format!("{prefix}/14")).unwrap();
    if committed {
        let bytes = next.unwrap();
        assert_eq!(bytes.len(), 16 * MIB);
        assert!(bytes.iter().all(|byte| *byte == 14));
    } else {
        assert!(next.is_none());
    }
    assert_eq!(
        record_storage::get_batch(&batch, "unrelated-preference").unwrap().as_deref(),
        Some(b"keep".as_slice())
    );
    assert!(record_storage::get_batch(&batch, crate::confirmed_replay::snapshot::installed::BASE_KEY)
        .unwrap()
        .is_none());
}

#[test]
fn byte_pressure_retires_numeric_oldest_records_atomically_and_preserves_bytes() {
    let directory = tempfile::tempdir().unwrap();
    seed(directory.path());
    let store = LmdbStore::open(directory.path(), 32, 256 * MIB).unwrap();
    let anchor = anchor();
    {
        let mut batch = store.tx_begin_write().unwrap();
        assert!(make_room(&mut batch, &anchor, 13, MAX_RECORD_BYTES as usize + 1).is_err());
    }
    verify(&store, false);
    for conflict in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        if conflict {
            batch
                .put_meta(&archived_record_key(&anchor, 10), b"conflict")
                .unwrap();
        }
        assert_eq!(
            make_room(&mut batch, &anchor, 13, 16 * MIB).is_err(),
            conflict
        );
        // Both earlier moves happened before the conflict at height 10.
        assert!(record_storage::get_batch(&batch, &archived_record_key(&anchor, 1))
            .unwrap()
            .is_some());
        assert!(record_storage::get_batch(&batch, &archived_record_key(&anchor, 2))
            .unwrap()
            .is_some());
        drop(batch);
        verify(&store, false);
    }
    let mut batch = store.tx_begin_write().unwrap();
    make_room(&mut batch, &anchor, 13, 16 * MIB).unwrap();
    stage_successor(&mut batch);
    batch.commit().unwrap();
    store.force_sync().unwrap();
    drop(store);
    let store = LmdbStore::open(directory.path(), 32, 256 * MIB).unwrap();
    verify(&store, true);
    let read = store.tx_begin_read().unwrap();
    let inventory =
        crate::checkpoint_store::inventory::fingerprints(&store, &read, &prefix(&anchor), 14)
            .unwrap();
    assert_eq!(
        inventory.keys().copied().collect::<Vec<_>>(),
        vec![11, 12, 13, 14]
    );
}

#[cfg(unix)]
mod crash;
