//! Connection registry — maps peer IDs to their TCP write halves.
//!
//! Shared between the P2P listener (which registers new connections) and
//! the outbound message drain (which writes framed messages to peers).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{mpsc, Mutex, RwLock};

use burst_consensus::{ActiveElections, OnlineWeightSampler, RepWeightCache};
use burst_crypto::{decode_address, verify_signature};
use burst_network::{BandwidthThrottle, MessageDedup, PeerManager, PeerTelemetry};
use burst_store::account::AccountStore;
use burst_store::block::BlockStore;
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, PublicKey, Timestamp};

use crate::bootstrap::{AscPullAckPayload, BootstrapMessage, BootstrapServer};
use crate::metrics::NodeMetrics;
use crate::priority_queue::BlockPriorityQueue;
use crate::wire_message::{TelemetryAckMessage, WireMessage};

mod dial_retry;

/// Maximum message body size (matches protocol codec limit).
const MAX_MESSAGE_SIZE: usize = burst_types::MAX_P2P_MESSAGE_BYTES;

/// Read timeout for peer connections.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Which side opened a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// We dialed the peer.
    Outbound,
    /// The peer dialed us.
    Inbound,
}

/// A live peer connection and its per-connection state — BURST's analogue of
/// rsnano's `Channel`. Consolidates the write half, the per-connection
/// bandwidth throttle, and the identity/dedup metadata that were previously
/// three parallel maps keyed by `peer_id`.
///
/// Peers are identified by their handshake `node_id` (a `brst_` address string)
/// — NOT their IP — so nodes sharing an IP (localhost cluster, or several nodes
/// behind one NAT) are treated as distinct, matching rsnano.
struct Channel {
    /// Bounded outbound queue drained by this channel's own writer task. Sends
    /// use `try_send` (drop-on-full), so a slow or stalled peer backs up only
    /// its own queue and can never head-of-line-block writes to other peers
    /// (the single sequential drain used to do exactly that). Ordering is
    /// preserved (FIFO, one writer per channel).
    outbound_tx: mpsc::Sender<Vec<u8>>,
    node_id: String,
    direction: Direction,
    dial_address: Option<String>,
    /// Monotonic id distinguishing this physical connection from any prior one
    /// under the same peer_id, so a superseded socket's teardown can't evict
    /// the live entry.
    conn_id: u64,
}

/// Outbound queue depth per channel.
const CHANNEL_QUEUE_CAP: usize = 256;

/// Spawn a channel's dedicated writer task. It owns the TCP write half and a
/// per-connection bandwidth throttle, drains the queue in order, and writes
/// length-prefixed frames. It exits — closing the write half — on any write
/// error or when the sender is dropped (channel removed); the peer's read loop
/// then observes the close and cleans up.
fn spawn_channel_writer(
    mut writer: OwnedWriteHalf,
    mut rx: mpsc::Receiver<Vec<u8>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut throttle = BandwidthThrottle::default();
        while let Some(payload) = rx.recv().await {
            if !throttle.try_consume(payload.len() as u64) {
                continue; // throttled — drop this message
            }
            let len = (payload.len() as u32).to_be_bytes();
            if writer.write_all(&len).await.is_err()
                || writer.write_all(&payload).await.is_err()
                || writer.flush().await.is_err()
            {
                break;
            }
        }
    })
}

/// Decide, on a simultaneous-connect conflict, whether the NEW connection
/// should survive (dropping the existing one) or be rejected.
///
/// Deterministic across both endpoints: the surviving connection is always the
/// one initiated by the lower node_id. Both peers compute the same answer, so
/// exactly one connection survives (no churn, no "both drop" partition).
///
/// `keep_new = (our_id < peer_id) == (new is Outbound)`.
pub fn prefer_new_on_conflict(our_node_id: &str, peer_node_id: &str, new_dir: Direction) -> bool {
    (our_node_id < peer_node_id) == (new_dir == Direction::Outbound)
}

/// Registry of active peer TCP write halves, enabling the outbound
/// message drain to route messages to the correct peer stream.
pub struct ConnectionRegistry {
    pub(crate) historical_finality: Arc<crate::historical_finality::HistoricalFinality>,
    closed: bool,
    recovering: bool,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    /// All live channels, keyed by `peer_id`.
    channels: HashMap<String, Channel>,
    next_conn_id: u64,
    /// Addresses currently being dialed (Nano's "attempts list"). Prevents
    /// several internal tasks (bootstrap, peer cache, reachout) from racing to
    /// dial the same peer at once and creating duplicate connections that then
    /// evict each other. Keyed by the dial target `"ip:port"`.
    dialing: HashSet<String>,
    dial_retry: dial_retry::DialRetry,
    confirmation_requests: HashMap<BlockHash, std::time::Instant>,
    checkpoint_inbox: Option<mpsc::Sender<crate::checkpoint_sync::Incoming>>,
    witness_inbox: Option<mpsc::Sender<crate::checkpoint_sync::Incoming>>,
    recovery_snapshot_inbox: Option<mpsc::Sender<crate::checkpoint_sync::Incoming>>,
    witness_requests: Option<mpsc::Sender<crate::checkpoint_sync::witness::Command>>,
    governance_inbox: Option<mpsc::Sender<crate::governance_content::transfer::Incoming>>,
    recovery_content: Option<Arc<crate::governance_content::transfer::Transfers>>,
    pub(crate) checkpoint_accepted: Arc<tokio::sync::Notify>,
    pub(crate) checkpoint_installed: Arc<tokio::sync::Notify>,
    pub(crate) recovery_projection: Arc<crate::confirmed_replay::snapshot::installed::ProjectionCache>,
    pub(crate) checkpoint_sync_status: Arc<crate::checkpoint_sync::tip::Published>,
    #[cfg(test)]
    pub(crate) recovery_probe: Option<fn(&str)>,
    agreement_inbox: Option<mpsc::Sender<crate::checkpoint_transport::Incoming>>,
    #[cfg(test)]
    pub(crate) partitioned_peers: HashSet<String>,
    #[cfg(test)]
    pub(crate) partition_drops: std::sync::atomic::AtomicU64,
}

impl ConnectionRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            historical_finality: Arc::default(),
            closed: false,
            recovering: false,
            tasks: Vec::new(),
            channels: HashMap::new(),
            next_conn_id: 1,
            dialing: HashSet::new(),
            dial_retry: dial_retry::DialRetry::default(),
            confirmation_requests: HashMap::new(),
            checkpoint_inbox: None,
            witness_inbox: None,
            recovery_snapshot_inbox: None,
            witness_requests: None,
            governance_inbox: None,
            recovery_content: None,
            checkpoint_accepted: Arc::default(),
            checkpoint_installed: Arc::default(),
            recovery_projection: Arc::default(),
            checkpoint_sync_status: Arc::default(),
            #[cfg(test)]
            recovery_probe: None,
            agreement_inbox: None,
            #[cfg(test)]
            partitioned_peers: HashSet::new(),
            #[cfg(test)]
            partition_drops: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub(crate) fn for_recovery() -> Self {
        Self {
            recovering: true,
            ..Self::new()
        }
    }

    /// Open ordinary traffic only after startup has published its runtime state.
    /// Closing the registry is terminal; publication cannot reopen it.
    pub(crate) fn finish_recovery(&mut self) {
        if !self.closed {
            self.recovering = false;
        }
    }

    pub(crate) fn is_recovering(&self) -> bool {
        self.recovering && !self.closed
    }

    fn permits(&self, message: &WireMessage) -> bool {
        if self.closed {
            return false;
        }
        if !self.recovering {
            return true;
        }
        match message {
            WireMessage::Handshake(_) | WireMessage::Keepalive(_) => true,
            WireMessage::GovernanceContent(_) => {
                self.governance_inbox
                    .as_ref()
                    .is_some_and(|sender| !sender.is_closed())
                    && self.recovery_content.is_some()
            }
            WireMessage::CheckpointSync(message) => {
                matches!(
                    message,
                    crate::checkpoint_sync::CheckpointSyncMessage::WitnessRequest { .. }
                        | crate::checkpoint_sync::CheckpointSyncMessage::WitnessChunk { .. }
                        | crate::checkpoint_sync::CheckpointSyncMessage::WitnessHeadsRequest { .. }
                ) && message.is_bounded()
                    || (self
                        .recovery_snapshot_inbox
                        .as_ref()
                        .is_some_and(|sender| !sender.is_closed())
                        && crate::checkpoint_sync::snapshot::is_bounded(message))
            }
            _ => false,
        }
    }

    pub(crate) fn track_task(&mut self, task: tokio::task::JoinHandle<()>) {
        self.tasks.retain(|task| !task.is_finished());
        if self.closed {
            task.abort();
        }
        self.tasks.push(task);
    }

    pub(crate) fn set_witness_requests(
        &mut self,
        sender: mpsc::Sender<crate::checkpoint_sync::witness::Command>,
    ) {
        self.witness_requests = Some(sender);
    }

    pub(crate) fn set_witness_inbox(
        &mut self,
        sender: mpsc::Sender<crate::checkpoint_sync::Incoming>,
    ) {
        self.witness_inbox = Some(sender);
    }

    pub(crate) fn witness_requests(
        &self,
    ) -> Option<mpsc::Sender<crate::checkpoint_sync::witness::Command>> {
        self.witness_requests.clone()
    }

    pub(crate) fn close(&mut self) {
        self.closed = true;
        self.channels.clear();
        self.dialing.clear();
        self.dial_retry = dial_retry::DialRetry::default();
        for task in &self.tasks {
            task.abort();
        }
    }

    pub(crate) fn take_tasks(&mut self) -> Vec<tokio::task::JoinHandle<()>> {
        std::mem::take(&mut self.tasks)
    }

    pub(crate) fn set_checkpoint_inbox(
        &mut self,
        sender: mpsc::Sender<crate::checkpoint_sync::Incoming>,
    ) {
        self.checkpoint_inbox = Some(sender);
    }

    #[cfg(test)]
    pub(crate) fn checkpoint_inbox(
        &self,
    ) -> Option<mpsc::Sender<crate::checkpoint_sync::Incoming>> {
        self.checkpoint_inbox.clone()
    }

    pub fn set_agreement_inbox(
        &mut self,
        sender: mpsc::Sender<crate::checkpoint_transport::Incoming>,
    ) {
        self.agreement_inbox = Some(sender);
    }

    pub(crate) fn set_recovery_snapshot_inbox(
        &mut self,
        sender: mpsc::Sender<crate::checkpoint_sync::Incoming>,
    ) -> bool {
        if !self.is_recovering()
            || self
                .recovery_snapshot_inbox
                .as_ref()
                .is_some_and(|sender| !sender.is_closed())
        {
            return false;
        }
        self.recovery_snapshot_inbox = Some(sender);
        true
    }

    pub(crate) fn set_governance_inbox(
        &mut self,
        sender: mpsc::Sender<crate::governance_content::transfer::Incoming>,
    ) {
        self.governance_inbox = Some(sender);
    }

    pub(crate) fn set_recovery_content(
        &mut self,
        transfers: Arc<crate::governance_content::transfer::Transfers>,
    ) {
        self.recovery_content = Some(transfers);
    }

    pub(crate) fn recovery_content(
        &self,
    ) -> Option<Arc<crate::governance_content::transfer::Transfers>> {
        self.recovery_content.clone()
    }

    fn dispatch_agreement(&self, peer: &str, packet: crate::checkpoint_transport::Packet) {
        if !packet.is_bounded() {
            return;
        }
        if let Some(sender) = &self.agreement_inbox {
            let _ = sender.try_send(crate::checkpoint_transport::Incoming {
                peer: peer.to_owned(),
                packet,
            });
        }
    }

    pub(crate) fn dispatch_checkpoint(
        &self,
        peer: &str,
        message: crate::checkpoint_sync::CheckpointSyncMessage,
    ) {
        if !message.is_bounded() {
            return;
        }
        let witness = matches!(
            message,
            crate::checkpoint_sync::CheckpointSyncMessage::WitnessRequest { .. }
                | crate::checkpoint_sync::CheckpointSyncMessage::WitnessChunk { .. }
                | crate::checkpoint_sync::CheckpointSyncMessage::WitnessHeadsRequest { .. }
        );
        let inbox =
            if self.is_recovering() && crate::checkpoint_sync::snapshot::is_bounded(&message) {
                self.recovery_snapshot_inbox.as_ref()
            } else if witness {
                self.witness_inbox
                    .as_ref()
                    .or(self.checkpoint_inbox.as_ref())
            } else {
                self.checkpoint_inbox.as_ref()
            };
        if let Some(sender) = inbox {
            let _ = sender.try_send(crate::checkpoint_sync::Incoming {
                peer: peer.to_owned(),
                message,
            });
        }
    }

    /// Try to claim a dial slot for `addr`. Returns `true` if the caller may
    /// proceed (no other task is dialing it and we're not already connected to
    /// it), `false` if it should abort. Callers MUST pair a `true` with
    /// [`end_dial`](Self::end_dial).
    pub fn begin_dial(&mut self, addr: &str) -> bool {
        if self.closed
            || self.channels.contains_key(addr)
            || self
                .channels
                .values()
                .any(|c| c.dial_address.as_deref() == Some(addr))
        {
            return false; // already have an outbound to this address
        }
        self.dial_retry.ready(addr, std::time::Instant::now())
            && self.dialing.insert(addr.to_string())
    }

    /// Release a dial slot claimed by [`begin_dial`](Self::begin_dial).
    pub fn end_dial(&mut self, addr: &str, success: bool) {
        if self.dialing.remove(addr) {
            self.dial_retry
                .finish(addr, success, std::time::Instant::now());
        }
    }

    /// Register a peer's write half. If a previous connection existed for this
    /// peer, it is replaced (the old writer is dropped, closing its half).
    /// Assigns a fresh generation id (no dedup — for callers that already
    /// resolved identity). Prefer [`register_dedup`](Self::register_dedup).
    pub fn insert(&mut self, peer_id: String, writer: OwnedWriteHalf) -> u64 {
        if self.closed {
            return 0;
        }
        let conn_id = self.next_conn_id;
        self.next_conn_id += 1;
        let (tx, rx) = mpsc::channel(CHANNEL_QUEUE_CAP);
        self.track_task(spawn_channel_writer(writer, rx));
        self.channels.insert(
            peer_id,
            Channel {
                outbound_tx: tx,
                node_id: String::new(),
                direction: Direction::Outbound,
                dial_address: None,
                conn_id,
            },
        );
        conn_id
    }

    /// Register a connection with deterministic simultaneous-connect dedup,
    /// keyed by the peer's handshake `node_id`.
    ///
    /// If a live connection to the same node_id already exists in the opposite
    /// direction, keep exactly one — the one initiated by the lower node_id
    /// (see [`prefer_new_on_conflict`]). Both endpoints agree, so exactly one
    /// survives. Returns `Some(conn_id)` if the new connection is kept (spawn
    /// its read loop with this id) or `None` if it should be rejected.
    pub fn register_dedup(
        &mut self,
        peer_node_id: String,
        direction: Direction,
        writer: OwnedWriteHalf,
        our_node_id: &str,
    ) -> Option<u64> {
        if self.closed {
            return None;
        }
        let peer_id = peer_node_id.clone();
        // Look for any existing live connection to the same node.
        let existing: Option<(String, Direction)> = self
            .channels
            .iter()
            .find(|(_, c)| c.node_id == peer_node_id)
            .map(|(id, c)| (id.clone(), c.direction));

        if let Some((existing_id, existing_dir)) = existing {
            if direction == Direction::Outbound {
                // Even a losing dial verified an endpoint for this identity.
                // Retain it on the surviving inbound channel to avoid redials.
                if let Some(channel) = self.channels.get_mut(&existing_id) {
                    channel.dial_address = writer.peer_addr().ok().map(|a| a.to_string());
                }
            }
            if existing_dir == direction {
                // Same node, same direction — a duplicate dial (bootstrap, peer
                // cache, and reachout can all race to dial the same peer at
                // startup) or a duplicate inbound. Keep the existing one; reject
                // the new. This is the "attempts list" that stops the duplicate
                // from overwriting and dropping the live connection.
                return None;
            }
            // Opposite directions — a genuine simultaneous connect. Keep exactly
            // one, deterministically (the connection from the lower node_id).
            if prefer_new_on_conflict(our_node_id, &peer_node_id, direction) {
                // New wins — drop the existing (its read loop no-ops on cleanup
                // via the generation guard).
                self.remove(&existing_id);
            } else {
                // New loses — reject it (the existing connection survives).
                return None;
            }
        }

        let conn_id = self.next_conn_id;
        self.next_conn_id += 1;
        let dial_address = if direction == Direction::Outbound {
            writer.peer_addr().ok().map(|a| a.to_string())
        } else {
            None
        };
        let (tx, rx) = mpsc::channel(CHANNEL_QUEUE_CAP);
        self.track_task(spawn_channel_writer(writer, rx));
        self.channels.insert(
            peer_id,
            Channel {
                outbound_tx: tx,
                node_id: peer_node_id,
                direction,
                dial_address,
                conn_id,
            },
        );
        Some(conn_id)
    }

    /// Remove a peer's channel. Dropping it drops the outbound sender, which
    /// ends the channel's writer task (closing the write half). Returns whether
    /// a channel was present.
    pub fn remove(&mut self, peer_id: &str) -> bool {
        self.channels.remove(peer_id).is_some()
    }

    /// Generation-guarded removal: only remove if the current entry is the
    /// same physical connection (matching `conn_id`). Prevents a superseded
    /// socket's cleanup from evicting the live connection that replaced it.
    pub fn remove_if(&mut self, peer_id: &str, conn_id: u64) -> bool {
        if self
            .channels
            .get(peer_id)
            .is_some_and(|c| c.conn_id == conn_id)
        {
            self.remove(peer_id);
            true
        } else {
            false
        }
    }

    /// Queue a framed message for a peer (non-blocking). Returns `true` if it
    /// was enqueued, `false` if the peer is unknown or its outbound queue is
    /// full, or the payload exceeds the message limit (drop-on-full
    /// backpressure — a slow peer never stalls others).
    /// Bandwidth throttling and the actual socket write happen in the channel's
    /// writer task, so this never blocks on a slow/stalled peer.
    pub fn send(&self, peer_id: &str, payload: Vec<u8>) -> bool {
        #[cfg(test)]
        if self.partition_drops_peer(peer_id) {
            return false;
        }
        if payload.len() > MAX_MESSAGE_SIZE {
            return false;
        }
        if self.closed
            || (self.recovering
                && !WireMessage::decode(&payload).is_ok_and(|message| self.permits(&message)))
        {
            return false;
        }
        match self.channels.get(peer_id) {
            Some(c) => c.outbound_tx.try_send(payload).is_ok(),
            None => false,
        }
    }

    /// A bootstrap peer need not be every requested block's representative.
    /// Relay known hashes with a global, bounded retry window to stop cycles.
    fn forward_confirmation_request(
        &mut self,
        source: &str,
        hashes: &[BlockHash],
        remaining_hops: u8,
        now: std::time::Instant,
    ) -> usize {
        const WINDOW: Duration = Duration::from_secs(2);
        const CAPACITY: usize = 4096;
        if remaining_hops == 0 || remaining_hops > 3 {
            return 0;
        }
        self.confirmation_requests
            .retain(|_, seen| now.duration_since(*seen) < WINDOW);
        let mut pending = Vec::new();
        for hash in hashes.iter().take(8) {
            if self.confirmation_requests.len() >= CAPACITY {
                break;
            }
            if let std::collections::hash_map::Entry::Vacant(entry) =
                self.confirmation_requests.entry(*hash)
            {
                entry.insert(now);
                pending.push(*hash);
            }
        }
        if pending.is_empty() {
            return 0;
        }
        let request = WireMessage::ConfirmReq(crate::wire_message::ConfirmReqMsg {
            block_hashes: pending,
            remaining_hops: remaining_hops - 1,
        });
        let Ok(bytes) = request.encode() else {
            return 0;
        };
        self.channels
            .keys()
            .filter(|peer| peer.as_str() != source)
            .filter(|peer| self.send(peer, bytes.clone()))
            .count()
    }

    /// Transport permission only. Historical replies may be signed by a
    /// representative whose delegation has since changed. Their weight is
    /// still resolved independently against the receiving election's roster.
    fn requested_vote(&self, vote: &crate::wire_message::WireVote) -> bool {
        !vote.block_hashes.is_empty()
            && vote.block_hashes.iter().all(|hash| {
                self.confirmation_requests
                    .get(hash)
                    .is_some_and(|sent| sent.elapsed() < Duration::from_secs(2))
            })
    }

    /// Number of registered connections.
    pub fn len(&self) -> usize {
        self.channels.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// All registered peer IDs.
    pub fn peer_ids(&self) -> Vec<&String> {
        self.channels.keys().collect()
    }

    // Drop before decoding/deduplication, in both directions and across
    // reconnects. Fixture partitions must cover bootstrap and certificates too.
    #[cfg(test)]
    fn partition_drops_peer(&self, peer_id: &str) -> bool {
        if !self.partitioned_peers.contains(peer_id) {
            return false;
        }
        self.partition_drops
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        true
    }
}

impl Default for ConnectionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn a background task that reads framed messages from a peer's read
/// half, deserializes them as [`WireMessage`]s, and dispatches to the
/// appropriate subsystem.
///
/// On read error or disconnect the peer is cleaned up from both the
/// connection registry and the peer manager.
#[allow(clippy::too_many_arguments)]
pub fn spawn_peer_read_loop(
    peer_id: String,
    conn_id: u64,
    reader: OwnedReadHalf,
    block_queue: Arc<BlockPriorityQueue>,
    connection_registry: Arc<RwLock<ConnectionRegistry>>,
    peer_manager: Arc<RwLock<PeerManager>>,
    metrics: Arc<NodeMetrics>,
    active_elections: Arc<RwLock<ActiveElections>>,
    rep_weights: Arc<RwLock<RepWeightCache>>,
    message_dedup: Arc<Mutex<MessageDedup>>,
    online_weight_sampler: Arc<Mutex<OnlineWeightSampler>>,
    store: Arc<LmdbStore>,
    our_params_hash: burst_types::BlockHash,
    bootstrap_feedback: mpsc::Sender<crate::bootstrap::BootstrapFeedback>,
    vote_generator: Arc<Mutex<burst_consensus::VoteGenerator>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Immediately gossip our known peers to this freshly-connected peer so
        // the mesh converges in seconds instead of waiting for the periodic
        // keepalive cycle (rsnano sends a keepalive on connect). Covers every
        // connect path — inbound and outbound both spawn this loop after the
        // connection is registered, so `registry.get(peer_id)` finds the writer.
        {
            let self_peers: Vec<String> = {
                let pm = peer_manager.read().await;
                pm.random_peers_with_self(8)
                    .iter()
                    .map(|a| format!("{}:{}", a.ip, a.port))
                    .collect()
            };
            let msg =
                crate::wire_message::WireMessage::Keepalive(crate::wire_message::KeepaliveMsg {
                    peers: self_peers,
                });
            if let Ok(bytes) = msg.encode() {
                let _ = connection_registry.read().await.send(&peer_id, bytes);
            }
        }

        let result = peer_read_loop(
            &peer_id,
            reader,
            &block_queue,
            &active_elections,
            &rep_weights,
            &peer_manager,
            &message_dedup,
            &online_weight_sampler,
            &connection_registry,
            &store,
            our_params_hash,
            &bootstrap_feedback,
            &vote_generator,
        )
        .await;
        match &result {
            Ok(()) => {
                tracing::info!(peer = %peer_id, "peer disconnected (clean close)");
            }
            Err(e) => {
                tracing::warn!(peer = %peer_id, error = %e, "peer disconnected with error");
            }
        }

        // Clean up connection registry — generation-guarded so a superseded
        // socket (replaced by dedup) doesn't evict the live connection.
        {
            // Match registration's lock order and keep the generation check
            // atomic with the metadata update. Otherwise a reconnect can land
            // between remove_if and mark_disconnected and be marked offline.
            let mut pm = peer_manager.write().await;
            let mut registry = connection_registry.write().await;
            let removed = registry.remove_if(&peer_id, conn_id);
            metrics.peer_count.set(registry.len() as i64);
            if removed {
                pm.mark_disconnected(&peer_id);
            }
        }

        tracing::debug!(peer = %peer_id, "peer cleaned up after disconnect");
    })
}

/// Inner read loop: reads length-prefixed frames and dispatches them.
///
/// Integrates message deduplication, peer reputation rewards, and online
/// weight sampling for effective quorum computation.
#[allow(clippy::too_many_arguments)]
async fn peer_read_loop(
    peer_id: &str,
    mut reader: OwnedReadHalf,
    block_queue: &BlockPriorityQueue,
    active_elections: &RwLock<ActiveElections>,
    rep_weights: &RwLock<RepWeightCache>,
    peer_manager: &RwLock<PeerManager>,
    message_dedup: &Mutex<MessageDedup>,
    online_weight_sampler: &Mutex<OnlineWeightSampler>,
    connection_registry: &RwLock<ConnectionRegistry>,
    store: &LmdbStore,
    our_params_hash: burst_types::BlockHash,
    bootstrap_feedback: &mpsc::Sender<crate::bootstrap::BootstrapFeedback>,
    vote_generator: &Mutex<burst_consensus::VoteGenerator>,
) -> Result<(), std::io::Error> {
    let mut last_vote_request = None;
    loop {
        // Read the 4-byte length prefix with a timeout
        let mut len_buf = [0u8; 4];
        match tokio::time::timeout(READ_TIMEOUT, reader.read_exact(&mut len_buf)).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "read timeout (30s idle)",
                ));
            }
        }

        let body_len = u32::from_be_bytes(len_buf) as usize;
        if body_len > MAX_MESSAGE_SIZE {
            tracing::warn!(
                peer = %peer_id,
                size = body_len,
                "peer sent oversized message, disconnecting"
            );
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("message too large: {body_len} > {MAX_MESSAGE_SIZE}"),
            ));
        }

        // Read the message body
        let mut body = vec![0u8; body_len];
        match tokio::time::timeout(READ_TIMEOUT, reader.read_exact(&mut body)).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "read timeout waiting for message body",
                ));
            }
        }

        #[cfg(test)]
        if connection_registry
            .read()
            .await
            .partition_drops_peer(peer_id)
        {
            continue;
        }
        let message = WireMessage::decode(&body);
        if let Ok(message) = &message {
            if !connection_registry.read().await.permits(message) {
                continue;
            }
        }
        // Only gossip is globally deduplicated. Request IDs are scoped to a
        // requesting peer, and periodic telemetry/keepalives may be identical.
        // Dropping those across peers prevents new nodes from bootstrapping.
        if message.as_ref().is_ok_and(is_deduplicated_gossip) {
            let msg_hash = MessageDedup::hash_message(&body);
            let mut dedup = message_dedup.lock().await;
            // Admission may defer a block until its committee or dependencies
            // arrive. Suppress immediate loops, not later identical retries.
            if dedup.is_duplicate_for(&msg_hash, Duration::from_secs(1), std::time::Instant::now()) {
                tracing::trace!(
                    peer = %peer_id,
                    "dropped duplicate message"
                );
                continue;
            }
        }

        // Update last-seen timestamp for idle detection.
        {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let mut pm = peer_manager.write().await;
            pm.touch(peer_id, now);
        }

        // Try to deserialize as a WireMessage (the canonical P2P envelope).
        match message {
            Ok(WireMessage::Block(block)) => {
                tracing::debug!(
                    peer = %peer_id,
                    hash = %block.hash,
                    "received block from peer"
                );
                if block_queue.push(*block).await {
                    // Reward peer for delivering a new block (+1 reputation).
                    let mut pm = peer_manager.write().await;
                    pm.reward(peer_id, 1);
                } else {
                    tracing::warn!(
                        peer = %peer_id,
                        "block priority queue full, dropping block"
                    );
                }
            }
            Ok(WireMessage::Vote(vote)) => {
                if !is_vote_signature_valid(&vote) {
                    continue;
                }
                {
                    let rw = rep_weights.read().await;
                    if rw.weight(&vote.voter) == 0
                        && !active_elections
                            .read()
                            .await
                            .has_authorized_voter(&vote.voter)
                        && !connection_registry.read().await.requested_vote(&vote)
                    {
                        tracing::trace!(voter = %vote.voter, "ignoring zero-weight vote");
                        continue;
                    }
                }
                {
                    let now = unix_now_secs();
                    let mut sampler = online_weight_sampler.lock().await;
                    sampler.record_vote(&vote.voter, now);
                }
                relay_vote(peer_id, &vote, connection_registry, message_dedup).await;
                let historical = connection_registry.read().await.historical_finality.clone();
                historical.receive_and_enqueue(&vote, block_queue).await;
                dispatch_vote(peer_id, &vote, active_elections, rep_weights).await;
            }
            Ok(WireMessage::ConfirmReq(req)) => {
                let now = std::time::Instant::now();
                if req.block_hashes.is_empty()
                    || req.block_hashes.len() > 8
                    || req.remaining_hops > 3
                    || last_vote_request
                        .is_some_and(|last| now.duration_since(last) < Duration::from_secs(1))
                {
                    continue;
                }
                last_vote_request = Some(now);
                let known: Vec<_> = req
                    .block_hashes
                    .iter()
                    .copied()
                    .filter(|hash| store.block_store().get_block(hash).is_ok())
                    .collect();
                connection_registry
                    .write()
                    .await
                    .forward_confirmation_request(peer_id, &known, req.remaining_hops, now);
                for hash in req.block_hashes {
                    let vote = {
                        let mut generator = vote_generator.lock().await;
                        crate::durable_votes::respond_vote(store, &mut generator, hash)
                    };
                    match vote {
                        Ok(Some(vote)) => {
                            tracing::debug!(peer = %peer_id, %hash, voter = %vote.voter, is_final = vote.is_final, "replying with reserved consensus vote");
                            let reply =
                                WireMessage::ConfirmAck(crate::wire_message::ConfirmAckMsg {
                                    vote: crate::wire_message::WireVote {
                                        voter: vote.voter,
                                        block_hashes: vec![vote.block_hash],
                                        is_final: vote.is_final,
                                        timestamp: vote.timestamp,
                                        sequence: vote.sequence,
                                        signature: vote.signature,
                                    },
                                });
                            if let Ok(bytes) = reply.encode() {
                                connection_registry.read().await.send(peer_id, bytes);
                            }
                        }
                        Ok(None) => {}
                        Err(error) => tracing::debug!(%hash, %error, "vote request declined"),
                    }
                }
            }
            Ok(WireMessage::ConfirmAck(ack)) => {
                if !is_vote_signature_valid(&ack.vote) {
                    continue;
                }
                {
                    let rw = rep_weights.read().await;
                    if rw.weight(&ack.vote.voter) == 0
                        && !active_elections
                            .read()
                            .await
                            .has_authorized_voter(&ack.vote.voter)
                        && !connection_registry.read().await.requested_vote(&ack.vote)
                    {
                        tracing::trace!(voter = %ack.vote.voter, "ignoring zero-weight confirm_ack");
                        continue;
                    }
                }
                {
                    let now = unix_now_secs();
                    let mut sampler = online_weight_sampler.lock().await;
                    sampler.record_vote(&ack.vote.voter, now);
                }
                relay_vote(peer_id, &ack.vote, connection_registry, message_dedup).await;
                let historical = connection_registry.read().await.historical_finality.clone();
                historical.receive_and_enqueue(&ack.vote, block_queue).await;
                dispatch_vote(peer_id, &ack.vote, active_elections, rep_weights).await;
            }
            Ok(WireMessage::Keepalive(ka)) => {
                tracing::trace!(
                    peer = %peer_id,
                    peers = ka.peers.len(),
                    "received keepalive"
                );
                if !ka.peers.is_empty() {
                    let mut parsed: Vec<burst_messages::PeerAddress> =
                        Vec::with_capacity(ka.peers.len());
                    for addr_str in &ka.peers {
                        let parts: Vec<&str> = addr_str.rsplitn(2, ':').collect();
                        if parts.len() == 2 {
                            if let Ok(port) = parts[0].parse::<u16>() {
                                let ip = parts[1].to_string();
                                parsed.push(burst_messages::PeerAddress { ip, port });
                            }
                        }
                    }

                    // Slot 0 is the sender's self-advertised address (peering_addr).
                    // Store it so keepalive messages use the NAT-traversed address.
                    let peering = parsed.first().cloned();

                    let mut pm = peer_manager.write().await;
                    pm.process_keepalive(parsed);
                    if let Some(pa) = peering {
                        pm.set_peering_addr(peer_id, pa);
                    }
                }
            }
            Ok(WireMessage::Bootstrap(msg)) => match msg {
                BootstrapMessage::AscPullReq { id, payload } => {
                    let account_store = store.account_store();
                    let resp = BootstrapServer::handle_asc_pull_req(
                        id,
                        &payload,
                        |account, start, count| match crate::bootstrap::stored_blocks_after(
                            store, account, start, count,
                        ) {
                            Ok(blocks) => blocks,
                            Err(error) => {
                                tracing::error!(%account, %start, %error, "cannot serve bootstrap block history");
                                Vec::new()
                            }
                        },
                        |account| match account_store.get_account(account) {
                            Ok(info) => (info.head, info.block_count),
                            Err(_) => (BlockHash::ZERO, 0),
                        },
                        |start_account, count| match account_store
                            .iter_accounts_paged(Some(start_account), count as usize)
                        {
                            Ok(accts) => accts.into_iter().map(|a| (a.address, a.head)).collect(),
                            Err(_) => Vec::new(),
                        },
                    );
                    if let Ok(bytes) = WireMessage::Bootstrap(resp).encode() {
                        connection_registry.read().await.send(peer_id, bytes);
                    }
                }
                BootstrapMessage::AscPullAck { id, payload } => match payload {
                    AscPullAckPayload::Blocks(blocks) => {
                        if blocks.len() <= crate::bootstrap::ASC_PULL_MAX_BLOCKS as usize {
                            let _ = bootstrap_feedback.try_send(
                                crate::bootstrap::BootstrapFeedback::Blocks {
                                    peer_id: peer_id.to_owned(),
                                    id,
                                    blocks,
                                },
                            );
                        }
                    }
                    AscPullAckPayload::Frontiers(entries) => {
                        if entries.len() <= crate::bootstrap::ASC_PULL_MAX_FRONTIERS as usize {
                            let _ = bootstrap_feedback.try_send(
                                crate::bootstrap::BootstrapFeedback::Frontiers {
                                    peer_id: peer_id.to_owned(),
                                    id,
                                    entries,
                                },
                            );
                        }
                    }
                    AscPullAckPayload::AccountInfo { .. } => {
                        tracing::trace!(peer = %peer_id, "received asc-pull account-info ack");
                    }
                },
            },
            Ok(WireMessage::Handshake(hs)) => {
                tracing::debug!(
                    peer = %peer_id,
                    node_id = %hs.node_id,
                    "received handshake"
                );
            }
            Ok(WireMessage::VerificationRequest(msg)) => {
                tracing::debug!(
                    peer = %peer_id,
                    target = %msg.target,
                    endorser = %msg.endorser,
                    "received verification request"
                );
            }
            Ok(WireMessage::CheckpointSync(message)) => {
                connection_registry
                    .read()
                    .await
                    .dispatch_checkpoint(peer_id, message);
            }
            Ok(WireMessage::CheckpointAgreement(packet)) => {
                connection_registry
                    .read()
                    .await
                    .dispatch_agreement(peer_id, packet);
            }
            Ok(WireMessage::VerificationVote(msg)) => {
                tracing::debug!(
                    peer = %peer_id,
                    target = %msg.target,
                    voter = %msg.voter,
                    "received verification vote"
                );
            }
            Ok(WireMessage::GovernanceContent(packet)) => {
                if let Some(sender) = &connection_registry.read().await.governance_inbox {
                    let _ = sender.try_send(crate::governance_content::transfer::Incoming {
                        peer: peer_id.to_owned(),
                        packet,
                    });
                }
            }
            Ok(WireMessage::GovernanceProposal(msg)) => {
                tracing::debug!(
                    peer = %peer_id,
                    proposal = %msg.proposal_hash,
                    proposer = %msg.proposer,
                    "received governance proposal"
                );
            }
            Ok(WireMessage::GovernanceVote(msg)) => {
                tracing::debug!(
                    peer = %peer_id,
                    proposal = %msg.proposal_hash,
                    voter = %msg.voter,
                    "received governance vote"
                );
            }
            Ok(WireMessage::TelemetryReq) => {
                tracing::trace!(
                    peer = %peer_id,
                    "received telemetry request, sending response"
                );
                let block_count = store.block_store().block_count().unwrap_or(0);
                let account_count = store.account_store().account_count().unwrap_or(0);
                let peer_count = {
                    let pm = peer_manager.read().await;
                    pm.connected_count() as u32
                };

                let ack = WireMessage::TelemetryAck(TelemetryAckMessage {
                    block_count,
                    cemented_count: 0,
                    unchecked_count: 0,
                    account_count,
                    bandwidth_cap: 0,
                    peer_count,
                    protocol_version: burst_types::PROTOCOL_VERSION as u8,
                    uptime: 0,
                    genesis_hash: burst_types::BlockHash::ZERO,
                    major_version: 0,
                    minor_version: 1,
                    patch_version: 0,
                    timestamp: unix_now_secs(),
                    params_hash: our_params_hash,
                });
                if let Ok(bytes) = ack.encode() {
                    let registry = connection_registry.read().await;
                    registry.send(peer_id, bytes);
                }
            }
            Ok(WireMessage::TelemetryAck(msg)) => {
                tracing::trace!(
                    peer = %peer_id,
                    peer_count = msg.peer_count,
                    blocks = msg.block_count,
                    version = format!("{}.{}.{}", msg.major_version, msg.minor_version, msg.patch_version),
                    "received telemetry from peer"
                );
                let mut pm = peer_manager.write().await;
                pm.update_telemetry(
                    peer_id,
                    PeerTelemetry {
                        block_count: msg.block_count,
                        cemented_count: msg.cemented_count,
                        account_count: msg.account_count,
                        peer_count: msg.peer_count,
                        protocol_version: msg.protocol_version,
                        uptime: msg.uptime,
                        major_version: msg.major_version,
                        minor_version: msg.minor_version,
                        patch_version: msg.patch_version,
                        timestamp: msg.timestamp,
                    },
                );
            }
            Err(_) => {
                tracing::trace!(
                    peer = %peer_id,
                    body_len = body.len(),
                    "failed to deserialize wire message, dropping"
                );
            }
        }
    }
}

/// Verify the Ed25519 signature on a wire vote.
///
/// Uses the domain-separated payload shared with the vote generator.
pub(crate) fn is_vote_signature_valid(vote: &crate::wire_message::WireVote) -> bool {
    if vote.block_hashes.is_empty() || vote.block_hashes.len() > 8 {
        return false;
    }
    let pubkey_bytes = match decode_address(vote.voter.as_str()) {
        Some(bytes) => bytes,
        None => {
            tracing::warn!(voter = %vote.voter, "rejected vote: unable to decode voter address");
            return false;
        }
    };

    // Reconstruct the SAME canonical payload the signer used (domain-tagged and
    // bound to the voter identity) so signer and verifier can never diverge.
    let msg = burst_consensus::vote_signing_payload(
        &vote.voter,
        vote.is_final,
        vote.timestamp,
        vote.sequence,
        &vote.block_hashes,
    );

    let public_key = PublicKey(pubkey_bytes);
    if !verify_signature(&msg, &vote.signature, &public_key) {
        tracing::warn!(voter = %vote.voter, "rejected vote with invalid signature");
        return false;
    }
    true
}

/// Route a received vote (from Vote or ConfirmAck) to active elections.
async fn dispatch_vote(
    peer_id: &str,
    vote: &crate::wire_message::WireVote,
    active_elections: &RwLock<ActiveElections>,
    rep_weights: &RwLock<RepWeightCache>,
) {
    let weight = {
        let rw = rep_weights.read().await;
        rw.weight(&vote.voter)
    };
    let now = Timestamp::new(unix_now_secs());
    let mut ae = active_elections.write().await;
    for block_hash in &vote.block_hashes {
        let Some(root) = ae.candidate_root(block_hash) else {
            tracing::trace!(peer = %peer_id, hash = %block_hash, "vote for unregistered candidate");
            continue;
        };
        match ae.process_signed_vote(
            &root,
            burst_consensus::VoteInfo::new(
                vote.voter.clone(),
                *block_hash,
                weight,
                vote.is_final,
                Timestamp::new(vote.timestamp / 1000),
                vote.sequence,
            ),
            now,
        ) {
            Ok(Some(status)) => {
                tracing::info!(
                    peer = %peer_id,
                    winner = %status.winner,
                    tally = status.tally,
                    "election confirmed by incoming vote"
                );
            }
            Ok(None) => {
                if let Some(election) = ae.get_election(&root) {
                    tracing::debug!(hash = %block_hash, voter = %vote.voter, is_final = vote.is_final,
                        threshold = election.confirmation_threshold(), final_tally = ?election.final_tally,
                        "vote did not complete quorum");
                }
            }
            Err(e) => {
                tracing::trace!(
                    peer = %peer_id,
                    hash = %block_hash,
                    error = %e,
                    "vote routing failed"
                );
            }
        }
    }
}

/// Helper: current UNIX timestamp in seconds.
fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn is_deduplicated_gossip(message: &WireMessage) -> bool {
    matches!(message, WireMessage::Block(_))
}

/// Relay authenticated votes across non-mesh topologies. Normalize acknowledgments
/// to the same gossip encoding so cycles cannot amplify the same signed vote.
async fn relay_vote(
    source: &str,
    vote: &crate::wire_message::WireVote,
    registry: &RwLock<ConnectionRegistry>,
    dedup: &Mutex<MessageDedup>,
) -> bool {
    let Ok(bytes) = WireMessage::Vote(vote.clone()).encode() else {
        return false;
    };
    if dedup
        .lock()
        .await
        .is_duplicate(&MessageDedup::hash_message(&bytes))
    {
        return false;
    }
    let registry = registry.read().await;
    for peer in registry.peer_ids() {
        if peer != source {
            registry.send(peer, bytes.clone());
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{prefer_new_on_conflict, Direction};

    #[test]
    fn proof_packets_have_a_separate_bounded_inbox() {
        use crate::checkpoint_sync::CheckpointSyncMessage as Message;
        let mut registry = super::ConnectionRegistry::new();
        let (proof_tx, mut proofs) = tokio::sync::mpsc::channel(1);
        let (checkpoint_tx, mut checkpoints) = tokio::sync::mpsc::channel(1);
        registry.set_witness_inbox(proof_tx);
        registry.set_checkpoint_inbox(checkpoint_tx);
        let proof = Message::WitnessRequest {
            start: burst_types::BlockHash::new([1; 32]),
            offset: 0,
            digest: None,
        };
        registry.dispatch_checkpoint("peer", proof.clone());
        registry.dispatch_checkpoint("peer", proof);
        assert!(checkpoints.try_recv().is_err());
        assert!(matches!(
            proofs.try_recv().unwrap().message,
            Message::WitnessRequest { .. }
        ));
        assert!(proofs.try_recv().is_err());
        registry.dispatch_checkpoint(
            "peer",
            Message::Request {
                height: 1,
                record_only: true,
                offset: 0,
                digest: None,
            },
        );
        assert!(matches!(
            checkpoints.try_recv().unwrap().message,
            Message::Request { .. }
        ));
        assert!(proofs.try_recv().is_err());
        registry.dispatch_checkpoint(
            "peer",
            Message::WitnessRequest {
                start: burst_types::BlockHash::new([1; 32]),
                offset: 1,
                digest: None,
            },
        );
        assert!(proofs.try_recv().is_err());
    }

    #[tokio::test]
    async fn closing_registry_reaps_readers_and_refuses_late_connection_work() {
        use super::*;
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(directory.path(), 32, 16 << 20).unwrap());
        let weak = Arc::downgrade(&store);
        let mut registry = ConnectionRegistry::new();
        let retained = store.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        registry.track_task(tokio::spawn(async move {
            let _retained = retained;
            started.send(()).unwrap();
            std::future::pending::<()>().await;
        }));
        ready.await.unwrap();
        registry.close();
        assert!(!registry.begin_dial("127.0.0.1:1"));
        let retained = store.clone();
        registry.track_task(tokio::spawn(async move {
            let _retained = retained;
            std::future::pending::<()>().await;
        }));
        for task in registry.take_tasks() {
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap_err();
        }
        assert_eq!(weak.strong_count(), 1);
        drop(store);
        assert!(weak.upgrade().is_none());
        LmdbStore::open(directory.path(), 29, 32 << 20).unwrap();
    }

    #[tokio::test]
    async fn relayed_ack_and_gossip_share_one_dedup_identity() {
        use super::*;
        let registry = RwLock::new(ConnectionRegistry::new());
        let dedup = Mutex::new(MessageDedup::new(100));
        let vote = crate::wire_message::WireVote {
            voter: burst_types::WalletAddress::new("brst_rep"),
            block_hashes: vec![BlockHash::new([1; 32])],
            is_final: true,
            timestamp: 100,
            sequence: 1,
            signature: burst_types::Signature([0; 64]),
        };
        assert!(relay_vote("first", &vote, &registry, &dedup).await);
        assert!(!relay_vote("second", &vote, &registry, &dedup).await);
        let mut later = vote;
        later.sequence += 1;
        assert!(relay_vote("first", &later, &registry, &dedup).await);
    }

    #[tokio::test]
    async fn wire_votes_route_competitors_to_one_election() {
        use super::*;
        let root = burst_types::BlockHash::new([1; 32]);
        let a = burst_types::BlockHash::new([2; 32]);
        let b = burst_types::BlockHash::new([3; 32]);
        let now = Timestamp::new(unix_now_secs());
        let voter = burst_types::WalletAddress::new("brst_alice");
        let bob = burst_types::WalletAddress::new("brst_bob");
        let carol = burst_types::WalletAddress::new("brst_carol");
        let mut weights = RepWeightCache::new();
        weights
            .rebuild_from_accounts([(true, voter.clone()), (true, bob), (true, carol)].into_iter());
        let mut elections = ActiveElections::new(2, weights.total_weight());
        elections.register_candidate(root, a, now).unwrap();
        elections.register_candidate(root, b, now).unwrap();
        let elections = RwLock::new(elections);
        let weights = RwLock::new(weights);
        // Signature checking precedes dispatch in the read loop. This test
        // isolates routing, which must not treat either candidate as a root.
        let mut vote = crate::wire_message::WireVote {
            voter: voter.clone(),
            block_hashes: vec![a],
            is_final: true,
            timestamp: now.as_secs(),
            sequence: 1,
            signature: burst_types::Signature([0; 64]),
        };
        dispatch_vote("test", &vote, &elections, &weights).await;
        vote.block_hashes = vec![b, burst_types::BlockHash::new([99; 32])];
        vote.sequence = 2;
        dispatch_vote("test", &vote, &elections, &weights).await;
        let elections = elections.read().await;
        assert_eq!(elections.election_count(), 1);
        let election = elections.get_election(&root).unwrap();
        assert_eq!(
            election.final_tally.get(&a),
            Some(&burst_consensus::rep_weights::WEIGHT_PER_HUMAN)
        );
        assert_eq!(election.final_tally.get(&b), None);
        assert!(!election.is_confirmed());
    }

    async fn socket_pair() -> (tokio::net::TcpStream, tokio::net::TcpStream) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        (client, server)
    }

    #[tokio::test]
    async fn recovery_snapshot_traffic_requires_an_active_exclusive_inbox() {
        use super::*;
        use crate::checkpoint_sync::CheckpointSyncMessage as Message;
        let mut registry = ConnectionRegistry::for_recovery();
        let packet = Message::SnapshotQuery { minimum_height: 1 };
        let wire = WireMessage::CheckpointSync(packet.clone());
        assert!(!registry.permits(&wire));
        let (sender, mut inbox) = mpsc::channel(2);
        assert!(registry.set_recovery_snapshot_inbox(sender.clone()));
        assert!(!registry.set_recovery_snapshot_inbox(sender));
        assert!(registry.permits(&wire));
        assert!(
            !registry.permits(&WireMessage::CheckpointSync(Message::Request {
                height: 1,
                record_only: true,
                offset: 0,
                digest: None
            }))
        );
        assert!(
            !registry.permits(&WireMessage::CheckpointSync(Message::SnapshotQuery {
                minimum_height: 0
            }))
        );
        registry.dispatch_checkpoint("peer", packet);
        assert_eq!(inbox.recv().await.unwrap().peer, "peer");
        drop(inbox);
        assert!(!registry.permits(&wire));
        registry.finish_recovery();
        let (sender, _inbox) = mpsc::channel(2);
        assert!(!registry.set_recovery_snapshot_inbox(sender));
    }

    #[test]
    fn recovery_content_requires_an_active_service_and_never_opens_block_traffic() {
        use super::*;
        let mut registry = ConnectionRegistry::for_recovery();
        let request =
            WireMessage::GovernanceContent(crate::governance_content::transfer::Packet::Request {
                proposal: BlockHash::new([1; 32]),
                offset: 0,
            });
        assert!(!registry.permits(&request));
        let (sender, receiver) = mpsc::channel(1);
        registry.set_governance_inbox(sender);
        assert!(!registry.permits(&request));
        registry.set_recovery_content(Arc::new(
            crate::governance_content::transfer::Transfers::default(),
        ));
        assert!(registry.permits(&request));
        let block = crate::genesis_state::block(
            burst_types::NetworkId::Dev,
            &burst_types::ProtocolParams::default(),
        );
        assert!(!registry.permits(&WireMessage::Block(Box::new(block))));
        drop(receiver);
        assert!(!registry.permits(&request));
        registry.close();
        assert!(!registry.permits(&request));
    }

    #[tokio::test]
    async fn recovery_transport_filters_before_admission_and_dedup() {
        use super::*;
        use crate::checkpoint_sync::CheckpointSyncMessage;
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let registry = RwLock::new(ConnectionRegistry::for_recovery());
        let (sender, mut proofs) = mpsc::channel(4);
        registry.write().await.set_witness_inbox(sender);
        let queue = BlockPriorityQueue::new(4);
        let elections = RwLock::new(ActiveElections::new(2, 0));
        let weights = RwLock::new(RepWeightCache::new());
        let peers = RwLock::new(PeerManager::new(4));
        let dedup = Mutex::new(MessageDedup::new(100));
        let sampler = Mutex::new(OnlineWeightSampler::new(60));
        let address = crate::genesis_key::genesis_address(burst_types::NetworkId::Dev);
        let generator = Mutex::new(burst_consensus::VoteGenerator::new(
            address.clone(),
            [0; 32],
        ));
        let (feedback, _received) = mpsc::channel(4);
        let (mut client, server) = socket_pair().await;
        let (reader, writer) = server.into_split();
        registry.write().await.insert("peer".into(), writer);
        let block = WireMessage::Block(Box::new(burst_ledger::genesis::create_genesis_block(
            &burst_ledger::genesis::GenesisConfig {
                network: burst_types::NetworkId::Dev,
                creator: address,
            },
        )));
        let proof = WireMessage::CheckpointSync(CheckpointSyncMessage::WitnessRequest {
            start: BlockHash::new([1; 32]),
            offset: 0,
            digest: None,
        });
        let traffic = async {
            assert!(!registry.read().await.send("peer", block.encode().unwrap()));
            assert!(!registry.read().await.send("peer", vec![255]));
            assert!(registry.read().await.send("peer", proof.encode().unwrap()));
            let size = client.read_u32().await.unwrap();
            let mut reply = vec![0; size as usize];
            client.read_exact(&mut reply).await.unwrap();
            assert!(matches!(
                WireMessage::decode(&reply).unwrap(),
                WireMessage::CheckpointSync(_)
            ));
            for operational in [false, true] {
                if operational {
                    registry.write().await.finish_recovery();
                }
                for message in [&block, &proof] {
                    let bytes = message.encode().unwrap();
                    client.write_u32(bytes.len() as u32).await.unwrap();
                    client.write_all(&bytes).await.unwrap();
                }
                // The proof follows the block on the same socket, so receipt
                // proves the read loop already processed the earlier frame.
                proofs.recv().await.unwrap();
                assert_eq!(queue.len().await, usize::from(operational));
            }
            // An admission failure does not make the wire payload permanently
            // invalid. The same bytes must reach admission again after backoff.
            assert!(queue.try_pop().await.is_some());
            for retry in [false, true] {
                if retry {
                    tokio::time::sleep(Duration::from_millis(1100)).await;
                }
                for message in [&block, &proof] {
                    let bytes = message.encode().unwrap();
                    client.write_u32(bytes.len() as u32).await.unwrap();
                    client.write_all(&bytes).await.unwrap();
                }
                proofs.recv().await.unwrap();
                assert_eq!(queue.len().await, usize::from(retry));
            }
            registry.write().await.close();
            registry.write().await.finish_recovery();
            assert!(!registry.read().await.permits(&proof));
            assert!(!registry.read().await.permits(&block));
            client.shutdown().await.unwrap();
        };
        let reading = peer_read_loop(
            "peer",
            reader,
            &queue,
            &elections,
            &weights,
            &peers,
            &dedup,
            &sampler,
            &registry,
            &store,
            BlockHash::ZERO,
            &feedback,
            &generator,
        );
        let (_, result) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(traffic, reading)
        })
        .await
        .unwrap();
        assert!(result.is_err());
        for task in registry.write().await.take_tasks() {
            let _ = task.await;
        }
    }

    #[tokio::test]
    async fn confirmation_requests_cross_a_hub_without_echoing_or_unbounded_dedup() {
        use super::*;
        let mut registry = ConnectionRegistry::new();
        let (_source, source) = socket_pair().await;
        let (mut receiver, target) = socket_pair().await;
        registry.insert("source".into(), source.into_split().1);
        registry.insert("target".into(), target.into_split().1);
        let now = std::time::Instant::now();
        let hash = BlockHash::new([1; 32]);
        assert_eq!(
            registry.forward_confirmation_request("source", &[hash], 0, now),
            0
        );
        assert_eq!(
            registry.forward_confirmation_request("source", &[hash], 4, now),
            0
        );
        assert_eq!(
            registry.forward_confirmation_request("source", &[hash, hash], 3, now),
            1
        );
        let length = receiver.read_u32().await.unwrap() as usize;
        let mut body = vec![0; length];
        receiver.read_exact(&mut body).await.unwrap();
        let WireMessage::ConfirmReq(request) = WireMessage::decode(&body).unwrap() else {
            panic!("not a confirmation request")
        };
        assert_eq!(request.block_hashes, vec![hash]);
        assert_eq!(request.remaining_hops, 2);
        assert_eq!(
            registry.forward_confirmation_request("target", &[hash], 2, now),
            0
        );
        assert_eq!(
            registry.forward_confirmation_request(
                "source",
                &[hash],
                3,
                now + Duration::from_secs(2)
            ),
            1
        );
        let now = now + Duration::from_secs(2);
        for i in 0u64..5000 {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&i.to_be_bytes());
            registry.forward_confirmation_request("source", &[BlockHash::new(bytes)], 3, now);
        }
        assert_eq!(registry.confirmation_requests.len(), 4096);
        registry.forward_confirmation_request("source", &[hash], 3, now + Duration::from_secs(2));
        assert_eq!(registry.confirmation_requests.len(), 1);
    }

    #[tokio::test]
    async fn a_retried_vote_can_arrive_before_then_after_its_candidate() {
        use super::*;
        let voter = burst_types::WalletAddress::new("brst_rep");
        let hash = BlockHash::new([1; 32]);
        let registry = RwLock::new(ConnectionRegistry::new());
        let dedup = Mutex::new(MessageDedup::new(100));
        let weights = RwLock::new(RepWeightCache::new());
        weights
            .write()
            .await
            .rebuild_from_accounts([(true, voter.clone())].into_iter());
        let elections = RwLock::new(ActiveElections::new(2, weights.read().await.total_weight()));
        let vote = crate::wire_message::WireVote {
            voter,
            block_hashes: vec![hash],
            is_final: true,
            timestamp: unix_now_secs(),
            sequence: 1,
            signature: burst_types::Signature([0; 64]),
        };
        assert!(relay_vote("first", &vote, &registry, &dedup).await);
        dispatch_vote("first", &vote, &elections, &weights).await;
        elections
            .write()
            .await
            .register_candidate(hash, hash, Timestamp::new(unix_now_secs()))
            .unwrap();
        assert!(!relay_vote("second", &vote, &registry, &dedup).await);
        dispatch_vote("second", &vote, &elections, &weights).await;
        assert!(elections
            .read()
            .await
            .get_election(&hash)
            .unwrap()
            .is_confirmed());
    }

    #[tokio::test]
    async fn relaying_a_requested_historical_vote_does_not_grant_local_voting_power() {
        use super::*;
        let hash = BlockHash::new([1; 32]);
        let voter = burst_types::WalletAddress::new("brst_old_representative");
        let mut registry = ConnectionRegistry::new();
        let mut vote = crate::wire_message::WireVote {
            voter: voter.clone(),
            block_hashes: vec![hash],
            is_final: true,
            timestamp: unix_now_secs() * 1000,
            sequence: 1,
            signature: burst_types::Signature([0; 64]),
        };
        assert!(!registry.requested_vote(&vote));
        registry.forward_confirmation_request("requester", &[hash], 3, std::time::Instant::now());
        assert!(registry.requested_vote(&vote));
        let mut elections = ActiveElections::new(1, 0);
        elections
            .set_authority(HashMap::from([(
                burst_types::WalletAddress::new("brst_current_representative"),
                10_000,
            )]))
            .unwrap();
        elections
            .register_candidate(hash, hash, Timestamp::new(unix_now_secs()))
            .unwrap();
        let elections = RwLock::new(elections);
        dispatch_vote(
            "relay",
            &vote,
            &elections,
            &RwLock::new(RepWeightCache::new()),
        )
        .await;
        assert!(elections
            .read()
            .await
            .get_election(&hash)
            .unwrap()
            .final_tally
            .is_empty());
        vote.block_hashes.push(BlockHash::new([2; 32]));
        assert!(!registry.requested_vote(&vote));
        vote.block_hashes.pop();
        registry
            .confirmation_requests
            .insert(hash, std::time::Instant::now() - Duration::from_secs(3));
        assert!(!registry.requested_vote(&vote));
    }

    #[tokio::test]
    async fn identity_routing_dedup_and_generation_guard() {
        use super::*;
        let mut registry = ConnectionRegistry::new();
        let (mut a, sa) = socket_pair().await;
        let (mut b, sb) = socket_pair().await;
        let (_, wa) = sa.into_split();
        let (_, wb) = sb.into_split();
        let first = registry
            .register_dedup("a".into(), Direction::Inbound, wa, "0")
            .unwrap();
        registry
            .register_dedup("b".into(), Direction::Inbound, wb, "0")
            .unwrap();
        assert_eq!(registry.len(), 2);
        assert!(!registry.send("a", vec![0; MAX_MESSAGE_SIZE + 1]));
        assert!(registry.send("a", vec![1]));
        assert!(registry.send("b", vec![2]));
        let mut frame = [0; 5];
        tokio::time::timeout(Duration::from_secs(1), a.read_exact(&mut frame))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame, [0, 0, 0, 1, 1]);
        tokio::time::timeout(Duration::from_secs(1), b.read_exact(&mut frame))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame, [0, 0, 0, 1, 2]);
        let (_duplicate, server) = socket_pair().await;
        assert!(registry
            .register_dedup("a".into(), Direction::Inbound, server.into_split().1, "0")
            .is_none());
        let (_replacement, server) = socket_pair().await;
        let second = registry
            .register_dedup("a".into(), Direction::Outbound, server.into_split().1, "0")
            .unwrap();
        assert!(!registry.remove_if("a", first));
        assert_eq!(registry.len(), 2);
        assert!(registry.remove_if("a", second));
        assert_eq!(registry.len(), 1);
    }

    #[tokio::test]
    async fn losing_outbound_dial_is_remembered_on_surviving_inbound() {
        let mut registry = super::ConnectionRegistry::new();
        let (_first, inbound) = socket_pair().await;
        registry
            .register_dedup("a".into(), Direction::Inbound, inbound.into_split().1, "z")
            .unwrap();
        let (outbound, _server) = socket_pair().await;
        let endpoint = outbound.peer_addr().unwrap().to_string();
        assert!(registry
            .register_dedup(
                "a".into(),
                Direction::Outbound,
                outbound.into_split().1,
                "z"
            )
            .is_none());
        assert_eq!(registry.len(), 1);
        assert!(!registry.begin_dial(&endpoint));
    }

    #[test]
    fn repeated_peer_requests_and_liveness_messages_are_not_globally_deduplicated() {
        use super::*;
        use crate::bootstrap::AscPullReqPayload;
        let request = WireMessage::Bootstrap(BootstrapMessage::AscPullReq {
            id: 0,
            payload: AscPullReqPayload::Frontiers {
                start_account: burst_types::WalletAddress::new("brst_"),
                count: 128,
            },
        });
        assert!(!is_deduplicated_gossip(&request));
        assert!(!is_deduplicated_gossip(&WireMessage::TelemetryReq));
        assert!(!is_deduplicated_gossip(&WireMessage::Keepalive(
            crate::wire_message::KeepaliveMsg { peers: vec![] }
        )));
    }

    // The core invariant: on a simultaneous connect between two nodes, both
    // ends independently keep the SAME physical connection — the one initiated
    // by the lower-addressed node — so exactly one survives (no churn, no
    // "both drop" partition).
    #[test]
    fn tie_break_keeps_lower_addressed_initiator_consistently() {
        let low = "1.1.1.1:7076";
        let high = "2.2.2.2:7076";

        // Low node: keeps its own outbound, rejects the peer's inbound.
        assert!(prefer_new_on_conflict(low, high, Direction::Outbound));
        assert!(!prefer_new_on_conflict(low, high, Direction::Inbound));

        // High node: rejects its own outbound, keeps the peer's inbound.
        assert!(!prefer_new_on_conflict(high, low, Direction::Outbound));
        assert!(prefer_new_on_conflict(high, low, Direction::Inbound));

        // ⇒ Surviving link is low→high: the low node keeps it as Outbound, the
        // high node keeps it as Inbound. Same physical connection on both ends.
    }

    #[test]
    fn tie_break_is_symmetric_for_any_pair() {
        // For any distinct pair, exactly one direction survives, agreed by both.
        for (a, b) in [
            ("10.0.0.1:7076", "10.0.0.2:7076"),
            ("4.223.174.176:7076", "20.101.66.128:7076"),
            ("168.63.71.72:7076", "20.101.66.128:7076"),
        ] {
            let (low, high) = if a < b { (a, b) } else { (b, a) };
            // low keeps outbound; high keeps inbound; both = the low→high link.
            assert!(prefer_new_on_conflict(low, high, Direction::Outbound));
            assert!(prefer_new_on_conflict(high, low, Direction::Inbound));
            // and the losing direction is rejected on both ends.
            assert!(!prefer_new_on_conflict(low, high, Direction::Inbound));
            assert!(!prefer_new_on_conflict(high, low, Direction::Outbound));
        }
    }
}
