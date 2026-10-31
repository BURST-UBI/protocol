//! Reclaim unowned shared-head nodes without waiting for another record write.
use crate::shutdown::ShutdownController;
use burst_store::StoreError;
use burst_store_lmdb::LmdbStore;
use std::{sync::Arc, time::Duration};

const NODES_PER_TICK: usize = 256;

pub(crate) fn start(
    store: Arc<LmdbStore>,
    shutdown: Arc<ShutdownController>,
) -> tokio::task::JoinHandle<()> {
    // Production maintenance reclaims unowned nodes; legacy-record migration is disabled.
    start_with_migration(store, shutdown, false)
}

pub(super) fn start_with_migration(
    store: Arc<LmdbStore>,
    shutdown: Arc<ShutdownController>,
    migrate: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut migration = super::shared::migration::Worker::default();
        let mut stop = shutdown.subscribe();
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        timer.tick().await;
        loop {
            tokio::select! {
                biased;
                _ = stop.recv() => break,
                _ = timer.tick() => {}
            }
            if shutdown.is_requested() {
                break;
            }
            let store = store.clone();
            let shutdown = shutdown.clone();
            // Await the bounded worker before scheduling again or completing shutdown.
            match tokio::task::spawn_blocking(move || {
                let result = reclaim(&store, &shutdown).and_then(|_| {
                    if migrate {
                        migration.tick(&store, &shutdown)?;
                    }
                    Ok(())
                });
                (migration, result)
            })
            .await
            {
                Ok((worker, result)) => {
                    migration = worker;
                    if let Err(error) = result {
                        tracing::warn!(%error, "shared checkpoint maintenance deferred");
                    }
                }
                Err(error) => {
                    tracing::error!(%error, "shared checkpoint cleanup worker failed");
                    break;
                }
            }
        }
    })
}

fn reclaim(store: &LmdbStore, shutdown: &ShutdownController) -> Result<usize, StoreError> {
    if shutdown.is_requested() {
        return Ok(0);
    }
    {
        let txn = store.tx_begin_read()?;
        if !store.read_view(&txn)?.checkpoint_heads_need_cleanup()? {
            return Ok(0);
        }
    }
    let mut batch = store.tx_begin_write()?;
    if shutdown.is_requested() {
        return Ok(0);
    }
    let result = batch.reclaim_checkpoint_heads(NODES_PER_TICK)?;
    if shutdown.is_requested() {
        return Ok(0);
    }
    batch.commit()?;
    Ok(result.nodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store_lmdb::checkpoint_heads::HeadNode;

    #[test]
    fn cleanup_is_bounded_resumes_after_reopen_and_skips_idle_or_cancelled_writes() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let shutdown = ShutdownController::new();
        let mut batch = store.tx_begin_write().unwrap();
        for index in 0u64..600 {
            let mut account = [0; 32];
            account[..8].copy_from_slice(&index.to_be_bytes());
            batch
                .intern_checkpoint_head(&HeadNode::Leaf {
                    account,
                    height: 1,
                    hash: [7; 32],
                })
                .unwrap();
        }
        batch.commit().unwrap();
        let before = store.env().info().last_txn_id;
        let cancelled = ShutdownController::new();
        cancelled.shutdown();
        assert_eq!(reclaim(&store, &cancelled).unwrap(), 0);
        assert_eq!(store.env().info().last_txn_id, before);
        assert_eq!(reclaim(&store, &shutdown).unwrap(), NODES_PER_TICK);
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        assert_eq!(reclaim(&store, &shutdown).unwrap(), NODES_PER_TICK);
        assert_eq!(reclaim(&store, &shutdown).unwrap(), 88);
        let before = store.env().info().last_txn_id;
        assert_eq!(reclaim(&store, &shutdown).unwrap(), 0);
        assert_eq!(store.env().info().last_txn_id, before);
    }
    #[tokio::test]
    async fn scheduled_cleanup_progresses_without_record_writes_and_joins_on_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 32, 16 << 20).unwrap());
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .intern_checkpoint_head(&HeadNode::Leaf {
                account: [3; 32],
                height: 1,
                hash: [7; 32],
            })
            .unwrap();
        batch.commit().unwrap();
        let shutdown = Arc::new(ShutdownController::new());
        let worker = start(store.clone(), shutdown.clone());
        let completed = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let pending = {
                    let reader = store.tx_begin_read().unwrap();
                    store
                        .read_view(&reader)
                        .unwrap()
                        .checkpoint_heads_need_cleanup()
                        .unwrap()
                };
                if !pending {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap();
        assert!(
            completed.is_ok(),
            "periodic worker must progress without record writes"
        );
    }
}
