use super::*;
use burst_store::meta::MetaStore;

#[tokio::test]
async fn proof_service_starts_without_authority_reuses_its_queue_and_stops() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(LmdbStore::open(directory.path(), 32, 16 << 20).unwrap());
    store
        .meta_store()
        .put_meta(
            burst_store::meta::CERTIFIED_PAYMENT_AUTHORITY_KEY,
            b"not initialized",
        )
        .unwrap();
    assert!(crate::payment_authority::PaymentAuthority::read(&store).is_err());
    let before = store
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let registry = Arc::new(RwLock::new(ConnectionRegistry::new()));
    let shutdown = Arc::new(ShutdownController::new());
    start(
        Arc::clone(&store),
        Arc::clone(&registry),
        Arc::clone(&shutdown),
    )
    .await;
    let sender = registry.read().await.witness_requests().unwrap();
    start(
        Arc::clone(&store),
        Arc::clone(&registry),
        Arc::clone(&shutdown),
    )
    .await;
    assert!(sender.same_channel(&registry.read().await.witness_requests().unwrap()));
    let (result, received) = tokio::sync::oneshot::channel();
    sender
        .send(Command {
            roots: Roots::Plan(Vec::new()),
            wanted: Vec::new(),
            result,
        })
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), received)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    shutdown.shutdown();
    let tasks = registry.write().await.take_tasks();
    assert_eq!(tasks.len(), 1);
    for task in tasks {
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    }
    assert!(sender.is_closed());
    assert!(shutdown.failure().is_none());
    assert_eq!(
        store
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
}
