use super::*;
use burst_store::meta::MetaStore;
use burst_types::{Timestamp, WalletAddress};
use burst_verification::VerificationOrchestrator;

fn fixture() -> (tempfile::TempDir, LmdbStore, Vec<(String, Vec<u8>)>) {
    let directory = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
    let mut engine = VerificationOrchestrator::new();
    engine.initialize_genesis(&WalletAddress::new("brst_target"));
    let mut rows = Vec::new();
    normalized::visit_state_rows(&engine, |key, bytes| {
        rows.push((key, bytes));
        Ok(())
    })
    .unwrap();
    store
        .meta_store()
        .put_meta("verification_records_format", &[2])
        .unwrap();
    for (key, bytes) in &rows {
        store.meta_store().put_meta(key, bytes).unwrap();
    }
    (directory, store, rows)
}

#[test]
fn verification_pages_pin_state_and_preserve_explicit_completion() {
    let (_dir, store, rows) = fixture();
    let view = store.tx_begin_read().unwrap();
    let mut reader = VerificationReferenceReader::new(&store, &view).unwrap();
    let target = rows
        .iter()
        .find(|(key, _)| key.starts_with(normalized::PREFIX))
        .unwrap();
    store.meta_store().delete_meta(&target.0).unwrap();
    store
        .meta_store()
        .put_meta("verification_records_format", &[1])
        .unwrap();
    let mut total = 0;
    let mut references = Vec::new();
    let mut done = false;
    for _ in 0..rows.len() + 1 {
        let page = reader
            .visit_next_page(1, 4096, |r| {
                references.push(format!("{r:?}"));
                Ok(())
            })
            .unwrap();
        assert!(page.rows <= 1 && page.bytes <= 4096);
        total += page.rows;
        if page.done {
            done = true;
            break;
        }
    }
    assert!(done);
    assert_eq!(total, rows.len());
    assert_eq!(
        references
            .iter()
            .filter(|r| **r
                == format!(
                    "{:?}",
                    Reference::Account(&WalletAddress::new("brst_target"))
                ))
            .count(),
        2
    );
    assert!(references
        .iter()
        .any(|r| *r == format!("{:?}", Reference::Boundary(Timestamp::new(0)))));
    let complete = reader
        .visit_next_page(1, 4096, |_| panic!("already complete"))
        .unwrap();
    assert_eq!(complete.rows, 0);
    assert!(complete.done);
    drop(reader);
    drop(view);
    let view = store.tx_begin_read().unwrap();
    assert!(VerificationReferenceReader::new(&store, &view).is_err());
}

#[test]
fn verification_pages_reject_layout_corruption_budgets_and_callback_failures() {
    let (_dir, store, rows) = fixture();
    let view = store.tx_begin_read().unwrap();
    for (count, bytes) in [(0, 4096), (101, 4096), (1, 0), (1, 1), (1, (64 << 20) + 1)] {
        let mut reader = VerificationReferenceReader::new(&store, &view).unwrap();
        assert!(reader
            .visit_next_page(count, bytes, |_| panic!("invalid budget"))
            .is_err());
        assert!(reader.visit_next_page(1, 4096, |_| Ok(())).is_err());
    }
    let mut reader = VerificationReferenceReader::new(&store, &view).unwrap();
    let mut calls = 0;
    assert!(matches!(reader.visit_next_page(100, 4096, |_| {
        calls += 1; Err(StoreError::Backend("stop".into()))
    }), Err(StoreError::Backend(reason)) if reason == "stop"));
    assert_eq!(calls, 1);
    assert!(reader.visit_next_page(1, 4096, |_| Ok(())).is_err());
    drop(reader);
    drop(view);
    for marker in [None, Some(vec![1]), Some(vec![3])] {
        match marker {
            Some(bytes) => store
                .meta_store()
                .put_meta("verification_records_format", &bytes)
                .unwrap(),
            None => store
                .meta_store()
                .delete_meta("verification_records_format")
                .unwrap(),
        }
        let view = store.tx_begin_read().unwrap();
        assert!(VerificationReferenceReader::new(&store, &view).is_err());
    }
    store
        .meta_store()
        .put_meta("verification_records_format", &[2])
        .unwrap();
    store
        .meta_store()
        .put_meta("verification_orchestrator", b"mixed")
        .unwrap();
    let view = store.tx_begin_read().unwrap();
    assert!(VerificationReferenceReader::new(&store, &view).is_err());
    drop(view);
    store
        .meta_store()
        .delete_meta("verification_orchestrator")
        .unwrap();
    for key in [
        "verification_state/v1/1/legacy",
        "verification_state/v3/unknown",
    ] {
        store.meta_store().put_meta(key, b"invalid").unwrap();
        let view = store.tx_begin_read().unwrap();
        let mut reader = VerificationReferenceReader::new(&store, &view).unwrap();
        assert!(reader.visit_next_page(100, 4096, |_| Ok(())).is_err());
        assert!(reader.visit_next_page(1, 4096, |_| Ok(())).is_err());
        drop(reader);
        drop(view);
        store.meta_store().delete_meta(key).unwrap();
    }
    let target = rows
        .iter()
        .find(|(key, _)| key.starts_with(normalized::PREFIX))
        .unwrap();
    let mut trailing = target.1.clone();
    trailing.push(0);
    store.meta_store().put_meta(&target.0, &trailing).unwrap();
    let view = store.tx_begin_read().unwrap();
    let mut reader = VerificationReferenceReader::new(&store, &view).unwrap();
    assert!(reader
        .visit_next_page(100, 4096, |_| panic!("corrupt header"))
        .is_err());
    drop(reader);
    drop(view);
    store
        .meta_store()
        .delete_meta("verification_state/v1/0")
        .unwrap();
    let view = store.tx_begin_read().unwrap();
    assert!(VerificationReferenceReader::new(&store, &view).is_err());
}
