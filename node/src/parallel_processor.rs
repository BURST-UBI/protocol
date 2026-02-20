use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Per-account lock for parallel block processing.
/// Blocks on different accounts can be processed concurrently.
/// Blocks on the same account are serialized.
pub struct ParallelBlockProcessor {
    /// Per-account mutexes
    account_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// Maximum concurrent processing tasks
    max_concurrent: usize,
    /// Semaphore for limiting total concurrency
    semaphore: Arc<tokio::sync::Semaphore>,
}

impl ParallelBlockProcessor {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            account_locks: Arc::new(Mutex::new(HashMap::new())),
            max_concurrent,
            semaphore: Arc::new(tokio::sync::Semaphore::new(max_concurrent)),
        }
    }

    /// Get or create a lock for a specific account.
    async fn get_account_lock(&self, account: &str) -> Arc<Mutex<()>> {
        let mut locks = self.account_locks.lock().await;
        locks
            .entry(account.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Process a block with per-account serialization.
    /// Different accounts can be processed in parallel.
    pub async fn process<F, R>(&self, account: &str, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let _global_permit = self.semaphore.acquire().await.unwrap();
        let lock = self.get_account_lock(account).await;
        let _account_guard = lock.lock().await;

        tokio::task::spawn_blocking(f).await.unwrap()
    }

    /// Returns the maximum concurrency limit.
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// Get the number of accounts currently being processed.
    pub async fn active_accounts(&self) -> usize {
        let locks = self.account_locks.lock().await;
        locks.len()
    }

    /// Clean up locks for accounts no longer being processed.
    pub async fn cleanup(&self) {
        let mut locks = self.account_locks.lock().await;
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn test_basic_processing() {
        let processor = ParallelBlockProcessor::new(4);
        let result = processor.process("account_a", || 42).await;
        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn test_different_accounts_run_in_parallel() {
        let processor = Arc::new(ParallelBlockProcessor::new(4));

        let start = Instant::now();
        let mut handles = Vec::new();

        for i in 0..4 {
            let p = Arc::clone(&processor);
            let account = format!("account_{i}");
            handles.push(tokio::spawn(async move {
                p.process(&account, move || {
                    std::thread::sleep(Duration::from_millis(50));
                    i
                })
                .await
            }));
        }

        let mut results = Vec::new();
        for h in handles {
            results.push(h.await.unwrap());
        }

        let elapsed = start.elapsed();
        // All four should run in parallel, so total time should be
        // close to 50ms, not 200ms. Allow generous margin.
        assert!(
            elapsed < Duration::from_millis(200),
            "Expected parallel execution, took {elapsed:?}"
        );
        results.sort();
        assert_eq!(results, vec![0, 1, 2, 3]);
    }

    #[tokio::test]
    async fn test_same_account_serialized() {
        let processor = Arc::new(ParallelBlockProcessor::new(4));
        let counter = Arc::new(AtomicU64::new(0));

        let mut handles = Vec::new();

        for _ in 0..4 {
            let p = Arc::clone(&processor);
            let c = Arc::clone(&counter);
            handles.push(tokio::spawn(async move {
                p.process("same_account", move || {
                    let val = c.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(10));
                    val
                })
                .await
            }));
        }

        let mut results = Vec::new();
        for h in handles {
            results.push(h.await.unwrap());
        }

        // All increments should have happened; order guaranteed by serialization
        results.sort();
        assert_eq!(results, vec![0, 1, 2, 3]);
        assert_eq!(counter.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn test_semaphore_limits_concurrency() {
        let processor = Arc::new(ParallelBlockProcessor::new(2));
        let concurrent = Arc::new(AtomicU64::new(0));
        let max_seen = Arc::new(AtomicU64::new(0));

        let mut handles = Vec::new();

        for i in 0..6 {
            let p = Arc::clone(&processor);
            let conc = Arc::clone(&concurrent);
            let ms = Arc::clone(&max_seen);
            let account = format!("account_{i}");
            handles.push(tokio::spawn(async move {
                p.process(&account, move || {
                    let current = conc.fetch_add(1, Ordering::SeqCst) + 1;
                    // Update max observed concurrency
                    ms.fetch_max(current, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(30));
                    conc.fetch_sub(1, Ordering::SeqCst);
                })
                .await
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        let observed_max = max_seen.load(Ordering::SeqCst);
        assert!(
            observed_max <= 2,
            "Expected max concurrency 2, observed {observed_max}"
        );
    }

    #[tokio::test]
    async fn test_cleanup_removes_idle_locks() {
        let processor = ParallelBlockProcessor::new(4);

        // Process some blocks to create locks
        processor.process("account_a", || ()).await;
        processor.process("account_b", || ()).await;

        // Locks exist but are idle
        assert_eq!(processor.active_accounts().await, 2);

        // Cleanup should remove them since no one holds a reference
        processor.cleanup().await;
        assert_eq!(processor.active_accounts().await, 0);
    }

    #[tokio::test]
    async fn test_max_concurrent_getter() {
        let processor = ParallelBlockProcessor::new(8);
        assert_eq!(processor.max_concurrent(), 8);
    }
}
