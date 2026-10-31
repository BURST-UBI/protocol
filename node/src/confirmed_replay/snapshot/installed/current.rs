use super::*;
use super::metadata::RecoveryControl;
use crate::{checkpoint_store::CertifiedCheckpoint, connection_registry::ConnectionRegistry};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Authenticate the accepted tip, not just the installed payment epoch. Missing
/// ancestry is fetched through the separate proof worker, without a writer held.
pub(crate) async fn recover_current_tip_with_cache(
    store: Arc<LmdbStore>,
    network: NetworkId,
    params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
    cached: Option<ReplayCache>,
) -> Result<ReplayCache, StoreError> {
    recover_current_tip(store, network, params, registry, cached, RecoveryControl::default())
        .await?.ok_or_else(|| invalid("uncancelled tip recovery stopped"))
}

pub(crate) async fn recover_current_tip_until_shutdown(
    store: Arc<LmdbStore>, network: NetworkId, params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>, cached: Option<ReplayCache>,
    shutdown: Arc<crate::shutdown::ShutdownController>,
) -> Result<Option<ReplayCache>, StoreError> {
    let control = RecoveryControl::for_shutdown(&registry, shutdown).await;
    recover_current_tip(store, network, params, registry, cached, control).await
}

async fn recover_current_tip(
    store: Arc<LmdbStore>, network: NetworkId, params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>, mut cached: Option<ReplayCache>,
    control: RecoveryControl,
) -> Result<Option<ReplayCache>, StoreError> {
    let genesis = crate::genesis_state::block(network, &params).hash;
    cached = cached.filter(|cache| {
        let anchor = cache.recovered.continuation.anchor();
        anchor.network() == network && anchor.genesis_hash() == genesis
    });
    let hint = cached.as_ref().and_then(|cache| cache.base);
    let Some(mut loaded) = load_view(store.clone(), network, params.clone(), hint, control.clone()).await? else { return Ok(None); };
    loop {
        let previous = loaded.commitment;
        let result = authenticate_view(
            store.clone(),
            network,
            params.clone(),
            registry.clone(),
            loaded,
            cached.take(),
            control.clone(),
        )
        .await;
        match result {
            Ok(checked) => return Ok(checked),
            Err(error) => {
                let Some(current) = load_view(store.clone(), network, params.clone(), None, control.clone()).await? else { return Err(error); };
                let next = current.commitment;
                // A changed read set requires fresh authentication, not acceptance
                // of the old result. Missing bases and unchanged corruption fail.
                if (previous.is_some() && next.is_none())
                    || (next == previous && !matches!(error, StoreError::StaleRead(_)))
                {
                    return Err(error);
                }
                loaded = current;
                tokio::task::yield_now().await;
            }
        }
    }
}

struct LoadedBase {
    snapshot: Option<crate::confirmed_replay::snapshot::archive::AuthenticatedSnapshot>,
    commitment: Option<BaseCommitment>,
}

async fn load_view(
    source: Arc<LmdbStore>,
    network: NetworkId,
    initial: ProtocolParams,
    cached_base: Option<BaseCommitment>,
    control: RecoveryControl,
) -> Result<Option<LoadedBase>, StoreError> {
    if control.checkpoint("tip-recovery:before-base") { return Ok(None); }
    tokio::task::spawn_blocking(move || {
        let transaction = source.tx_begin_read()?;
        let genesis = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(network, &initial)?;
        let cancelled = std::cell::Cell::new(false);
        if let Some(expected) = cached_base {
            let verified = (|| {
                let Some(mut reader) = pages::Reader::open(|key| {
                    if control.checkpoint("tip-recovery:base-page") {
                        cancelled.set(true);
                        return Err(invalid("tip recovery cancelled while checking base"));
                    }
                    source.meta_store().get_meta_txn(&transaction, key)
                })? else { return Ok::<_, StoreError>(None); };
                std::io::copy(&mut reader, &mut std::io::sink()).map_err(invalid)?;
                reader.commitment().map(Some)
            })();
            if cancelled.get() { return Ok(None); }
            if verified? == Some(expected) {
                if control.checkpoint("tip-recovery:cached-base-verified") { return Ok(None); }
                return Ok(Some(LoadedBase { snapshot: None, commitment: Some(expected) }));
            }
        }
        let loaded = read_base_at_until(
            |key| source.meta_store().get_meta_txn(&transaction, key),
            &genesis, source.env().path(), &|stage| {
                if control.checkpoint(stage) {
                    cancelled.set(true);
                    return Err(invalid("tip recovery cancelled while reading base"));
                }
                Ok(())
            },
        );
        // Only the callback's own interruption becomes cancellation. An actual
        // storage/authentication error racing shutdown still propagates.
        if cancelled.get() { return Ok(None); }
        let loaded = loaded?;
        if control.checkpoint("tip-recovery:base-read") { return Ok(None); }
        let (snapshot, commitment) = match loaded {
            Some((snapshot, commitment)) => (Some(snapshot), Some(commitment)),
            None => (None, None),
        };
        Ok(Some(LoadedBase { snapshot, commitment }))
    }).await.map_err(invalid)?
}

async fn authenticate_view(
    store: Arc<LmdbStore>,
    network: NetworkId,
    params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
    loaded: LoadedBase,
    cached: Option<ReplayCache>,
    control: RecoveryControl,
) -> Result<Option<ReplayCache>, StoreError> {
    if control.checkpoint("tip-recovery:authenticate") { return Ok(None); }
    let commitment = loaded.commitment;
    let cached_height = cached
        .as_ref()
        .map(|cache| cache.recovered.continuation.anchor().height());
    let base_matches = cached
        .as_ref()
        .is_some_and(|cache| cache.base == commitment);
    let resumed = match cached.filter(|cache| cache.base == commitment) {
        Some(cache) => match cache
            .recovered
            .advance_target_from_peers_until(store.clone(), registry.clone(), None, control.clone())
            .await
        {
            Ok(Some(recovered)) => Some(recovered),
            Ok(None) => return Ok(None),
            Err(error @ StoreError::StaleRead(_)) => {
                tracing::debug!(%error, ?cached_height, "tip recovery rejected stale replay observations");
                None
            }
            Err(error) => return Err(error),
        },
        None => None,
    };
    tracing::debug!(
        ?cached_height,
        base_matches,
        reused = resumed.is_some(),
        "tip recovery selected replay path"
    );
    let recovered = match (resumed, loaded.snapshot, commitment) {
        (Some(recovered), _, _) => recovered,
        (None, Some(base), _) => {
            match base.recover_from_peers_until(store.clone(), registry, None, control.clone()).await? {
                Some(recovered) => recovered, None => return Ok(None),
            }
        }
        (None, None, Some(_)) => return Err(StoreError::StaleRead(
            "cached tip observations require fresh base authentication".into(),
        )),
        (None, None, None) => {
            match RecoveredSnapshot::recover_genesis_from_peers_until(
                store.clone(),
                network,
                params.clone(),
                registry,
                None,
                control.clone(),
            ).await? { Some(recovered) => recovered, None => return Ok(None) }
        }
    };
    if control.checkpoint("tip-recovery:before-final-check") { return Ok(None); }
    tokio::task::spawn_blocking(move || {
        let transaction = store.tx_begin_write()?;
        if let Some(commitment) = commitment {
            commitment.verify(|key| transaction.get_meta(key))?;
        } else if transaction.get_meta(BASE_KEY)?.is_some() {
            return Err(invalid("snapshot base appeared during tip recovery"));
        }
        recovered.verify_records_unchanged(&transaction)?;
        if CertifiedCheckpoint::stored_tip(&store, network, &params)?
            != recovered.continuation.anchor().height()
        {
            return Err(invalid(
                "accepted checkpoint tip precedes recovered snapshot",
            ));
        }
        drop(transaction);
        if control.checkpoint("tip-recovery:complete") { return Ok(None); }
        Ok(Some(ReplayCache {
            base: commitment,
            recovered,
        }))
    })
    .await
    .map_err(invalid)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shutdown::ShutdownController;
    use burst_store::meta::MetaStore;

    #[tokio::test(flavor = "current_thread")]
    async fn tip_cached_base_checks_later_pages_and_cancellation() {
        use crate::paged_metadata::{Location, PAGE_BYTES};
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(directory.path(), 32, 16 << 20).unwrap());
        // Byte verification must cover the whole stream, even without decoding it.
        let bytes = vec![7; PAGE_BYTES * 3 + 29];
        let mut batch = store.tx_begin_write().unwrap();
        pages::write(&mut batch, &bytes).unwrap();
        batch.commit().unwrap();
        let commitment = {
            let transaction = store.tx_begin_read().unwrap();
            let mut reader = pages::Reader::open(|key| store.meta_store().get_meta_txn(&transaction, key))
                .unwrap().unwrap();
            std::io::copy(&mut reader, &mut std::io::sink()).unwrap();
            reader.commitment().unwrap()
        };
        let loaded = load_view(store.clone(), NetworkId::Dev, ProtocolParams::default(),
            Some(commitment), RecoveryControl::default()).await.unwrap().unwrap();
        assert!(loaded.snapshot.is_none());
        assert!(loaded.commitment == Some(commitment));

        let registry = Arc::new(RwLock::new(ConnectionRegistry::new()));
        let shutdown = Arc::new(ShutdownController::new());
        static STOP: std::sync::Mutex<Option<Arc<ShutdownController>>> = std::sync::Mutex::new(None);
        static PAGES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        *STOP.lock().unwrap() = Some(shutdown.clone());
        PAGES.store(0, std::sync::atomic::Ordering::SeqCst);
        registry.write().await.recovery_probe = Some(|stage| {
            if stage == "tip-recovery:base-page"
                && PAGES.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 2 {
                STOP.lock().unwrap().as_ref().unwrap().shutdown();
            }
        });
        let control = RecoveryControl::for_shutdown(&registry, shutdown).await;
        assert!(load_view(store.clone(), NetworkId::Dev, ProtocolParams::default(),
            Some(commitment), control).await.unwrap().is_none());
        STOP.lock().unwrap().take();

        let key = pages::SnapshotLocation.page_key(2);
        let mut page = store.meta_store().get_meta(&key).unwrap();
        page[0] ^= 1;
        store.meta_store().put_meta(&key, &page).unwrap();
        assert!(load_view(store, NetworkId::Dev, ProtocolParams::default(),
            Some(commitment), RecoveryControl::default()).await.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tip_shutdown_waits_for_owned_base_reader_and_returns_no_authority() {
        type Gate = (tokio::sync::oneshot::Sender<()>, std::sync::mpsc::Receiver<()>);
        static GATE: std::sync::Mutex<Option<Gate>> = std::sync::Mutex::new(None);
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(directory.path(), 32, 16 << 20).unwrap());
        let registry = Arc::new(RwLock::new(ConnectionRegistry::new()));
        let shutdown = Arc::new(ShutdownController::new());
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        *GATE.lock().unwrap() = Some((started, wait));
        registry.write().await.recovery_probe = Some(|stage| {
            if stage == "tip-recovery:base-read" {
                let (started, wait) = GATE.lock().unwrap().take().unwrap();
                started.send(()).unwrap();
                wait.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            }
        });
        let worker = tokio::spawn(recover_current_tip_until_shutdown(
            store.clone(), NetworkId::Dev, ProtocolParams::default(), registry,
            None, shutdown.clone(),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), ready).await.unwrap().unwrap();
        shutdown.shutdown();
        tokio::task::yield_now().await;
        let detached = worker.is_finished();
        release.send(()).unwrap();
        assert!(!detached, "shutdown acknowledged before the disk worker exited");
        assert!(worker.await.unwrap().unwrap().is_none());
        assert!(store.meta_store().get_meta_txn(&store.tx_begin_read().unwrap(), BASE_KEY).unwrap().is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tip_shutdown_before_load_and_corrupt_base_remain_distinct() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(directory.path(), 32, 16 << 20).unwrap());
        store.meta_store().put_meta(BASE_KEY, b"corrupt").unwrap();
        let registry = Arc::new(RwLock::new(ConnectionRegistry::new()));
        let stopped = Arc::new(ShutdownController::new());
        stopped.shutdown();
        assert!(recover_current_tip_until_shutdown(store.clone(), NetworkId::Dev,
            ProtocolParams::default(), registry.clone(), None, stopped).await.unwrap().is_none());
        assert!(recover_current_tip_until_shutdown(store.clone(), NetworkId::Dev,
            ProtocolParams::default(), registry, None, Arc::new(ShutdownController::new())).await.is_err());
        assert_eq!(store.meta_store().get_meta(BASE_KEY).unwrap(), b"corrupt");
    }
}
