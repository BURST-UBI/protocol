//! Peer channels, outbound queue routing and inbound message dispatch.
//! Each channel owns queue senders; a separate task owns its socket writer.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{mpsc, Mutex, RwLock};

use burst_consensus::{ActiveElections, OnlineWeightSampler, RepWeightCache};
use burst_crypto::{decode_address, verify_signature};
use burst_network::{MessageDedup, PeerManager, PeerTelemetry};
use burst_network::admission::{ConnectionPermit, PeerAdmission, PeerLimits};
use burst_network::bandwidth::{BandwidthLimits, SharedBandwidth};
use burst_store::account::AccountStore;
use burst_store::block::BlockStore;
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, PublicKey, Timestamp};

use crate::bootstrap::{AscPullAckPayload, BootstrapMessage, BootstrapServer};
use crate::metrics::NodeMetrics;
use crate::priority_queue::BlockPriorityQueue;
use crate::wire_message::{TelemetryAckMessage, WireMessage};

mod dial_retry;
mod receive_profile;
mod vote_dispatch;
use receive_profile::{ReceiveProfile, Stage};

// Diagnostic only: preserve the exact wire identity across queue and socket stages.
// No decoding or hashing is performed when the opt-in trace is disabled.
pub(crate) fn trace_final_vote_transport(payload: &[u8], stage: &str, endpoint: &str) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var_os("BURST_FINALITY_STAGE_LOG").is_some()) {
        return;
    }
    let Ok(message) = WireMessage::decode(payload) else { return; };
    let vote = match &message {
        WireMessage::Vote(vote) => vote,
        WireMessage::ConfirmAck(ack) => &ack.vote,
        _ => return,
    };
    if !vote.is_final { return; }
    let envelope = MessageDedup::hash_message(payload);
    tracing::debug!(stage, endpoint, ?envelope, voter = %vote.voter,
        sequence = vote.sequence, timestamp = vote.timestamp, hashes = ?vote.block_hashes,
        "final vote transport stage");
}

/// Maximum message body size (matches protocol codec limit).
const MAX_MESSAGE_BYTES: usize = burst_types::MAX_P2P_MESSAGE_BYTES;

/// Separate deadline for each length-prefix read and each body read.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Which side opened a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// We dialed the peer.
    Outbound,
    /// The peer dialed us.
    Inbound,
}

/// A connection identified by its authenticated node ID. Nodes sharing an IP
/// have separate channels, subject to the shared IP admission limits.
struct Channel {
    /// Separate bounded control and bulk queues. Enqueueing never waits for
    /// socket writes; the writer preserves order within each class.
    outbound: burst_network::egress::Sender,
    admission: Arc<ConnectionPermit>,
    node_id: String,
    direction: Direction,
    dial_address: Option<String>,
    /// Monotonic id distinguishing this physical connection from any prior one
    /// under the same peer_id, so a superseded socket's teardown can't evict
    /// the live entry.
    conn_id: u64,
}

/// Outbound queue depth per channel.
#[cfg(test)]
const CHANNEL_QUEUE_CAPACITY: usize = burst_network::egress::BULK_QUEUE_CAPACITY;

/// Spawn a socket writer with separate control and bulk queues. Control is
/// selected first; both classes share per-peer and node-wide bandwidth budgets.
/// The writer sends length-prefixed frames and exits on timeout or write failure.
fn spawn_channel_writer(
    writer: OwnedWriteHalf, peer_id: String,
    shared: Arc<SharedBandwidth>, admission: Arc<ConnectionPermit>,
) -> (burst_network::egress::Sender, tokio::task::JoinHandle<()>) {
    burst_network::egress::spawn(writer, shared, admission, move |payload, stage| {
        trace_final_vote_transport(payload, stage, &peer_id);
    })
}

/// For opposite-direction connections between distinct node IDs, prefer the
/// one initiated by the lexicographically lower ID. Both endpoints agree.
pub fn prefer_new_on_conflict(our_node_id: &str, peer_node_id: &str, new_dir: Direction) -> bool {
    (our_node_id < peer_node_id) == (new_dir == Direction::Outbound)
}

/// Registered peer channels and the workers that route their traffic.
pub struct ConnectionRegistry {
    pub(crate) historical_finality: Arc<crate::historical_finality::HistoricalFinality>,
    vote_dispatch: Option<vote_dispatch::Dispatcher>,
    closed: bool,
    admission: Arc<PeerAdmission>,
    bandwidth: Arc<SharedBandwidth>,
    recovering: bool,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    /// All live channels, keyed by `peer_id`.
    channels: HashMap<String, Channel>,
    next_conn_id: u64,
    /// Dial target strings reserved by in-flight outbound attempts. Different
    /// aliases for the same endpoint remain separate until identity deduplication.
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
    pub(crate) recovery_projection:
        Arc<crate::confirmed_replay::snapshot::installed::ProjectionCache>,
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
            vote_dispatch: None,
            closed: false,
            admission: PeerAdmission::new(50, PeerLimits::default()),
            bandwidth: Arc::new(SharedBandwidth::new(&BandwidthLimits::default())),
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

    pub(crate) fn configure_network_limits(&mut self, max_peers: usize, peers: PeerLimits, bandwidth: &BandwidthLimits) {
        assert!(self.channels.is_empty() && self.dialing.is_empty());
        self.admission = PeerAdmission::new(max_peers, peers);
        self.bandwidth = Arc::new(SharedBandwidth::new(bandwidth));
    }

    pub(crate) fn admit(&self, address: std::net::IpAddr, direction: Direction) -> Option<Arc<ConnectionPermit>> {
        if self.closed { return None; }
        self.admission.acquire(address, direction == Direction::Outbound)
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

    pub(crate) fn start_vote_dispatch(
        &mut self,
        elections: Arc<RwLock<ActiveElections>>,
        weights: Arc<RwLock<RepWeightCache>>,
    ) {
        assert!(self.vote_dispatch.is_none(), "vote dispatcher already started");
        let (dispatcher, task) = vote_dispatch::spawn(elections, weights);
        self.vote_dispatch = Some(dispatcher);
        self.track_task(task);
    }

    pub(crate) fn close(&mut self) {
        self.closed = true;
        self.vote_dispatch = None;
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

    pub(crate) fn source_ip(&self, peer_id: &str) -> Option<std::net::IpAddr> {
        self.channels.get(peer_id).map(|channel| channel.admission.bucket())
    }

    /// Reserve an address string unless connected, reserved, cooling down, or
    /// closed. Pair a successful reservation with [`end_dial`](Self::end_dial).
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

    /// Replace a channel without identity deduplication. Returns zero if closed
    /// or admission fails, otherwise a fresh connection ID. The old writer may
    /// drain its queue before exiting. Prefer [`register_dedup`](Self::register_dedup).
    pub fn insert(&mut self, peer_id: String, writer: OwnedWriteHalf) -> u64 {
        if self.closed {
            return 0;
        }
        let Some(admission) = writer.peer_addr().ok().and_then(|addr| self.admit(addr.ip(), Direction::Outbound)) else { return 0; };
        let conn_id = self.next_conn_id;
        self.next_conn_id += 1;
        let (outbound, writer_task) = spawn_channel_writer(writer, peer_id.clone(), self.bandwidth.clone(), admission.clone());
        self.track_task(writer_task);
        self.channels.insert(
            peer_id,
            Channel {
                outbound,
                admission,
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
        let address = writer.peer_addr().ok()?.ip();
        let admission = self.admit(address, direction)?;
        self.register_admitted(peer_node_id, direction, writer, our_node_id, admission)
    }

    pub(crate) fn register_admitted(
        &mut self, peer_node_id: String, direction: Direction, writer: OwnedWriteHalf,
        our_node_id: &str, admission: Arc<ConnectionPermit>,
    ) -> Option<u64> {
        if self.closed || !admission.matches(writer.peer_addr().ok()?.ip(), direction == Direction::Outbound) {
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
                // Keep the existing connection for a same-direction duplicate.
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
        let (outbound, writer_task) = spawn_channel_writer(writer, peer_id.clone(), self.bandwidth.clone(), admission.clone());
        self.track_task(writer_task);
        self.channels.insert(
            peer_id,
            Channel {
                outbound,
                admission,
                node_id: peer_node_id,
                direction,
                dial_address,
                conn_id,
            },
        );
        Some(conn_id)
    }

    /// Remove a channel and close its queue senders. The writer may drain queued
    /// frames before exiting; this does not abort or join either socket task.
    /// Returns whether the channel was registered.
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

    /// Try to enqueue a payload without waiting for the socket. False means
    /// size, recovery, channel or queue checks rejected it. True means queued,
    /// not delivered: bulk bandwidth limits or later socket errors can drop it.
    pub fn send(&self, peer_id: &str, payload: Vec<u8>) -> bool {
        #[cfg(test)]
        if self.partition_drops_peer(peer_id) {
            return false;
        }
        if payload.len() > MAX_MESSAGE_BYTES {
            return false;
        }
        if self.closed
            || (self.recovering
                && !WireMessage::decode(&payload).ok().is_some_and(|message| self.permits(&message)))
        {
            return false;
        }
        match self.channels.get(peer_id) {
            Some(c) => {
                trace_final_vote_transport(&payload, "channel_enqueue_attempt", peer_id);
                let class = WireMessage::outbound_class(&payload);
                match c.outbound.try_send(payload, class) {
                    Ok(()) => true,
                    Err(error) => {
                        trace_final_vote_transport(&error, "channel_rejected", peer_id);
                        false
                    }
                }
            },
            None => false,
        }
    }

    /// A bootstrap peer need not be every requested block's representative.
    /// Relay up to eight requested hashes without checking local block presence.
    /// A hop limit and a shared two-second dedup window limit forwarding cycles.
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
/// When the read loop returns, remove this connection if its generation still
/// matches. Aborting the task bypasses this cleanup.
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
    admission: Arc<ConnectionPermit>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Retain admission until both socket halves and registry ownership end.
        let _admission = admission;
        if !connection_registry.read().await.channels.get(&peer_id)
            .is_some_and(|channel| channel.conn_id == conn_id) { return; }
        // Offer peer addresses on connection rather than waiting for keepalive.
        {
            let self_peers: Vec<String> = {
                let peers = peer_manager.read().await;
                peers.random_peers_with_self(8)
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
            let mut peers = peer_manager.write().await;
            let mut registry = connection_registry.write().await;
            let removed = registry.remove_if(&peer_id, conn_id);
            metrics.peer_count.set(registry.len() as i64);
            if removed {
                peers.mark_disconnected(&peer_id);
            }
        }

        tracing::debug!(peer = %peer_id, "peer cleaned up after disconnect");
    })
}

// Peer liveness is stored at second precision. Rewriting the same value on
// every frame adds a shared writer acquisition without extending liveness.
async fn touch_peer_second(
    manager: &RwLock<PeerManager>,
    peer_id: &str,
    last_touched: &mut Option<u64>,
    now: u64,
) {
    if *last_touched == Some(now) { return; }
    manager.write().await.touch(peer_id, now);
    // Commit the local cursor only after the write: cancellation must not hide
    // the next attempt, and a backward clock change must still be recorded.
    *last_touched = Some(now);
}

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
    store: &Arc<LmdbStore>,
    our_params_hash: burst_types::BlockHash,
    bootstrap_feedback: &mpsc::Sender<crate::bootstrap::BootstrapFeedback>,
    vote_generator: &Arc<Mutex<burst_consensus::VoteGenerator>>,
) -> Result<(), std::io::Error> {
    let source_ip = reader.peer_addr()?.ip();
    // At most one active and one pending eight-hash request per connection.
    // Slow signer/storage work must not prevent this socket receiving votes.
    let (responses, mut pending_responses) = mpsc::channel::<Vec<BlockHash>>(1);
    let stage_log = std::env::var_os("BURST_FINALITY_STAGE_LOG").is_some();
    let reading = async {
        let mut last_vote_request = None;
        let mut last_touched = None;
        let mut verified_votes = MessageDedup::new(512);
        let mut profile = ReceiveProfile::new(stage_log, peer_id, store.env().path());
        loop {
            profile.stage(Stage::ReadFrame);
            // The prefix and body each have their own deadline.
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
            if body_len > MAX_MESSAGE_BYTES {
                tracing::warn!(
                    peer = %peer_id,
                    size = body_len,
                    "peer sent oversized message, disconnecting"
                );
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("message too large: {body_len} > {MAX_MESSAGE_BYTES}"),
                ));
            }

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

            profile.stage(Stage::Permit);
            if stage_log {
                trace_final_vote_transport(&body, "frame_received", &store.env().path().to_string_lossy());
            }
            #[cfg(test)]
            if connection_registry
                .read()
                .await
                .partition_drops_peer(peer_id)
            {
                continue;
            }
            profile.stage(Stage::Decode);
            let message = WireMessage::decode(&body);
            profile.stage(Stage::Permit);
            if let Ok(message) = &message {
                if !connection_registry.read().await.permits(message) {
                    continue;
                }
            }
            // Only gossip is globally deduplicated. Request IDs are scoped to a
            // requesting peer, and periodic telemetry/keepalives may be identical.
            // Dropping those across peers prevents new nodes from bootstrapping.
            profile.stage(Stage::Dedup);
            if message.as_ref().is_ok_and(is_deduplicated_gossip) {
                let msg_hash = MessageDedup::hash_message(&body);
                let mut dedup = message_dedup.lock().await;
                // Admission may defer a block until its committee or dependencies
                // arrive. Suppress immediate loops, not later identical retries.
                if dedup.apply_digest_with_window(&msg_hash, Duration::from_secs(1), std::time::Instant::now())
                {
                    tracing::trace!(
                        peer = %peer_id,
                        "dropped duplicate message"
                    );
                    continue;
                }
            }

            // Record frame activity before message validation, including decode errors.
            // Recovery-filtered frames and duplicate blocks have already been skipped.
            profile.stage(Stage::Liveness);
            {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                touch_peer_second(peer_manager, peer_id, &mut last_touched, now).await;
            }

            profile.stage(match &message {
                Ok(WireMessage::Block(_)) => Stage::Block,
                Ok(WireMessage::ConfirmReq(_)) => Stage::ConfirmRequest,
                Ok(WireMessage::Keepalive(_)) => Stage::Keepalive,
                Ok(WireMessage::Bootstrap(_)) => Stage::Bootstrap,
                _ => Stage::Other,
            });
            match message {
                Ok(WireMessage::Block(block)) => {
                    tracing::debug!(
                        peer = %peer_id,
                        hash = %block.hash,
                        "received block from peer"
                    );
                    match enqueue_network_block(&store.block_store(), block_queue, *block, source_ip).await {
                        Some(true) => {
                            let mut peers = peer_manager.write().await;
                            peers.reward(peer_id, 1);
                        }
                        Some(false) => tracing::warn!(peer = %peer_id,
                            "block priority queue full, dropping block"),
                        None => tracing::debug!(peer = %peer_id, "ignored already stored network block"),
                    }
                }
                Ok(WireMessage::Vote(vote)) => {
                    profile.stage(Stage::Signature);
                    if !is_vote_signature_valid_cached(&vote, &mut verified_votes) {
                        continue;
                    }
                    profile.stage(Stage::Authorize);
                    {
                        let weights = rep_weights.read().await;
                        if weights.weight(&vote.voter) == 0
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
                    profile.stage(Stage::Online);
                    {
                        let now = unix_now_secs();
                        let mut sampler = online_weight_sampler.lock().await;
                        sampler.record_vote(&vote.voter, now);
                    }
                    profile.stage(Stage::Dispatch);
                    if stage_log {
                        trace_final_vote_transport(&body, "authenticated", &store.env().path().to_string_lossy());
                        for hash in &vote.block_hashes {
                            tracing::debug!(receiver = ?store.env().path(), %hash,
                                voter = %vote.voter, is_final = vote.is_final,
                                sequence = vote.sequence, timestamp = vote.timestamp,
                                "authenticated vote ready for routing");
                        }
                    }
                    if stage_log {
                        tracing::Instrument::instrument(
                            dispatch_authenticated(peer_id, &vote, active_elections, rep_weights, connection_registry, &mut profile),
                            tracing::debug_span!("vote_receiver", receiver = ?store.env().path()),
                        ).await?;
                    } else {
                        dispatch_authenticated(peer_id, &vote, active_elections, rep_weights, connection_registry, &mut profile).await?;
                    }
                    // Dispatch locally before awaiting relay and historical recovery.
                    profile.stage(Stage::Relay);
                    relay_vote(peer_id, &vote, connection_registry, message_dedup).await;
                    profile.stage(Stage::History);
                    let historical = connection_registry.read().await.historical_finality.clone();
                    historical.receive_and_enqueue(&vote, block_queue).await;
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
                    if responses.try_send(req.block_hashes).is_err() {
                        tracing::debug!(peer = %peer_id, "vote response queue full; request deferred to peer retry");
                    }
                }

                Ok(WireMessage::ConfirmAck(ack)) => {
                    profile.stage(Stage::Signature);
                    if !is_vote_signature_valid_cached(&ack.vote, &mut verified_votes) {
                        continue;
                    }
                    profile.stage(Stage::Authorize);
                    {
                        let weights = rep_weights.read().await;
                        if weights.weight(&ack.vote.voter) == 0
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
                    profile.stage(Stage::Online);
                    {
                        let now = unix_now_secs();
                        let mut sampler = online_weight_sampler.lock().await;
                        sampler.record_vote(&ack.vote.voter, now);
                    }
                    profile.stage(Stage::Dispatch);
                    if stage_log {
                        trace_final_vote_transport(&body, "authenticated", &store.env().path().to_string_lossy());
                        for hash in &ack.vote.block_hashes {
                            tracing::debug!(receiver = ?store.env().path(), %hash,
                                voter = %ack.vote.voter, is_final = ack.vote.is_final,
                                sequence = ack.vote.sequence, timestamp = ack.vote.timestamp,
                                "authenticated vote ready for routing");
                        }
                    }
                    if stage_log {
                        tracing::Instrument::instrument(
                            dispatch_authenticated(peer_id, &ack.vote, active_elections, rep_weights, connection_registry, &mut profile),
                            tracing::debug_span!("vote_receiver", receiver = ?store.env().path()),
                        ).await?;
                    } else {
                        dispatch_authenticated(peer_id, &ack.vote, active_elections, rep_weights, connection_registry, &mut profile).await?;
                    }
                    // Dispatch locally before awaiting relay and historical recovery.
                    profile.stage(Stage::Relay);
                    relay_vote(peer_id, &ack.vote, connection_registry, message_dedup).await;
                    profile.stage(Stage::History);
                    let historical = connection_registry.read().await.historical_finality.clone();
                    historical.receive_and_enqueue(&ack.vote, block_queue).await;
                }
                Ok(WireMessage::Keepalive(keepalive)) => {
                    tracing::trace!(
                        peer = %peer_id,
                        peers = keepalive.peers.len(),
                        "received keepalive"
                    );
                    if !keepalive.peers.is_empty() {
                        let mut parsed: Vec<burst_messages::PeerAddress> =
                            Vec::with_capacity(keepalive.peers.len());
                        for addr_str in &keepalive.peers {
                            let parts: Vec<&str> = addr_str.rsplitn(2, ':').collect();
                            if parts.len() == 2 {
                                if let Ok(port) = parts[0].parse::<u16>() {
                                    let ip = parts[1].to_string();
                                    parsed.push(burst_messages::PeerAddress { ip, port });
                                }
                            }
                        }

                        // Treat the first parsable entry as the advertised peering address.
                        // This does not prove that the endpoint is reachable.
                        let peering = parsed.first().cloned();

                        let mut peers = peer_manager.write().await;
                        peers.process_keepalive(parsed);
                        if let Some(pa) = peering {
                            peers.set_peering_addr(peer_id, pa);
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
                                .accounts_page(Some(start_account), count as usize)
                            {
                                Ok(accounts) => accounts.into_iter().map(|a| (a.address, a.head)).collect(),
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
                Ok(WireMessage::Handshake(handshake)) => {
                    tracing::debug!(
                        peer = %peer_id,
                        node_id = %handshake.node_id,
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
                        let peers = peer_manager.read().await;
                        peers.connected_count() as u32
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
                    let mut peers = peer_manager.write().await;
                    peers.update_telemetry(
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
    };
    let responding = async {
        while let Some(hashes) = pending_responses.recv().await {
            for hash in hashes {
                let vote = {
                    let generator = vote_generator.clone().lock_owned().await;
                    crate::durable_votes::worker::respond(store.clone(), generator, hash).await
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
    };
    // Preserve the whole framed-reader future while polling replies. Completing
    // a reply must never cancel a partially consumed read_exact frame.
    tokio::select! {
        result = reading => result,
        _ = responding => Ok(()),
    }
}

// This cache proves only the signature on these exact bytes. Authority, routing
// and historical recovery still run on every arrival, including cached retries.
fn is_vote_signature_valid_cached(
    vote: &crate::wire_message::WireVote,
    verified: &mut MessageDedup,
) -> bool {
    if vote.block_hashes.is_empty() || vote.block_hashes.len() > 8 { return false; }
    let mut signed = burst_consensus::vote_signing_payload(
        &vote.voter, vote.is_final, vote.timestamp, vote.sequence, &vote.block_hashes,
    );
    signed.extend_from_slice(&vote.signature.0);
    let hash = MessageDedup::hash_message(&signed);
    if verified.contains(&hash) { return true; }
    if !is_vote_signature_valid(vote) { return false; }
    verified.apply_digest(&hash);
    true
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

async fn dispatch_authenticated(
    peer: &str,
    vote: &crate::wire_message::WireVote,
    elections: &RwLock<ActiveElections>,
    weights: &RwLock<RepWeightCache>,
    registry: &RwLock<ConnectionRegistry>,
    profile: &mut ReceiveProfile<'_>,
) -> std::io::Result<()> {
    let dispatcher = {
        let registry = registry.read().await;
        if registry.closed {
            return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "connection registry closed"));
        }
        registry.vote_dispatch.clone()
    };
    if let Some(dispatcher) = dispatcher {
        dispatcher.dispatch(peer, vote).await
    } else {
        dispatch_vote_profiled(peer, vote, elections, weights, profile).await;
        Ok(())
    }
}

/// Route a received vote (from Vote or ConfirmAck) to active elections.
#[cfg(test)]
async fn dispatch_vote(
    peer_id: &str,
    vote: &crate::wire_message::WireVote,
    active_elections: &RwLock<ActiveElections>,
    rep_weights: &RwLock<RepWeightCache>,
) {
    let mut profile = ReceiveProfile::new(false, peer_id, std::path::Path::new(""));
    dispatch_vote_profiled(peer_id, vote, active_elections, rep_weights, &mut profile).await;
}

async fn dispatch_vote_profiled(
    peer_id: &str,
    vote: &crate::wire_message::WireVote,
    active_elections: &RwLock<ActiveElections>,
    rep_weights: &RwLock<RepWeightCache>,
    profile: &mut ReceiveProfile<'_>,
) {
    profile.stage(Stage::DispatchWait);
    // Skip votes known to have no effect without waiting for the write lock.
    // If the read lock is busy, use normal routing.
    if let Ok(active) = active_elections.try_read() {
        if vote.block_hashes.iter().all(|hash| active.signed_vote_has_no_effect(
            hash, &vote.voter, vote.is_final, vote.sequence,
        )) {
            return;
        }
    }
    profile.stage(Stage::DispatchWeight);
    let weight = {
        let weights = rep_weights.read().await;
        weights.weight(&vote.voter)
    };
    let now = Timestamp::new(unix_now_secs());
    profile.stage(Stage::DispatchWait);
    let mut elections = active_elections.write().await;
    profile.stage(Stage::DispatchRoute);
    route_vote(peer_id, vote, weight, &mut elections, now);
}

fn route_vote(
    peer_id: &str,
    vote: &crate::wire_message::WireVote,
    weight: u128,
    elections: &mut ActiveElections,
    now: Timestamp,
) {
    for block_hash in &vote.block_hashes {
        let root = elections.candidate_root(block_hash);
        match elections.route_signed_vote(
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
                if let Some(election) = root.and_then(|root| elections.get_election(&root)) {
                    tracing::debug!(hash = %block_hash, voter = %vote.voter, is_final = vote.is_final,
                        threshold = election.confirmation_threshold(), final_tally = ?election.final_tally,
                        soft_quorum = election.has_quorum(), confirmed = election.is_confirmed(),
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

// Skip already stored blocks before queueing. This is not an authentication
// cache: a failed lookup still enters validation,
// and a later rollback allows the same payload to be queued again.
async fn enqueue_network_block(
    store: &(dyn BlockStore + Sync),
    queue: &BlockPriorityQueue,
    block: burst_ledger::StateBlock,
    source_ip: std::net::IpAddr,
) -> Option<bool> {
    if store.exists(&block.hash).unwrap_or(false) { return None; }
    Some(queue.push_from_peer(block, source_ip).await)
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn is_deduplicated_gossip(message: &WireMessage) -> bool {
    matches!(message, WireMessage::Block(_))
}

/// Relay caller-authenticated votes using one encoding for votes and acknowledgments.
/// The bounded digest cache suppresses cycles while it retains the entry.
/// True means a new relay attempt, not successful delivery to any peer.
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
        .apply_digest(&MessageDedup::hash_message(&bytes))
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
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn stored_network_duplicates_do_not_fill_ingress_and_rollback_allows_retry() {
        use super::*;
        let store = burst_nullables::NullStore::new();
        let queue = BlockPriorityQueue::new(1);
        let original = crate::genesis_state::block(
            burst_types::NetworkId::Dev, &burst_types::ProtocolParams::default());
        store.put_block(&original.hash, &bincode::serialize(&original).unwrap()).unwrap();
        assert_eq!(enqueue_network_block(&store, &queue, original.clone(), "127.0.0.1".parse().unwrap()).await, None);
        assert!(queue.is_empty().await);
        store.delete_block(&original.hash).unwrap();
        assert_eq!(enqueue_network_block(&store, &queue, original.clone(), "127.0.0.1".parse().unwrap()).await, Some(true));
        assert_eq!(queue.pop().await.hash, original.hash);
        assert_eq!(enqueue_network_block(&store, &queue, original, "127.0.0.1".parse().unwrap()).await, Some(true));
    }

    #[tokio::test]
    async fn peer_activity_coalesces_same_second_without_losing_changed_time() {
        use super::*;
        let manager = RwLock::new(PeerManager::new(4));
        assert!(manager.write().await.register_connection(
            "peer".into(), burst_messages::PeerAddress { ip: "127.0.0.1".into(), port: 7000 }, true, 0));
        let mut touched = None;
        touch_peer_second(&manager, "peer", &mut touched, 10).await;
        let held = manager.write().await;
        // An unchanged timestamp needs neither a read nor a write lock.
        tokio::time::timeout(Duration::from_millis(100),
            touch_peer_second(&manager, "peer", &mut touched, 10)).await.unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(10),
            touch_peer_second(&manager, "peer", &mut touched, 11)).await.is_err());
        assert_eq!(touched, Some(10), "cancelled write must not advance the cursor");
        drop(held);
        for now in [11, 9] {
            touch_peer_second(&manager, "peer", &mut touched, now).await;
            assert_eq!(manager.read().await.iter_connected().next().unwrap().1.last_seen_secs, now);
        }
        // A new reader starts without a cursor, including after reconnect.
        touched = None;
        touch_peer_second(&manager, "peer", &mut touched, 9).await;
        assert_eq!(touched, Some(9));
    }

    #[test]
    fn verified_vote_cache_does_not_admit_invalid_or_modified_signatures() {
        use super::*;
        let key = burst_crypto::keypair_from_seed(&[7; 32]);
        let rep = burst_crypto::derive_address(&key.public);
        let mut generator = burst_consensus::VoteGenerator::new(rep.clone(), key.private.0);
        let signed = generator.generate_final_vote(BlockHash::new([1; 32]));
        let vote = crate::wire_message::WireVote {
            voter: rep, block_hashes: vec![signed.block_hash], is_final: true,
            timestamp: signed.timestamp, sequence: signed.sequence, signature: signed.signature,
        };
        let mut cache = MessageDedup::new(2);
        let mut bad = vote.clone();
        bad.signature.0[0] ^= 1;
        for _ in 0..2 { assert!(!is_vote_signature_valid_cached(&bad, &mut cache)); }
        assert!(cache.is_empty());
        for _ in 0..3 { assert!(is_vote_signature_valid_cached(&vote, &mut cache)); }
        assert_eq!(cache.len(), 1);
        for field in 0..6 {
            let mut changed = vote.clone();
            match field {
                0 => changed.signature.0[0] ^= 1,
                1 => changed.sequence += 1,
                2 => changed.timestamp += 1,
                3 => changed.is_final = false,
                4 => changed.block_hashes[0] = BlockHash::new([2; 32]),
                _ => changed.voter = burst_types::WalletAddress::new("brst_invalid"),
            }
            assert!(!is_vote_signature_valid_cached(&changed, &mut cache));
            assert_eq!(cache.len(), 1);
        }
        for byte in [2, 3] {
            let signed = generator.generate_final_vote(BlockHash::new([byte; 32]));
            let mut next = vote.clone();
            next.block_hashes = vec![signed.block_hash];
            next.sequence = signed.sequence;
            next.timestamp = signed.timestamp;
            next.signature = signed.signature;
            assert!(is_vote_signature_valid_cached(&next, &mut cache));
        }
        assert_eq!(cache.len(), 2);
        assert!(is_vote_signature_valid_cached(&vote, &mut cache));
        assert_eq!(cache.len(), 2);
    }

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
        LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
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

    #[tokio::test]
    async fn pending_admission_transfers_to_socket_and_releases_after_writer_stops() {
        use super::*;
        let mut registry = ConnectionRegistry::new();
        registry.configure_network_limits(4, PeerLimits { per_ip: 1, per_subnet: 4, outbound_reserved: 1 }, &BandwidthLimits::default());
        let (client, server) = socket_pair().await;
        let ip = server.peer_addr().unwrap().ip();
        let permit = registry.admit(ip, Direction::Inbound).unwrap();
        assert!(registry.admit(ip, Direction::Inbound).is_none());
        assert!(registry.register_admitted("peer".into(), Direction::Inbound, server.into_split().1, "local", permit).is_some());
        assert!(registry.admit(ip, Direction::Inbound).is_none());
        assert!(registry.remove("peer"));
        for task in registry.take_tasks() { task.await.unwrap(); }
        assert!(registry.admit(ip, Direction::Inbound).is_some());
        drop(client);
    }

    #[tokio::test]
    async fn full_bulk_queue_cannot_displace_control_message() {
        use super::*;
        let mut registry = ConnectionRegistry::new();
        let (mut client, server) = socket_pair().await;
        assert_ne!(registry.insert("peer".into(), server.into_split().1), 0);
        for _ in 0..CHANNEL_QUEUE_CAPACITY { assert!(registry.send("peer", vec![0; 8])); }
        assert!(!registry.send("peer", vec![0; 8]));
        let control = WireMessage::TelemetryReq.encode().unwrap();
        assert!(registry.send("peer", control.clone()));
        let mut header = [0; 4];
        tokio::time::timeout(Duration::from_secs(2), client.read_exact(&mut header)).await.unwrap().unwrap();
        let mut body = vec![0; u32::from_be_bytes(header) as usize];
        client.read_exact(&mut body).await.unwrap();
        assert_eq!(body, control);
        registry.close();
        for task in registry.take_tasks() { let _ = task.await; }
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
        let store = Arc::new(LmdbStore::open(directory.path(), 32, 16 << 20).unwrap());
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
        let generator = Arc::new(Mutex::new(burst_consensus::VoteGenerator::new(
            address.clone(),
            [0; 32],
        )));
        let (feedback, _received) = mpsc::channel(4);
        let (mut client, server) = socket_pair().await;
        let (reader, writer) = server.into_split();
        registry.write().await.insert("peer".into(), writer);
        let block = WireMessage::Block(Box::new(burst_ledger::genesis::legacy_genesis_fixture(
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
            // A request must not hold up following frames while signing is busy.
            let held_signer = generator.lock().await;
            let request = WireMessage::ConfirmReq(crate::wire_message::ConfirmReqMsg {
                block_hashes: vec![BlockHash::new([77; 32])], remaining_hops: 0,
            });
            for operational in [false, true] {
                if operational {
                    registry.write().await.finish_recovery();
                    let bytes = request.encode().unwrap();
                    client.write_u32(bytes.len() as u32).await.unwrap();
                    client.write_all(&bytes).await.unwrap();
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
            // The same payload can re-enter the queue after the dedup window.
            // This fixture removes it directly without running admission.
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
            drop(held_signer);
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
    async fn authenticated_wire_votes_dispatch_while_relay_dedup_is_held() {
        use super::*;
        for acknowledged in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let store = Arc::new(LmdbStore::open(directory.path(), 32, 16 << 20).unwrap());
            let registry = RwLock::new(ConnectionRegistry::new());
            let queue = BlockPriorityQueue::new(4);
            let keys = burst_crypto::keypair_from_seed(&[19; 32]);
            let voter = burst_crypto::derive_address(&keys.public);
            let weights = Arc::new(RwLock::new(RepWeightCache::new()));
            weights.write().await.rebuild_from_accounts([(true, voter.clone())].into_iter());
            let hash = BlockHash::new([1; 32]);
            let mut active = ActiveElections::new(2, weights.read().await.total_weight());
            active.register_candidate(hash, hash, Timestamp::new(unix_now_secs())).unwrap();
            let elections = Arc::new(RwLock::new(active));
            registry.write().await.start_vote_dispatch(elections.clone(), weights.clone());
            let peers = RwLock::new(PeerManager::new(4));
            let dedup = Mutex::new(MessageDedup::new(100));
            let sampler = Mutex::new(OnlineWeightSampler::new(60));
            let generator = Arc::new(Mutex::new(burst_consensus::VoteGenerator::new(
                voter.clone(), [0; 32],
            )));
            let (feedback, _received) = mpsc::channel(4);
            let (mut client, server) = socket_pair().await;
            let (reader, writer) = server.into_split();
            registry.write().await.insert("peer".into(), writer);
            let mut vote = crate::wire_message::WireVote {
                voter, block_hashes: vec![hash], is_final: true,
                timestamp: unix_now_secs() * 1000, sequence: 1,
                signature: burst_types::Signature([0; 64]),
            };
            vote.signature = burst_crypto::sign_message(&burst_consensus::vote_signing_payload(
                &vote.voter, vote.is_final, vote.timestamp, vote.sequence, &vote.block_hashes,
            ), &keys.private);
            let message = if acknowledged {
                WireMessage::ConfirmAck(crate::wire_message::ConfirmAckMsg { vote })
            } else {
                WireMessage::Vote(vote)
            };
            let traffic = async {
                let held_relay = dedup.lock().await;
                let bytes = message.encode().unwrap();
                client.write_u32(bytes.len() as u32).await.unwrap();
                client.write_all(&bytes).await.unwrap();
                while !elections.read().await.get_election(&hash).unwrap().is_confirmed() {
                    tokio::task::yield_now().await;
                }
                assert_eq!(held_relay.len(), 0);
                drop(held_relay);
                client.shutdown().await.unwrap();
            };
            let reading = peer_read_loop("peer", reader, &queue, &elections, &weights,
                &peers, &dedup, &sampler, &registry, &store, BlockHash::ZERO,
                &feedback, &generator);
            let (_, result) = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(traffic, reading)
            }).await.unwrap();
            assert!(result.is_err());
            assert_eq!(dedup.lock().await.len(), 1, "relay still runs after local dispatch");
            registry.write().await.close();
            for task in registry.write().await.take_tasks() { let _ = task.await; }
        }
    }

    #[tokio::test]
    async fn redundant_votes_share_election_reads_but_new_finals_require_writer() {
        use super::*;
        use crate::wire_message::WireVote;
        let keys = burst_crypto::keypair_from_seed(&[39; 32]);
        let voter = burst_crypto::derive_address(&keys.public);
        let hash = BlockHash::new([39; 32]);
        let now = Timestamp::new(unix_now_secs());
        let weights = RwLock::new(RepWeightCache::new());
        weights.write().await.rebuild_from_accounts([(true, voter.clone())].into_iter());
        let weight = weights.read().await.weight(&voter);
        let mut active = ActiveElections::new(2, weight * 10);
        active.register_candidate(hash, hash, now).unwrap();
        active.route_signed_vote(burst_consensus::VoteInfo::new(voter.clone(), hash,
            weight, false, now, 10), now).unwrap();
        let elections = RwLock::new(active);
        let mut vote = WireVote { voter, block_hashes: vec![hash], is_final: false,
            timestamp: now.as_secs() * 1_000, sequence: 10, signature: burst_types::Signature([0; 64]) };
        let sign = |vote: &mut WireVote| {
            vote.signature = burst_crypto::sign_message(&burst_consensus::vote_signing_payload(
                &vote.voter, vote.is_final, vote.timestamp, vote.sequence, &vote.block_hashes), &keys.private);
            assert!(is_vote_signature_valid(vote));
        };
        sign(&mut vote);
        let held = elections.read().await;
        tokio::time::timeout(Duration::from_secs(1), dispatch_vote("peer", &vote, &elections, &weights))
            .await.expect("redundant authenticated soft vote must not acquire the writer");
        vote.is_final = true;
        sign(&mut vote);
        let routing = dispatch_vote("peer", &vote, &elections, &weights);
        tokio::pin!(routing);
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut routing).await.is_err());
        drop(held);
        tokio::time::timeout(Duration::from_secs(1), routing).await.unwrap();
        assert!(elections.read().await.get_election(&hash).unwrap().last_votes[&vote.voter].is_final);
    }

    #[tokio::test]
    async fn authenticated_early_vote_confirms_on_registration_without_retry() {
        use super::*;
        let keys = burst_crypto::keypair_from_seed(&[19; 32]);
        let voter = burst_crypto::derive_address(&keys.public);
        let hash = BlockHash::new([1; 32]);
        let registry = RwLock::new(ConnectionRegistry::new());
        let dedup = Mutex::new(MessageDedup::new(100));
        let weights = RwLock::new(RepWeightCache::new());
        weights
            .write()
            .await
            .rebuild_from_accounts([(true, voter.clone())].into_iter());
        let elections = RwLock::new(ActiveElections::new(2, weights.read().await.total_weight()));
        let mut vote = crate::wire_message::WireVote {
            voter,
            block_hashes: vec![hash],
            is_final: true,
            timestamp: unix_now_secs(),
            sequence: 1,
            signature: burst_types::Signature([0; 64]),
        };
        vote.signature = burst_crypto::sign_message(&burst_consensus::vote_signing_payload(
            &vote.voter, vote.is_final, vote.timestamp, vote.sequence, &vote.block_hashes,
        ), &keys.private);
        let mut verified = MessageDedup::new(2);
        assert!(is_vote_signature_valid_cached(&vote, &mut verified));
        assert!(relay_vote("first", &vote, &registry, &dedup).await);
        dispatch_vote("first", &vote, &elections, &weights).await;
        elections
            .write()
            .await
            .register_candidate(hash, hash, Timestamp::new(unix_now_secs()))
            .unwrap();
        assert!(elections.read().await.get_election(&hash).unwrap().is_confirmed(),
            "authenticated early vote must be replayed without a network retry");
        assert!(is_vote_signature_valid_cached(&vote, &mut verified));
        assert_eq!(verified.len(), 1);
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
        assert!(!registry.send("a", vec![0; MAX_MESSAGE_BYTES + 1]));
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

    // These strings test lexicographic ordering; production uses node IDs.
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
