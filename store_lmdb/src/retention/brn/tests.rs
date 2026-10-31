use super::*;
use burst_brn::{BrnWalletState, retention::Reference};
use burst_types::{Timestamp, WalletAddress, TxHash};
use burst_store::pending::PendingStore;
use std::collections::BTreeSet;

fn hash(n: u8) -> BlockHash { BlockHash::new([n; 32]) }
fn fixture() -> (tempfile::TempDir, LmdbEnvironment, BrnWalletState) {
    let (dir, store, _) = super::super::tests::fixture();
    let mut state = BrnWalletState::new(Timestamp::new(1));
    state.paused_intervals.push((Timestamp::new(2), Timestamp::new(3)));
    state.record_release(hash(4), 5, &[hash(4), hash(5)], Timestamp::new(6)).unwrap();
    state.record_unproven_release(hash(7), 8, Timestamp::new(9)).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch.initialize_brn_wallet_records_if_empty().unwrap());
    batch.put_brn_wallet_state(&WalletAddress::new("brst_owner"), &state).unwrap();
    batch.commit().unwrap();
    (dir, store, state)
}

fn scan(reader: &mut BrnWalletReferenceReader<'_, '_>, rows: usize, bytes: usize) -> Result<(usize, BTreeSet<String>), StoreError> {
    let mut seen = BTreeSet::new();
    let mut count = 0;
    for _ in 0..100 {
        let page = reader.visit_next_page(rows, bytes, |r| { seen.insert(format!("{r:?}")); Ok(()) })?;
        assert!(page.rows <= rows && page.bytes <= bytes);
        count += page.rows;
        if page.done { return Ok((count, seen)); }
    }
    panic!("fixture scan did not finish");
}

#[test]
fn brn_wallet_pages_share_the_trst_cut_across_later_writes() {
    let (_dir, store, mut state) = fixture();
    let mut source = store.trst_reference_reader().unwrap();
    let mut reader = source.brn_wallet_references().unwrap();
    state.record_unproven_release(hash(10), 11, Timestamp::new(12)).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_brn_wallet_state(&WalletAddress::new("brst_owner"), &state).unwrap();
    batch.commit().unwrap();
    store.pending_store().delete_pending(&WalletAddress::new("brst_holder"), &TxHash::new([8; 32])).unwrap();
    let (count, seen) = scan(&mut reader, 1, 256).unwrap();
    assert_eq!(count, 4);
    for value in [Reference::Block(hash(4)), Reference::Block(hash(5)), Reference::Outcome(hash(7)),
        Reference::Boundary(Timestamp::new(1)), Reference::Boundary(Timestamp::new(2)),
        Reference::Boundary(Timestamp::new(3)), Reference::Boundary(Timestamp::new(9))]
    { assert!(seen.contains(&format!("{value:?}"))); }
    assert!(!seen.contains(&format!("{:?}", Reference::Outcome(hash(10)))));
    drop(reader);
    let (_, trst) = super::super::tests::scan(&mut source, 1);
    assert!(trst.contains(&format!("{:?}", TrstReference::Block(hash(8)))));
    drop(source);
    let source = store.trst_reference_reader().unwrap();
    let (count, seen) = scan(&mut source.brn_wallet_references().unwrap(), 100, 256).unwrap();
    assert_eq!(count, 5);
    assert!(seen.contains(&format!("{:?}", Reference::Outcome(hash(10)))));
}

#[test]
fn brn_wallet_page_errors_invalidate_the_scan_and_preserve_callback_error() {
    let (_dir, store, _) = fixture();
    let source = store.trst_reference_reader().unwrap();
    for (rows, bytes) in [(0, 100), (101, 100), (1, 0), (1, MAX_PAGE_BYTES + 1), (1, 1)] {
        let mut reader = source.brn_wallet_references().unwrap();
        assert!(reader.visit_next_page(rows, bytes, |_| panic!("invalid budget emitted roots")).is_err());
        assert!(reader.visit_next_page(1, 256, |_| Ok(())).is_err());
    }
    let mut reader = source.brn_wallet_references().unwrap();
    let mut calls = 0;
    let result = reader.visit_next_page(100, 1024, |_| {
        calls += 1;
        Err(StoreError::Backend("sink unavailable".into()))
    });
    assert!(matches!(result, Err(StoreError::Backend(reason)) if reason == "sink unavailable"));
    assert_eq!(calls, 1);
    assert!(reader.visit_next_page(100, 1024, |_| Ok(())).is_err());
}

#[test]
fn brn_wallet_pages_reject_orphans_misplaced_headers_and_malformed_rows() {
    for case in 0..5 {
        let (_dir, store, _) = fixture();
        let owner = WalletAddress::new(if case == 0 { "brst_missing" } else { "brst_owner" });
        let mut key = wallet_storage::row_prefix(&owner).unwrap();
        let mut bytes = bincode::serialize(&(Timestamp::new(1), Timestamp::new(2))).unwrap();
        match case {
            0 => { key.push(1); key.extend_from_slice(&0u64.to_be_bytes()); }
            1 => key.extend_from_slice(wallet_storage::HEADER),
            2 => key.push(99),
            3 => { key.push(1); key.extend_from_slice(&0u64.to_be_bytes()); bytes.push(0); }
            _ => { key = wallet_storage::ROW_PREFIX.to_vec(); key.extend_from_slice(&u32::MAX.to_be_bytes()); }
        }
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_brn_meta(&key, &bytes).unwrap();
        batch.commit().unwrap();
        let source = store.trst_reference_reader().unwrap();
        let mut reader = source.brn_wallet_references().unwrap();
        assert!(scan(&mut reader, 1, 1024).is_err(), "case {case}");
        assert!(reader.visit_next_page(1, 1024, |_| Ok(())).is_err());
    }
    let (_dir, store, _) = super::super::tests::fixture();
    let source = store.trst_reference_reader().unwrap();
    assert!(source.brn_wallet_references().is_err(), "legacy layout unsupported");
}
