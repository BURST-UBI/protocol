use std::future::Future;
use tokio::task::JoinHandle;

/// One serialized admission worker per node. Its future owns the queue,
/// reservations and publication state; synchronous database waits stay off
/// runtime threads. Track the blocking handle itself so shutdown cannot detach
/// an in-flight writer by aborting an outer async task.
pub(super) fn spawn(work: impl Future<Output = ()> + Send + 'static) -> JoinHandle<()> {
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || runtime.block_on(work))
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::meta::MetaStore;
    use std::{sync::Arc, time::Duration};

    #[tokio::test(flavor = "current_thread")]
    async fn writer_wait_keeps_runtime_live_and_shutdown_waits_for_publication() {
        let directory = tempfile::tempdir().unwrap();
        let store =
            Arc::new(burst_store_lmdb::LmdbStore::open(directory.path(), 32, 16 << 20).unwrap());
        let held_store = store.clone();
        let (held, acquired) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _writer = held_store.tx_begin_write().unwrap();
            held.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        acquired.await.unwrap();
        let publication = Arc::new(tokio::sync::Mutex::new(false));
        let live = publication.clone();
        let work_store = store.clone();
        let (started, waiting) = tokio::sync::oneshot::channel();
        let worker = spawn(async move {
            let mut published = live.lock().await;
            started.send(()).unwrap();
            let mut batch = work_store.tx_begin_write().unwrap();
            batch
                .put_meta("admission-worker-test", b"committed")
                .unwrap();
            batch.commit().unwrap();
            *published = true;
        });
        waiting.await.unwrap();
        let mut drain = tokio::spawn(crate::shutdown::drain_tasks(
            vec![worker],
            Duration::from_millis(20),
        ));
        // The current-thread timer progresses while LMDB's writer is held.
        // drain_tasks must still join the actual worker after its abort request.
        assert!(tokio::time::timeout(Duration::from_millis(60), &mut drain)
            .await
            .is_err());
        assert!(
            publication.try_lock().is_err(),
            "publication ownership was released early"
        );
        assert!(matches!(
            store.meta_store().get_meta("admission-worker-test"),
            Err(burst_store::StoreError::NotFound(_))
        ));
        release.send(()).unwrap();
        holder.join().unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), drain)
                .await
                .unwrap()
                .unwrap()
                .is_err(),
            "the shutdown timeout remains a failure, even when the worker later drains"
        );
        assert!(*publication.lock().await);
        assert_eq!(
            store
                .meta_store()
                .get_meta("admission-worker-test")
                .unwrap(),
            b"committed".to_vec()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn idle_worker_receives_shutdown_and_panics_are_join_errors() {
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let worker = spawn(async move {
            stopped.await.unwrap();
        });
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), worker)
            .await
            .unwrap()
            .unwrap();
        assert!(spawn(async {
            panic!("admission worker fixture");
        })
        .await
        .unwrap_err()
        .is_panic());
    }
}
