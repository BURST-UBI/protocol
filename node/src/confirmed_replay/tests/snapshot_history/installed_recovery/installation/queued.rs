use super::*;
use crate::checkpoint_sync::witness;
use std::time::Duration;

#[tokio::test]
async fn concurrent_proof_requests_wait_and_cancel_without_busy_rejection() {
    let (fixture, _dir, source, selection, _) = selection::setup();
    let base = authenticate(&source, &fixture.params, selection.checkpoint.parent);
    let (_service_dir, service_store) = suffix_store(&[]);
    let source = Arc::new(service_store);
    let before = source
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let registry = Arc::new(RwLock::new(ConnectionRegistry::new()));
    let shutdown = Arc::new(crate::shutdown::ShutdownController::new());
    witness::start(source.clone(), registry.clone(), shutdown.clone()).await;
    let sender = registry.read().await.witness_requests().unwrap();
    let command = |result| witness::Command {
        roots: witness::Roots::Account(base.history_cursor(&address(0)).unwrap().unwrap()),
        wanted: vec![fixture.blocks[0].hash],
        result,
    };
    let (first, first_reply) = tokio::sync::oneshot::channel();
    sender.send(command(first)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while sender.capacity() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (second, mut second_reply) = tokio::sync::oneshot::channel();
    sender.send(command(second)).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut second_reply)
            .await
            .is_err()
    );
    assert_eq!(sender.capacity(), 0, "the active job must keep its slot");
    drop(first_reply);
    tokio::time::timeout(Duration::from_secs(3), async {
        while sender.capacity() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cancelling the active request did not release the queued request");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut second_reply)
            .await
            .is_err()
    );
    shutdown.shutdown();
    for task in registry.write().await.take_tasks() {
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    }
    assert!(second_reply.await.is_err());
    assert!(sender.is_closed());
    assert!(shutdown.failure().is_none());
    assert_eq!(
        source
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
}
