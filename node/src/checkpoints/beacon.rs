//! Fetch only the exact randomness already frozen by an accepted checkpoint.
//! Relay failure leaves that request pending; it never changes its round or pool.

use std::sync::Arc;
use std::time::Duration;

use burst_store_lmdb::LmdbStore;
use burst_types::{NetworkId, ProtocolParams, Timestamp};
use burst_vrf::DrandClient;

use crate::checkpoint_store::{
    chain::{self, CertifiedEvent},
    CertifiedCheckpoint,
};
use crate::confirmed_replay::snapshot::SelectionSource;
use crate::shutdown::ShutdownController;

enum SelectionQueue {
    Recovered(Box<crate::confirmed_replay::snapshot::installed::ReplayCache>),
    Full(Vec<CertifiedCheckpoint>),
}

impl SelectionSource for SelectionQueue {
    fn len(&self) -> usize {
        match self {
            Self::Recovered(cache) => cache.recovered.selections.len(),
            Self::Full(selections) => selections.len(),
        }
    }

    fn read_selection(
        &self,
        position: usize,
    ) -> Result<std::borrow::Cow<'_, CertifiedCheckpoint>, crate::confirmed_replay::ReplayError>
    {
        match self {
            Self::Recovered(cache) => cache.recovered.selections.read_selection(position),
            Self::Full(selections) => selections.read_selection(position),
        }
    }
}

pub(crate) struct Live {
    pub registry: Arc<tokio::sync::RwLock<crate::connection_registry::ConnectionRegistry>>,
    pub orchestrator: Arc<tokio::sync::Mutex<burst_verification::VerificationOrchestrator>>,
    pub recovery: Option<crate::confirmed_replay::snapshot::installed::ReplayCache>,
}

#[cfg(test)]
impl From<Arc<tokio::sync::Mutex<burst_verification::VerificationOrchestrator>>> for Live {
    fn from(
        orchestrator: Arc<tokio::sync::Mutex<burst_verification::VerificationOrchestrator>>,
    ) -> Self {
        Self {
            registry: Arc::new(tokio::sync::RwLock::new(
                crate::connection_registry::ConnectionRegistry::new(),
            )),
            orchestrator,
            recovery: None,
        }
    }
}

async fn selections(
    store: Arc<LmdbStore>,
    network: NetworkId,
    params: ProtocolParams,
    live: Option<&Live>,
    cached: &mut Option<crate::confirmed_replay::snapshot::installed::ReplayCache>,
) -> Result<SelectionQueue, burst_store::StoreError> {
    if let Some(live) = live {
        let recovered =
            crate::confirmed_replay::snapshot::installed::recover_current_tip_with_cache(
                store.clone(),
                network,
                params.clone(),
                live.registry.clone(),
                cached.take(),
            )
            .await?;
        return Ok(SelectionQueue::Recovered(Box::new(recovered)));
    }
    let source = store.clone();
    let replay_params = params.clone();
    let (base, full) = tokio::task::spawn_blocking(move || {
        let base = crate::confirmed_replay::snapshot::installed::load_base(
            &source,
            network,
            &replay_params,
        )?;
        let full = if base.is_none() {
            selection_checkpoints(chain::load(&source, network, &replay_params)?)
        } else {
            Vec::new()
        };
        Ok::<_, burst_store::StoreError>((base, full))
    })
    .await
    .map_err(|error| burst_store::StoreError::Corruption(error.to_string()))??;
    if base.is_none() {
        return Ok(SelectionQueue::Full(full));
    }
    Err(burst_store::StoreError::Corruption(
        "compact beacon recovery requires the gated peer runtime".into(),
    ))
}

/// Outcomes do not request randomness. Committee publication separately checks
/// the installed payment epoch under its writer, including mixed ancestry.
pub(crate) fn selection_checkpoints(chain: Vec<CertifiedEvent>) -> Vec<CertifiedCheckpoint> {
    chain
        .into_iter()
        .filter_map(|event| match event {
            CertifiedEvent::Selection(value) => Some(*value),
            CertifiedEvent::Outcome(_)
            | CertifiedEvent::Roster(_)
            | CertifiedEvent::Activation(_)
            | CertifiedEvent::Deadline(_)
            | CertifiedEvent::Governance(_) => None,
        })
        .collect()
}

pub(crate) async fn run(
    store: Arc<LmdbStore>,
    network: NetworkId,
    params: ProtocolParams,
    client: Arc<DrandClient>,
    shutdown: Arc<ShutdownController>,
    poll_interval: Duration,
    mut live: Option<Live>,
) {
    let mut stopped = shutdown.subscribe();
    let mut interval = tokio::time::interval(poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut completed_revision = None;
    let mut recovered = live.as_mut().and_then(|live| live.recovery.take());
    loop {
        tokio::select! {
            biased;
            _ = stopped.recv() => return,
            _ = interval.tick() => (),
        }
        let revision = store.tx_begin_read().ok().and_then(|view| view.generation());
        if revision.is_some() && completed_revision == revision {
            continue;
        }
        let chain = tokio::select! {
            biased;
            _ = stopped.recv() => return,
            result = selections(store.clone(), network, params.clone(), live.as_ref(), &mut recovered) => result,
        };
        // A peer worker can close its response channel during the same poll
        // that shutdown is broadcast, after select polled the stop receiver.
        if !matches!(
            stopped.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ) {
            return;
        }
        let chain = match chain {
            Ok(chain) => chain,
            Err(error) => {
                tracing::error!(%error, "cannot replay checkpoint beacon queue");
                shutdown.fail("checkpoint beacon replay", &error);
                return;
            }
        };
        // This includes older decisions whose relay was unavailable when newer
        // checkpoints arrived. Restart reconstructs the same queue from disk.
        let latest = chain.iter_selections().map(|checkpoint| {
                let checkpoint = checkpoint?;
                Ok((
                    checkpoint.replayed().request().target().clone(),
                    checkpoint.replayed().lifecycle(),
                ))
            })
            .collect::<Result<std::collections::HashMap<_, _>, crate::confirmed_replay::ReplayError>>();
        let latest = match latest {
            Ok(latest) => latest,
            Err(error) => {
                shutdown.fail("checkpoint beacon queue read", &error);
                return;
            }
        };
        let mut pending = false;
        for checkpoint in chain.iter_selections() {
            let mut checkpoint = match checkpoint {
                Ok(checkpoint) => checkpoint.into_owned(),
                Err(error) => {
                    shutdown.fail("checkpoint beacon queue read", &error);
                    return;
                }
            };
            if matches!(&chain, SelectionQueue::Recovered(_)) && checkpoint.beacon().is_some() {
                let source = store.clone();
                let replay_params = params.clone();
                let refreshed = tokio::task::spawn_blocking(move || {
                    let beacon = checkpoint.beacon().cloned().expect("checked beacon");
                    checkpoint.attach_beacon(&source, &replay_params, beacon)?;
                    Ok::<_, burst_store::StoreError>(checkpoint)
                })
                .await;
                checkpoint = match refreshed {
                    Ok(Ok(checkpoint)) => checkpoint,
                    Ok(Err(error)) => {
                        shutdown.fail("checkpoint beacon persistence", &error);
                        return;
                    }
                    Err(error) => {
                        shutdown.fail("checkpoint beacon persistence worker", &error);
                        return;
                    }
                };
            }
            // A later certified lifecycle's replay proves completion of the old
            // one. Do not reinstall its committee into the new challenge.
            if latest[checkpoint.replayed().request().target()] != checkpoint.replayed().lifecycle()
            {
                continue;
            }
            if let Some(live) = &live {
                checkpoint = match crate::checkpoint_selection::install(
                    Arc::clone(&store),
                    Arc::clone(&live.orchestrator),
                    checkpoint,
                    params.clone(),
                )
                .await
                {
                    Ok(Some(checkpoint)) => checkpoint,
                    Ok(None) => {
                        pending = true;
                        continue;
                    }
                    Err(error) => {
                        tracing::error!(%error, "cannot install certified selection");
                        shutdown.fail("certified selection installation", &error);
                        return;
                    }
                };
            }
            if checkpoint.beacon().is_some() {
                continue;
            }
            let request = checkpoint.replayed().request();
            let release = match request.beacon_time() {
                Ok(release) => release,
                Err(error) => {
                    tracing::error!(%error, "invalid certified beacon schedule");
                    shutdown.fail("certified beacon schedule", &error);
                    return;
                }
            };
            if Timestamp::now().as_secs() < release {
                pending = true;
                continue;
            }
            let round = request.round();
            let result = tokio::select! {
                biased;
                _ = stopped.recv() => return,
                result = client.fetch_round(round) => result,
            };
            let beacon = match result
                .and_then(|beacon| DrandClient::beacon_to_random_output(&beacon))
            {
                Ok(beacon) => beacon,
                Err(error) => {
                    pending = true;
                    tracing::warn!(round, %error, "certified selection is waiting for its beacon");
                    continue;
                }
            };
            let write_store = Arc::clone(&store);
            let write_params = params.clone();
            let persisted = tokio::task::spawn_blocking(move || {
                // Recheck the certificate and proof at the durable boundary,
                // including a concurrent copy received from another peer.
                checkpoint.attach_beacon(&write_store, &write_params, beacon)?;
                Ok::<_, burst_store::StoreError>(checkpoint)
            })
            .await;
            let checkpoint = match persisted {
                Ok(Ok(checkpoint)) => checkpoint,
                Ok(Err(error)) => {
                    tracing::error!(round, %error, "cannot persist certified checkpoint beacon");
                    shutdown.fail("checkpoint beacon persistence", &error);
                    return;
                }
                Err(error) => {
                    tracing::error!(round, %error, "checkpoint beacon persistence worker failed");
                    shutdown.fail("checkpoint beacon persistence worker", &error);
                    return;
                }
            };
            tracing::info!(round, "certified checkpoint beacon persisted");
            if let Some(live) = &live {
                if let Err(error) = crate::checkpoint_selection::install(
                    Arc::clone(&store),
                    Arc::clone(&live.orchestrator),
                    checkpoint,
                    params.clone(),
                )
                .await
                {
                    tracing::error!(round, %error, "cannot install certified committee");
                    shutdown.fail("certified committee installation", &error);
                    return;
                }
            }
        }
        if let SelectionQueue::Recovered(cache) = chain {
            recovered = Some(*cache);
        }
        // Only skip an unchanged, fully completed queue. Any committed write
        // (including beacon replacement/corruption) forces authenticated replay.
        // Pending requests still retry even when no database write occurs.
        // Environment metadata can precede the snapshot visible to readers.
        // An unknown or changed read cut cannot certify an idle queue.
        if !pending && revision.is_some() {
            let after = store.tx_begin_read().ok().and_then(|view| view.generation());
            if after == revision { completed_revision = revision; }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::meta::MetaStore;

    #[tokio::test]
    async fn corrupt_local_checkpoint_queue_stops_the_node_and_preserves_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(directory.path(), 32, 16 * 1024 * 1024).unwrap());
        let params = ProtocolParams::default();
        let anchor =
            crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &params)
                .unwrap();
        let key = format!(
            "checkpoint_chain/v1/{}/{}/head",
            NetworkId::Dev.as_str(),
            anchor.genesis_hash()
        );
        let corrupt = [1, 2, 3];
        store.meta_store().put_meta(&key, &corrupt).unwrap();
        let shutdown = Arc::new(ShutdownController::new());
        let mut stopped = shutdown.subscribe();
        tokio::time::timeout(
            Duration::from_secs(2),
            run(
                store.clone(),
                NetworkId::Dev,
                params,
                Arc::new(DrandClient::mainnet().unwrap()),
                shutdown.clone(),
                Duration::from_millis(10),
                None,
            ),
        )
        .await
        .unwrap();
        assert!(stopped.try_recv().is_ok());
        assert!(shutdown
            .failure()
            .unwrap()
            .starts_with("checkpoint beacon replay:"));
        assert_eq!(store.meta_store().get_meta(&key).unwrap(), corrupt);
    }

    #[tokio::test]
    async fn committed_changes_invalidate_a_completed_beacon_queue() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(directory.path(), 32, 16 << 20).unwrap());
        let params = ProtocolParams::default();
        let anchor =
            crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &params)
                .unwrap();
        let key = format!("checkpoint_chain/v1/dev/{}/head", anchor.genesis_hash());
        let shutdown = Arc::new(ShutdownController::new());
        let worker = tokio::spawn(run(
            store.clone(),
            NetworkId::Dev,
            params,
            Arc::new(DrandClient::mainnet().unwrap()),
            shutdown.clone(),
            Duration::from_millis(5),
            None,
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(shutdown.failure().is_none());
        let before = store.env().info().last_txn_id;
        store.meta_store().put_meta(&key, &[1, 2, 3]).unwrap();
        assert!(store.env().info().last_txn_id > before);
        tokio::time::timeout(Duration::from_secs(2), worker)
            .await
            .unwrap()
            .unwrap();
        assert!(shutdown
            .failure()
            .unwrap()
            .starts_with("checkpoint beacon replay:"));
        assert_eq!(store.meta_store().get_meta(&key).unwrap(), [1, 2, 3]);
    }
}
