use super::*;
use crate::confirmed_replay::snapshot::installed;

/// Initialization evidence only. New imports and signing retain their own
/// replay checks; this value cannot authorize either operation.
pub(crate) struct Checked {
    store: Arc<LmdbStore>,
    network: NetworkId,
    params: ProtocolParams,
    recovery: Option<installed::ReplayCache>,
}

impl Checked {
    #[cfg(test)]
    pub(super) fn full(
        store: Arc<LmdbStore>,
        network: NetworkId,
        params: ProtocolParams,
    ) -> Result<Self, StoreError> {
        crate::checkpoint_store::chain::load(&store, network, &params)?;
        Ok(Self {
            store,
            network,
            params,
            recovery: None,
        })
    }

    pub(super) fn into_parts(self) -> (Arc<LmdbStore>, NetworkId, ProtocolParams) {
        (self.store, self.network, self.params)
    }

    pub(crate) fn take_recovery(&mut self) -> Option<installed::ReplayCache> {
        self.recovery.take()
    }
}

/// Initialization authenticates the accepted tip; imports retain their own checks.
#[cfg(test)]
pub(crate) async fn authenticate(
    store: Arc<LmdbStore>,
    network: NetworkId,
    params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
) -> Result<Checked, StoreError> {
    authenticate_with_cache(store, network, params, registry, None).await
}

#[cfg(test)]
pub(crate) async fn authenticate_with_cache(
    store: Arc<LmdbStore>,
    network: NetworkId,
    params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
    cached: Option<installed::ReplayCache>,
) -> Result<Checked, StoreError> {
    let recovery = installed::recover_current_tip_with_cache(
        store.clone(),
        network,
        params.clone(),
        registry,
        cached,
    )
    .await?;
    Ok(Checked {
        store,
        network,
        params,
        recovery: Some(recovery),
    })
}

/// Keep polling until return so requested shutdown can drain the current disk
/// worker. Dropping the future does not wait for cleanup; cancelled work grants
/// no authority.
pub(super) async fn authenticate_until_shutdown(
    store: Arc<LmdbStore>, network: NetworkId, params: ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>, cached: Option<installed::ReplayCache>,
    shutdown: Arc<ShutdownController>,
) -> Result<Option<Checked>, StoreError> {
    let recovery = installed::recover_current_tip_until_shutdown(
        store.clone(), network, params.clone(), registry, cached, shutdown,
    ).await?;
    Ok(recovery.map(|recovery| Checked { store, network, params, recovery: Some(recovery) }))
}
