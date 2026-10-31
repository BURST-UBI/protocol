use super::*;
use crate::confirmed_replay::{
    scratch_file::ScratchLimit,
    snapshot::history::{DurableHistoryInputs, DurableHistorySession, HistoryContext},
};
use burst_types::NetworkId;

fn hash(n: u64) -> BlockHash {
    let mut bytes = [0; 32];
    bytes[..8].copy_from_slice(&n.to_be_bytes());
    BlockHash::new(bytes)
}
fn fixture(directory: &std::path::Path, count: u64) -> Arc<LmdbStore> {
    let store = Arc::new(LmdbStore::open(directory, 64, 16 << 20).unwrap());
    let mut writer = store.tx_begin_write().unwrap();
    for n in 1..=count {
        writer
            .put_block_with_account(&hash(n), &[1, 2], &WalletAddress::new("brst_cached"), n)
            .unwrap();
    }
    writer.commit().unwrap();
    store
}
async fn session(directory: &std::path::Path) -> DurableHistorySession {
    let limit = ScratchLimit::acquire_reserved(
        directory,
        8 << 20,
        DurableHistoryInputs::reservation(1 << 20).unwrap(),
    )
    .unwrap();
    let context = HistoryContext {
        network: NetworkId::Dev,
        genesis: hash(8),
        epoch: hash(9),
        state_hash: hash(10),
    };
    DurableHistorySession::open(
        directory.to_path_buf(),
        context,
        1 << 20,
        limit,
        &crate::shutdown::ShutdownController::new(),
    )
    .await
    .unwrap()
    .unwrap()
}
fn attach(session: &DurableHistorySession, capture: &mut InventoryCapture) {
    for _ in 0..32 {
        if capture.prepare_cache(session).unwrap() {
            return;
        }
    }
    panic!("bounded inventory refresh did not finish");
}
fn inventory(captured: CapturedInventory, store: &LmdbStore) -> RetainedInventory {
    let writer = store.tx_begin_write().unwrap();
    RetainedInventory {
        rows: captured.rows,
        limits: captured.limits,
        count: captured.count,
        accounts: AccountInventory::read(writer.account_entries().unwrap()).unwrap(),
    }
}
fn save(session: &DurableHistorySession, header: Header, pages: &[Page], seal: bool) {
    let raw = session.select_inventory().unwrap();
    raw.raw.append(0, &encode(&header).unwrap()).unwrap();
    for (index, page) in pages.iter().enumerate() {
        raw.raw
            .append(index as u64 + 1, &encode(page).unwrap())
            .unwrap();
    }
    if seal {
        raw.raw.seal(pages.len() as u64 + 1).unwrap();
    }
}
fn observation(n: u64) -> Observation {
    Observation {
        hash: hash(n),
        account: WalletAddress::new("brst_cached"),
        height: n,
        length: 2,
        digest: burst_crypto::blake2b_256(&[1, 2]),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn partial_and_complete_inventory_reopen_reuse_raw_pages() {
    let directory = tempfile::tempdir().unwrap();
    let store = fixture(directory.path(), 201);
    let session = session(directory.path()).await;
    let mut capture = InventoryCapture::configured(store.clone()).unwrap();
    attach(&session, &mut capture);
    assert!(!capture.advance(100, MAX_BYTES).unwrap());
    assert_eq!(
        session
            .select_inventory()
            .unwrap()
            .raw
            .progress()
            .unwrap()
            .positions,
        2
    );
    drop(capture);
    drop(session);
    drop(store);
    for complete in [false, true] {
        let store = Arc::new(LmdbStore::open(directory.path(), 64, 16 << 20).unwrap());
        let session = self::session(directory.path()).await;
        let mut capture = InventoryCapture::configured(store.clone()).unwrap();
        attach(&session, &mut capture);
        assert_eq!(capture.cache.as_ref().unwrap().sealed, complete);
        assert_eq!(
            capture
                .cache
                .as_ref()
                .unwrap()
                .page()
                .unwrap()
                .unwrap()
                .rows
                .len(),
            100,
            "reopen discarded saved observations"
        );
        while !capture.advance(100, MAX_BYTES).unwrap() {}
        let captured = capture.finish().unwrap();
        assert_eq!(captured.count, 201);
        let retained = inventory(captured, &store);
        retained
            .verify_unchanged(&store.tx_begin_write().unwrap())
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn stale_inventory_refresh_preserves_other_streams() {
    let directory = tempfile::tempdir().unwrap();
    let store = fixture(directory.path(), 201);
    let session = session(directory.path()).await;
    let raw = session.select_inventory().unwrap();
    let history = raw.raw.select_stream([41; 32]).unwrap();
    history.append(0, b"keep account history").unwrap();
    let mut capture = InventoryCapture::configured(store.clone()).unwrap();
    attach(&session, &mut capture);
    assert!(!capture.advance(100, MAX_BYTES).unwrap());
    drop(capture);
    let mut writer = store.tx_begin_write().unwrap();
    writer
        .put_block_with_account(&hash(1), &[9, 8], &WalletAddress::new("brst_cached"), 1)
        .unwrap();
    writer.commit().unwrap();
    let mut capture = InventoryCapture::configured(store.clone()).unwrap();
    attach(&session, &mut capture);
    assert!(capture.cache.as_ref().unwrap().page().unwrap().is_none());
    assert_eq!(
        history.read_position(0).unwrap().unwrap(),
        b"keep account history"
    );
    while !capture.advance(100, MAX_BYTES).unwrap() {}
    inventory(capture.finish().unwrap(), &store)
        .verify_unchanged(&store.tx_begin_write().unwrap())
        .unwrap();
    assert_eq!(
        history.read_position(0).unwrap().unwrap(),
        b"keep account history"
    );
    assert!(
        raw.raw.progress().is_err(),
        "old inventory owner survived refresh"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn raw_cache_digest_and_seal_cannot_authorize_changed_payload() {
    let directory = tempfile::tempdir().unwrap();
    let store = fixture(directory.path(), 1);
    let session = session(directory.path()).await;
    let mut capture = InventoryCapture::configured(store.clone()).unwrap();
    let mut forged = observation(1);
    forged.digest = [77; 32];
    save(
        &session,
        capture.header,
        &[Page {
            rows: vec![forged.clone()],
            end: true,
        }],
        true,
    );
    attach(&session, &mut capture);
    assert!(capture.advance(100, MAX_BYTES).unwrap());
    // Cache digest/seal certify storage integrity only. Actual bytes and the
    // final writer comparison must still reject a validly encoded forged row.
    assert!(forged
        .read_bytes(&store, &store.tx_begin_read().unwrap())
        .is_err());
    assert!(inventory(capture.finish().unwrap(), &store)
        .verify_unchanged(&store.tx_begin_write().unwrap())
        .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn malformed_cached_pages_poison_capture_without_publication() {
    for case in [
        "rows",
        "order",
        "empty",
        "short",
        "unsealed-end",
        "sealed-incomplete",
        "bytes",
        "version",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let store = fixture(directory.path(), 2);
        let session = session(directory.path()).await;
        let mut capture = InventoryCapture::configured(store).unwrap();
        let mut header = capture.header;
        let mut pages = vec![Page {
            rows: vec![observation(1), observation(2)],
            end: true,
        }];
        match case {
            "rows" => pages[0].rows = (1..=101).map(observation).collect(),
            "order" => pages[0].rows.reverse(),
            "empty" => {
                pages[0].rows.clear();
                pages[0].end = false;
            }
            "short" => {
                pages[0].rows.pop();
            }
            "unsealed-end" => pages.push(Page {
                rows: vec![],
                end: true,
            }),
            "sealed-incomplete" => pages[0].end = false,
            "bytes" => pages[0].rows[0].length = MAX_BYTES,
            "version" => header.version = 2,
            _ => unreachable!(),
        }
        save(&session, header, &pages, case != "unsealed-end");
        if case == "version" {
            assert!(
                InventoryCache::prepare(session.select_inventory().unwrap(), capture.header)
                    .is_err()
            );
            assert!(capture.finish().is_err());
            continue;
        }
        attach(&session, &mut capture);
        assert!(capture.advance(100, MAX_BYTES).is_err(), "accepted {case}");
        assert!(capture.advance(100, MAX_BYTES).is_err());
        assert!(capture.finish().is_err());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn full_history_cache_does_not_block_fresh_inventory_capture() {
    for prepared in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store = fixture(directory.path(), 201);
        let session = session(directory.path()).await;
        let raw = session.select_inventory().unwrap();
        let history = raw.raw.select_stream([44; 32]).unwrap();
        let mut capture = InventoryCapture::configured(store.clone()).unwrap();
        if prepared {
            attach(&session, &mut capture);
        }
        let mut position = 0u64;
        for size in [16 << 10, 8] {
            loop {
                let mut bytes = vec![11; size];
                bytes[..8].copy_from_slice(&position.to_be_bytes());
                match history.append_checked(position, &bytes) {
                    Ok(_) => position += 1,
                    Err(InputWriteError::Capacity) => break,
                    Err(error) => panic!("unexpected history fill error: {error}"),
                }
                assert!(position < 2048);
            }
        }
        assert!(position > 0);
        let first = history.read_position(0).unwrap().unwrap();
        let last = history.read_position(position - 1).unwrap().unwrap();
        if !prepared {
            attach(&session, &mut capture);
        }
        assert!(!capture.advance(100, MAX_BYTES).unwrap());
        assert!(
            capture.cache.is_none(),
            "capacity did not disable optional inventory writes"
        );
        while !capture.advance(100, MAX_BYTES).unwrap() {}
        inventory(capture.finish().unwrap(), &store)
            .verify_unchanged(&store.tx_begin_write().unwrap())
            .unwrap();
        assert_eq!(history.progress().unwrap().positions, position);
        assert_eq!(history.read_position(0).unwrap().unwrap(), first);
        assert_eq!(history.read_position(position - 1).unwrap().unwrap(), last);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn buffered_inventory_tail_reopens_from_durable_prefix_and_seals_complete() {
    let directory = tempfile::tempdir().unwrap();
    let store = fixture(directory.path(), 401);
    let session = session(directory.path()).await;
    let mut capture = InventoryCapture::configured(store.clone()).unwrap();
    attach(&session, &mut capture);
    for _ in 0..3 {
        assert!(!capture.advance(100, MAX_BYTES).unwrap());
    }
    let cache = capture.cache.as_ref().unwrap();
    assert_eq!(cache.pending.len(), 2);
    assert!(cache.pending_bytes <= MAX_PAGE_BYTES as usize);
    assert_eq!(
        session
            .select_inventory()
            .unwrap()
            .raw
            .progress()
            .unwrap()
            .positions,
        2
    );
    drop(capture);
    drop(session);
    drop(store);

    let store = Arc::new(LmdbStore::open(directory.path(), 64, 16 << 20).unwrap());
    let session = self::session(directory.path()).await;
    let mut capture = InventoryCapture::configured(store.clone()).unwrap();
    attach(&session, &mut capture);
    assert!(!capture.advance(100, MAX_BYTES).unwrap());
    assert!(capture.last_page_cached);
    assert!(!capture.advance(100, MAX_BYTES).unwrap());
    assert!(!capture.last_page_cached);
    while !capture.advance(100, MAX_BYTES).unwrap() {}
    let cache = capture.cache.as_ref().unwrap();
    assert!(cache.sealed);
    assert!(cache.pending.is_empty());
    let progress = session.select_inventory().unwrap().raw.progress().unwrap();
    assert!(progress.complete);
    assert_eq!(progress.positions, 6);
    inventory(capture.finish().unwrap(), &store)
        .verify_unchanged(&store.tx_begin_write().unwrap())
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn inventory_cache_batches_flush_at_count_and_byte_limits() {
    for large in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let session = session(directory.path()).await;
        let header = Header {
            version: 1,
            revision: 0,
            rows: 1000,
        };
        let mut cache = InventoryCache::prepare(session.select_inventory().unwrap(), header)
            .unwrap()
            .unwrap();
        cache
            .accept(
                &Page {
                    rows: vec![observation(1)],
                    end: false,
                },
                false,
            )
            .unwrap();
        assert_eq!(cache.available, 2);
        let mut row = observation(1);
        if large {
            row.account = WalletAddress::new(format!("brst_{}", "x".repeat(400_000)));
        }
        let page = Page {
            rows: vec![row],
            end: false,
        };
        let count = if large { 3 } else { 128 };
        for _ in 0..count {
            cache.accept(&page, false).unwrap();
        }
        assert!(cache.pending_bytes <= MAX_PAGE_BYTES as usize);
        assert!(cache.pending.len() < 128);
        assert_eq!(cache.available, if large { 4 } else { 130 });
        assert_eq!(cache.pending.len(), if large { 1 } else { 0 });
        assert_eq!(cache.next, cache.available + cache.pending.len() as u64);
        let raw = session.select_inventory().unwrap();
        assert_eq!(raw.raw.progress().unwrap().positions, cache.available);
        assert!(!raw.raw.progress().unwrap().complete);
    }
}
