use super::*;

pub(in crate::confirmed_replay::tests) async fn assert_successor(
    fixture: Fixture,
    directory: tempfile::TempDir,
    store: LmdbStore,
    base_height: usize,
) {
    assert_successor_with_policy(fixture, directory, store, base_height, false).await;
}

pub(in crate::confirmed_replay::tests) async fn assert_successor_after_retirement(
    fixture: Fixture,
    directory: tempfile::TempDir,
    store: LmdbStore,
    base_height: usize,
) {
    assert_successor_with_policy(fixture, directory, store, base_height, true).await;
}

async fn assert_successor_with_policy(
    fixture: Fixture,
    directory: tempfile::TempDir,
    store: LmdbStore,
    base_height: usize,
    retire: bool,
) {
    let events =
        crate::checkpoint_store::chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let last = &events.last().unwrap().certificate().checkpoint;
    let epoch = last.hash();
    let now = last.cutoff + 1;
    let transfers: Vec<_> = events
        .iter()
        .skip(base_height)
        .map(|event| {
            let height = event.certificate().checkpoint.height;
            (
                height,
                CertifiedCheckpoint::export_transfer(
                    &store,
                    NetworkId::Dev,
                    &fixture.params,
                    height,
                )
                .unwrap()
                .unwrap(),
            )
        })
        .collect();
    assert!(!transfers.is_empty());
    let prepared = prepare_lifecycle_archive(fixture, directory, store, Some(base_height));
    let store = LmdbStore::open(&prepared.source_config.data_dir, 32, 32 << 20).unwrap();
    let bytes = export_bytes(&store, &prepared.params, prepared.epoch);
    drop(store);
    let archive_dir = tempfile::tempdir().unwrap();
    let path = archive_dir.path().join("bootstrap.snapshot");
    std::fs::write(&path, &bytes).unwrap();
    let mut config = prepared.target_config.clone();
    config.snapshot_bootstrap = Some(path);
    if retire {
        config.recovery_scratch_bytes = Some(64 << 20);
    }
    let source = Running::start(prepared.source_config.clone(), now).await;
    source.ready(prepared.epoch).await;
    let mut target = Running::start(config.clone(), now).await;
    target.ready(prepared.epoch).await;
    assert_lifecycle(&source.store, &target.store, None);
    let mut scan_cursor = None;
    if retire {
        eprintln!("appeal retirement: base ready; stopping before physical retirement");
        let stopped = target.stop().await;
        let offline = Arc::clone(&stopped.store);
        let processor = stopped.block_processor.clone();
        let counts = stopped.ledger_cache.clone();
        drop(stopped);
        // Model a full-history node at the installed cut, using the independent
        // archival source's primary body/height rows. Economics stay unchanged.
        let rows = source
            .store
            .block_store()
            .indexed_blocks_txn(&source.store.tx_begin_read().unwrap(), 131_072, 64 << 20)
            .unwrap();
        {
            let mut batch = offline.tx_begin_write().unwrap();
            for row in rows {
                let account = batch.get_account(&row.account).unwrap();
                assert!(row.height <= account.confirmation_height);
                batch
                    .put_block_with_account(&row.hash, &row.bytes, &row.account, row.height)
                    .unwrap();
            }
            batch.commit().unwrap();
        }
        normalize_retention_fixture(&offline, &prepared.params);
        let before_count = offline.block_store().block_count().unwrap();
        let before_metadata = offline
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        counts.reset(offline.tx_begin_write().unwrap().ledger_counts().unwrap());
        let stop = Arc::new(crate::shutdown::ShutdownController::new());
        let worker = Arc::new(crate::body_retirement::Worker {
            store: offline.clone(),
            network: NetworkId::Dev,
            params: prepared.params.clone(),
            registry: source.registry.clone(),
            processor,
            counts: counts.clone(),
            shutdown: stop.clone(),
            prepared_probe: None,
        });
        let policy = crate::config::BodyRetirementConfig {
            enabled: true,
            max_scratch_bytes: Some(64 << 20),
            retain_seconds: 0,
            max_examined: 131_072,
            max_bodies: 2,
            max_bytes: 64 << 20,
            ..Default::default()
        };
        if base_height == 3 {
            assert_retirement_fetch_shutdown(&worker, &policy).await;
            let stage = std::time::Instant::now();
            assert_retirement_publication_boundaries(&worker, &policy, before_count).await;
            eprintln!(
                "retirement_stage contention_ms={}",
                stage.elapsed().as_millis()
            );
            let mut scan = policy.clone();
            scan.retain_seconds = u64::MAX;
            scan.max_examined = 1;
            let stage = std::time::Instant::now();
            let page = worker.clone().run_once(scan, None).await.unwrap().unwrap();
            eprintln!(
                "retirement_stage initial_page_ms={}",
                stage.elapsed().as_millis()
            );
            assert_eq!(page.examined, 1);
            assert!(page.hashes.is_empty());
            assert!(!page.complete);
            scan_cursor = Some((page.epoch, page.resume_after.unwrap()));
        }
        let stage = std::time::Instant::now();
        let report = worker
            .clone()
            .run_once(policy.clone(), None)
            .await
            .unwrap()
            .unwrap();
        eprintln!(
            "retirement_stage deletion_ms={}",
            stage.elapsed().as_millis()
        );
        assert_eq!(report.epoch, prepared.epoch);
        let retired = report.hashes;
        assert_eq!(retired.len(), 2, "fixture must retire actual prefix bodies");
        assert_eq!(counts.block_count(), before_count - 2);
        stop.shutdown();
        assert!(worker.run_once(policy, None).await.unwrap().is_none());
        assert_eq!(
            offline.block_store().block_count().unwrap(),
            before_count - 2
        );
        assert_eq!(
            offline
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before_metadata
        );
        offline.force_sync().unwrap();
        drop(offline);
        let stage = std::time::Instant::now();
        target = Running::start(config.clone(), now).await;
        target.ready(prepared.epoch).await;
        eprintln!(
            "retirement_stage pruned_restart_ms={}",
            stage.elapsed().as_millis()
        );
        for hash in retired {
            assert!(!target.store.block_store().exists(&hash).unwrap());
        }
        assert_lifecycle(&source.store, &target.store, None);
        eprintln!("appeal retirement: pruned base recovered; importing signed successors");
    }
    for (height, transfer) in transfers {
        CertifiedCheckpoint::import_transfer(
            &source.store,
            NetworkId::Dev,
            &prepared.params,
            height,
            &transfer,
            &crate::checkpoint_store::historical::HistoricalAdmissions::default(),
        )
        .unwrap();
    }
    let stage = std::time::Instant::now();
    source.ready(epoch).await;
    target.ready(epoch).await;
    eprintln!(
        "retirement_stage successor_ready_ms={}",
        stage.elapsed().as_millis()
    );
    if let Some((previous_epoch, after)) = scan_cursor {
        assert_ne!(previous_epoch, epoch);
        let stage = std::time::Instant::now();
        let stopped = target.stop().await;
        eprintln!(
            "retirement_stage advanced_stop_ms={}",
            stage.elapsed().as_millis()
        );
        let before = stopped.store.block_store().block_count().unwrap();
        let rows = stopped
            .store
            .block_store()
            .indexed_blocks_txn(&stopped.store.tx_begin_read().unwrap(), 131_072, 64 << 20)
            .unwrap();
        let expected = rows.iter().find(|row| row.hash > after).map(|row| row.hash);
        let worker = Arc::new(crate::body_retirement::Worker {
            store: stopped.store.clone(),
            network: NetworkId::Dev,
            params: prepared.params.clone(),
            registry: source.registry.clone(),
            processor: stopped.block_processor.clone(),
            counts: stopped.ledger_cache.clone(),
            shutdown: Arc::new(crate::shutdown::ShutdownController::new()),
            prepared_probe: None,
        });
        let scan = crate::config::BodyRetirementConfig {
            enabled: true,
            max_scratch_bytes: Some(64 << 20),
            retain_seconds: u64::MAX,
            max_examined: 1,
            ..Default::default()
        };
        let stage = std::time::Instant::now();
        let page = worker.run_once(scan, Some(after)).await.unwrap().unwrap();
        eprintln!(
            "retirement_stage advanced_page_ms={}",
            stage.elapsed().as_millis()
        );
        assert_eq!(page.epoch, epoch);
        assert_eq!(page.examined, usize::from(expected.is_some()));
        assert_eq!(page.resume_after, Some(expected.unwrap_or(after)));
        assert!(page.hashes.is_empty());
        assert_eq!(stopped.store.block_store().block_count().unwrap(), before);
        assert_eq!(stopped.ledger_cache.block_count(), before);
        drop(stopped);
        let stage = std::time::Instant::now();
        target = Running::start(config.clone(), now).await;
        target.ready(epoch).await;
        eprintln!(
            "retirement_stage advanced_restart_ms={}",
            stage.elapsed().as_millis()
        );
    }
    assert_lifecycle(&source.store, &target.store, None);
    assert_trst_state(
        &source.store,
        &target.store,
        prepared.params.trst_expiry_secs,
    );
    assert_eq!(
        installed::read_stored_base_bytes(&target.store)
            .unwrap()
            .unwrap(),
        bytes
    );
    assert!(
        crate::checkpoint_store::chain::load(&target.store, NetworkId::Dev, &prepared.params)
            .is_err()
    );
    let first = burn(&target, &prepared.params, epoch, prepared.epoch, now).await;
    target.stop().await;
    let target = Running::start(config, now + 1).await;
    target.ready(epoch).await;
    assert_lifecycle(&source.store, &target.store, Some(first.into_tx_hash()));
    assert!(target.store.block_store().exists(&first).unwrap());
    burn(&target, &prepared.params, epoch, prepared.epoch, now + 1).await;
    if retire && base_height == 3 {
        assert_return_credit_claims(target, &prepared.params, now + 1).await;
    } else {
        target.stop().await;
    }
    source.stop().await;
}

async fn assert_return_credit_claims(target: Running, params: &ProtocolParams, now: u64) {
    let mut claims = Vec::new();
    for seed in 0..=20 {
        let entries = target
            .store
            .pending_store()
            .get_pending_for_account_with_hashes(&address(seed))
            .unwrap();
        let credits: Vec<_> = entries
            .into_iter()
            .filter(|(_, pending)| pending.return_credit)
            .collect();
        if !credits.is_empty() {
            let state = target.client.signing_state(&address(seed)).await.unwrap();
            claims.push((seed, state, credits));
        }
    }
    assert_eq!(
        claims
            .iter()
            .map(|(_, _, credits)| credits.len())
            .sum::<usize>(),
        2
    );
    let stopped = target.stop().await;
    let wallets = {
        let batch = stopped.store.tx_begin_write().unwrap();
        crate::trst_storage::load(&batch, params.trst_expiry_secs)
            .unwrap()
            .save_wallets()
    };
    for (seed, mut state, credits) in claims {
        let key = burst_crypto::keypair_from_seed(&[seed; 32]);
        for (source, pending) in credits {
            assert_eq!(pending.source, address(seed));
            let transaction = burst_transactions::Transaction::Receive(
                build_receive_tx(&address(seed), source, pending.amount, Timestamp::new(now))
                    .unwrap(),
            );
            let block = sign_state_block(
                build_state_block(&state, &transaction, TxHash::ZERO, params.params_hash())
                    .unwrap(),
                &key.private,
            );
            let mut batch = stopped.store.tx_begin_write().unwrap();
            let applied =
                crate::payment_transition::apply(&mut batch, &block, params, block.timestamp)
                    .unwrap();
            assert!(batch.get_pending(&address(seed), &source).is_err());
            assert_eq!(
                applied.trst.save_wallets(),
                wallets,
                "return receipt must not deliver tokens again"
            );
            assert_eq!(batch.get_account(&address(seed)).unwrap().head, block.hash);
            batch.commit().unwrap();
            state.head = block.hash;
            state.block_count += 1;
            state.trst_balance = block.trst_balance;
            state.release_sources.clear();
            // A newly signed successor must fail because the receipt was spent,
            // rather than merely failing a duplicate hash or previous-head check.
            let duplicate = sign_state_block(
                build_state_block(&state, &transaction, TxHash::ZERO, params.params_hash())
                    .unwrap(),
                &key.private,
            );
            assert_ne!(duplicate.hash, block.hash);
            let mut batch = stopped.store.tx_begin_write().unwrap();
            assert!(batch.get_pending(&address(seed), &source).is_err());
            let error = crate::payment_transition::apply(
                &mut batch,
                &duplicate,
                params,
                duplicate.timestamp,
            )
            .err()
            .unwrap();
            assert!(
                matches!(error, burst_store::StoreError::Corruption(ref message)
                if message == &burst_store::StoreError::NotFound(source.to_string()).to_string()),
                "duplicate credit failed for an unrelated reason: {error}"
            );
            drop(batch);
            let batch = stopped.store.tx_begin_write().unwrap();
            assert_eq!(batch.get_account(&address(seed)).unwrap().head, block.hash);
            assert_eq!(
                crate::trst_storage::load(&batch, params.trst_expiry_secs)
                    .unwrap()
                    .save_wallets(),
                wallets
            );
        }
    }
}

async fn assert_retirement_publication_boundaries(
    original: &Arc<crate::body_retirement::Worker>,
    policy: &crate::config::BodyRetirementConfig,
    expected_count: u64,
) {
    let make_worker = || {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let worker = Arc::new(crate::body_retirement::Worker {
            store: original.store.clone(),
            network: original.network,
            params: original.params.clone(),
            registry: original.registry.clone(),
            processor: original.processor.clone(),
            counts: original.counts.clone(),
            shutdown: Arc::new(crate::shutdown::ShutdownController::new()),
            prepared_probe: Some(Arc::new(move |event| {
                if let crate::body_retirement::Probe::Prepared(built) = event {
                    sender.send(built).unwrap();
                }
            })),
        });
        (worker, receiver)
    };
    // Hold the real publication mutex so the write deterministically lands
    // after closure preparation, not merely somewhere during recovery.
    let guard = original.processor.clone().lock_owned().await;
    let (worker, mut prepared) = make_worker();
    let attempt = tokio::spawn(worker.run_once(policy.clone(), None));
    tokio::time::timeout(Duration::from_secs(10), prepared.recv())
        .await
        .unwrap()
        .unwrap();
    original
        .store
        .meta_store()
        .put_meta("retirement_concurrent_write", &[1])
        .unwrap();
    drop(guard);
    let rejected = tokio::time::timeout(Duration::from_secs(10), attempt)
        .await
        .unwrap()
        .unwrap();
    let error = match rejected {
        Err(error) => error,
        Ok(_) => panic!("stale retirement published"),
    };
    assert!(
        matches!(&error, burst_store::StoreError::Corruption(message)
        if message == "body change coverage does not match the current cut"),
        "{error}"
    );
    assert_eq!(
        original.store.block_store().block_count().unwrap(),
        expected_count
    );
    assert_eq!(original.counts.block_count(), expected_count);
    original
        .store
        .meta_store()
        .delete_meta("retirement_concurrent_write")
        .unwrap();

    // Exercise the actual periodic runner. Holding publication across multiple
    // configured intervals must leave exactly one prepared attempt, and shutdown
    // must join it without requiring that mutex to be released.
    let guard = original.processor.clone().lock_owned().await;
    let (worker, mut prepared) = make_worker();
    let mut scheduled = policy.clone();
    scheduled.interval_secs = 1;
    let task = worker.clone().start(scheduled);
    tokio::time::timeout(Duration::from_secs(10), prepared.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(matches!(
        prepared.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    worker.shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        original.store.block_store().block_count().unwrap(),
        expected_count
    );
    assert_eq!(original.counts.block_count(), expected_count);
    drop(guard);

    // Two bounded pages must share one catalogue and its directory-wide lease.
    // No body is age-eligible, so this checks the real periodic cache without
    // changing the later deletion/restart fixture.
    let (worker, mut prepared) = make_worker();
    let mut paged_policy = policy.clone();
    paged_policy.interval_secs = 2;
    paged_policy.retain_seconds = u64::MAX;
    paged_policy.max_examined = 1;
    let guard = original.processor.clone().lock_owned().await;
    let task = worker.clone().start(paged_policy);
    assert!(
        tokio::time::timeout(Duration::from_secs(10), prepared.recv())
            .await
            .unwrap()
            .unwrap()
    );
    assert!(
        crate::confirmed_replay::scratch_file::ScratchLimit::acquire(
            original.store.env().path(),
            policy.max_scratch_bytes.unwrap(),
        )
        .is_err(),
        "prepared catalogue lost its scratch lease"
    );
    drop(guard);
    // The two-second interval separates scans, not pages of this one scan.
    // Keep the existing publication/shutdown checks and require catalogue reuse.
    assert!(
        !tokio::time::timeout(Duration::from_secs(1), prepared.recv())
            .await
            .unwrap()
            .unwrap(),
        "compatible second page rebuilt the catalogue"
    );
    worker.shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    // Recovery caches may outlive this worker. The next worker shares their
    // existing aggregate limit; shutdown must release the catalogue, not disable
    // enforcement for other retained files.
    let map_bytes = ((policy.max_scratch_bytes.unwrap() / 4).min(16 << 20) & !4095) as usize;
    let reserved =
        crate::confirmed_replay::snapshot::history::DurableHistoryInputs::reservation(map_bytes)
            .unwrap();
    let shared = crate::confirmed_replay::scratch_file::ScratchLimit::acquire_shared_reserved(
        original.store.env().path(),
        policy.max_scratch_bytes.unwrap(),
        reserved,
    )
    .expect("retained recovery policy prevented a compatible next worker");
    assert!(
        crate::confirmed_replay::scratch_file::ScratchLimit::acquire_shared_reserved(
            original.store.env().path(),
            policy.max_scratch_bytes.unwrap() + 1,
            reserved,
        )
        .is_err(),
        "retained recovery files allowed their budget to expand"
    );
    drop(shared);
    assert_eq!(
        original.store.block_store().block_count().unwrap(),
        expected_count
    );
    assert_eq!(original.counts.block_count(), expected_count);

    // A tracked change after page preparation defers publication while keeping
    // this catalogue. Net-zero frontier writes preserve the fixture's live state
    // but both images must be reconciled before the next page can be prepared.
    let (worker, mut prepared) = make_worker();
    let mut retry_policy = policy.clone();
    retry_policy.interval_secs = 1;
    retry_policy.retain_seconds = u64::MAX;
    retry_policy.max_examined = 1;
    let guard = original.processor.clone().lock_owned().await;
    let task = worker.clone().start(retry_policy);
    assert!(
        tokio::time::timeout(Duration::from_secs(10), prepared.recv())
            .await
            .unwrap()
            .unwrap()
    );
    {
        use burst_store::frontier::FrontierStore;
        let (account, head) = original
            .store
            .frontier_store()
            .all_frontiers()
            .unwrap()
            .into_iter()
            .next()
            .expect("retirement fixture has no frontier");
        let mut writer = original.store.tx_begin_write().unwrap();
        writer.put_frontier(&account, &BlockHash::ZERO).unwrap();
        writer.put_frontier(&account, &head).unwrap();
        writer.commit().unwrap();
    }
    let deferred_at = tokio::time::Instant::now();
    drop(guard);
    assert!(
        !tokio::time::timeout(Duration::from_secs(10), prepared.recv())
            .await
            .unwrap()
            .unwrap(),
        "covered publication deferral discarded its catalogue"
    );
    assert!(
        deferred_at.elapsed() >= Duration::from_secs(1),
        "deferred attempt bypassed interval backoff"
    );
    worker.shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        original.store.block_store().block_count().unwrap(),
        expected_count
    );
    assert_eq!(original.counts.block_count(), expected_count);

    // Once a complete age-ineligible scan finishes, unchanged ticks must not
    // prepare the same state again. Any committed write invalidates that shortcut.
    let (worker, mut prepared) = make_worker();
    let mut idle_policy = policy.clone();
    idle_policy.interval_secs = 1;
    idle_policy.retain_seconds = u64::MAX;
    let task = worker.clone().start(idle_policy);
    tokio::time::timeout(Duration::from_secs(10), prepared.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(matches!(
        prepared.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    original
        .store
        .meta_store()
        .put_meta("retirement_idle_invalidation", &[1])
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), prepared.recv())
        .await
        .unwrap()
        .unwrap();
    worker.shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        original.store.block_store().block_count().unwrap(),
        expected_count
    );
    assert_eq!(original.counts.block_count(), expected_count);
    original
        .store
        .meta_store()
        .delete_meta("retirement_idle_invalidation")
        .unwrap();
}

async fn assert_retirement_fetch_shutdown(
    original: &Arc<crate::body_retirement::Worker>,
    policy: &crate::config::BodyRetirementConfig,
) {
    let stage = std::time::Instant::now();
    let rows = original
        .store
        .block_store()
        .indexed_blocks_txn(&original.store.tx_begin_read().unwrap(), 131_072, 64 << 20)
        .unwrap();
    let before_count = original.store.block_store().block_count().unwrap();
    let before_metadata = original
        .store
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    // Choose an actual required historical body, never genesis or a frontier.
    // Removing its local copy models sparse historical storage; restore the exact
    // bytes/indexes after cancellation before the deletion scenario continues.
    let candidates: Vec<_> = {
        let batch = original.store.tx_begin_write().unwrap();
        rows.iter()
            .filter(|row| {
                row.height > 1
                    && row.height < batch.get_account(&row.account).unwrap().confirmation_height
            })
            .map(|row| row.hash)
            .collect()
    };
    let checked = installed::recover_metadata_from_peers(
        original.store.clone(),
        NetworkId::Dev,
        original.params.clone(),
        original.registry.clone(),
    )
    .await
    .unwrap()
    .unwrap();
    let missing = {
        let cutoff = checked.recovered().continuation.anchor().cutoff();
        let prepared = checked
            .prepare_token_indexes_at(&original.store, Timestamp::new(cutoff))
            .unwrap();
        let closed = prepared
            .body_requirements(&original.store)
            .unwrap()
            .close(Default::default())
            .unwrap();
        candidates
            .into_iter()
            .find(|hash| closed.has_body(*hash).unwrap())
            .expect("fixture requires historical body")
    };
    let row = rows.iter().find(|row| row.hash == missing).unwrap();
    {
        let mut batch = original.store.tx_begin_write().unwrap();
        batch.delete_block(&missing).unwrap();
        batch.commit().unwrap();
    }
    original.counts.dec_block_count();
    let registry = Arc::new(tokio::sync::RwLock::new(crate::ConnectionRegistry::new()));
    let (sender, mut requests) =
        tokio::sync::mpsc::channel::<crate::checkpoint_sync::witness::Command>(1);
    registry.write().await.set_witness_requests(sender);
    registry.write().await.recovery_probe = Some(|stage| {
        eprintln!("retirement_fetch_shutdown recovery_stage={stage}");
    });
    let stop = Arc::new(crate::shutdown::ShutdownController::new());
    let worker = Arc::new(crate::body_retirement::Worker {
        store: original.store.clone(),
        network: original.network,
        params: original.params.clone(),
        registry,
        processor: original.processor.clone(),
        counts: original.counts.clone(),
        shutdown: stop.clone(),
        prepared_probe: None,
    });
    let attempt = tokio::spawn(worker.run_once(policy.clone(), None));
    let command = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(!command.wanted.is_empty());
    // Keep the response sender alive: only shutdown, not transport disconnection,
    // can release this worker before the ordinary proof timeout.
    stop.shutdown();
    assert!(tokio::time::timeout(Duration::from_secs(5), attempt)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .is_none());
    assert!(command.result.is_closed());
    assert_eq!(
        original.store.block_store().block_count().unwrap(),
        before_count - 1
    );
    assert_eq!(original.counts.block_count(), before_count - 1);
    assert_eq!(
        original
            .store
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before_metadata
    );
    {
        let mut batch = original.store.tx_begin_write().unwrap();
        batch
            .put_block_with_account(&row.hash, &row.bytes, &row.account, row.height)
            .unwrap();
        batch.commit().unwrap();
    }
    original.counts.inc_block_count();
    assert_eq!(
        original.store.block_store().block_count().unwrap(),
        before_count
    );
    eprintln!(
        "retirement_stage fetch_shutdown_ms={}",
        stage.elapsed().as_millis()
    );
}
