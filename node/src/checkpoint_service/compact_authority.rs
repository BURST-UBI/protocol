use super::*;
use crate::confirmed_replay::snapshot::installed::{self, RecoveredSnapshot, ReplayCache};
use burst_consensus::checkpoint::CheckpointAnchor;

#[derive(Clone)]
pub(crate) struct Authority {
    pub base: Arc<RecoveredSnapshot>,
    commitment: Option<installed::BaseCommitment>,
}

impl Authority {
    pub async fn replay_copy(
        &self,
        store: &burst_store_lmdb::LmdbStore,
    ) -> Result<ReplayCache, StoreError> {
        let base = self.base.clone();
        let commitment = self.commitment;
        let directory = store.env().path().to_path_buf();
        tokio::task::spawn_blocking(move || {
            Ok(ReplayCache {
                base: commitment,
                recovered: base.fork(&directory)?,
            })
        })
        .await
        .map_err(|error| StoreError::Corruption(error.to_string()))?
    }
}

/// Recover the exact installed compact authority, not the accepted tip.
/// A previous authority requires forward movement on that authenticated branch.
/// No storage or running worker is changed, including on failed recovery.
pub async fn recover_compact_authority(
    settings: &Settings,
    registry: Arc<RwLock<ConnectionRegistry>>,
    previous: Option<CheckpointAnchor>,
) -> Result<Option<Arc<RecoveredSnapshot>>, StoreError> {
    Ok(recover_authority(settings, registry, previous, None)
        .await?
        .map(|authority| authority.base))
}

pub(crate) async fn recover_authority(
    settings: &Settings,
    registry: Arc<RwLock<ConnectionRegistry>>,
    previous: Option<CheckpointAnchor>,
    mut cached: Option<ReplayCache>,
) -> Result<Option<Authority>, StoreError> {
    loop {
        let revision = settings.store.env().info().last_txn_id;
        let result = recover_once(settings, registry.clone(), previous.clone(), &mut cached).await;
        if result.as_ref().is_err_and(|error| {
            retry_changed_inputs(error, revision, settings.store.env().info().last_txn_id)
        }) {
            // Peer retrieval releases storage locks. Payments and successor
            // installation can invalidate the captured history or journals.
            // Recheck retained replay progress, then reconstruct installed
            // metadata. Partially checked economics never become authority.
            tracing::debug!("compact recovery inputs changed; retrying");
            tokio::task::yield_now().await;
            continue;
        }
        return result;
    }
}

fn retry_changed_inputs(error: &StoreError, before: usize, after: usize) -> bool {
    // A cached observation can predate this attempt's transaction counter.
    // Explicit stale-read evidence requires fresh authentication even when
    // the counter sampled around this attempt has not changed.
    matches!(error, StoreError::StaleRead(_)) || before != after
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_observations_retry_without_hiding_unchanged_corruption() {
        assert!(retry_changed_inputs(
            &StoreError::StaleRead("inventory changed".into()),
            7,
            7
        ));
        assert!(retry_changed_inputs(
            &StoreError::Corruption("changed during read".into()),
            7,
            8
        ));
        assert!(!retry_changed_inputs(
            &StoreError::Corruption("invalid proof".into()),
            7,
            7
        ));
    }
}

async fn recover_once(
    settings: &Settings,
    registry: Arc<RwLock<ConnectionRegistry>>,
    previous: Option<CheckpointAnchor>,
    cached: &mut Option<ReplayCache>,
) -> Result<Option<Authority>, StoreError> {
    let recovered = installed::recover_metadata_retaining_progress(
        settings.store.clone(),
        settings.network,
        settings.initial_params.clone(),
        registry,
        cached,
    )
    .await?;
    let Some(recovered) = recovered else {
        if previous.is_some() {
            return Err(StoreError::Corruption(
                "compact authority base disappeared during rotation".into(),
            ));
        }
        return Ok(None);
    };
    let store = settings.store.clone();
    tokio::task::spawn_blocking(move || {
        recovered
            .into_checkpoint_recovery(&store, previous.as_ref())
            .map(|cache| {
                Some(Authority {
                    base: Arc::new(cache.recovered),
                    commitment: cache.base,
                })
            })
    })
    .await
    .map_err(|error| StoreError::Corruption(error.to_string()))?
}
