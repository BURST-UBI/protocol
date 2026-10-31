//! Bounded checkpoint-record transfer over authenticated peer connections.
//! Transferred history is authenticated and replayed before archival or import.
//! The coordinated installer publishes the certified cut; ordinary bootstrap
//! validates subsequent blocks under its installed payment epoch.

use std::sync::Arc;
use std::time::{Duration, Instant};

use burst_crypto::blake2b_256;
use burst_store::{meta::MetaStore, StoreError};
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, NetworkId, ProtocolParams};
use tokio::sync::{mpsc, RwLock};

use crate::checkpoint_store::historical::HistoricalAdmissions;
#[cfg(test)]
use burst_messages::checkpoint_sync::MAX_TRANSFER_BYTES;
use crate::checkpoint_store::CertifiedCheckpoint;
use crate::connection_registry::ConnectionRegistry;
use crate::shutdown::ShutdownController;
use crate::wire_message::WireMessage;

pub(crate) mod authority;
mod input;
pub mod snapshot;
pub(crate) mod tip;
pub mod witness;

use burst_messages::checkpoint_sync::CHUNK_BYTES;
pub(crate) const INBOX_CAPACITY: usize = 32;
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
const PEER_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const TRANSFER_TIMEOUT: Duration = Duration::from_secs(120);

pub use burst_messages::checkpoint_sync::CheckpointSyncMessage;

pub(crate) struct Incoming {
    pub peer: String,
    pub message: CheckpointSyncMessage,
}

struct Download {
    peer: String,
    height: u64,
    record_only: bool,
    digest: Option<BlockHash>,
    total: Option<u32>,
    bytes: Vec<u8>,
    started_at: Instant,
    last_progress_at: Instant,
    last_request_at: Instant,
}

impl Download {
    fn request(&self) -> CheckpointSyncMessage {
        CheckpointSyncMessage::Request {
            height: self.height,
            offset: self.bytes.len() as u32,
            digest: self.digest,
            record_only: self.record_only,
        }
    }

    fn append(
        &mut self,
        message: &CheckpointSyncMessage,
        now: Instant,
    ) -> Result<bool, StoreError> {
        let CheckpointSyncMessage::Chunk {
            height,
            offset,
            total,
            digest,
            data,
        } = message
        else {
            return Err(invalid("expected checkpoint chunk"));
        };
        if !message.is_bounded()
            || *height != self.height
            || *offset as usize != self.bytes.len()
            || self.digest.is_some_and(|expected| expected != *digest)
            || self.total.is_some_and(|expected| expected != *total)
        {
            return Err(invalid(
                "checkpoint chunks do not match the requested record",
            ));
        }
        self.digest = Some(*digest);
        self.total = Some(*total);
        // Grow only with bytes actually received, never a peer's advertised size.
        self.bytes.extend_from_slice(data);
        self.last_progress_at = now;
        if self.bytes.len() == *total as usize {
            if BlockHash::new(blake2b_256(&self.bytes)) != *digest {
                return Err(invalid("checkpoint record digest mismatch"));
            }
            return Ok(true);
        }
        Ok(false)
    }
}

struct Export {
    height: u64,
    record_only: bool,
    digest: BlockHash,
    bytes: Vec<u8>,
    loaded_at: Instant,
}

impl Export {
    fn completed(height: u64, record_only: bool, bytes: Vec<u8>) -> Self {
        Self {
            height,
            record_only,
            digest: BlockHash::new(blake2b_256(&bytes)),
            bytes,
            // Export authentication may outlast RETRY_INTERVAL. Queued duplicates must
            // get a full cache interval after that work, not before it.
            loaded_at: Instant::now(),
        }
    }
}

struct Driver {
    tip: tip::Discovery,
    tip_served: tip::ReplyLimit,
    snapshots: Option<snapshot::Transfer>,
    store: Arc<LmdbStore>,
    network: NetworkId,
    params: ProtocolParams,
    download: Option<Download>,
    completed: Option<(u64, Vec<u8>)>,
    awaiting_install: Option<BlockHash>,
    export: Option<Export>,
    export_recovery: crate::checkpoint_store::ExportRecovery,
    next_poll: Instant,
    peer_cursor: usize,
    refresh_heights: std::collections::VecDeque<u64>,
    refresh_cache: Option<(burst_store_lmdb::write_batch::CleanStorageCut, Vec<u64>)>,
    #[cfg(test)]
    refresh_scans: usize,
    archived_refresh: crate::checkpoint_store::ArchivedRefresh,
    selection_refresh: Option<(BlockHash, crate::confirmed_replay::snapshot::installed::SelectionRefresh)>,
    projection: Option<Arc<crate::confirmed_replay::snapshot::installed::ProjectionCache>>,
    refresh: bool,
    serve_window_start: Instant,
    response_attempts: usize,
    load_attempts: usize,
    admissions: Arc<HistoricalAdmissions>,
}

fn invalid(message: impl ToString) -> StoreError {
    StoreError::Corruption(message.to_string())
}

impl Driver {
    #[cfg(test)]
    fn new(
        store: Arc<LmdbStore>,
        network: NetworkId,
        params: ProtocolParams,
        admissions: Arc<HistoricalAdmissions>,
    ) -> Result<Self, StoreError> {
        Ok(Self::authenticated(
            authority::Checked::full(store, network, params)?,
            admissions,
        ))
    }

    fn authenticated(authority: authority::Checked, admissions: Arc<HistoricalAdmissions>) -> Self {
        let (store, network, params) = authority.into_parts();
        let now = Instant::now();
        Self {
            tip: tip::Discovery::new(now),
            tip_served: tip::ReplyLimit::default(),
            snapshots: None,
            store,
            network,
            params,
            download: None,
            completed: None,
            awaiting_install: None,
            export: None,
            export_recovery: Default::default(),
            next_poll: now,
            peer_cursor: 0,
            refresh_heights: Default::default(),
            refresh_cache: None,
            #[cfg(test)]
            refresh_scans: 0,
            archived_refresh: Default::default(),
            selection_refresh: None,
            projection: None,
            refresh: false,
            serve_window_start: now,
            response_attempts: 0,
            load_attempts: 0,
            admissions,
        }
    }

    fn refresh_candidates(&mut self) -> Result<Vec<u64>, StoreError> {
        let before = self.store.tx_begin_read()?.clean_storage_cut();
        if let Some((cut, heights)) = &self.refresh_cache {
            if before.as_ref().is_some_and(|current| cut.same_committed_view(current)) {
                return Ok(heights.clone());
            }
        }
        self.refresh_cache = None;
        #[cfg(test)]
        { self.refresh_scans += 1; }
        let heights = crate::checkpoint_store::selection_refresh_heights(
            &self.store, self.network, &self.params,
        )?;
        let after = self.store.tx_begin_read()?.clean_storage_cut();
        // These are scheduling hints, not import authority. Retain neither an
        // archive nor a reader, and never reuse a discovery spanning a commit.
        if let (Some(before), Some(after)) = (before, after) {
            if before.same_committed_view(&after) {
                self.refresh_cache = Some((after, heights.clone()));
            }
        }
        Ok(heights)
    }

    fn poll_network(
        &mut self,
        peers: Vec<String>,
        now: Instant,
        import_active: bool,
    ) -> Result<Vec<Incoming>, StoreError> {
        let mut replies = Vec::new();
        if let Some(reply) = self.tip.poll(&peers, now) {
            replies.push(reply);
        }
        if import_active {
            return Ok(replies);
        }
        if let Some(snapshots) = &mut self.snapshots {
            if let Some(reply) = snapshots.poll(peers.clone(), now) {
                replies.push(reply);
            }
        }
        replies.extend(self.poll(peers, now)?);
        Ok(replies)
    }

    fn poll(
        &mut self,
        mut peers: Vec<String>,
        now: Instant,
    ) -> Result<Option<Incoming>, StoreError> {
        if let Some(parent) = self.awaiting_install {
            if self.store.meta_store().payment_epoch()? == parent {
                return Ok(None);
            }
            self.awaiting_install = None;
        }
        if let Some(download) = &mut self.download {
            if now.duration_since(download.last_progress_at) >= PEER_TIMEOUT
                || now.duration_since(download.started_at) >= TRANSFER_TIMEOUT
            {
                tracing::debug!(height = download.height, peer = %download.peer,
                    bytes = download.bytes.len(), elapsed_ms = now.duration_since(download.started_at).as_millis(),
                    "checkpoint download timed out");
                self.download = None;
            } else if now.duration_since(download.last_request_at) >= RETRY_INTERVAL {
                download.last_request_at = now;
                return Ok(Some(Incoming {
                    peer: download.peer.clone(),
                    message: download.request(),
                }));
            } else {
                return Ok(None);
            }
        }
        if now < self.next_poll || peers.is_empty() {
            return Ok(None);
        }
        peers.sort();
        let scheduling_started = Instant::now();
        let tip = CertifiedCheckpoint::stored_tip(&self.store, self.network, &self.params)?;
        // Alternate chain extension with bounded refresh of older records so a
        // delayed beacon can arrive after its certificate and after later cuts.
        let height = if self.refresh && tip > 0 {
            if self.refresh_heights.is_empty() {
                self.refresh_heights = self.refresh_candidates()?.into();
                // Finish the current selection cycle before adopting a newer
                // epoch, so frequent installations cannot starve later rows.
                if let Some(projection) = self.projection.as_ref().filter(|_| {
                    self.selection_refresh.as_ref().is_none_or(|(_, cursor)| cursor.at_cycle_start())
                }) {
                    if let Some(cursor) = projection.selection_refresh(
                        &self.store, self.network, self.params.params_hash(),
                        self.selection_refresh.as_ref().map(|(epoch, _)| *epoch),
                    )? {
                        self.selection_refresh = Some(cursor);
                    }
                }
                if let Some((_, cursor)) = &mut self.selection_refresh {
                    if let Some(height) = cursor.next(&self.store)? {
                        if !self.refresh_heights.contains(&height) {
                            self.refresh_heights.push_back(height);
                        }
                    }
                }
                if let Some(height) =
                    self.archived_refresh
                        .next(&self.store, self.network, &self.params)?
                {
                    if !self.refresh_heights.contains(&height) {
                        self.refresh_heights.push_back(height);
                    }
                }
            }
            self.refresh_heights
                .pop_front()
                .unwrap_or_else(|| tip.checked_add(1).unwrap_or(1))
        } else {
            tip.checked_add(1).unwrap_or(1)
        };
        self.refresh = !self.refresh;
        let peer = peers[self.peer_cursor % peers.len()].clone();
        tracing::debug!(tip, height, %peer, elapsed_ms = scheduling_started.elapsed().as_millis(),
            "checkpoint download scheduled");
        self.peer_cursor = self.peer_cursor.wrapping_add(1);
        let download = Download {
            peer: peer.clone(),
            height,
            record_only: true,
            digest: None,
            total: None,
            bytes: Vec::new(),
            started_at: now,
            last_progress_at: now,
            last_request_at: now,
        };
        let message = download.request();
        self.download = Some(download);
        self.next_poll = now + PEER_TIMEOUT;
        Ok(Some(Incoming { peer, message }))
    }

    fn handle(&mut self, incoming: Incoming, now: Instant) -> Result<Option<Incoming>, StoreError> {
        if !incoming.message.is_bounded() {
            return Err(invalid("checkpoint message exceeds transport bounds"));
        }
        match &incoming.message {
            CheckpointSyncMessage::TipRequest { nonce } => {
                if !self.tip_served.allow(&incoming.peer, now) {
                    return Ok(None);
                }
                let height =
                    CertifiedCheckpoint::stored_tip(&self.store, self.network, &self.params)?;
                let epoch = self.store.meta_store().payment_epoch()?;
                if epoch.is_zero() {
                    return Ok(None);
                }
                Ok(Some(Incoming {
                    peer: incoming.peer,
                    message: CheckpointSyncMessage::TipObservation {
                        nonce: *nonce,
                        height,
                        epoch,
                    },
                }))
            }
            CheckpointSyncMessage::TipObservation {
                nonce,
                height,
                epoch,
            } => {
                self.tip
                    .receive(&incoming.peer, *nonce, *height, *epoch, now);
                Ok(None)
            }
            CheckpointSyncMessage::WitnessRequest { .. }
            | CheckpointSyncMessage::WitnessChunk { .. }
            | CheckpointSyncMessage::WitnessHeadsRequest { .. } => {
                Err(invalid("witness traffic requires the proof-only service"))
            }
            CheckpointSyncMessage::SnapshotRequest { .. }
            | CheckpointSyncMessage::SnapshotChunk { .. }
            | CheckpointSyncMessage::SnapshotQuery { .. }
            | CheckpointSyncMessage::SnapshotOffer { .. } => match &mut self.snapshots {
                Some(transfer) => transfer.handle(incoming, now),
                None => Ok(None),
            },
            CheckpointSyncMessage::Request {
                height,
                offset,
                digest,
                record_only,
            } => {
                if now.duration_since(self.serve_window_start) >= RETRY_INTERVAL {
                    self.serve_window_start = now;
                    self.response_attempts = 0;
                    self.load_attempts = 0;
                }
                if self.response_attempts >= 64 {
                    return Ok(None);
                }
                self.response_attempts += 1;
                let reload = self.export.as_ref().is_none_or(|cached| {
                    cached.height != *height
                        || cached.record_only != *record_only
                        || (*offset == 0 && now.duration_since(cached.loaded_at) >= RETRY_INTERVAL)
                });
                if reload {
                    if self.load_attempts >= 4 {
                        return Ok(None);
                    }
                    self.load_attempts += 1;
                    let export_started = Instant::now();
                    tracing::debug!(height, record_only, "checkpoint export started");
                    let Some(bytes) = CertifiedCheckpoint::export_transfer_with_recovery(
                        &self.store,
                        self.network,
                        &self.params,
                        *height,
                        *record_only,
                        &mut self.export_recovery,
                    )?
                    else {
                        tracing::debug!(
                            height,
                            elapsed_ms = export_started.elapsed().as_millis(),
                            "checkpoint export unavailable"
                        );
                        return Ok(None);
                    };
                    tracing::debug!(
                        height,
                        bytes = bytes.len(),
                        elapsed_ms = export_started.elapsed().as_millis(),
                        "checkpoint export finished"
                    );
                    self.export = Some(Export::completed(*height, *record_only, bytes));
                }
                let cached = self
                    .export
                    .as_ref()
                    .ok_or_else(|| invalid("checkpoint export missing"))?;
                if digest.is_some_and(|expected| expected != cached.digest)
                    || *offset as usize >= cached.bytes.len()
                {
                    return Ok(None);
                }
                let start = *offset as usize;
                let end = (start + CHUNK_BYTES).min(cached.bytes.len());
                Ok(Some(Incoming {
                    peer: incoming.peer,
                    message: CheckpointSyncMessage::Chunk {
                        height: *height,
                        offset: *offset,
                        total: cached.bytes.len() as u32,
                        digest: cached.digest,
                        data: cached.bytes[start..end].to_vec(),
                    },
                }))
            }
            CheckpointSyncMessage::Chunk { height, offset, .. } => {
                let Some(active) = &self.download else {
                    return Ok(None);
                };
                if incoming.peer != active.peer
                    || *height != active.height
                    || *offset as usize != active.bytes.len()
                {
                    return Ok(None); // unsolicited or a duplicate response
                }
                let mut download = self
                    .download
                    .take()
                    .ok_or_else(|| invalid("checkpoint download missing"))?;
                self.next_poll = now + PEER_TIMEOUT;
                if download.append(&incoming.message, now)? {
                    tracing::debug!(
                        height,
                        bytes = download.bytes.len(),
                        elapsed_ms = now.duration_since(download.started_at).as_millis(),
                        "checkpoint download complete"
                    );
                    if download.record_only
                        || crate::checkpoint_store::uses_incremental_replay(
                            &self.store,
                            self.network,
                            &self.params,
                        )?
                    {
                        self.completed = Some((download.height, download.bytes));
                        return Ok(None);
                    }
                    if CertifiedCheckpoint::export_transfer(
                        &self.store,
                        self.network,
                        &self.params,
                        download.height,
                    )?
                    .is_some_and(|stored| stored == download.bytes)
                    {
                        self.next_poll = now + RETRY_INTERVAL;
                        return Ok(None);
                    }
                    CertifiedCheckpoint::import_transfer(
                        &self.store,
                        self.network,
                        &self.params,
                        download.height,
                        &download.bytes,
                        &self.admissions,
                    )?;
                    // Successful transfers use the bounded polling interval,
                    // not the timeout reserved for stalled or invalid peers.
                    self.next_poll = now + RETRY_INTERVAL;
                    tracing::debug!(
                        height = download.height,
                        "imported replay-validated checkpoint record"
                    );
                    Ok(None)
                } else {
                    download.last_request_at = now;
                    let reply = Incoming {
                        peer: download.peer.clone(),
                        message: download.request(),
                    };
                    self.download = Some(download);
                    Ok(Some(reply))
                }
            }
        }
    }
}

pub(crate) struct Settings {
    pub network: NetworkId,
    pub params: ProtocolParams,
    pub snapshots: snapshot::Config,
    pub recovery: Option<crate::confirmed_replay::snapshot::installed::ReplayCache>,
}

pub(crate) async fn run(
    store: Arc<LmdbStore>,
    mut settings: Settings,
    registry: Arc<RwLock<ConnectionRegistry>>,
    mut inbox: mpsc::Receiver<Incoming>,
    shutdown: Arc<ShutdownController>,
    admissions: Arc<HistoricalAdmissions>,
) {
    let published = registry.read().await.checkpoint_sync_status.clone();
    witness::start(
        Arc::clone(&store),
        Arc::clone(&registry),
        Arc::clone(&shutdown),
    )
    .await;
    let mut stop = shutdown.subscribe();
    let snapshot_staging = store.env().path().to_path_buf();
    let mut authority = match authority::authenticate_until_shutdown(
        store, settings.network, settings.params.clone(), registry.clone(),
        settings.recovery.take(), shutdown.clone(),
    ).await {
        Ok(Some(authority)) => authority,
        Ok(None) => return,
        Err(error) => {
            shutdown.fail("checkpoint sync authentication", &error);
            return;
        }
    };
    let recovery = authority.take_recovery();
    let projection = registry.read().await.recovery_projection.clone();
    type ImportResult = (
        crate::checkpoint_store::historical::snapshot_transfer::Session,
        u64,
        Result<Option<BlockHash>, StoreError>,
    );
    let mut importing: Option<
        std::pin::Pin<Box<dyn std::future::Future<Output = ImportResult> + Send>>,
    > = None;
    let (mut driver, imports) = match tokio::task::spawn_blocking(move || {
        let base = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            settings.network,
            &settings.params,
        )?;
        let snapshots = snapshot::Transfer::new(settings.snapshots, base, &snapshot_staging)?;
        let mut driver = Driver::authenticated(authority, admissions);
        driver.export_recovery = crate::checkpoint_store::ExportRecovery::from_recovery(
            recovery.as_ref(),
            &driver.store,
            driver.network,
            &driver.params,
        )?;
        driver.selection_refresh = recovery.as_ref().map(|cache| {
            Ok::<_, StoreError>((cache.recovered.continuation.anchor().hash(),
                cache.recovered.selections.refresh_cursor()?))
        }).transpose()?;
        driver.projection = Some(projection);
        driver.snapshots = Some(snapshots);
        let imports =
            crate::checkpoint_store::historical::snapshot_transfer::Session::from_recovery(
                recovery,
            );
        Ok::<_, StoreError>((driver, imports))
    })
    .await
    {
        Ok(Ok(driver)) => driver,
        Ok(Err(error)) => {
            tracing::error!(%error, "checkpoint sync initialization failed");
            shutdown.fail("checkpoint sync initialization", &error);
            return;
        }
        Err(error) => {
            tracing::error!(%error, "checkpoint sync worker failed");
            shutdown.fail("checkpoint sync worker", &error);
            return;
        }
    };
    let mut imports = Some(imports);
    let mut timer = tokio::time::interval(RETRY_INTERVAL);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut scheduler = input::Scheduler::default();
    let (accepted, installed) = {
        let registry = registry.read().await;
        (
            registry.checkpoint_accepted.clone(),
            registry.checkpoint_installed.clone(),
        )
    };
    loop {
        let scheduled = tokio::select! {
            result = async { importing.as_mut().expect("active import").await }, if importing.is_some() => {
                let (session, height, result) = result;
                importing = None;
                imports = Some(session);
                match result {
                    Ok(parent) => {
                        tracing::debug!(height, awaiting_install = ?parent, "checkpoint transfer imported");
                        driver.awaiting_install = parent;
                        if parent.is_some() { accepted.notify_one(); }
                        #[cfg(test)]
                        if let Some(probe) = registry.read().await.recovery_probe {
                            probe("checkpoint_record_accepted");
                        }
                        driver.next_poll = Instant::now();
                        timer.reset_immediately();
                    }
                    Err(error) => {
                        driver.next_poll = Instant::now() + PEER_TIMEOUT;
                        tracing::debug!(%error, height, "compact checkpoint transfer deferred");
                    }
                }
                Ok(None)
            },
            scheduled = scheduler.next(&mut stop, &mut inbox, &mut timer) => scheduled,
            _ = installed.notified() => Ok(None),
        };
        let incoming = match scheduled {
            Ok(incoming) => incoming,
            Err(()) => break,
        };
        let peers = if incoming.is_none() {
            registry
                .read()
                .await
                .peer_ids()
                .into_iter()
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        // One import owns the replay session. Continue serving tip observations
        // while it awaits proofs, but do not download another checkpoint.
        let import_active = importing.is_some();
        let result = tokio::task::spawn_blocking(move || {
            let now = Instant::now();
            let result = match incoming {
                Some(incoming) => driver
                    .handle(incoming, now)
                    .map(|reply| reply.into_iter().collect()),
                None => driver.poll_network(peers, now, import_active),
            };
            (driver, result)
        })
        .await;
        let (returned, reply) = match result {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(%error, "checkpoint sync worker failed");
                shutdown.fail("checkpoint sync worker", &error);
                break;
            }
        };
        driver = returned;
        if let Some((height, bytes)) = driver.completed.take() {
            let mut session = imports.take().expect("one checkpoint import at a time");
            let store = driver.store.clone();
            let network = driver.network;
            let params = driver.params.clone();
            let registry = registry.clone();
            importing = Some(Box::pin(async move {
                let result = session
                    .import(store, network, params, height, bytes, registry)
                    .await;
                (session, height, result)
            }));
        }
        if let (Ok(epoch), Ok(height)) = (
            driver.store.meta_store().payment_epoch(),
            CertifiedCheckpoint::stored_tip(&driver.store, driver.network, &driver.params),
        ) {
            let now = Instant::now();
            published.update(
                driver.tip.expires(now),
                driver
                    .tip
                    .view(now, epoch, height, driver.awaiting_install.is_some()),
            );
        }
        match reply {
            Ok(replies) => {
                for reply in replies {
                    match WireMessage::CheckpointSync(reply.message).encode() {
                        Ok(bytes) => {
                            registry.read().await.send(&reply.peer, bytes);
                        }
                        Err(error) => tracing::error!(%error, "cannot encode checkpoint response"),
                    }
                }
            }
            Err(error) => tracing::debug!(%error, "checkpoint synchronization deferred"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_reuses_unchanged_empty_views_and_never_caches_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
        let mut driver = Driver::new(
            store.clone(), NetworkId::Dev, ProtocolParams::default(), Arc::default(),
        ).unwrap();
        for _ in 0..10 { assert!(driver.refresh_candidates().unwrap().is_empty()); }
        assert_eq!(driver.refresh_scans, 1);

        // Scheduling never reauthenticates economic snapshot pages. Imports still do.
        store.meta_store().put_meta("checkpoint_snapshot/pages/v1/00000000", b"changed").unwrap();
        assert!(driver.refresh_candidates().unwrap().is_empty());
        assert_eq!(driver.refresh_scans, 2);
        store.meta_store().put_meta(
            crate::confirmed_replay::snapshot::installed::BASE_KEY, b"invalid archive",
        ).unwrap();
        assert!(driver.refresh_candidates().unwrap().is_empty());
        assert!(crate::confirmed_replay::snapshot::installed::load_base(
            &store, NetworkId::Dev, &driver.params,
        ).is_err());
        let genesis = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            NetworkId::Dev, &driver.params,
        ).unwrap();
        store.meta_store().put_meta(
            &format!("checkpoint_chain/v1/dev/{}/head", genesis.genesis_hash()),
            &8u64.to_be_bytes(),
        ).unwrap();
        for turn in 0..6 {
            driver.download = None;
            let now = Instant::now();
            driver.next_poll = now;
            let request = driver.poll(vec!["peer".into()], now).unwrap().unwrap();
            assert!(matches!(request.message, CheckpointSyncMessage::Request { height: 9, .. }));
            assert_eq!(driver.refresh, turn % 2 == 0);
        }
        assert_eq!(driver.refresh_scans, 4);
        let head = format!("checkpoint_chain/v1/dev/{}/head", genesis.genesis_hash());
        store.meta_store().put_meta(&head, b"invalid").unwrap();
        for _ in 0..2 {
            assert!(driver.refresh_candidates().is_err());
            assert!(driver.refresh_cache.is_none());
        }
        assert_eq!(driver.refresh_scans, 6);
    }

    #[test]
    fn active_import_keeps_tip_service_live_without_starting_another_download() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
        let params = ProtocolParams::default();
        let epoch = crate::genesis_state::block(NetworkId::Dev, &params).hash;
        store
            .meta_store()
            .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, epoch.as_bytes())
            .unwrap();
        let mut driver = Driver::new(store, NetworkId::Dev, params, Arc::default()).unwrap();
        let now = Instant::now();
        let height =
            CertifiedCheckpoint::stored_tip(&driver.store, driver.network, &driver.params).unwrap();
        let requests = driver.poll_network(vec!["peer".into()], now, true).unwrap();
        assert_eq!(requests.len(), 1);
        let CheckpointSyncMessage::TipRequest { nonce } = requests[0].message else {
            panic!("an import must permit tip polling, not another checkpoint download");
        };
        assert!(driver.download.is_none());
        assert!(driver.completed.is_none());
        driver
            .handle(
                Incoming {
                    peer: "peer".into(),
                    message: CheckpointSyncMessage::TipObservation {
                        nonce,
                        height,
                        epoch,
                    },
                },
                now,
            )
            .unwrap();
        assert_eq!(
            driver.tip.view(now, epoch, height, false)["phase"],
            "peer_matches"
        );
        let reply = driver
            .handle(
                Incoming {
                    peer: "peer".into(),
                    message: CheckpointSyncMessage::TipRequest { nonce: 99 },
                },
                now,
            )
            .unwrap()
            .unwrap();
        assert!(
            matches!(reply.message, CheckpointSyncMessage::TipObservation {
            nonce: 99, epoch: actual, ..
        } if actual == epoch)
        );
        assert!(driver
            .handle(
                Incoming {
                    peer: "peer".into(),
                    message: CheckpointSyncMessage::TipRequest { nonce: 100 }
                },
                now
            )
            .unwrap()
            .is_none());
        let other = driver
            .handle(
                Incoming {
                    peer: "other".into(),
                    message: CheckpointSyncMessage::TipRequest { nonce: 101 },
                },
                now,
            )
            .unwrap()
            .unwrap();
        assert_eq!(other.peer, "other");
        assert!(
            matches!(other.message, CheckpointSyncMessage::TipObservation {
            nonce: 101, epoch: actual, ..
        } if actual == epoch),
            "one peer must not consume another peer's reply allowance"
        );
        let later = now + PEER_TIMEOUT * 4;
        let requests = driver
            .poll_network(vec!["peer".into()], later, true)
            .unwrap();
        assert!(requests
            .iter()
            .all(|request| matches!(request.message, CheckpointSyncMessage::TipRequest { .. })));
        assert!(driver.download.is_none());
        assert_eq!(
            driver.tip.view(later, epoch, height, false)["phase"],
            "unknown",
            "an active import must not extend the lifetime of a peer observation"
        );
        let requests = driver
            .poll_network(vec!["peer".into()], later, false)
            .unwrap();
        assert!(requests
            .iter()
            .any(|request| matches!(request.message, CheckpointSyncMessage::Request { .. })));
        assert!(
            driver.download.is_some(),
            "downloads resume once the import finishes"
        );
    }

    fn download(now: Instant) -> Download {
        Download {
            peer: "peer".into(),
            height: 1,
            record_only: false,
            digest: None,
            total: None,
            bytes: Vec::new(),
            started_at: now,
            last_progress_at: now,
            last_request_at: now,
        }
    }

    #[test]
    fn all_receivers_request_records_and_preserve_the_format_on_continuation() {
        use burst_store::meta::MetaStore;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
        let mut driver = Driver::new(
            store.clone(),
            NetworkId::Dev,
            ProtocolParams::default(),
            Arc::default(),
        )
        .unwrap();
        let now = Instant::now();
        let first = driver.poll(vec!["peer".into()], now).unwrap().unwrap();
        assert!(matches!(
            first.message,
            CheckpointSyncMessage::Request {
                record_only: true,
                ..
            }
        ));
        // Scheduling hint only. Import still authenticates this epoch independently.
        store
            .meta_store()
            .put_meta(
                burst_store::meta::PAYMENT_EPOCH_KEY,
                BlockHash::new([9; 32]).as_bytes(),
            )
            .unwrap();
        driver.download = None;
        driver.next_poll = now;
        let first = driver.poll(vec!["peer".into()], now).unwrap().unwrap();
        assert!(matches!(
            first.message,
            CheckpointSyncMessage::Request {
                record_only: true,
                ..
            }
        ));
        let receiving = driver.download.as_mut().unwrap();
        let digest = BlockHash::new([8; 32]);
        receiving
            .append(
                &CheckpointSyncMessage::Chunk {
                    height: receiving.height,
                    offset: 0,
                    total: (CHUNK_BYTES + 1) as u32,
                    digest,
                    data: vec![0; CHUNK_BYTES],
                },
                now,
            )
            .unwrap();
        assert!(
            matches!(receiving.request(), CheckpointSyncMessage::Request {
            record_only: true, offset, digest: Some(actual), ..
        } if offset == CHUNK_BYTES as u32 && actual == digest)
        );
    }

    #[test]
    fn completed_export_reuses_queued_retries_then_expires() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
        let mut driver = Driver::new(
            store,
            NetworkId::Dev,
            ProtocolParams::default(),
            Arc::default(),
        )
        .unwrap();
        // Simulate a request whose export took longer than the retry interval.
        // No sleeping is needed: completion must stamp its own current time.
        let requested = Instant::now() - RETRY_INTERVAL * 6;
        let bytes = vec![9; 32];
        let cached = Export::completed(1, true, bytes.clone());
        let completed = cached.loaded_at;
        assert!(completed.duration_since(requested) >= RETRY_INTERVAL * 6);
        let digest = cached.digest;
        driver.export = Some(cached);
        let request = || Incoming {
            peer: "peer".into(),
            message: CheckpointSyncMessage::Request {
                height: 1,
                offset: 0,
                digest: None,
                record_only: true,
            },
        };
        for elapsed in [Duration::ZERO, RETRY_INTERVAL / 2] {
            let response = driver
                .handle(request(), completed + elapsed)
                .unwrap()
                .unwrap();
            assert!(matches!(response.message, CheckpointSyncMessage::Chunk {
                data, digest: actual, ..
            } if data == bytes && actual == digest));
            assert_eq!(driver.load_attempts, 0, "queued retry must not repeat export work");
        }
        // An expired first-chunk request still reloads. The empty fixture has
        // no durable record, so it must not serve the old bytes indefinitely.
        assert!(driver
            .handle(request(), completed + RETRY_INTERVAL)
            .unwrap()
            .is_none());
        assert_eq!(driver.load_attempts, 1);
    }

    #[test]
    fn checkpoint_export_cache_does_not_mix_requested_formats() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
        let mut driver = Driver::new(
            store,
            NetworkId::Dev,
            ProtocolParams::default(),
            Arc::default(),
        )
        .unwrap();
        let now = Instant::now();
        for record_only in [false, true] {
            let bytes = vec![if record_only { 3 } else { 2 }; 10];
            let digest = BlockHash::new(blake2b_256(&bytes));
            driver.export = Some(Export {
                height: 1,
                record_only,
                digest,
                bytes: bytes.clone(),
                loaded_at: now,
            });
            let request = |format| Incoming {
                peer: "peer".into(),
                message: CheckpointSyncMessage::Request {
                    height: 1,
                    offset: 0,
                    digest: None,
                    record_only: format,
                },
            };
            let response = driver.handle(request(record_only), now).unwrap().unwrap();
            assert!(
                matches!(response.message, CheckpointSyncMessage::Chunk { data, digest: actual, .. } if data == bytes && actual == digest)
            );
            // No record exists in this store: a format switch must try a new export,
            // not serve the old cached bytes under the new request.
            assert!(driver.handle(request(!record_only), now).unwrap().is_none());
        }
    }

    #[test]
    fn later_heights_keep_transfer_bounds_and_exact_request_matching() {
        let now = Instant::now();
        let bytes = vec![7; CHUNK_BYTES];
        let digest = BlockHash::new(blake2b_256(&bytes));
        for height in [129, 1000, u64::MAX] {
            let request = CheckpointSyncMessage::Request {
                height,
                record_only: false,
                offset: 0,
                digest: None,
            };
            assert!(request.is_bounded());
            let chunk = CheckpointSyncMessage::Chunk {
                height,
                offset: 0,
                total: CHUNK_BYTES as u32,
                digest,
                data: bytes.clone(),
            };
            assert!(chunk.is_bounded());
            let mut receiving = download(now);
            assert!(receiving.append(&chunk, now).is_err());
            assert!(receiving.bytes.is_empty());
            receiving.height = height;
            assert!(receiving.append(&chunk, now).unwrap());
            assert_eq!(receiving.bytes, bytes);
            assert!(!CheckpointSyncMessage::Request {
                height,
                record_only: true,
                offset: MAX_TRANSFER_BYTES as u32,
                digest: Some(digest),
            }
            .is_bounded());
            assert!(!CheckpointSyncMessage::Chunk {
                height,
                offset: 0,
                total: MAX_TRANSFER_BYTES as u32 + 1,
                digest,
                data: bytes.clone(),
            }
            .is_bounded());
        }
        assert!(!CheckpointSyncMessage::Request {
            height: 0,
            record_only: true,
            offset: 0,
            digest: None,
        }
        .is_bounded());
    }

    #[test]
    fn accepted_transfer_waits_for_installation_without_peer_backoff() {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
        let mut driver = Driver::new(
            store.clone(),
            NetworkId::Dev,
            ProtocolParams::default(),
            Arc::default(),
        )
        .unwrap();
        let parent = BlockHash::new([1; 32]);
        store
            .meta_store()
            .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, parent.as_bytes())
            .unwrap();
        let now = Instant::now();
        driver.awaiting_install = Some(parent);
        driver.next_poll = now;
        assert!(driver.poll(vec!["peer".into()], now).unwrap().is_none());
        assert!(driver.download.is_none());
        assert_eq!(driver.next_poll, now);
        store
            .meta_store()
            .put_meta(
                burst_store::meta::PAYMENT_EPOCH_KEY,
                BlockHash::new([2; 32]).as_bytes(),
            )
            .unwrap();
        assert!(driver.poll(vec!["peer".into()], now).unwrap().is_some());
        assert!(driver.awaiting_install.is_none());
    }

    #[test]
    fn polling_advances_later_tips_without_height_overflow() {
        use burst_store::meta::MetaStore;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
        let params = ProtocolParams::default();
        let genesis =
            crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &params)
                .unwrap();
        let mut driver =
            Driver::new(store.clone(), NetworkId::Dev, params, Arc::default()).unwrap();
        let key = format!("checkpoint_chain/v1/dev/{}/head", genesis.genesis_hash());
        // Scheduling only: these synthetic tips do not authorize an import.
        for (tip, expected) in [(128u64, 129u64), (999, 1000), (u64::MAX, 1)] {
            store
                .meta_store()
                .put_meta(&key, &tip.to_be_bytes())
                .unwrap();
            driver.download = None;
            driver.refresh = false;
            let now = Instant::now();
            driver.next_poll = now;
            let request = driver.poll(vec!["peer".into()], now).unwrap().unwrap();
            assert!(
                matches!(request.message, CheckpointSyncMessage::Request { height, .. } if height == expected)
            );
            assert!(driver.download.as_ref().unwrap().bytes.is_empty());
        }
    }

    #[test]
    fn records_larger_than_wire_limit_are_chunked_without_changing_bytes() {
        let now = Instant::now();
        let bytes: Vec<u8> = (0..(burst_types::MAX_P2P_MESSAGE_BYTES * 3 + 7))
            .map(|i| i as u8)
            .collect();
        let digest = BlockHash::new(blake2b_256(&bytes));
        let mut receiving = download(now);
        for (index, data) in bytes.chunks(CHUNK_BYTES).enumerate() {
            let chunk = CheckpointSyncMessage::Chunk {
                height: 1,
                offset: (index * CHUNK_BYTES) as u32,
                total: bytes.len() as u32,
                digest,
                data: data.to_vec(),
            };
            let wire = WireMessage::CheckpointSync(chunk.clone());
            let encoded = wire.encode_framed().unwrap();
            assert!(encoded.len() < burst_types::MAX_P2P_MESSAGE_BYTES);
            let (WireMessage::CheckpointSync(decoded), _): (WireMessage, usize) =
                WireMessage::decode_framed(&encoded).unwrap()
            else {
                panic!("checkpoint frame")
            };
            let complete = receiving.append(&decoded, now).unwrap();
            assert_eq!(complete, (index + 1) * CHUNK_BYTES >= bytes.len());
        }
        assert_eq!(receiving.bytes, bytes);
    }

    #[test]
    fn transfer_rejects_mixed_truncated_oversized_and_forged_records() {
        let now = Instant::now();
        let bytes = vec![7; CHUNK_BYTES + 1];
        let digest = BlockHash::new(blake2b_256(&bytes));
        let first = CheckpointSyncMessage::Chunk {
            height: 1,
            offset: 0,
            total: bytes.len() as u32,
            digest,
            data: bytes[..CHUNK_BYTES].to_vec(),
        };
        for invalid_chunk in [
            CheckpointSyncMessage::Chunk {
                height: 1,
                offset: 0,
                total: MAX_TRANSFER_BYTES as u32 + 1,
                digest,
                data: vec![7; CHUNK_BYTES],
            },
            CheckpointSyncMessage::Chunk {
                height: 1,
                offset: 0,
                total: bytes.len() as u32,
                digest,
                data: vec![7; CHUNK_BYTES - 1],
            },
            CheckpointSyncMessage::Chunk {
                height: 2,
                offset: CHUNK_BYTES as u32,
                total: bytes.len() as u32,
                digest,
                data: vec![7],
            },
            CheckpointSyncMessage::Chunk {
                height: 1,
                offset: CHUNK_BYTES as u32,
                total: bytes.len() as u32,
                digest: BlockHash::ZERO,
                data: vec![7],
            },
            CheckpointSyncMessage::Chunk {
                height: 1,
                offset: CHUNK_BYTES as u32,
                total: bytes.len() as u32,
                digest,
                data: vec![8],
            },
        ] {
            let mut receiving = download(now);
            receiving.append(&first, now).unwrap();
            assert!(receiving.append(&invalid_chunk, now).is_err());
        }
        assert!(!CheckpointSyncMessage::Request {
            height: 1,
            offset: 1,
            record_only: true,
            digest: Some(digest)
        }
        .is_bounded());
        assert!(!CheckpointSyncMessage::Request {
            height: 1,
            offset: CHUNK_BYTES as u32,
            record_only: false,
            digest: None
        }
        .is_bounded());
    }

    #[tokio::test]
    async fn malformed_completed_transfers_keep_the_peer_backoff() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
        let mut driver = Driver::new(
            store,
            NetworkId::Dev,
            ProtocolParams::default(),
            Arc::default(),
        )
        .unwrap();
        let now = Instant::now();
        let peers = vec!["a".into(), "b".into()];
        assert_eq!(driver.poll(peers.clone(), now).unwrap().unwrap().peer, "a");
        let bytes = vec![0];
        assert!(driver
            .handle(
                Incoming {
                    peer: "a".into(),
                    message: CheckpointSyncMessage::Chunk {
                        height: 1,
                        offset: 0,
                        total: 1,
                        digest: BlockHash::new(blake2b_256(&bytes)),
                        data: bytes,
                    },
                },
                now
            )
            .unwrap()
            .is_none());
        let (height, bytes) = driver.completed.take().unwrap();
        assert!(
            crate::checkpoint_store::historical::snapshot_transfer::import(
                driver.store.clone(),
                driver.network,
                driver.params.clone(),
                height,
                bytes,
                Arc::new(RwLock::new(ConnectionRegistry::new())),
            )
            .await
            .is_err()
        );
        assert!(driver.download.is_none());
        assert!(driver.poll(peers.clone(), now + RETRY_INTERVAL).unwrap().is_none());
        assert_eq!(
            driver
                .poll(peers, now + PEER_TIMEOUT)
                .unwrap()
                .unwrap()
                .peer,
            "b"
        );
    }

    #[test]
    fn unsolicited_chunks_cannot_allocate_and_stalled_peers_are_rotated() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
        let mut driver = Driver::new(
            store,
            NetworkId::Dev,
            ProtocolParams::default(),
            Arc::default(),
        )
        .unwrap();
        let now = Instant::now();
        let peers = vec!["a".into(), "b".into()];
        let first = driver.poll(peers.clone(), now).unwrap().unwrap();
        assert_eq!(first.peer, "a");
        let chunk = CheckpointSyncMessage::Chunk {
            height: 1,
            offset: 0,
            total: MAX_TRANSFER_BYTES as u32,
            digest: BlockHash::ZERO,
            data: vec![0; CHUNK_BYTES],
        };
        assert!(driver
            .handle(
                Incoming {
                    peer: "b".into(),
                    message: chunk
                },
                now
            )
            .unwrap()
            .is_none());
        assert!(driver.download.as_ref().unwrap().bytes.is_empty());
        let retry = driver.poll(peers.clone(), now + RETRY_INTERVAL).unwrap().unwrap();
        assert_eq!(retry.peer, "a");
        let next = driver.poll(peers, now + PEER_TIMEOUT).unwrap().unwrap();
        assert_eq!(next.peer, "b");
    }
}
