use super::*;
use std::sync::Arc;
use tokio::sync::OwnedMutexGuard;

// The caller acquires the generator mutex before spawning blocking work.
// One job per shared mutex can run; cancelling its caller does not release the
// guard or stop the job. Separate generator mutexes are not serialized here.
async fn run<T: Send + 'static>(
    store: Arc<LmdbStore>,
    mut generator: OwnedMutexGuard<VoteGenerator>,
    operation: impl FnOnce(&LmdbStore, &mut VoteGenerator) -> T + Send + 'static,
) -> Result<T, StoreError> {
    let queued = std::env::var_os("BURST_FINALITY_STAGE_LOG").map(|_| std::time::Instant::now());
    tokio::task::spawn_blocking(move || {
        if let Some(queued) = queued {
            tracing::debug!(receiver = ?store.env().path(), voter = %generator.representative,
                elapsed_us = queued.elapsed().as_micros(), "vote worker started");
        }
        let started = queued.map(|_| std::time::Instant::now());
        let result = operation(&store, &mut generator);
        if let Some(started) = started {
            tracing::debug!(receiver = ?store.env().path(), voter = %generator.representative,
                elapsed_us = started.elapsed().as_micros(), "vote worker finished");
        }
        result
    })
    .await
    .map_err(|error| StoreError::Backend(format!("durable vote worker failed: {error}")))
}

#[cfg(test)]
pub(crate) async fn generate(
    store: Arc<LmdbStore>,
    generator: OwnedMutexGuard<VoteGenerator>,
    hash: BlockHash,
    is_final: bool,
) -> Result<GeneratedVote, VoteGenerationError> {
    run(store, generator, move |store, generator| {
        generate_vote(store, generator, hash, is_final)
    })
    .await?
}

pub(crate) async fn respond(
    store: Arc<LmdbStore>,
    generator: OwnedMutexGuard<VoteGenerator>,
    hash: BlockHash,
) -> Result<Option<GeneratedVote>, StoreError> {
    run(store, generator, move |store, generator| {
        respond_vote(store, generator, hash)
    })
    .await?
}

pub(crate) async fn responses(
    store: Arc<LmdbStore>,
    generator: OwnedMutexGuard<VoteGenerator>,
    hashes: Vec<BlockHash>,
) -> Result<Vec<Result<Option<GeneratedVote>, StoreError>>, StoreError> {
    run(store, generator, move |store, generator| {
        respond_votes(store, generator, &hashes)
    })
    .await
}

pub(crate) async fn finals(
    store: Arc<LmdbStore>,
    generator: OwnedMutexGuard<VoteGenerator>,
    hashes: Vec<BlockHash>,
) -> Result<Vec<Result<GeneratedVote, VoteGenerationError>>, StoreError> {
    run(store, generator, move |store, generator| {
        generate_final_votes(store, generator, &hashes)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::sync::{oneshot, Mutex};

    #[tokio::test]
    async fn cancelled_vote_caller_keeps_serialization_until_worker_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 32, 16 << 20).unwrap());
        let key = burst_crypto::keypair_from_seed(&[7; 32]);
        let signer = Arc::new(Mutex::new(VoteGenerator::new(
            burst_crypto::derive_address(&key.public),
            key.private.0,
        )));
        let (entered, started) = oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let guard = signer.clone().lock_owned().await;
        let first_store = store.clone();
        let first = tokio::spawn(run(first_store, guard, move |_, generator| {
            entered.send(()).unwrap();
            // A regression that blocks the runtime must fail, not hang forever.
            released.recv_timeout(Duration::from_secs(2)).unwrap();
            generator.advance_sequence_to(7);
        }));
        started.await.unwrap();
        assert!(
            !first.is_finished(),
            "blocking operation ran on the async runtime"
        );
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        assert!(signer.try_lock().is_err());
        let (entered, mut started) = oneshot::channel();
        let next_signer = signer.clone();
        let second = tokio::spawn(async move {
            let guard = next_signer.lock_owned().await;
            run(store, guard, move |_, generator| {
                entered.send(()).unwrap();
                generator.sequence()
            })
            .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut started)
                .await
                .is_err()
        );
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), started)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.await.unwrap().unwrap(), 7);
        assert!(signer.try_lock().is_ok());
    }

    #[tokio::test]
    async fn worker_panic_returns_error_and_releases_signer() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 32, 16 << 20).unwrap());
        let key = burst_crypto::keypair_from_seed(&[7; 32]);
        let signer = Arc::new(Mutex::new(VoteGenerator::new(
            burst_crypto::derive_address(&key.public),
            key.private.0,
        )));
        let result = run::<()>(store, signer.clone().lock_owned().await, |_, _| {
            panic!("worker fault")
        })
        .await;
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("durable vote worker failed"));
        assert_eq!(signer.try_lock().unwrap().sequence(), 0);
    }
}
