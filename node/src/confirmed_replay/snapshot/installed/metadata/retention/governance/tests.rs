use super::*;
use burst_store::{meta::MetaStore, brn::BrnStore};
use burst_types::{BlockHash, TxHash, WalletAddress};
use burst_transactions::governance::GovernanceVote;

fn fixture() -> (tempfile::TempDir, LmdbStore, Vec<(String, Vec<u8>)>) {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    // These raw rows test reference extraction, not recovery authentication.
    let records = [
        GovernanceRecord::PendingEffects { changes: vec![], amendments: vec![], evictions: vec![] },
        GovernanceRecord::Vote { proposal: TxHash::new([1;32]), promotion: false,
            voter: WalletAddress::new("brst_voter"), vote: GovernanceVote::Yea },
        GovernanceRecord::KeyBallot { proposal: TxHash::new([1;32]), owner: WalletAddress::new("brst_owner"),
            grant: BlockHash::new([2;32]), vote: GovernanceVote::Nay },
    ];
    let rows: Vec<_> = records.iter().map(|r| r.storage_entry().unwrap()).collect();
    store.meta_store().put_meta("governance_records_format", &[1]).unwrap();
    for (key, bytes) in &rows { store.meta_store().put_meta(key, bytes).unwrap(); }
    (dir, store, rows)
}

#[test]
fn governance_pages_pin_ballot_references_and_accept_absent_engine() {
    let (_dir, store, rows) = fixture();
    let view = store.tx_begin_read().unwrap();
    let mut reader = GovernanceReferenceReader::new(&store, &view).unwrap();
    for (key, _) in &rows { store.meta_store().delete_meta(key).unwrap(); }
    let mut roots = Vec::new();
    let mut count = 0;
    let mut done = false;
    for _ in 0..rows.len()+2 {
        let page = reader.visit_next_page(1, 4096, |r| { roots.push(format!("{r:?}")); Ok(()) }).unwrap();
        assert!(page.rows <= 1 && page.bytes <= 4096);
        count += page.rows;
        if page.done { done = true; break; }
    }
    assert!(done); assert_eq!(count, 3);
    let proposal = format!("{:?}", Reference::Proposal(TxHash::new([1;32])));
    assert_eq!(roots, [proposal.clone(), format!("{:?}", Reference::Account(&WalletAddress::new("brst_voter"))),
        proposal, format!("{:?}", Reference::Account(&WalletAddress::new("brst_owner"))),
        format!("{:?}", Reference::Grant(BlockHash::new([2;32])))]);
    let page = reader.visit_next_page(1, 4096, |_| panic!("completed")).unwrap();
    assert!(page.done && page.rows == 0);
    drop(reader); drop(view);
    let view = store.tx_begin_read().unwrap();
    let mut reader = GovernanceReferenceReader::new(&store, &view).unwrap();
    let page = reader.visit_next_page(1, 4096, |_| panic!("absent engine")).unwrap();
    assert!(!page.done && page.rows == 0);
    assert!(reader.visit_next_page(1, 4096, |_| panic!("absent bindings")).unwrap().done);
}

#[test]
fn governance_pages_reject_mixed_layout_corruption_and_poisoned_scans() {
    let (_dir, store, rows) = fixture();
    let view = store.tx_begin_read().unwrap();
    for (count, bytes) in [(0,4096), (101,4096), (1,0), (1,1), (1,(64 << 20)+1)] {
        let mut reader = GovernanceReferenceReader::new(&store, &view).unwrap();
        assert!(reader.visit_next_page(count, bytes, |_| panic!("bad budget")).is_err());
        assert!(reader.visit_next_page(1,4096, |_| Ok(())).is_err());
    }
    let mut reader = GovernanceReferenceReader::new(&store, &view).unwrap();
    let mut calls = 0;
    assert!(matches!(reader.visit_next_page(100,4096, |_| {
        calls += 1; Err(StoreError::Backend("stop".into()))
    }), Err(StoreError::Backend(reason)) if reason == "stop"));
    assert_eq!(calls,1);
    assert!(reader.visit_next_page(1,4096, |_| Ok(())).is_err());
    drop(reader); drop(view);
    for marker in [None, Some(vec![2])] {
        match marker { Some(bytes) => store.meta_store().put_meta("governance_records_format", &bytes).unwrap(),
            None => store.meta_store().delete_meta("governance_records_format").unwrap() }
        let view = store.tx_begin_read().unwrap();
        assert!(GovernanceReferenceReader::new(&store, &view).is_err());
    }
    store.meta_store().put_meta("governance_records_format", &[1]).unwrap();
    for (key, bytes) in [("governance_state/v2/unknown".to_string(), vec![0]),
        (rows[1].0.clone(), { let mut b=rows[1].1.clone(); b.push(0); b })] {
        store.meta_store().put_meta(&key, &bytes).unwrap();
        let view = store.tx_begin_read().unwrap();
        let mut reader = GovernanceReferenceReader::new(&store, &view).unwrap();
        assert!(reader.visit_next_page(100,4096, |_| Ok(())).is_err());
        assert!(reader.visit_next_page(1,4096, |_| Ok(())).is_err());
        drop(reader); drop(view);
        store.meta_store().delete_meta(&key).unwrap();
    }
    // Bypass the production virtual aggregate adapter to create corrupt storage.
    store.brn_store().put_meta(b"governance_engine", b"mixed").unwrap();
    let view = store.tx_begin_read().unwrap();
    assert!(GovernanceReferenceReader::new(&store, &view).is_err());

}

#[test]
fn endorsement_bindings_retain_typed_roots_and_pin_the_original_carrier() {
    let (_dir, store, _) = fixture();
    let proposal = BlockHash::new([0xab;32]);
    let account = WalletAddress::new("brst_supporter");
    let key = format!("governance_endorsement/{proposal}/{account}");
    let carrier = BlockHash::new([3;32]);
    store.meta_store().put_meta(&key, carrier.as_bytes()).unwrap();
    let view = store.tx_begin_read().unwrap();
    let mut reader = GovernanceReferenceReader::new(&store, &view).unwrap();
    store.meta_store().put_meta(&key, BlockHash::new([4;32]).as_bytes()).unwrap();
    // The first phase does not complete the scan, even when it consumes all rows.
    assert!(!reader.visit_next_page(100,4096, |_| Ok(())).unwrap().done);
    let mut roots = Vec::new();
    let page = reader.visit_next_page(100,4096, |r| { roots.push(format!("{r:?}")); Ok(()) }).unwrap();
    assert!(page.done); assert_eq!(page.rows,1);
    assert_eq!(roots, [format!("{:?}", Reference::Proposal(proposal.into_tx_hash())),
        format!("{:?}", Reference::Account(&account)), format!("{:?}", Reference::Block(carrier))]);
    for stop in 1..=3 {
        let mut calls = 0;
        assert!(matches!(visit_endorsement(&key, carrier.as_bytes(), |_| {
            calls += 1; if calls == stop { Err(StoreError::Backend("stop".into())) } else { Ok(()) }
        }), Err(StoreError::Backend(reason)) if reason == "stop"));
        assert_eq!(calls,stop);
    }
    for bad in ["governance_endorsement/stale".to_string(),
        format!("governance_endorsement/{}/{account}", proposal.to_string().to_uppercase()),
        format!("governance_endorsement/{}/{account}", BlockHash::ZERO),
        format!("{key}/extra"), format!("governance_endorsement/{proposal}/brst_"),
        format!("governance_endorsement/v2/{proposal}/{account}")] {
        assert!(visit_endorsement(&bad, carrier.as_bytes(), |_| panic!("malformed key")).is_err());
    }
    for bytes in [vec![], vec![0;32], vec![1;31], vec![1;33]] {
        assert!(visit_endorsement(&key, &bytes, |_| panic!("malformed carrier")).is_err());
    }
    drop(reader); drop(view);
    let view = store.tx_begin_read().unwrap();
    let mut reader = GovernanceReferenceReader::new(&store, &view).unwrap();
    reader.visit_next_page(100,4096, |_| Ok(())).unwrap();
    let mut found = false;
    reader.visit_next_page(100,4096, |r| { found |= r == Reference::Block(BlockHash::new([4;32])); Ok(()) }).unwrap();
    assert!(found);
    drop(reader); drop(view);
    store.meta_store().put_meta(&key, &[0;32]).unwrap();
    let view = store.tx_begin_read().unwrap();
    let mut reader = GovernanceReferenceReader::new(&store, &view).unwrap();
    reader.visit_next_page(100,4096, |_| Ok(())).unwrap();
    assert!(reader.visit_next_page(100,4096, |_| Ok(())).is_err());
    assert!(reader.visit_next_page(100,4096, |_| Ok(())).is_err());
}
