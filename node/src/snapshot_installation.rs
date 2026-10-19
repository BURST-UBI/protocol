//! Local, gated control handle for installing an authenticated archive on a
//! running node. This does not expose a remote RPC or authorize history pruning.

use crate::checkpoint_sync::witness::{self, Roots};
use crate::confirmed_replay::snapshot::{archive::AuthenticatedSnapshot, history::HistoricalBlock};
use burst_consensus::checkpoint::CheckpointAnchor;
use burst_store::StoreError;
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, NetworkId, ProtocolParams};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, RwLock, Semaphore};

#[derive(Clone)]
pub struct SnapshotInstaller {
    max_archive_bytes: u64,
    store: Arc<LmdbStore>,
    network: NetworkId,
    params: ProtocolParams,
    registry: Arc<RwLock<crate::ConnectionRegistry>>,
    shutdown: Arc<crate::shutdown::ShutdownController>,
    sender: mpsc::Sender<Request>,
    active: Arc<Semaphore>,
    #[cfg(test)]
    fail_sync: Arc<std::sync::atomic::AtomicBool>,
}

pub(crate) struct Request {
    pub candidate: Prepared,
    pub reply: oneshot::Sender<Result<BlockHash, String>>,
    pub _permit: OwnedSemaphorePermit,
}

pub(crate) struct Prepared {
    snapshot: AuthenticatedSnapshot,
    proofs: Vec<HistoricalBlock>,
    network: NetworkId,
    params: ProtocolParams,
    #[cfg(test)]
    fail_sync: bool,
}

impl Prepared {
    pub(crate) fn anchor(&self) -> &CheckpointAnchor {
        self.snapshot.anchor()
    }

    pub(crate) fn commit(
        self,
        store: &LmdbStore,
    ) -> Result<crate::confirmed_replay::installation::InstalledCheckpoint, StoreError> {
        let projected = self
            .snapshot
            .reconcile_installation(
                store.tx_begin_write()?,
                self.network,
                &self.params,
                &self.proofs,
            )?
            .project_ledger(&[])?
            .project_economics()?;
        #[cfg(test)]
        let projected = projected.with_sync_failure(self.fail_sync);
        projected.commit_runtime(store)
    }
}

impl SnapshotInstaller {
    #[cfg(test)]
    pub(crate) fn fail_next_sync(&self) {
        self.fail_sync
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn channel(
        max_archive_bytes: u64,
        store: Arc<LmdbStore>,
        network: NetworkId,
        params: ProtocolParams,
        registry: Arc<RwLock<crate::ConnectionRegistry>>,
        shutdown: Arc<crate::shutdown::ShutdownController>,
    ) -> (Self, mpsc::Receiver<Request>) {
        let (sender, receiver) = mpsc::channel(1);
        (
            Self {
                max_archive_bytes,
                store,
                network,
                params,
                registry,
                shutdown,
                sender,
                active: Arc::new(Semaphore::new(1)),
                #[cfg(test)]
                fail_sync: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
            receiver,
        )
    }

    /// Prepare proofs without blocking admission, then ask the single ledger
    /// worker to recheck and publish the archive under its normal engine locks.
    /// Cancellation after submission does not undo a commit already in progress.
    pub async fn install(&self, path: PathBuf) -> Result<BlockHash, String> {
        if !cfg!(test) && !burst_verification::checkpoint_selection_enabled(self.network) {
            return Err("live snapshot installation is behind the release gate".into());
        }
        let permit = self
            .active
            .clone()
            .try_acquire_owned()
            .map_err(|_| "snapshot installation is already active")?;
        let mut stop = self.shutdown.subscribe();
        tokio::select! {
            biased;
            _ = stop.recv() => Err("node is shutting down".into()),
            result = self.prepare_and_install(path, permit) => result,
        }
    }

    async fn prepare_and_install(
        &self,
        path: PathBuf,
        permit: OwnedSemaphorePermit,
    ) -> Result<BlockHash, String> {
        let store = self.store.clone();
        let network = self.network;
        let params = self.params.clone();
        let max_archive_bytes = self.max_archive_bytes;
        let (snapshot, roots, wanted, committed, permit) = tokio::task::spawn_blocking(move || {
            let genesis =
                crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(network, &params)
                    .map_err(|error| error.to_string())?;
            let snapshot = AuthenticatedSnapshot::read_file_with_budget(
                &genesis,
                &path,
                store.env().path(),
                max_archive_bytes,
            )
            .map_err(|error| error.to_string())?;
            let needs = snapshot
                .installation_needs(&store)
                .map_err(|error| error.to_string())?;
            let mut wanted: Vec<_> = needs
                .snapshot_heads
                .iter()
                .chain(&needs.cemented_heads)
                .map(|head| head.hash)
                .chain(needs.confirmation_targets)
                .collect();
            wanted.sort_by_key(|hash| *hash.as_bytes());
            wanted.dedup();
            let roots = needs
                .snapshot_heads
                .iter()
                .map(|head| {
                    snapshot
                        .history_cursor(&head.account)
                        .map_err(|error| error.to_string())?
                        .ok_or_else(|| "snapshot account has no authenticated head".to_string())
                })
                .collect::<Result<Vec<_>, String>>()?;
            let committed = snapshot
                .committed_history_cursors(&wanted)
                .map_err(|error| error.to_string())?;
            Ok::<_, String>((snapshot, Roots::Plan(roots), wanted, committed, permit))
        })
        .await
        .map_err(|error| error.to_string())??;
        let proofs = witness::request_with_committed(&self.registry, roots, wanted, committed)
            .await
            .map_err(|error| error.to_string())?;
        let candidate = Prepared {
            snapshot,
            proofs,
            network: self.network,
            params: self.params.clone(),
            #[cfg(test)]
            fail_sync: self
                .fail_sync
                .swap(false, std::sync::atomic::Ordering::AcqRel),
        };
        let (reply, response) = oneshot::channel();
        self.sender
            .try_send(Request {
                candidate,
                reply,
                _permit: permit,
            })
            .map_err(|_| "snapshot installer is unavailable".to_string())?;
        response
            .await
            .map_err(|_| "snapshot installer stopped before replying".to_string())?
    }
}
