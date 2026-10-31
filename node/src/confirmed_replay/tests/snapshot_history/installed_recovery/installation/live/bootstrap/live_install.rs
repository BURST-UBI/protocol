use super::*;

#[cfg(unix)]
mod crash;
mod races;
mod sync_failure;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_snapshot_installation_publishes_engines_and_restarts() {
    install_and_restart(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_snapshot_installation_replaces_existing_base_and_restarts() {
    install_and_restart(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_snapshot_installation_fetches_peer_proofs_and_restarts() {
    install_and_restart(false, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_snapshot_installation_replaces_base_with_peer_proofs() {
    install_and_restart(true, true).await;
}

async fn install_and_restart(replace: bool, peer_proofs: bool) {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let (prepared, _) = prepare_active();
    let store = LmdbStore::open(&prepared.source_config.data_dir, 32, 32 << 20).unwrap();
    let bytes = export_bytes(&store, &prepared.params, prepared.epoch);
    drop(store);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("live.snapshot");
    std::fs::write(&path, &bytes).unwrap();
    let source = Running::start(prepared.source_config.clone(), prepared.now + 1).await;
    source.ready(prepared.epoch).await;
    // Withhold ordinary checkpoint catch-up, not ancestry proofs. Otherwise it
    // can install the archive's epoch before the explicit live install reaches it.
    let withheld = if peer_proofs {
        let mut registry = source.registry.write().await;
        let original = registry.checkpoint_inbox().unwrap();
        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        registry.set_checkpoint_inbox(sender);
        Some((original, receiver))
    } else {
        None
    };
    let mut config = prepared.target_config.clone();
    config.snapshot_transfer.max_archive_bytes = bytes.len().max(prepared.bytes.len()) as u64;
    config.data_dir = directory.path().join("target");
    if !peer_proofs {
        config.bootstrap_peers.clear();
    }
    let target = Running::start(config.clone(), prepared.now + 1).await;
    let genesis = crate::genesis_state::block(NetworkId::Dev, &prepared.params).hash;
    target.ready(genesis).await;
    let oversized = directory.path().join("over-budget.snapshot");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len(config.snapshot_transfer.max_archive_bytes + 1)
        .unwrap();
    let error = target.installer.install(oversized).await.unwrap_err();
    assert!(error.contains("local file admission bounds"), "{error}");
    target.ready(genesis).await;
    let proofs = if peer_proofs {
        None
    } else {
        let service = WitnessService::start(source.store.clone(), target.store.clone(), 0).await;
        target
            .registry
            .write()
            .await
            .set_witness_requests(service.registry.read().await.witness_requests().unwrap());
        Some(service)
    };
    let previous = if replace {
        let old_path = directory.path().join("old.snapshot");
        std::fs::write(&old_path, &prepared.bytes).unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(90), target.installer.install(old_path))
                .await
                .unwrap()
                .unwrap(),
            prepared.old_epoch
        );
        target.ready(prepared.old_epoch).await;
        assert_eq!(
            installed::read_stored_base_bytes(&target.store)
                .unwrap()
                .unwrap(),
            prepared.bytes
        );
        prepared.old_epoch
    } else {
        genesis
    };
    target
        .store
        .meta_store()
        .reserve_consensus_vote_at(
            &previous,
            &address(0),
            &address(30),
            &BlockHash::ZERO,
            &BlockHash::new([98; 32]),
            true,
            0,
            Some(0),
        )
        .unwrap();
    let mut promises = target
        .store
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"consensus_final/")
        .unwrap();
    assert!(!promises.is_empty());
    let corrupt = directory.path().join("corrupt.snapshot");
    std::fs::write(&corrupt, b"not an authenticated archive").unwrap();
    assert!(target.installer.install(corrupt).await.is_err());
    target.ready(previous).await;
    assert_retained(&target.store, &promises);
    if !replace {
        cancel_preparation(&target, &path).await;
        target.ready(previous).await;
        assert_retained(&target.store, &promises);
        assert!(matches!(
            target.store.meta_store().get_meta(installed::BASE_KEY),
            Err(burst_store::StoreError::NotFound(_))
        ));
    }
    let installed = if replace {
        tokio::time::timeout(
            Duration::from_secs(90),
            target.installer.install(path.clone()),
        )
        .await
        .unwrap()
        .unwrap()
    } else {
        let (epoch, retained) = install_with_late_promise(&target, &path, previous).await;
        assert_eq!(retained.len(), promises.len() + 1);
        assert_retained(&target.store, &promises);
        promises = retained;
        epoch
    };
    assert_eq!(installed, prepared.epoch);
    target.ready(prepared.epoch).await;
    assert_retained(&target.store, &promises);
    assert_lifecycle(&source.store, &target.store, None);
    assert_eq!(
        installed::read_stored_base_bytes(&target.store)
            .unwrap()
            .unwrap(),
        bytes
    );
    assert_trst_state(
        &source.store,
        &target.store,
        prepared.params.trst_expiry_secs,
    );
    assert!(target
        .installer
        .install(path)
        .await
        .unwrap_err()
        .contains("does not advance"));
    assert_retained(&target.store, &promises);
    let first = burn(
        &target,
        &prepared.params,
        prepared.epoch,
        previous,
        prepared.now + 1,
    )
    .await;
    target.stop().await;
    if let Some(proofs) = proofs {
        assert!(proofs.stop().await > 0);
    }
    if let Some((original, mut withheld)) = withheld {
        assert!(
            withheld.try_recv().is_ok(),
            "peer never requested checkpoint catch-up"
        );
        source.registry.write().await.set_checkpoint_inbox(original);
    }
    config.bootstrap_peers = prepared.target_config.bootstrap_peers.clone();
    // Admission policy must not make previously installed state unreadable.
    config.snapshot_transfer.max_archive_bytes = 25;
    let target = Running::start(config, prepared.now + 2).await;
    target.ready(prepared.epoch).await;
    assert!(target.store.block_store().exists(&first).unwrap());
    assert_retained(&target.store, &promises);
    assert_lifecycle(&source.store, &target.store, Some(first.into_tx_hash()));
    burn(
        &target,
        &prepared.params,
        prepared.epoch,
        previous,
        prepared.now + 2,
    )
    .await;
    target.stop().await;
    source.stop().await;
}

async fn cancel_preparation(target: &Running, path: &std::path::Path) {
    let original = target.registry.read().await.witness_requests().unwrap();
    let (sender, mut requests) = tokio::sync::mpsc::channel(1);
    target.registry.write().await.set_witness_requests(sender);
    let installer = target.installer.clone();
    let archive = path.to_path_buf();
    let pending = tokio::spawn(async move { installer.install(archive).await });
    let request = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        target
            .installer
            .install(path.to_path_buf())
            .await
            .unwrap_err(),
        "snapshot installation is already active"
    );
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    assert!(request.result.is_closed());
    drop(request);
    target.registry.write().await.set_witness_requests(original);
}

async fn install_with_late_promise(
    target: &Running,
    path: &std::path::Path,
    previous: BlockHash,
) -> (BlockHash, Vec<(Vec<u8>, Vec<u8>)>) {
    let original = target.registry.read().await.witness_requests().unwrap();
    let (sender, mut requests) = tokio::sync::mpsc::channel(1);
    target.registry.write().await.set_witness_requests(sender);
    let installer = target.installer.clone();
    let archive = path.to_path_buf();
    let pending = tokio::spawn(async move { installer.install(archive).await });
    let request = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .unwrap()
        .unwrap();
    target
        .store
        .meta_store()
        .reserve_consensus_vote_at(
            &previous,
            &address(0),
            &address(29),
            &BlockHash::ZERO,
            &BlockHash::new([97; 32]),
            true,
            0,
            Some(0),
        )
        .unwrap();
    let retained = target
        .store
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"consensus_final/")
        .unwrap();
    target
        .registry
        .write()
        .await
        .set_witness_requests(original.clone());
    original.send(request).await.unwrap();
    let epoch = tokio::time::timeout(Duration::from_secs(90), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_retained(&target.store, &retained);
    (epoch, retained)
}
