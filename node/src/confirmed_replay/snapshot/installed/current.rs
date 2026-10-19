use super::*;
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
    mut cached: Option<ReplayCache>,
) -> Result<ReplayCache, StoreError> {
    let mut loaded = load_view(store.clone(), network, params.clone()).await?;
    loop {
        let previous = loaded.as_ref().map(|(_, commitment)| *commitment);
        let result = authenticate_view(
            store.clone(),
            network,
            params.clone(),
            registry.clone(),
            loaded,
            cached.take(),
        )
        .await;
        match result {
            Ok(checked) => return Ok(checked),
            Err(error) => {
                let current = load_view(store.clone(), network, params.clone()).await?;
                let next = current.as_ref().map(|(_, commitment)| *commitment);
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

type LoadedBase = Option<(
    crate::confirmed_replay::snapshot::archive::AuthenticatedSnapshot,
    BaseCommitment,
)>;

async fn load_view(
    source: Arc<LmdbStore>,
    network: NetworkId,
    initial: ProtocolParams,
) -> Result<LoadedBase, StoreError> {
    tokio::task::spawn_blocking(move || {
        let transaction = source.tx_begin_read()?;
        read_base(
            |key| source.meta_store().get_meta_txn(&transaction, key),
            network,
            &initial,
            source.env().path(),
        )
    })
    .await
    .map_err(invalid)?
}

async fn authenticate_view(
    store: Arc<LmdbStore>,
    network: NetworkId,
    params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
    loaded: LoadedBase,
    cached: Option<ReplayCache>,
) -> Result<ReplayCache, StoreError> {
    let commitment = loaded.as_ref().map(|(_, commitment)| *commitment);
    let cached_height = cached
        .as_ref()
        .map(|cache| cache.recovered.continuation.anchor().height());
    let base_matches = cached
        .as_ref()
        .is_some_and(|cache| cache.base == commitment);
    let resumed = match cached.filter(|cache| cache.base == commitment) {
        Some(cache) => match cache
            .recovered
            .advance_target_from_peers(store.clone(), registry.clone(), None)
            .await
        {
            Ok(recovered) => Some(recovered),
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
    let recovered = match (resumed, loaded) {
        (Some(recovered), _) => recovered,
        (None, Some((base, _))) => {
            base.recover_stored_successors_from_peers(store.clone(), registry)
                .await?
        }
        (None, None) => {
            RecoveredSnapshot::recover_genesis_from_peers(
                store.clone(),
                network,
                params.clone(),
                registry,
                None,
            )
            .await?
        }
    };
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
        Ok(ReplayCache {
            base: commitment,
            recovered,
        })
    })
    .await
    .map_err(invalid)?
}
