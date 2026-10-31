use super::*;
use burst_store::{meta::MetaStore, pending::{PendingInfo, PendingStore}};
use burst_trst::{MergeNode, MergeSource, MergerGraph, TrstEngine};
use burst_types::{Timestamp, TxHash, WalletAddress};
use std::collections::{BTreeSet, HashMap};

fn hash(n: u8) -> TxHash { TxHash::new([n; 32]) }
fn receipt() -> PendingInfo {
    PendingInfo { source: WalletAddress::new("brst_sender"), amount: 1,
        timestamp: Timestamp::new(1), provenance: Vec::new(), return_credit: true }
}
pub(super) fn fixture() -> (tempfile::TempDir, LmdbEnvironment, usize) {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
    let owner = WalletAddress::new("brst_holder");
    let mut engine = TrstEngine::with_expiry(10);
    for n in [1, 2] {
        let token = engine.mint(hash(n), owner.clone(), 1, owner.clone(), Timestamp::new(1)).unwrap();
        engine.track_token(token);
    }
    let mut graph = MergerGraph::new();
    graph.record_merge(MergeNode { merge_tx: hash(3), source_origins: vec![
        MergeSource { origin: hash(1), amount: 1 }, MergeSource { origin: hash(2), amount: 1 }],
        total_amount: 2, revoked_contribs: HashMap::new() });
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta("trst_state_format", &[2]).unwrap();
    batch.put_meta("trst_graph_format", &[1]).unwrap();
    let mut count = 1;
    for (_, row) in holding_rows::delta(&owner, None, Some(&engine.wallets[&owner])).unwrap() {
        let (key, value) = row.unwrap().storage_entry().unwrap();
        batch.put_meta(&key, &value).unwrap();
        count += 1;
    }
    engine.visit_wallet_records(|row| {
        if !matches!(row, WalletRecord::Portfolio { .. } | WalletRecord::Holding { .. }) {
            let (key, value) = row.storage_entry().unwrap();
            batch.put_meta(&key, &value).unwrap();
            count += 1;
        }
        Ok::<_, ()>(())
    }).unwrap();
    for row in [WalletRecord::Invalidation { origin: hash(2), wallet_revoked: true },
        WalletRecord::Cause { origin: hash(2), cause: BlockHash::new([10; 32]) }] {
        let (key, value) = row.storage_entry().unwrap();
        batch.put_meta(&key, &value).unwrap();
        count += 1;
    }
    graph.visit_records(|row| {
        let (key, value) = row.storage_entry().unwrap();
        batch.put_meta(&key, &value).unwrap();
        count += 1;
        Ok::<_, ()>(())
    }).unwrap();
    batch.put_pending(&owner, hash(8).as_bytes(), &bincode::serialize(&receipt()).unwrap()).unwrap();
    batch.commit().unwrap();
    (dir, store, count)
}
fn owned(reference: TrstReference<'_>) -> String { format!("{reference:?}") }
pub(super) fn scan(reader: &mut TrstReferenceReader<'_>, limit: usize) -> (usize, BTreeSet<String>) {
    let mut rows = 0;
    let mut values = BTreeSet::new();
    for _ in 0..100 {
        let page = reader.visit_next_page(limit, MAX_PAGE_BYTES, |r| { values.insert(owned(r)); Ok(()) }).unwrap();
        assert!(page.rows <= limit);
        rows += page.rows;
        if page.done { return (rows, values); }
    }
    panic!("bounded fixture scan did not terminate");
}

#[test]
fn retention_writer_handoff_pins_checked_rows_and_rejects_edits() {
    let (_dir, store, count) = fixture();
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.get_meta("trst_state_format").unwrap(), Some(vec![2]));
    let mut reader = batch.trst_reference_reader().unwrap();
    drop(batch);
    // A later publication cannot alter the view pinned at the checked boundary.
    let owner = WalletAddress::new("brst_holder");
    store.pending_store().delete_pending(&owner, &hash(8)).unwrap();
    store.pending_store().put_pending(&owner, &hash(9), &receipt()).unwrap();
    let (rows, roots) = scan(&mut reader, 1);
    assert_eq!(rows, count);
    assert!(roots.contains(&owned(TrstReference::Block(BlockHash::new([8; 32])))));
    assert!(!roots.contains(&owned(TrstReference::Block(BlockHash::new([9; 32])))));
    drop(reader);

    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta("retention_handoff_test", &[1]).unwrap();
    assert!(batch.trst_reference_reader().is_err());
    batch.delete_meta("retention_handoff_test").unwrap();
    assert!(batch.trst_reference_reader().is_err());
    drop(batch);
    let batch = store.tx_begin_write().unwrap();
    let mut reader = batch.trst_reference_reader().unwrap();
    drop(batch);
    let (_, roots) = scan(&mut reader, 2);
    assert!(roots.contains(&owned(TrstReference::Block(BlockHash::new([9; 32])))));
    assert!(!roots.contains(&owned(TrstReference::Block(BlockHash::new([8; 32])))));
}

#[test]
fn retention_pages_pin_all_tables_and_include_holding_links() {
    let (_dir, store, count) = fixture();
    let mut reader = store.trst_reference_reader().unwrap();
    let first = reader.visit_next_page(1, MAX_PAGE_BYTES, |_| Ok(())).unwrap();
    assert_eq!(first.rows, 1);
    assert!(!first.done);
    let owner = WalletAddress::new("brst_holder");
    store.pending_store().delete_pending(&owner, &hash(8)).unwrap();
    store.pending_store().put_pending(&owner, &hash(9), &receipt()).unwrap();
    let (remaining, old) = scan(&mut reader, 1);
    assert_eq!(remaining + first.rows, count);
    assert!(old.contains(&owned(TrstReference::Block(BlockHash::new([8; 32])))));
    assert!(!old.contains(&owned(TrstReference::Block(BlockHash::new([9; 32])))));
    assert!(old.contains(&owned(TrstReference::Token(hash(1)))));
    assert!(old.contains(&owned(TrstReference::Token(hash(2)))));
    drop(reader);
    let (new_count, new) = scan(&mut store.trst_reference_reader().unwrap(), 2);
    assert_eq!(new_count, count);
    assert!(!new.contains(&owned(TrstReference::Block(BlockHash::new([8; 32])))));
    assert!(new.contains(&owned(TrstReference::Block(BlockHash::new([9; 32])))));
    assert!(new.contains(&owned(TrstReference::Account(&owner))));
    assert!(new.contains(&owned(TrstReference::Block(BlockHash::new([10; 32])))));
}

#[test]
fn retention_budget_callback_and_corruption_errors_cannot_report_completion() {
    let (_dir, store, _) = fixture();
    for (rows, bytes) in [(0, 1), (101, MAX_PAGE_BYTES), (1, 0), (1, MAX_PAGE_BYTES + 1), (1, 1)] {
        let mut reader = store.trst_reference_reader().unwrap();
        let mut calls = 0;
        assert!(reader.visit_next_page(rows, bytes, |_| { calls += 1; Ok(()) }).is_err());
        assert_eq!(calls, 0);
        assert!(reader.visit_next_page(1, MAX_PAGE_BYTES, |_| Ok(())).is_err());
    }
    let mut reader = store.trst_reference_reader().unwrap();
    let mut calls = 0;
    assert!(reader.visit_next_page(2, MAX_PAGE_BYTES, |_| { calls += 1; Err(invalid("sink failed")) }).is_err());
    assert_eq!(calls, 1);
    assert!(reader.visit_next_page(1, MAX_PAGE_BYTES, |_| Ok(())).is_err());
    drop(reader);
    store.meta_store().put_meta(&format!("{}0/bad", holding_rows::PREFIX), b"bad").unwrap();
    let mut reader = store.trst_reference_reader().unwrap();
    assert!(reader.visit_next_page(100, MAX_PAGE_BYTES, |_| Ok(())).is_err());
    assert!(reader.visit_next_page(100, MAX_PAGE_BYTES, |_| Ok(())).is_err());
}

#[test]
fn retention_rejects_legacy_mixed_layouts_and_pending_trailing_bytes() {
    let (_dir, store, _) = fixture();
    for (key, value) in [("trst_state_format", vec![1]), ("trst_graph_format", vec![9]),
        (TrstEngine::meta_key(), vec![0]), ("merger_graph", vec![0])] {
        let old = store.meta_store().get_meta(key).ok();
        store.meta_store().put_meta(key, &value).unwrap();
        assert!(store.trst_reference_reader().is_err());
        match old { Some(old) => store.meta_store().put_meta(key, &old).unwrap(), None => store.meta_store().delete_meta(key).unwrap() }
    }
    let owner = WalletAddress::new("brst_holder");
    let mut value = bincode::serialize(&receipt()).unwrap();
    value.push(0);
    let mut raw = store.env().write_txn().unwrap();
    let mut key = owner.as_str().as_bytes().to_vec();
    key.extend_from_slice(hash(8).as_bytes());
    store.pending_db.put(&mut raw, &key, &value).unwrap();
    raw.commit().unwrap();
    let mut reader = store.trst_reference_reader().unwrap();
    assert!(reader.visit_next_page(100, MAX_PAGE_BYTES, |_| Ok(())).is_err());
    assert!(reader.visit_next_page(1, MAX_PAGE_BYTES, |_| Ok(())).is_err());
}

#[test]
fn retention_byte_limited_pages_advance_without_materializing_the_inventory() {
    let (_dir, store, count) = fixture();
    let view = store.tx_begin_read().unwrap();
    let mut largest = 0;
    for (db, prefix) in [(store.meta_db, holding_rows::PREFIX.as_bytes()),
        (store.meta_db, WalletRecord::STORAGE_PREFIX.as_bytes()),
        (store.meta_db, GraphRecord::STORAGE_PREFIX.as_bytes()), (store.pending_db, &b""[..])] {
        for row in db.iter(&view.txn).unwrap() {
            let (key, bytes) = row.unwrap();
            if !key.starts_with(prefix) { continue; }
            largest = largest.max(key.len() + bytes.len());
        }
    }
    drop(view);
    let mut reader = store.trst_reference_reader().unwrap();
    let mut total = 0;
    let mut pages = 0;
    loop {
        let page = reader.visit_next_page(100, largest, |_| Ok(())).unwrap();
        assert!(page.bytes <= largest);
        assert!(page.rows > 0 || page.done);
        total += page.rows;
        pages += 1;
        if page.done { break; }
        assert!(pages <= count + 1);
    }
    assert_eq!(total, count);
    assert!(pages > 1);
}

#[test]
fn retention_cut_recheck_rejects_commits_edits_foreign_unknown_and_failed_views() {
    let (_dir, store, _) = fixture();
    let batch = store.tx_begin_write().unwrap();
    let reader = batch.trst_reference_reader().unwrap();
    batch.check_retention_cut(&reader).unwrap();
    drop(batch);
    {
        let mut batch = store.tx_begin_write().unwrap();
        batch.check_retention_cut(&reader).unwrap();
        batch.put_meta("cut_test", &[1]).unwrap();
        assert!(batch.check_retention_cut(&reader).is_err());
        batch.delete_meta("cut_test").unwrap();
        assert!(batch.check_retention_cut(&reader).is_err());
        // Abort: the committed generation and retained view remain unchanged.
    }
    store.tx_begin_write().unwrap().check_retention_cut(&reader).unwrap();
    let (_other_dir, other, _) = fixture();
    let other_batch = other.tx_begin_write().unwrap();
    let other_reader = other_batch.trst_reference_reader().unwrap();
    assert_eq!(reader.view.generation(), other_reader.view.generation());
    assert!(other_batch.check_retention_cut(&reader).is_err());
    assert!(other_batch.check_body_retention_cut(&reader).is_err());
    drop(other_batch);
    store.meta_store().put_meta("cut_test", &[1]).unwrap();
    store.meta_store().delete_meta("cut_test").unwrap();
    // Equal contents do not erase two intervening publications (ABA).
    assert!(store.tx_begin_write().unwrap().check_retention_cut(&reader).is_err());
    drop(reader);
    let mut unknown = store.trst_reference_reader().unwrap();
    let RetentionView::Owned(view) = &mut unknown.view else { panic!("fixture owns its reader"); };
    view.generation = None;
    assert!(store.tx_begin_write().unwrap().check_retention_cut(&unknown).is_err());
    assert!(store.tx_begin_write().unwrap().check_body_retention_cut(&unknown).is_err());
    drop(unknown);
    let mut failed = store.trst_reference_reader().unwrap();
    assert!(failed.visit_next_page(0, 1, |_| Ok(())).is_err());
    assert!(store.tx_begin_write().unwrap().check_retention_cut(&failed).is_err());
    assert!(store.tx_begin_write().unwrap().check_body_retention_cut(&failed).is_err());
}

#[test]
fn retention_cut_recheck_detects_new_confirmation_work_and_preserves_abort() {
    let (_dir, store, _) = fixture();
    let reader = store.tx_begin_write().unwrap().trst_reference_reader().unwrap();
    let queued = BlockHash::new([77;32]);
    store.meta_store().enqueue_confirmation(&queued).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch.check_retention_cut(&reader).is_err());
    drop(reader);
    let current = batch.trst_reference_reader().unwrap();
    batch.check_retention_cut(&current).unwrap();
    batch.acknowledge_confirmation(&queued).unwrap();
    drop(batch);
    let batch = store.tx_begin_write().unwrap();
    batch.check_retention_cut(&current).unwrap();
    assert!(batch.confirmation_pending(&queued).unwrap());
}

#[test]
fn clean_storage_cut_rejects_edits_commits_and_foreign_environments() {
    let (_dir, store, _) = fixture();
    let cut = store.tx_begin_write().unwrap().clean_storage_cut().unwrap();
    {
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch.matches_clean_storage_cut(&cut));
        batch.put_meta("cut_test", &[1]).unwrap();
        assert!(batch.clean_storage_cut().is_none());
        assert!(!batch.matches_clean_storage_cut(&cut));
        batch.delete_meta("cut_test").unwrap();
        assert!(!batch.matches_clean_storage_cut(&cut));
    }
    assert!(store.tx_begin_write().unwrap().matches_clean_storage_cut(&cut));
    let (_other_dir, other, _) = fixture();
    assert!(!other.tx_begin_write().unwrap().matches_clean_storage_cut(&cut));
    store.meta_store().put_meta("cut_test", &[1]).unwrap();
    store.meta_store().delete_meta("cut_test").unwrap();
    assert!(!store.tx_begin_write().unwrap().matches_clean_storage_cut(&cut));
    let next = store.tx_begin_write().unwrap().clean_storage_cut().unwrap();
    assert!(store.tx_begin_write().unwrap().matches_clean_storage_cut(&next));
}

#[test]
fn body_retention_covers_only_validated_soft_sequence_commits() {
    use crate::meta::ConsensusVoteReservation;
    let (_dir, store, _) = fixture();
    let epoch = BlockHash::new([91;32]);
    let voter = WalletAddress::new("brst_sequence_voter");
    let account = WalletAddress::new("brst_sequence_account");
    let previous = BlockHash::ZERO;
    let hash = BlockHash::new([92;32]);
    let meta = store.meta_store();
    meta.put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, epoch.as_bytes()).unwrap();
    let reader = store.tx_begin_write().unwrap().trst_reference_reader().unwrap();
    for expected in 1..=16 {
        assert_eq!(meta.reserve_consensus_vote(&epoch,&voter,&account,&previous,&hash,false,0).unwrap(),expected);
        let batch = store.tx_begin_write().unwrap();
        assert!(batch.check_retention_cut(&reader).is_err(), "exact cut must remain exact");
        batch.check_body_retention_cut(&reader).unwrap();
    }
    let votes: Vec<_> = (0..8).map(|_| ConsensusVoteReservation {
        payment_epoch: &epoch, representative: &voter, account: &account, previous: &previous,
        hash: &hash, is_final: false, sequence_floor: 0, timestamp: None, governance_limit: None,
    }).collect();
    assert_eq!(meta.reserve_consensus_votes(&votes).unwrap(), (17..=24).collect::<Vec<_>>());
    let mut batch = store.tx_begin_write().unwrap();
    batch.check_body_retention_cut(&reader).unwrap();
    batch.put_meta("dirty_retirement_writer", &[1]).unwrap();
    assert!(batch.check_body_retention_cut(&reader).is_err());
    drop(batch);
    store.tx_begin_write().unwrap().check_body_retention_cut(&reader).unwrap();
    assert_eq!(store.tx_begin_write().unwrap().get_meta(&format!("consensus_sequence/v1/{voter}")).unwrap(), Some(24u64.to_be_bytes().to_vec()));
    // A mixed reservation batch is not sequence-only, even when its first item is.
    let mut mixed = votes;
    mixed[7].is_final = true;
    meta.reserve_consensus_votes(&mixed).unwrap();
    assert!(store.tx_begin_write().unwrap().check_body_retention_cut(&reader).is_err());
    meta.reserve_consensus_vote(&epoch,&voter,&account,&previous,&hash,false,0).unwrap();
    assert!(store.tx_begin_write().unwrap().check_body_retention_cut(&reader).is_err());
    drop(reader);
    for kind in 0..3 {
        let reader = store.tx_begin_write().unwrap().trst_reference_reader().unwrap();
        match kind {
            0 => { meta.put_meta("untracked_retirement_change", &[1]).unwrap();
                   meta.delete_meta("untracked_retirement_change").unwrap(); }
            1 => { meta.enqueue_confirmation(&hash).unwrap(); }
            _ => { meta.put_meta(&format!("consensus_sequence/v1/{voter}"),&100u64.to_be_bytes()).unwrap(); }
        }
        meta.reserve_consensus_vote(&epoch,&voter,&account,&previous,&hash,false,0).unwrap();
        assert!(store.tx_begin_write().unwrap().check_body_retention_cut(&reader).is_err(),"untracked gap {kind}");
    }
}

#[test]
fn detached_body_cuts_release_readers_and_reject_gaps_foreign_views_and_reopen() {
    let (directory, store, _) = fixture();
    let epoch = BlockHash::new([91; 32]);
    let voter = WalletAddress::new("brst_detached_voter");
    let account = WalletAddress::new("brst_detached_account");
    let previous = BlockHash::ZERO;
    let hash = BlockHash::new([92; 32]);
    let meta = store.meta_store();
    meta.put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, epoch.as_bytes()).unwrap();
    let capture = || store.tx_begin_read().unwrap().clean_storage_cut().unwrap();
    let first = capture();
    store.tx_begin_write().unwrap().check_detached_body_retention_cut(&first).unwrap();
    assert!(first.body_dependencies_unchanged(&capture(), &store).unwrap());
    for _ in 0..16 {
        meta.reserve_consensus_vote(&epoch, &voter, &account, &previous, &hash, false, 0).unwrap();
        assert!(first.body_dependencies_unchanged(&capture(), &store).unwrap());
        store.tx_begin_write().unwrap().check_detached_body_retention_cut(&first).unwrap();
    }
    {
        let mut writer = store.tx_begin_write().unwrap();
        writer.check_detached_body_retention_cut(&first).unwrap();
        writer.put_meta("detached_dirty_writer", &[1]).unwrap();
        assert!(writer.check_detached_body_retention_cut(&first).is_err());
        // Aborting leaves the checked old cut reusable; nothing was published.
    }
    store.tx_begin_write().unwrap().check_detached_body_retention_cut(&first).unwrap();
    let later = capture();
    assert!(!later.body_dependencies_unchanged(&first, &store).unwrap());
    let (_other_directory, other, _) = fixture();
    let foreign = other.tx_begin_read().unwrap().clean_storage_cut().unwrap();
    assert!(other.tx_begin_write().unwrap().check_detached_body_retention_cut(&first).is_err());
    assert!(store.tx_begin_write().unwrap().check_detached_body_retention_cut(&foreign).is_err());
    assert!(!first.body_dependencies_unchanged(&foreign, &store).unwrap());
    assert!(!first.body_dependencies_unchanged(&later, &other).unwrap());
    // Final promises may introduce body roots, unlike soft sequence reservation.
    meta.reserve_consensus_vote(&epoch, &voter, &account, &previous, &hash, true, 0).unwrap();
    assert!(!first.body_dependencies_unchanged(&capture(), &store).unwrap());
    assert!(store.tx_begin_write().unwrap().check_detached_body_retention_cut(&first).is_err());
    for kind in 0..3 {
        let before = capture();
        match kind {
            0 => { meta.put_meta("detached_aba", &[1]).unwrap(); meta.delete_meta("detached_aba").unwrap(); }
            1 => { meta.enqueue_confirmation(&hash).unwrap(); }
            _ => { meta.put_meta(&format!("consensus_sequence/v1/{voter}"), &100u64.to_be_bytes()).unwrap(); }
        }
        meta.reserve_consensus_vote(&epoch, &voter, &account, &previous, &hash, false, 0).unwrap();
        assert!(!before.body_dependencies_unchanged(&capture(), &store).unwrap());
        assert!(store.tx_begin_write().unwrap().check_detached_body_retention_cut(&before).is_err());
    }
    let retained = capture();
    // An unassociated read view cannot produce an identity token.
    let raw = store.env().read_txn().unwrap();
    let unknown = crate::LmdbReadTransaction::from_ro(raw);
    assert!(unknown.clean_storage_cut().is_none());
    drop(unknown);
    drop(meta);
    drop(store);
    // Keeping only a cut cannot keep this environment open or authorize a newly
    // opened wrapper, even when the persisted native transaction ID is identical.
    let reopened = LmdbEnvironment::open(directory.path(), 32, 16 << 20).unwrap();
    let current = reopened.tx_begin_read().unwrap().clean_storage_cut().unwrap();
    assert!(!retained.body_dependencies_unchanged(&current, &reopened).unwrap());
    assert!(reopened.tx_begin_write().unwrap().check_detached_body_retention_cut(&retained).is_err());
}

#[test]
fn borrowed_retention_reader_preserves_snapshot_and_rejects_foreign_views() {
    let (_dir, store, count) = fixture();
    let view = store.tx_begin_read().unwrap();
    let cut = view.clean_storage_cut().unwrap();
    let owner = WalletAddress::new("brst_holder");
    let (_foreign_dir, foreign, _) = fixture();
    assert!(foreign.trst_reference_reader_view(&view).is_err());
    let mut first = store.trst_reference_reader_view(&view).unwrap();
    assert_eq!(first.visit_next_page(1, MAX_PAGE_BYTES, |_| Ok(())).unwrap().rows, 1);
    store.pending_store().delete_pending(&owner, &hash(8)).unwrap();
    store.pending_store().put_pending(&owner, &hash(9), &receipt()).unwrap();
    let mut second = store.trst_reference_reader_view(&view).unwrap();
    let (rows, roots) = scan(&mut second, 1);
    assert_eq!(rows, count);
    assert!(roots.contains(&owned(TrstReference::Block(BlockHash::new([8; 32])))));
    assert!(!roots.contains(&owned(TrstReference::Block(BlockHash::new([9; 32])))));
    assert_eq!(scan(&mut first, 1).0, count - 1);
    assert!(!store.tx_begin_write().unwrap().matches_clean_storage_cut(&cut));
    drop(first);
    drop(second);
    assert_eq!(scan(&mut store.trst_reference_reader_view(&view).unwrap(), 2).0, count);
    drop(view);
    let mut current = store.trst_reference_reader().unwrap();
    let (_, roots) = scan(&mut current, 1);
    assert!(!roots.contains(&owned(TrstReference::Block(BlockHash::new([8; 32])))));
    assert!(roots.contains(&owned(TrstReference::Block(BlockHash::new([9; 32])))));
}
