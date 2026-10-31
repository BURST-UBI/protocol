use super::{BlockHash, CementedBatch, LmdbStore, StoreError};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Run database work off runtime threads, one operation at a time per Worker.
/// A spawned operation retains the guard if its caller is cancelled. Waiting
/// callers are not count-bounded here, and cancellation does not undo a commit.
pub(crate) struct Worker {
    store: Arc<LmdbStore>,
    gate: Arc<Mutex<()>>,
}

impl Worker {
    pub(crate) fn new(store: Arc<LmdbStore>) -> Self {
        Self { store, gate: Arc::new(Mutex::new(())) }
    }

    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&LmdbStore) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let guard = self.gate.clone().lock_owned().await;
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            operation(&store)
        }).await.map_err(|error| StoreError::Backend(format!("cementation worker failed: {error}")))?
    }

    pub(crate) async fn pending(&self, cursor: Option<BlockHash>) -> Result<Vec<BlockHash>, StoreError> {
        self.run(move |store| store.pending_confirmation_batch(cursor, super::MAX_HANDOFF_BATCH)).await
    }

    pub(crate) async fn pending_group(&self, cursor: Option<BlockHash>)
        -> Result<super::PendingCementation, StoreError> {
        self.run(move |store| super::cement_pending_group(store, cursor)).await
    }

    #[cfg(test)]
    pub(crate) async fn group(&self, targets: Vec<BlockHash>)
        -> Result<Option<Vec<(BlockHash, CementedBatch)>>, StoreError> {
        self.run(move |store| super::cement_group(store, &targets)).await
    }

    pub(crate) async fn single(&self, target: BlockHash) -> Result<CementedBatch, StoreError> {
        self.run(move |store| super::cement(store, target)).await
    }

    pub(crate) async fn prune(&self) -> Result<usize, StoreError> {
        self.run(crate::fork_resolution::prune_finalized).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "current_thread")]
    async fn writer_wait_does_not_block_runtime_or_release_worker_on_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(directory.path(), 32, 16 << 20).unwrap());
        let worker = Arc::new(Worker::new(store.clone()));
        let (ready, started) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let _write = store.tx_begin_write().unwrap();
            ready.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        started.await.unwrap();
        let task_worker = worker.clone();
        let mut task = tokio::spawn(async move { task_worker.single(BlockHash::new([1;32])).await });
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut task).await.is_err());
        assert!(worker.gate.try_lock().is_err());
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(worker.gate.try_lock().is_err(), "blocking job retains serialization after caller cancellation");
        release.send(()).unwrap();
        writer.join().unwrap();
        let guard = tokio::time::timeout(Duration::from_secs(5), worker.gate.lock()).await.unwrap();
        drop(guard);
        assert!(worker.pending(None).await.unwrap().is_empty());
        assert_eq!(worker.prune().await.unwrap(), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn worker_preserves_errors_and_recovers_capacity_after_panic() {
        let directory = tempfile::tempdir().unwrap();
        let worker = Worker::new(Arc::new(LmdbStore::open(directory.path(), 32, 16 << 20).unwrap()));
        assert!(matches!(worker.run::<()>(|_| Err(StoreError::Corruption("fixture".into()))).await,
            Err(StoreError::Corruption(message)) if message == "fixture"));
        assert!(matches!(worker.run::<()>(|_| panic!("fixture worker panic")).await,
            Err(StoreError::Backend(_))));
        assert!(worker.group(Vec::new()).await.unwrap().unwrap().is_empty());
    }
}
