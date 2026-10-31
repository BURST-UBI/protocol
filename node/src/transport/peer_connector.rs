//! Outbound TCP connection, mutual authentication and peer registration.
//! Shared by bootstrap, periodic discovery and the peer cache connector.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, RwLock};

use burst_consensus::{ActiveElections, OnlineWeightSampler, RepWeightCache};
use burst_messages::PeerAddress;
use burst_network::{MessageDedup, PeerManager};
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, NetworkId};

use crate::connection_registry::{spawn_peer_read_loop, ConnectionRegistry, Direction};
use crate::metrics::NodeMetrics;
use crate::priority_queue::BlockPriorityQueue;
use crate::wire_message::HandshakeMsg;

/// Timeout for the initial TCP connection attempt.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Handshake allowance in the total connection-attempt deadline.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Services and local identity used to connect and start a peer read loop.
pub struct PeerConnectorContext {
    pub peer_manager: Arc<RwLock<PeerManager>>,
    pub connection_registry: Arc<RwLock<ConnectionRegistry>>,
    pub block_queue: Arc<BlockPriorityQueue>,
    pub metrics: Arc<NodeMetrics>,
    pub active_elections: Arc<RwLock<ActiveElections>>,
    pub rep_weights: Arc<RwLock<RepWeightCache>>,
    pub vote_generator: Arc<Mutex<burst_consensus::VoteGenerator>>,
    pub message_dedup: Arc<Mutex<MessageDedup>>,
    pub online_weight_sampler: Arc<Mutex<OnlineWeightSampler>>,
    pub store: Arc<LmdbStore>,
    pub node_private_key: burst_types::PrivateKey,
    pub node_address: burst_types::WalletAddress,
    pub params_hash: BlockHash,
    /// Our network id. The peer's cookie challenge must carry a matching
    /// `network_id` or we drop the connection — hard isolation between
    /// Dev/Test/Live independent of `params_hash`.
    pub network: NetworkId,
    /// Our local listening port. It is not necessarily our NAT's external port
    /// and must never be used as the connection identity.
    pub peering_port: u16,
    /// Channel for the read loop to report ascending-bootstrap ack results to
    /// the bootstrap task.
    pub bootstrap_feedback: tokio::sync::mpsc::Sender<crate::bootstrap::BootstrapFeedback>,
}

/// Result of a successful outbound connection.
pub struct ConnectedPeer {
    pub peer_id: String,
    pub peer_addr: PeerAddress,
}

struct DialGuard {
    registry: Arc<RwLock<ConnectionRegistry>>,
    address: String,
    finished: bool,
}
impl Drop for DialGuard {
    fn drop(&mut self) {
        if self.finished { return; }
        let registry = self.registry.clone();
        let address = self.address.clone();
        // Cancellation schedules dial-slot cleanup while the runtime is available.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { registry.write().await.end_dial(&address, false); });
        }
    }
}

/// Resolve `host:port`, connect, authenticate both peers and start a read loop.
/// Tries at most sixteen resolved addresses within a shared 15-second deadline;
/// registry lock waits before and after the attempt are outside that deadline.
/// Losing a duplicate-connection tie-break returns an error even if the peer
/// remains connected through the surviving socket.
pub async fn connect_to_peer(
    addr_str: &str,
    ctx: &PeerConnectorContext,
) -> Result<ConnectedPeer, String> {
    // Reserve this address string against concurrent outbound attempts.
    {
        let mut reg = ctx.connection_registry.write().await;
        if !reg.begin_dial(addr_str) {
            return Err("address connected, dialing, or waiting for retry".into());
        }
    }
    let mut guard = DialGuard { registry: ctx.connection_registry.clone(), address: addr_str.to_owned(), finished: false };
    let result = tokio::time::timeout(
        CONNECT_TIMEOUT + HANDSHAKE_TIMEOUT,
        connect_to_peer_inner(addr_str, ctx),
    )
    .await
    .unwrap_or_else(|_| {
        Err(format!(
            "connection attempt deadline exceeded for {addr_str}"
        ))
    });
    {
        let mut reg = ctx.connection_registry.write().await;
        reg.end_dial(addr_str, result.is_ok());
        guard.finished = true;
    }
    result
}

async fn connect_to_peer_inner(
    addr_str: &str,
    ctx: &PeerConnectorContext,
) -> Result<ConnectedPeer, String> {
    // Resolve first, then reserve the observed address before opening a socket.
    // The outer attempt timeout also bounds DNS and all address fallbacks.
    let addresses = tokio::net::lookup_host(addr_str).await
        .map_err(|e| format!("cannot resolve {addr_str}: {e}"))?;
    let mut connected = None;
    for address in addresses.take(16) {
        let Some(permit) = ctx.connection_registry.read().await.admit(address.ip(), Direction::Outbound) else { continue; };
        if let Ok(Ok(stream)) = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(address)).await {
            connected = Some((stream, permit));
            break;
        }
    }
    let (stream, admission) = connected.ok_or_else(|| format!("no permitted reachable address for {addr_str}"))?;
    burst_network::connection::configure_peer_stream(&stream)
        .map_err(|e| format!("TCP setup for {addr_str} failed: {e}"))?;
    let address = stream.peer_addr().map_err(|e| e.to_string())?;
    let peer_addr = PeerAddress { ip: address.ip().to_string(), port: address.port() };

    let (read_half, mut write_half) = stream.into_split();

    let mut read_half = read_half;
    let remote = crate::peer_handshake::outbound(
        &mut read_half,
        &mut write_half,
        HandshakeMsg {
            block_version: burst_ledger::CURRENT_BLOCK_VERSION,
            wire_version: burst_types::PROTOCOL_VERSION,
            node_id: ctx.node_address.clone(),
            cookie: None,
            cookie_signature: None,
            params_hash: ctx.params_hash,
            network_id: ctx.network,
            peering_port: ctx.peering_port,
        },
        &ctx.node_private_key,
    )
    .await?;
    let now = unix_now_secs();
    let peer_node_id = remote.node_id.as_str().to_owned();
    let our_node_id = ctx.node_address.as_str().to_string();
    let peer_id = peer_node_id.clone();

    // Register with deterministic dedup keyed by node_id: if the peer already
    // dialed us (opposite direction), keep exactly one — the connection from
    // the lower node_id — so simultaneous connects don't churn. `None` means
    // this outbound lost the tie-break; close it.
    let conn_id = {
        let mut pm = ctx.peer_manager.write().await;
        let mut registry = ctx.connection_registry.write().await;
        match registry.register_admitted(peer_node_id, Direction::Outbound, write_half, &our_node_id, admission.clone()) {
            Some(id) => {
                if !pm.register_connection(peer_id.clone(), peer_addr.clone(), true, now) {
                    registry.remove_if(&peer_id, id);
                    ctx.metrics.peer_count.set(registry.len() as i64);
                    return Err("peer banned or connection capacity reached".into());
                }
                ctx.metrics.peer_count.set(registry.len() as i64);
                id
            }
            None => {
                pm.record_dialed_address(&peer_id, peer_addr.clone());
                tracing::debug!(peer = %peer_id, "outbound superseded by peer's inbound (tie-break)");
                return Err("connection deduplicated".into());
            }
        }
    };

    // Both handshake proofs have been validated.
    let mut registry = ctx.connection_registry.write().await;
    let reader_task = spawn_peer_read_loop(
        peer_id.clone(),
        conn_id,
        read_half,
        Arc::clone(&ctx.block_queue),
        Arc::clone(&ctx.connection_registry),
        Arc::clone(&ctx.peer_manager),
        Arc::clone(&ctx.metrics),
        Arc::clone(&ctx.active_elections),
        Arc::clone(&ctx.rep_weights),
        Arc::clone(&ctx.message_dedup),
        Arc::clone(&ctx.online_weight_sampler),
        Arc::clone(&ctx.store),
        ctx.params_hash,
        ctx.bootstrap_feedback.clone(),
        Arc::clone(&ctx.vote_generator),
        admission,
    );
    registry.track_task(reader_task);

    Ok(ConnectedPeer { peer_id, peer_addr })
}

/// Match an address string against connected peers without DNS resolution.
/// Hostname aliases are not normalized to the same endpoint.
pub async fn is_peer_connected(addr_str: &str, pm: &RwLock<PeerManager>) -> bool {
    let parts: Vec<&str> = addr_str.rsplitn(2, ':').collect();
    if parts.len() == 2 {
        let key = format!("{}:{}", parts[1], parts[0]);
        let pm = pm.read().await;
        pm.is_connected(&key)
    } else {
        false
    }
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
