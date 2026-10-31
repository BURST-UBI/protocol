use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Account exclusion shared by speculative admission and blocking processing.
/// `process` holds a semaphore permit while waiting for the account lock and
/// running its closure. `try_reserve` uses a separate account-count check; it
/// does not acquire a semaphore permit.
pub struct AccountWorkPool {
    /// Per-account mutexes
    account_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// Process semaphore capacity and speculative account limit
    max_concurrent: usize,
    speculative: std::sync::Mutex<HashMap<String, std::sync::Weak<tokio::sync::OwnedMutexGuard<()>>>>,
    /// Semaphore for limiting total concurrency
    semaphore: Arc<tokio::sync::Semaphore>,
}

impl AccountWorkPool {
    pub fn new(max_concurrent: usize) -> Self {
        assert!(
            max_concurrent > 0,
            "parallel block processing requires a nonzero worker limit"
        );
        Self {
            account_locks: Arc::new(Mutex::new(HashMap::new())),
            max_concurrent,
            speculative: std::sync::Mutex::new(HashMap::new()),
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

    /// Queue siblings share the guard through admission, but only its creator
    /// prepares speculatively. This prevents a later worker racing a queued
    /// sibling's serial fallback after the original candidate has committed.
    /// Never wait after removing a block from a cancellation-safe queue.
    pub(crate) fn try_reserve(
        &self, account: &str,
    ) -> Option<(Arc<tokio::sync::OwnedMutexGuard<()>>, bool)> {
        let mut speculative = self.speculative.try_lock().ok()?;
        speculative.retain(|_, guard| guard.strong_count() > 0);
        if let Some(guard) = speculative.get(account).and_then(std::sync::Weak::upgrade) {
            return Some((guard, false));
        }
        let mut locks = self.account_locks.try_lock().ok()?;
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        if !locks.contains_key(account) && locks.len() >= self.max_concurrent {
            return None;
        }
        let guard = locks.entry(account.to_owned()).or_insert_with(|| Arc::new(Mutex::new(())))
            .clone().try_lock_owned().ok()?;
        let guard = Arc::new(guard);
        speculative.insert(account.to_owned(), Arc::downgrade(&guard));
        Some((guard, true))
    }

    /// Run blocking work under a global permit and the account lock.
    /// Waiting callers are not bounded here. Idle lock records remain until
    /// cleanup or try_reserve; cancellation does not stop a running closure.
    pub async fn process<F, R>(&self, account: &str, f: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let global_permit = self.semaphore.clone().acquire_owned().await.unwrap();
        let lock = self.get_account_lock(account).await;
        let account_guard = lock.lock_owned().await;

        // Cancelling the async caller does not cancel an already-running
        // blocking operation. Its guards must live with that operation.
        tokio::task::spawn_blocking(move || {
            let _global_permit = global_permit;
            let _account_guard = account_guard;
            f()
        })
        .await
        .unwrap()
    }

    /// Returns the maximum concurrency limit.
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// Number of retained account locks, including idle locks awaiting cleanup.
    pub async fn tracked_accounts(&self) -> usize {
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
    async fn speculative_reservations_are_exclusive_bounded_and_reclaimed() {
        let pool = AccountWorkPool::new(2);
        let (a, prepare) = pool.try_reserve("a").unwrap();
        assert!(prepare);
        let (sibling, prepare) = pool.try_reserve("a").unwrap();
        assert!(!prepare);
        assert!(Arc::ptr_eq(&a, &sibling));
        let (b, prepare) = pool.try_reserve("b").unwrap();
        assert!(prepare);
        assert!(pool.try_reserve("c").is_none());
        drop(a);
        let (later, prepare) = pool.try_reserve("a").unwrap();
        assert!(!prepare, "queued sibling must retain account ownership");
        assert!(Arc::ptr_eq(&later, &sibling));
        assert!(pool.try_reserve("c").is_none());
        drop(sibling);
        drop(later);
        let (c, prepare) = pool.try_reserve("c").unwrap();
        assert!(prepare);
        assert_eq!(pool.tracked_accounts().await, 2);
        drop(b);
        drop(c);
        for n in 0..1000 {
            let (guard, prepare) = pool.try_reserve(&n.to_string()).unwrap();
            assert!(prepare);
            assert_eq!(pool.tracked_accounts().await, 1);
            assert_eq!(pool.speculative.lock().unwrap().len(), 1);
            drop(guard);
        }
        pool.cleanup().await;
        assert_eq!(pool.tracked_accounts().await, 0);
    }

    #[test]
    #[should_panic(expected = "nonzero worker limit")]
    fn zero_workers_fail_instead_of_waiting_forever() {
        AccountWorkPool::new(0);
    }

    #[tokio::test]
    async fn cancellation_retains_account_and_capacity_guards_until_blocking_work_finishes() {
        for same_account in [true, false] {
            let pool = Arc::new(AccountWorkPool::new(if same_account {
                2
            } else {
                1
            }));
            let (entered, started) = tokio::sync::oneshot::channel();
            let (release, released) = std::sync::mpsc::channel();
            let first_processor = pool.clone();
            let first = tokio::spawn(async move {
                first_processor
                    .process("a", move || {
                        entered.send(()).unwrap();
                        released.recv().unwrap();
                    })
                    .await;
            });
            started.await.unwrap();
            first.abort();
            assert!(first.await.unwrap_err().is_cancelled());
            pool.cleanup().await;
            assert_eq!(
                pool.tracked_accounts().await,
                1,
                "cleanup discarded a lock still owned by blocking work"
            );
            let (entered, mut started) = tokio::sync::oneshot::channel();
            let second = tokio::spawn(async move {
                pool
                    .process(if same_account { "a" } else { "b" }, move || {
                        entered.send(()).unwrap();
                        42
                    })
                    .await
            });
            assert!(
                tokio::time::timeout(Duration::from_millis(25), &mut started)
                    .await
                    .is_err(),
                "cancelled caller released exclusion while its operation was running"
            );
            release.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(2), started)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(second.await.unwrap(), 42);
        }
    }

    #[tokio::test]
    async fn test_basic_processing() {
        let pool = AccountWorkPool::new(4);
        let result = pool.process("account_a", || 42).await;
        assert_eq!(result, 42);
    }

    #[tokio::test]
    async fn test_different_accounts_run_in_parallel() {
        let pool = Arc::new(AccountWorkPool::new(4));

        let start = Instant::now();
        let mut handles = Vec::new();

        for i in 0..4 {
            let pool = Arc::clone(&pool);
            let account = format!("account_{i}");
            handles.push(tokio::spawn(async move {
                pool.process(&account, move || {
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
        let pool = Arc::new(AccountWorkPool::new(4));
        let counter = Arc::new(AtomicU64::new(0));

        let mut handles = Vec::new();

        for _ in 0..4 {
            let pool = Arc::clone(&pool);
            let counter = Arc::clone(&counter);
            handles.push(tokio::spawn(async move {
                pool.process("same_account", move || {
                    let val = counter.fetch_add(1, Ordering::SeqCst);
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

        // Sorted results check completion, not acquisition order or exclusion.
        results.sort();
        assert_eq!(results, vec![0, 1, 2, 3]);
        assert_eq!(counter.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn test_semaphore_limits_concurrency() {
        let pool = Arc::new(AccountWorkPool::new(2));
        let concurrent = Arc::new(AtomicU64::new(0));
        let max_seen = Arc::new(AtomicU64::new(0));

        let mut handles = Vec::new();

        for i in 0..6 {
            let pool = Arc::clone(&pool);
            let concurrent = Arc::clone(&concurrent);
            let max_seen = Arc::clone(&max_seen);
            let account = format!("account_{i}");
            handles.push(tokio::spawn(async move {
                pool.process(&account, move || {
                    let current = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                    // Update max observed concurrency
                    max_seen.fetch_max(current, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(30));
                    concurrent.fetch_sub(1, Ordering::SeqCst);
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
        let pool = AccountWorkPool::new(4);

        // Process some blocks to create locks
        pool.process("account_a", || ()).await;
        pool.process("account_b", || ()).await;

        // Locks exist but are idle
        assert_eq!(pool.tracked_accounts().await, 2);

        // Cleanup should remove them since no one holds a reference
        pool.cleanup().await;
        assert_eq!(pool.tracked_accounts().await, 0);
    }

    #[tokio::test]
    async fn test_max_concurrent_getter() {
        let pool = AccountWorkPool::new(8);
        assert_eq!(pool.max_concurrent(), 8);
    }
}
