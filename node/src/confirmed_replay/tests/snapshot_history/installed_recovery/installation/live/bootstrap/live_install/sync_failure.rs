use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_snapshot_installation_sync_failure_stops_and_recovers() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let (prepared, _) = prepare_active();
    let source = Running::start(prepared.source_config.clone(), prepared.now + 1).await;
    source.ready(prepared.epoch).await;
    let bytes = export_bytes(&source.store, &prepared.params, prepared.epoch);
    let original = source.registry.read().await.checkpoint_inbox().unwrap();
    let (sender, _withheld) = tokio::sync::mpsc::channel(32);
    source.registry.write().await.set_checkpoint_inbox(sender);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("live.snapshot");
    std::fs::write(&path, &bytes).unwrap();
    let mut config = prepared.target_config.clone();
    config.data_dir = directory.path().join("node");
    let mut target = Running::start(config.clone(), prepared.now + 1).await;
    let genesis = crate::genesis_state::block(NetworkId::Dev, &prepared.params).hash;
    target.ready(genesis).await;
    target.installer.fail_next_sync();
    assert!(
        tokio::time::timeout(Duration::from_secs(90), target.installer.install(path))
            .await
            .unwrap()
            .is_err()
    );
    let mut task = target.task.take().unwrap();
    let (mut node, started) = tokio::time::timeout(Duration::from_secs(20), &mut task)
        .await
        .unwrap()
        .unwrap();
    started.unwrap();
    let error = node.stop().await.unwrap_err().to_string();
    assert!(error.contains("injected snapshot sync failure"), "{error}");
    assert_eq!(
        node.store.meta_store().payment_epoch().unwrap(),
        prepared.epoch
    );
    assert_eq!(
        installed::read_stored_base_bytes(&node.store)
            .unwrap()
            .unwrap(),
        bytes
    );
    assert_eq!(
        node.active_elections.read().await.authority_epoch(),
        Some(genesis)
    );
    for port in [config.port, config.rpc_port] {
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err());
    }
    drop(node);
    drop(target);
    source.registry.write().await.set_checkpoint_inbox(original);
    let target = Running::start(config, prepared.now + 2).await;
    target.ready(prepared.epoch).await;
    assert_lifecycle(&source.store, &target.store, None);
    burn(
        &target,
        &prepared.params,
        prepared.epoch,
        genesis,
        prepared.now + 2,
    )
    .await;
    target.stop().await;
    source.stop().await;
}
