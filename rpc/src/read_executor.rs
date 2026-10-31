//! A single shared blocking-read slot for handlers that use `ReadExecutor`.
use std::sync::Arc;

#[derive(Clone)]
pub struct ReadExecutor {
    workers: Arc<tokio::sync::Semaphore>,
}

impl Default for ReadExecutor {
    fn default() -> Self {
        Self {
            workers: Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
}

impl ReadExecutor {
    /// Admit one read per shared executor and reject concurrent calls immediately.
    /// The worker keeps the slot after HTTP cancellation until it exits. This limits
    /// concurrency, not the read's memory use or execution time.
    pub(crate) async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, crate::error::RpcError> + Send + 'static,
    ) -> Result<T, crate::error::RpcError> {
        let permit = self
            .workers
            .clone()
            .try_acquire_owned()
            .map_err(|_| crate::error::RpcError::RateLimited)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(|error| crate::error::RpcError::Server(error.to_string()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn blocking_read_rejects_overload_and_retains_capacity_after_cancellation() {
        let executor = ReadExecutor::default();
        let request_executor = executor.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let request = tokio::spawn(async move {
            request_executor
                .run(move || {
                    started.send(()).unwrap();
                    released
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                    Ok(7)
                })
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), ready)
            .await
            .unwrap()
            .unwrap();
        // This current-thread executor progresses while the worker is blocked.
        assert!(matches!(
            executor.run(|| Ok(8)).await,
            Err(crate::error::RpcError::RateLimited)
        ));
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        assert!(matches!(
            executor.run(|| Ok(9)).await,
            Err(crate::error::RpcError::RateLimited)
        ));
        release.send(()).unwrap();
        let permit = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            executor.workers.clone().acquire_owned(),
        )
        .await
        .unwrap()
        .unwrap();
        drop(permit);
        assert_eq!(executor.run(|| Ok(10)).await.unwrap(), 10);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_read_preserves_errors_and_releases_capacity_after_panic() {
        let executor = ReadExecutor::default();
        assert!(
            matches!(executor.run::<()>(|| Err(crate::error::RpcError::ProposalNotFound("missing".into()))).await,
            Err(crate::error::RpcError::ProposalNotFound(hash)) if hash == "missing")
        );
        assert!(matches!(
            executor.run::<()>(|| panic!("test worker panic")).await,
            Err(crate::error::RpcError::Server(_))
        ));
        assert_eq!(executor.run(|| Ok(11)).await.unwrap(), 11);
    }
}
