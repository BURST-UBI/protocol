use super::*;
use crate::block_undo::{UndoChange, UndoTable};

fn journal() -> BlockUndo {
    BlockUndo {
        block: BlockHash::new([1; 32]),
        account: burst_types::WalletAddress::new("brst_local"),
        height: 1,
        changes: vec![UndoChange {
            table: UndoTable::Meta,
            key: format!("governance_revision/v1/{}", TxHash::new([0xab; 32])).into_bytes(),
            before: None,
            after: Some(7u128.to_be_bytes().to_vec()),
        }],
    }
}

#[test]
fn rollback_pages_preserve_complete_images_counters_and_pinned_cut() {
    let (_dir, store, _) = super::super::tests::fixture();
    let record = journal();
    let key = crate::block_undo::key(&record.block);
    let revision = String::from_utf8(record.changes[0].key.clone()).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .put_meta(
            std::str::from_utf8(&key).unwrap(),
            &bincode::serialize(&record).unwrap(),
        )
        .unwrap();
    batch.put_meta(&revision, &7u128.to_be_bytes()).unwrap();
    batch.commit().unwrap();
    let parent = store.trst_reference_reader().unwrap();
    let mut reader = parent.rollback_records().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .delete_meta(std::str::from_utf8(&key).unwrap())
        .unwrap();
    batch.put_meta(&revision, &0u128.to_be_bytes()).unwrap();
    batch
        .put_meta("governance_revision-other", b"unrelated")
        .unwrap();
    batch.commit().unwrap();
    let mut journals = Vec::new();
    let mut revisions = Vec::new();
    let mut count = 0;
    for _ in 0..4 {
        let page = reader
            .visit_next_page(1, 4096, |row| {
                match row {
                    RollbackRecord::Journal(row) => journals.push(row.clone()),
                    RollbackRecord::GovernanceRevision { proposal, value } => {
                        revisions.push((proposal, value))
                    }
                }
                Ok(())
            })
            .unwrap();
        assert!(page.rows <= 1 && page.bytes <= 4096);
        count += page.rows;
        if page.done {
            break;
        }
    }
    assert_eq!(count, 2);
    assert_eq!(journals, [record]);
    assert_eq!(revisions, [(TxHash::new([0xab; 32]), 7)]);
    assert!(
        reader
            .visit_next_page(1, 4096, |_| panic!("complete"))
            .unwrap()
            .done
    );
    drop(reader);
    drop(parent);
    let parent = store.trst_reference_reader().unwrap();
    let mut reader = parent.rollback_records().unwrap();
    let page = reader
        .visit_next_page(100, 4096, |row| {
            assert!(matches!(
                row,
                RollbackRecord::GovernanceRevision { value: 0, .. }
            ));
            Ok(())
        })
        .unwrap();
    assert!(page.done);
    assert_eq!(page.rows, 1);
}

#[test]
fn rollback_pages_reject_unknown_versions_bad_images_and_poison_after_errors() {
    let (_dir, store, _) = super::super::tests::fixture();
    let record = journal();
    let key = crate::block_undo::key(&record.block);
    let bytes = bincode::serialize(&record).unwrap();
    let mut bad_hash = key.clone();
    *bad_hash.last_mut().unwrap() = 2;
    for (key, bytes) in [
        (b"block_undo/v2/unknown".to_vec(), bytes.clone()),
        (bad_hash, bytes.clone()),
        (key.clone(), {
            let mut b = bytes.clone();
            b.push(0);
            b
        }),
        (b"governance_revision/v2/unknown".to_vec(), vec![0; 16]),
        (record.changes[0].key.clone(), vec![0; 15]),
        (
            String::from_utf8(record.changes[0].key.clone())
                .unwrap()
                .to_uppercase()
                .replace("GOVERNANCE_REVISION/V1/", "governance_revision/v1/")
                .into_bytes(),
            vec![0; 16],
        ),
    ] {
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_meta(std::str::from_utf8(&key).unwrap(), &bytes)
            .unwrap();
        batch.commit().unwrap();
        let parent = store.trst_reference_reader().unwrap();
        let mut reader = parent.rollback_records().unwrap();
        assert!(reader
            .visit_next_page(100, 4096, |_| panic!("invalid row"))
            .is_err());
        assert!(reader.visit_next_page(100, 4096, |_| Ok(())).is_err());
        drop(reader);
        drop(parent);
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(
            batch.undo_value(UndoTable::Meta, &key).unwrap(),
            Some(bytes)
        );
        batch
            .delete_meta(std::str::from_utf8(&key).unwrap())
            .unwrap();
        batch.commit().unwrap();
    }
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .put_meta(std::str::from_utf8(&key).unwrap(), &bytes)
        .unwrap();
    batch.commit().unwrap();
    let parent = store.trst_reference_reader().unwrap();
    for (rows, bytes) in [
        (0, 4096),
        (101, 4096),
        (1, 0),
        (1, 1),
        (1, MAX_PAGE_BYTES + 1),
    ] {
        let mut reader = parent.rollback_records().unwrap();
        assert!(reader
            .visit_next_page(rows, bytes, |_| panic!("budget"))
            .is_err());
        assert!(reader.visit_next_page(1, 4096, |_| Ok(())).is_err());
    }
    let mut reader = parent.rollback_records().unwrap();
    assert!(
        matches!(reader.visit_next_page(100,4096, |_| Err(StoreError::Backend("stop".into()))),Err(StoreError::Backend(reason)) if reason=="stop")
    );
    assert!(reader.visit_next_page(1, 4096, |_| Ok(())).is_err());
}
