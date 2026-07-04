//! Connection registry — maps peer IDs to their TCP write halves.
//!
//! Shared between the P2P listener (which registers new connections) and
//! the outbound message drain (which writes framed messages to peers).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{Mutex, RwLock};

use burst_consensus::{ActiveElections, OnlineWeightSampler, RepWeightCache};
use burst_crypto::{decode_address, verify_signature};
use burst_ledger::{DagFrontier, StateBlock};
use burst_network::{BandwidthThrottle, MessageDedup, PeerManager, PeerTelemetry, SynCookies};
use burst_store::account::AccountStore;
use burst_store::block::BlockStore;
use burst_store_lmdb::LmdbStore;
use burst_types::{PublicKey, Signature, Timestamp, WalletAddress};

use crate::bootstrap::{BootstrapClient, BootstrapMessage, BootstrapServer};
use crate::metrics::NodeMetrics;
use crate::priority_queue::BlockPriorityQueue;
use crate::wire_message::{ConfirmAckMsg, TelemetryAckMessage, WireMessage, WireVote};

/// Maximum message body size (matches protocol codec limit).
const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024; // 16 MiB

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

/// Per-connection metadata used for deterministic dedup and generation-guarded
/// cleanup. Peers are identified by their handshake `node_id` (a `brst_`
/// address string) — NOT their IP — so nodes sharing an IP (localhost cluster,
/// or several nodes behind one NAT) are treated as distinct, matching rsnano.
struct ConnMeta {
    node_id: String,
    direction: Direction,
    /// Monotonic id distinguishing this physical connection from any prior one
    /// under the same peer_id, so a superseded socket's teardown can't evict
    /// the live entry.
    conn_id: u64,
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
    connections: HashMap<String, Arc<Mutex<OwnedWriteHalf>>>,
    throttles: HashMap<String, BandwidthThrottle>,
    meta: HashMap<String, ConnMeta>,
    next_conn_id: u64,
}

impl ConnectionRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            connections: HashMap::new(),
            throttles: HashMap::new(),
            meta: HashMap::new(),
            next_conn_id: 1,
        }
    }

    /// Register a peer's write half. If a previous connection existed for this
    /// peer, it is replaced (the old writer is dropped, closing its half).
    /// Assigns a fresh generation id (no dedup — for callers that already
    /// resolved identity). Prefer [`register_dedup`](Self::register_dedup).
    pub fn insert(&mut self, peer_id: String, writer: OwnedWriteHalf) -> u64 {
        let conn_id = self.next_conn_id;
        self.next_conn_id += 1;
        self.throttles.entry(peer_id.clone()).or_default();
        self.connections
            .insert(peer_id.clone(), Arc::new(Mutex::new(writer)));
        self.meta.insert(
            peer_id,
            ConnMeta {
                node_id: String::new(),
                direction: Direction::Outbound,
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
        peer_id: String,
        peer_node_id: String,
        direction: Direction,
        writer: OwnedWriteHalf,
        our_node_id: &str,
    ) -> Option<u64> {
        // Find a conflicting connection: same node_id, opposite direction.
        let conflict: Option<String> = self
            .meta
            .iter()
            .find(|(_, m)| m.node_id == peer_node_id && m.direction != direction)
            .map(|(id, _)| id.clone());

        if let Some(existing_id) = conflict {
            if prefer_new_on_conflict(our_node_id, &peer_node_id, direction) {
                // New wins — drop the existing (closes its socket; its read
                // loop later no-ops via the generation guard).
                self.remove(&existing_id);
            } else {
                // New loses — reject it (the existing connection survives).
                return None;
            }
        }

        let conn_id = self.next_conn_id;
        self.next_conn_id += 1;
        self.throttles.entry(peer_id.clone()).or_default();
        self.connections
            .insert(peer_id.clone(), Arc::new(Mutex::new(writer)));
        self.meta.insert(
            peer_id,
            ConnMeta {
                node_id: peer_node_id,
                direction,
                conn_id,
            },
        );
        Some(conn_id)
    }

    /// Remove a peer's write half, returning it if present.
    pub fn remove(&mut self, peer_id: &str) -> Option<Arc<Mutex<OwnedWriteHalf>>> {
        self.throttles.remove(peer_id);
        self.meta.remove(peer_id);
        self.connections.remove(peer_id)
    }

    /// Generation-guarded removal: only remove if the current entry is the
    /// same physical connection (matching `conn_id`). Prevents a superseded
    /// socket's cleanup from evicting the live connection that replaced it.
    pub fn remove_if(&mut self, peer_id: &str, conn_id: u64) -> bool {
        if self.meta.get(peer_id).is_some_and(|m| m.conn_id == conn_id) {
            self.remove(peer_id);
            true
        } else {
            false
        }
    }

    /// Check the outbound throttle for a peer. Returns `true` if the message
    /// can be sent (and consumes the bandwidth tokens), `false` if throttled.
    pub fn try_consume_outbound(&mut self, peer_id: &str, bytes: u64) -> bool {
        if let Some(throttle) = self.throttles.get_mut(peer_id) {
            throttle.try_consume(bytes)
        } else {
            true
        }
    }

    /// Look up a peer's write half (returns a cheaply cloned `Arc`).
    pub fn get(&self, peer_id: &str) -> Option<Arc<Mutex<OwnedWriteHalf>>> {
        self.connections.get(peer_id).cloned()
    }

    /// Number of registered connections.
    pub fn len(&self) -> usize {
        self.connections.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.connections.is_empty()
    }

    /// All registered peer IDs.
    pub fn peer_ids(&self) -> Vec<&String> {
        self.connections.keys().collect()
    }
}

impl Default for ConnectionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Write a length-prefixed frame (4-byte big-endian length + payload) to
/// the given write half. Returns `Ok(())` on success.
pub async fn write_framed(writer: &Mutex<OwnedWriteHalf>, payload: &[u8]) -> std::io::Result<()> {
    let mut w = writer.lock().await;
    let len_bytes = (payload.len() as u32).to_be_bytes();
    w.write_all(&len_bytes).await?;
    w.write_all(payload).await?;
    w.flush().await?;
    Ok(())
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
    syn_cookies: Option<Arc<Mutex<SynCookies>>>,
    peer_ip: String,
    frontier: Arc<RwLock<DagFrontier>>,
    store: Arc<LmdbStore>,
    our_params_hash: burst_types::BlockHash,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let result = peer_read_loop(
            &peer_id,
            reader,
            &block_queue,
            &active_elections,
            &rep_weights,
            &peer_manager,
            &message_dedup,
            &online_weight_sampler,
            syn_cookies.as_deref(),
            &peer_ip,
            &connection_registry,
            &frontier,
            &store,
            our_params_hash,
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
        let removed = {
            let mut registry = connection_registry.write().await;
            let removed = registry.remove_if(&peer_id, conn_id);
            metrics.peer_count.set(registry.len() as i64);
            removed
        };

        // Mark peer as disconnected in the peer manager only if this was the
        // live connection (not a superseded duplicate).
        if removed {
            let mut pm = peer_manager.write().await;
            pm.mark_disconnected(&peer_id);
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
    syn_cookies: Option<&Mutex<SynCookies>>,
    peer_ip: &str,
    connection_registry: &RwLock<ConnectionRegistry>,
    frontier: &RwLock<DagFrontier>,
    store: &LmdbStore,
    our_params_hash: burst_types::BlockHash,
) -> Result<(), std::io::Error> {
    // SYN cookie validation: inbound peers must respond with a signed cookie
    if let Some(cookies) = syn_cookies {
        let mut len_buf = [0u8; 4];
        match tokio::time::timeout(READ_TIMEOUT, reader.read_exact(&mut len_buf)).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "handshake timeout",
                ));
            }
        }

        let body_len = u32::from_be_bytes(len_buf) as usize;
        if body_len > MAX_MESSAGE_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "handshake message too large",
            ));
        }

        let mut body = vec![0u8; body_len];
        match tokio::time::timeout(READ_TIMEOUT, reader.read_exact(&mut body)).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "handshake body timeout",
                ));
            }
        }

        match bincode::deserialize::<WireMessage>(&body) {
            Ok(WireMessage::Handshake(hs)) => {
                if let Some(sig) = &hs.cookie_signature {
                    let mut cookie_mgr = cookies.lock().await;
                    if !cookie_mgr.verify(peer_ip, &hs.node_id, sig) {
                        tracing::warn!(peer = %peer_id, "SYN cookie verification failed");
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "SYN cookie verification failed",
                        ));
                    }
                    tracing::debug!(
                        peer = %peer_id,
                        node_id = %hs.node_id,
                        "SYN cookie verified"
                    );
                } else {
                    tracing::warn!(peer = %peer_id, "handshake missing cookie signature");
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "handshake missing cookie signature",
                    ));
                }
            }
            _ => {
                tracing::warn!(peer = %peer_id, "first message was not a handshake");
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "expected handshake as first message",
                ));
            }
        }
    }

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

        // Message-level deduplication: skip if we've already seen this payload.
        {
            let msg_hash = MessageDedup::hash_message(&body);
            let mut dedup = message_dedup.lock().await;
            if dedup.is_duplicate(&msg_hash) {
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
        match bincode::deserialize::<WireMessage>(&body) {
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
                    if rw.weight(&vote.voter) == 0 {
                        tracing::trace!(voter = %vote.voter, "ignoring zero-weight vote");
                        continue;
                    }
                }
                {
                    let now = unix_now_secs();
                    let mut sampler = online_weight_sampler.lock().await;
                    sampler.record_vote(&vote.voter, now);
                }
                dispatch_vote(peer_id, &vote, active_elections, rep_weights).await;
            }
            Ok(WireMessage::ConfirmReq(req)) => {
                tracing::debug!(
                    peer = %peer_id,
                    hashes = req.block_hashes.len(),
                    "received confirm_req"
                );
                let ae = active_elections.read().await;
                let mut confirmed_hashes = Vec::new();
                for hash in &req.block_hashes {
                    if let Some(election) = ae.get_election(hash) {
                        if election.is_confirmed() {
                            confirmed_hashes.push(*hash);
                        }
                    }
                }
                drop(ae);

                if !confirmed_hashes.is_empty() {
                    let vote = WireVote {
                        voter: WalletAddress::new("brst_node"),
                        block_hashes: confirmed_hashes,
                        is_final: true,
                        timestamp: unix_now_secs(),
                        sequence: 0,
                        signature: Signature([0u8; 64]),
                    };
                    let ack = WireMessage::ConfirmAck(ConfirmAckMsg { vote });
                    if let Ok(bytes) = bincode::serialize(&ack) {
                        let registry = connection_registry.read().await;
                        if let Some(writer) = registry.get(peer_id) {
                            if let Err(e) = write_framed(&writer, &bytes).await {
                                tracing::warn!(peer = %peer_id, "failed to send confirm_ack: {e}");
                            }
                        }
                    }
                }
            }
            Ok(WireMessage::ConfirmAck(ack)) => {
                if !is_vote_signature_valid(&ack.vote) {
                    continue;
                }
                {
                    let rw = rep_weights.read().await;
                    if rw.weight(&ack.vote.voter) == 0 {
                        tracing::trace!(voter = %ack.vote.voter, "ignoring zero-weight confirm_ack");
                        continue;
                    }
                }
                {
                    let now = unix_now_secs();
                    let mut sampler = online_weight_sampler.lock().await;
                    sampler.record_vote(&ack.vote.voter, now);
                }
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
                        if !pa.ip.is_empty() && pa.ip != "0.0.0.0" && pa.ip != "::" {
                            pm.set_peering_addr(peer_id, pa);
                        }
                    }
                }
            }
            Ok(WireMessage::Bootstrap(msg)) => match msg {
                BootstrapMessage::FrontierReq {
                    start_account,
                    max_count,
                } => {
                    tracing::debug!(peer = %peer_id, "received frontier request");
                    let frontier_entries: Vec<_> = {
                        let f = frontier.read().await;
                        f.iter().map(|(a, h)| (a.clone(), *h)).collect()
                    };
                    let resp = BootstrapServer::handle_frontier_req(
                        &start_account,
                        max_count,
                        &frontier_entries,
                    );
                    let wire_resp = WireMessage::Bootstrap(resp);
                    if let Ok(bytes) = bincode::serialize(&wire_resp) {
                        let registry = connection_registry.read().await;
                        if let Some(writer) = registry.get(peer_id) {
                            if let Err(e) = write_framed(&writer, &bytes).await {
                                tracing::warn!(peer = %peer_id, "failed to send frontier response: {e}");
                            }
                        }
                    }
                }
                BootstrapMessage::FrontierResp {
                    frontiers,
                    has_more,
                } => {
                    tracing::info!(
                        peer = %peer_id,
                        count = frontiers.len(),
                        has_more,
                        "received frontier response"
                    );
                    let local_frontiers: Vec<_> = {
                        let f = frontier.read().await;
                        f.iter().map(|(a, h)| (a.clone(), *h)).collect()
                    };
                    let mut client = BootstrapClient::new(10_000);
                    let requests =
                        client.process_frontier_resp(&frontiers, has_more, &local_frontiers);
                    for req in requests {
                        let wire_req = WireMessage::Bootstrap(req);
                        if let Ok(bytes) = bincode::serialize(&wire_req) {
                            let registry = connection_registry.read().await;
                            if let Some(writer) = registry.get(peer_id) {
                                if let Err(e) = write_framed(&writer, &bytes).await {
                                    tracing::warn!(peer = %peer_id, "failed to send bootstrap request: {e}");
                                }
                            }
                        }
                    }
                }
                BootstrapMessage::BulkPullReq { account, end } => {
                    tracing::debug!(peer = %peer_id, %account, "received bulk pull request");
                    let block_store = store.block_store();
                    let resp = BootstrapServer::handle_bulk_pull_req(
                        &account,
                        &end,
                        |hash| block_store.get_block(hash).ok(),
                        |acct| block_store.get_account_blocks(acct).unwrap_or_default(),
                    );
                    let wire_resp = WireMessage::Bootstrap(resp);
                    if let Ok(bytes) = bincode::serialize(&wire_resp) {
                        let registry = connection_registry.read().await;
                        if let Some(writer) = registry.get(peer_id) {
                            if let Err(e) = write_framed(&writer, &bytes).await {
                                tracing::warn!(peer = %peer_id, "failed to send bulk pull response: {e}");
                            }
                        }
                    }
                }
                BootstrapMessage::BulkPullResp { blocks } => {
                    let client = BootstrapClient::new(10_000);
                    let deserialized = client.process_bulk_pull_resp(&blocks);
                    tracing::info!(
                        peer = %peer_id,
                        count = deserialized.len(),
                        "received bulk pull response"
                    );
                    for block in deserialized {
                        if !block_queue.push(block).await {
                            tracing::warn!(peer = %peer_id, "block queue full during bootstrap");
                            break;
                        }
                    }
                }
                BootstrapMessage::BlockReq { hash } => {
                    tracing::debug!(peer = %peer_id, %hash, "received block request");
                    let block_store = store.block_store();
                    let resp =
                        BootstrapServer::handle_block_req(&hash, |h| block_store.get_block(h).ok());
                    let wire_resp = WireMessage::Bootstrap(resp);
                    if let Ok(bytes) = bincode::serialize(&wire_resp) {
                        let registry = connection_registry.read().await;
                        if let Some(writer) = registry.get(peer_id) {
                            if let Err(e) = write_framed(&writer, &bytes).await {
                                tracing::warn!(peer = %peer_id, "failed to send block response: {e}");
                            }
                        }
                    }
                }
                BootstrapMessage::BlockResp { block } => {
                    if let Some(bytes) = block {
                        if let Ok(blk) = bincode::deserialize::<StateBlock>(&bytes) {
                            tracing::debug!(peer = %peer_id, hash = %blk.hash, "received block response");
                            if !block_queue.push(blk).await {
                                tracing::warn!(peer = %peer_id, "block queue full during block fetch");
                            }
                        }
                    }
                }
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
            Ok(WireMessage::VerificationVote(msg)) => {
                tracing::debug!(
                    peer = %peer_id,
                    target = %msg.target,
                    voter = %msg.voter,
                    "received verification vote"
                );
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
                    protocol_version: 1,
                    uptime: 0,
                    genesis_hash: burst_types::BlockHash::ZERO,
                    major_version: 0,
                    minor_version: 1,
                    patch_version: 0,
                    timestamp: unix_now_secs(),
                    params_hash: our_params_hash,
                });
                if let Ok(bytes) = bincode::serialize(&ack) {
                    let registry = connection_registry.read().await;
                    if let Some(writer) = registry.get(peer_id) {
                        if let Err(e) = write_framed(&writer, &bytes).await {
                            tracing::warn!(peer = %peer_id, "failed to send telemetry ack: {e}");
                        }
                    }
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
/// The signed message is: timestamp (big-endian u64) || block_hashes (each 32 bytes).
/// The signature must be valid for the voter's public key.
fn is_vote_signature_valid(vote: &crate::wire_message::WireVote) -> bool {
    let pubkey_bytes = match decode_address(vote.voter.as_str()) {
        Some(bytes) => bytes,
        None => {
            tracing::warn!(voter = %vote.voter, "rejected vote: unable to decode voter address");
            return false;
        }
    };

    let mut msg = Vec::with_capacity(8 + vote.block_hashes.len() * 32);
    msg.extend_from_slice(&vote.timestamp.to_be_bytes());
    for hash in &vote.block_hashes {
        msg.extend_from_slice(hash.as_bytes());
    }

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
        match ae.process_vote(
            block_hash,
            &vote.voter,
            *block_hash,
            weight,
            vote.is_final,
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
            Ok(None) => {}
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

#[cfg(test)]
mod tests {
    use super::{prefer_new_on_conflict, Direction};

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
