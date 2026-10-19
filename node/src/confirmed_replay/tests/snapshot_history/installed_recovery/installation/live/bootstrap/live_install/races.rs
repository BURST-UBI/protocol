use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_snapshot_installation_rechecks_late_confirmation() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let (prepared, _) = prepare_active();
    let source = Arc::new(LmdbStore::open(&prepared.source_config.data_dir, 32, 32 << 20).unwrap());
    let bytes = export_bytes(&source, &prepared.params, prepared.epoch);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("live.snapshot");
    std::fs::write(&path, bytes).unwrap();
    let mut config = prepared.target_config.clone();
    config.data_dir = directory.path().join("target");
    config.bootstrap_peers.clear();
    let target = Running::start(config, prepared.now + 1).await;
    let genesis = crate::genesis_state::block(NetworkId::Dev, &prepared.params).hash;
    target.ready(genesis).await;
    let proofs = Service::start(source, target.store.clone(), 0).await;
    let (sender, mut requests) = tokio::sync::mpsc::channel(1);
    target.registry.write().await.set_witness_requests(sender);
    let installer = target.installer.clone();
    let pending = tokio::spawn(async move { installer.install(path).await });
    let request = tokio::time::timeout(Duration::from_secs(10), requests.recv())
        .await
        .unwrap()
        .unwrap();

    // Consensus may durably queue a winner before its body reaches this node.
    let winner = BlockHash::new([96; 32]);
    target
        .store
        .meta_store()
        .enqueue_confirmation(&winner)
        .unwrap();
    let before = target
        .store
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let count = target.store.block_store().block_count().unwrap();
    let sender = proofs.registry.read().await.witness_requests().unwrap();
    target
        .registry
        .write()
        .await
        .set_witness_requests(sender.clone());
    sender.send(request).await.unwrap();
    drop(sender);
    let error = tokio::time::timeout(Duration::from_secs(90), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(
        error.contains("snapshot omits a durable confirmation target"),
        "{error}"
    );
    target.ready(genesis).await;
    assert_eq!(target.store.meta_store().payment_epoch().unwrap(), genesis);
    assert_eq!(target.store.block_store().block_count().unwrap(), count);
    assert_eq!(
        target.store.meta_store().pending_confirmations().unwrap(),
        vec![winner]
    );
    assert_eq!(
        target
            .store
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    assert!(matches!(
        target.store.meta_store().get_meta(installed::BASE_KEY),
        Err(burst_store::StoreError::NotFound(_))
    ));
    target.stop().await;
    assert!(proofs.stop().await > 0);
}
