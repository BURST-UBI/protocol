//! Called by the single block-processing task between admissions and fork swaps.

use crate::confirmed_replay::installation::PreparedInstallation;
use crate::confirmed_replay::snapshot::installed::{self, PreparedCompactInstallation};
use burst_ledger::DagFrontier;
use burst_store::{meta::MetaStore, StoreError};
use burst_types::{BlockHash, NetworkId, ProtocolParams};
use burst_verification::VerificationOrchestrator;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

enum Prepared {
    Full(Box<PreparedInstallation>),
    Compact(Box<PreparedCompactInstallation>),
    Snapshot(Box<crate::snapshot_installation::Prepared>),
}

impl Prepared {
    fn anchor(&self) -> &burst_consensus::checkpoint::CheckpointAnchor {
        match self {
            Self::Full(candidate) => candidate.anchor(),
            Self::Compact(candidate) => candidate.anchor(),
            Self::Snapshot(candidate) => candidate.anchor(),
        }
    }

    fn commit(
        self,
        store: &burst_store_lmdb::LmdbStore,
    ) -> Result<
        (
            crate::confirmed_replay::installation::InstalledCheckpoint,
            Option<installed::ReplayCache>,
            Option<installed::CommittedProjection>,
        ),
        StoreError,
    > {
        match self {
            Self::Full(candidate) => candidate
                .commit(store)
                .map(|installed| (installed, None, None)),
            Self::Compact(candidate) => candidate
                .commit_with_recovery(store)
                .map(|(installed, cache, projection)| (installed, Some(cache), Some(projection))),
            Self::Snapshot(candidate) => candidate
                .commit(store)
                .map(|installed| (installed, None, None)),
        }
    }
}

pub(crate) struct Runtime {
    pub ledger: crate::fork_resolution::Runtime,
    pub verification: Arc<Mutex<VerificationOrchestrator>>,
    pub governance: Arc<Mutex<burst_governance::GovernanceEngine>>,
    pub delegation: Arc<Mutex<burst_governance::DelegationEngine>>,
    pub constitution: Arc<Mutex<burst_consti::ConstiEngine>>,
    pub network: NetworkId,
    pub initial_params: ProtocolParams,
    pub shutdown: Arc<crate::shutdown::ShutdownController>,
    pub registry: Arc<RwLock<crate::connection_registry::ConnectionRegistry>>,
    pub last_complete: Option<(u64, BlockHash)>,
    pub recovery: Option<installed::ReplayCache>,
    #[cfg(test)]
    pub publication_probe: Option<fn(&str)>,
}

impl Runtime {
    #[cfg(test)]
    fn publication_checkpoint(&self, stage: &str) {
        if let Some(probe) = self.publication_probe {
            probe(stage);
        }
    }

    pub async fn install_next(&mut self) -> Result<Option<ProtocolParams>, StoreError> {
        let tip = crate::checkpoint_store::CertifiedCheckpoint::stored_tip(
            &self.ledger.store,
            self.network,
            &self.initial_params,
        )?;
        let epoch = self.ledger.store.meta_store().payment_epoch()?;
        if self.last_complete == Some((tip, epoch)) {
            return Ok(None);
        }
        let started = std::time::Instant::now();
        tracing::debug!(tip, %epoch, cached = self.recovery.is_some(),
            "checkpoint installation preparation started");
        let store = self.ledger.store.clone();
        let network = self.network;
        let params = self.initial_params.clone();
        let candidate = match installed::recover_metadata_retaining_progress(
            store.clone(),
            network,
            params.clone(),
            self.registry.clone(),
            &mut self.recovery,
        )
        .await?
        {
            Some(metadata) => match metadata
                .prepare_next_with_recovery(store, self.registry.clone())
                .await?
            {
                installed::CompactPreparation::Ready(candidate) => {
                    Some(Prepared::Compact(candidate))
                }
                installed::CompactPreparation::CaughtUp(recovery) => {
                    self.recovery = Some(*recovery);
                    None
                }
            },
            None => tokio::task::spawn_blocking(move || {
                PreparedInstallation::next(&store, network, &params)
                    .map(|candidate| candidate.map(|candidate| Prepared::Full(Box::new(candidate))))
            })
            .await
            .map_err(|error| StoreError::Backend(error.to_string()))??,
        };
        let Some(candidate) = candidate else {
            self.last_complete = Some((tip, epoch));
            tracing::debug!(tip, %epoch, elapsed_ms = started.elapsed().as_millis(),
                "checkpoint installation caught up");
            return Ok(None);
        };
        tracing::debug!(tip, %epoch, elapsed_ms = started.elapsed().as_millis(),
            "checkpoint installation candidate prepared");
        let result = self.publish(candidate, tip).await.map(Some);
        tracing::debug!(tip, %epoch, elapsed_ms = started.elapsed().as_millis(),
            succeeded = result.is_ok(), "checkpoint installation publication finished");
        result
    }

    pub(crate) async fn install_snapshot(
        &mut self,
        candidate: crate::snapshot_installation::Prepared,
    ) -> Result<ProtocolParams, StoreError> {
        let height = candidate.anchor().height();
        self.publish(Prepared::Snapshot(Box::new(candidate)), height)
            .await
    }

    async fn publish(
        &mut self,
        candidate: Prepared,
        tip: u64,
    ) -> Result<ProtocolParams, StoreError> {
        let projection_cache = self.registry.read().await.recovery_projection.clone();
        // Match governance admission: governance -> delegation -> constitution
        // -> BRN -> TRST -> verification. Payment forks take the economic suffix,
        // followed by processor -> frontier -> weights -> scheduler -> elections.
        // No LMDB writer is held while awaiting these publication locks.
        let mut governance = self.governance.lock().await;
        let mut delegation = self.delegation.lock().await;
        let mut constitution = self.constitution.lock().await;
        let mut brn = self.ledger.brn.lock().await;
        let mut trst = self.ledger.trst.lock().await;
        let mut verification = self.verification.lock().await;
        let mut processor = self.ledger.processor.lock().await;
        let mut frontier = self.ledger.frontier.write().await;
        let mut weights = self.ledger.weights.write().await;
        let mut scheduler = self.ledger.scheduler.lock().await;
        let mut elections = self.ledger.elections.write().await;
        let mut backlog = self.ledger.backlog.lock().await;
        let mut broadcaster = self.ledger.broadcaster.lock().await;
        let mut next_weights = burst_consensus::RepWeightCache::new();
        next_weights.rebuild_from_accounts(
            candidate
                .anchor()
                .roster()
                .humans()
                .iter()
                .map(|human| (true, human.representative.clone())),
        );
        let mut next_elections = elections.clone();
        next_elections
            .install_authority(
                candidate.anchor().hash(),
                next_weights.all_weights(),
                candidate.anchor().quorum_bps(),
            )
            .map_err(|error| StoreError::Corruption(error.to_string()))?;
        let previous = elections.authority_epoch();
        #[cfg(test)]
        self.publication_checkpoint("prepared");
        let (installed, recovery, projection) = match candidate.commit(&self.ledger.store) {
            Ok(installed) => installed,
            Err(error) => {
                // A commit followed by a failed sync cannot leave these old
                // engines serving a database that has already changed epochs.
                let durable = self.ledger.store.meta_store().payment_epoch();
                if durable.ok() != previous {
                    self.shutdown.fail("checkpoint installation", &error);
                }
                return Err(error);
            }
        };
        #[cfg(test)]
        self.publication_checkpoint("committed");
        let epoch = installed.anchor.hash();
        let params = installed.governance.params;
        *brn = installed.brn;
        *trst = installed.trst;
        *verification = installed.verification;
        *governance = installed.governance.governance;
        *delegation = installed.governance.delegation;
        *constitution = installed.governance.constitution;
        #[cfg(test)]
        self.publication_checkpoint("economics");
        *weights = next_weights;
        *elections = next_elections;
        #[cfg(test)]
        self.publication_checkpoint("authority");
        *frontier = DagFrontier::new();
        for (address, hash) in installed.frontiers {
            frontier.update(address, hash);
        }
        processor.install_payment_epoch(epoch, installed.anchor.cutoff());
        processor.install_params(&params);
        scheduler.clear();
        backlog.clear();
        broadcaster.clear();
        self.ledger.counts.reset(installed.ledger_counts);
        #[cfg(test)]
        self.publication_checkpoint("complete");
        if installed.anchor.height() == tip {
            self.last_complete = Some((tip, epoch));
        }
        self.recovery = recovery;
        if let Some(projection) = projection {
            projection.publish(&projection_cache, &self.ledger.store)?;
        }
        tracing::info!(%epoch, retired = installed.retired_blocks.len(), minority = installed.minority_promises.len(), retained = installed.retained_promises.len(),
            "installed certified payment state and authority");
        Ok(params)
    }
}
