//! Reusable outbound peer connection logic.
//!
//! Extracts the TCP connect → cookie handshake → registration flow used by
//! the bootstrap task, the reachout loop, and the peer cache connector into
//! a single shared function.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{Mutex, RwLock};

use burst_consensus::{ActiveElections, OnlineWeightSampler, RepWeightCache};
use burst_ledger::DagFrontier;
use burst_messages::PeerAddress;
use burst_network::{MessageDedup, PeerManager};
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, NetworkId};

use crate::connection_registry::{spawn_peer_read_loop, ConnectionRegistry, Direction};
use crate::metrics::NodeMetrics;
use crate::priority_queue::BlockPriorityQueue;
use crate::wire_message::{HandshakeMsg, WireMessage};

/// Timeout for the initial TCP connection attempt.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for reading the cookie challenge from the remote peer.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Shared dependencies needed by `connect_to_peer`. All `Arc` fields are
/// cheaply cloneable; the private key bytes are copied manually because
/// `PrivateKey` implements `ZeroizeOnDrop` and intentionally omits `Clone`.
pub struct PeerConnectorContext {
    pub peer_manager: Arc<RwLock<PeerManager>>,
    pub connection_registry: Arc<RwLock<ConnectionRegistry>>,
    pub block_queue: Arc<BlockPriorityQueue>,
    pub metrics: Arc<NodeMetrics>,
    pub active_elections: Arc<RwLock<ActiveElections>>,
    pub rep_weights: Arc<RwLock<RepWeightCache>>,
    pub message_dedup: Arc<Mutex<MessageDedup>>,
    pub online_weight_sampler: Arc<Mutex<OnlineWeightSampler>>,
    pub frontier: Arc<RwLock<DagFrontier>>,
    pub store: Arc<LmdbStore>,
    pub node_private_key: burst_types::PrivateKey,
    pub node_address: burst_types::WalletAddress,
    pub params_hash: BlockHash,
    /// Our network id. The peer's cookie challenge must carry a matching
    /// `network_id` or we drop the connection — hard isolation between
    /// Dev/Test/Live independent of `params_hash`.
    pub network: NetworkId,
    /// Our listening/peering port, sent in the handshake so the peer keys us by
    /// our dialable address instead of the ephemeral source port.
    pub peering_port: u16,
}

/// Result of a successful outbound connection.
pub struct ConnectedPeer {
    pub peer_id: String,
    pub peer_addr: PeerAddress,
}

/// Attempt an outbound TCP connection to `addr_str` ("ip:port"), perform the
/// cookie handshake, register the peer in the connection registry and peer
/// manager, and spawn a read loop.
///
/// Returns `Ok(ConnectedPeer)` on success, `Err` on any failure.
pub async fn connect_to_peer(
    addr_str: &str,
    ctx: &PeerConnectorContext,
) -> Result<ConnectedPeer, String> {
    // Claim a dial slot (attempts list) so concurrent tasks don't both dial
    // the same peer and create duplicate connections. Always released below.
    {
        let mut reg = ctx.connection_registry.write().await;
        if !reg.begin_dial(addr_str) {
            return Err("already dialing or connected to this address".into());
        }
    }
    let result = connect_to_peer_inner(addr_str, ctx).await;
    {
        let mut reg = ctx.connection_registry.write().await;
        reg.end_dial(addr_str);
    }
    result
}

async fn connect_to_peer_inner(
    addr_str: &str,
    ctx: &PeerConnectorContext,
) -> Result<ConnectedPeer, String> {
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect(addr_str))
        .await
        .map_err(|_| format!("connection timed out to {addr_str}"))?
        .map_err(|e| format!("TCP connect to {addr_str} failed: {e}"))?;

    let parts: Vec<&str> = addr_str.rsplitn(2, ':').collect();
    let (port, ip) = if parts.len() == 2 {
        (
            parts[0].parse::<u16>().unwrap_or(7075),
            parts[1].to_string(),
        )
    } else {
        (7075, addr_str.to_string())
    };
    let peer_addr = PeerAddress {
        ip: ip.clone(),
        port,
    };
    let peer_id = format!("{ip}:{port}");

    let (read_half, mut write_half) = stream.into_split();

    // Read the cookie challenge from the peer
    let mut reader = tokio::io::BufReader::new(read_half);
    let mut remote_node_id: Option<burst_types::WalletAddress> = None;
    let mut remote_network: Option<NetworkId> = None;
    let cookie_opt = {
        let mut len_buf = [0u8; 4];
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, reader.read_exact(&mut len_buf)).await {
            Ok(Ok(_)) => {
                let body_len = u32::from_be_bytes(len_buf) as usize;
                if body_len > 0 && body_len < 65536 {
                    let mut body = vec![0u8; body_len];
                    if reader.read_exact(&mut body).await.is_ok() {
                        if let Ok(WireMessage::Handshake(hs)) =
                            bincode::deserialize::<WireMessage>(&body)
                        {
                            remote_node_id = Some(hs.node_id.clone());
                            remote_network = Some(hs.network_id);
                            hs.cookie
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        }
    };

    // Reject peers on a different network (Dev/Test/Live) before doing any
    // further work. Default params make `params_hash` identical across
    // networks, so `network_id` is the authoritative isolation check.
    if let Some(net) = remote_network {
        if net != ctx.network {
            tracing::debug!(
                peer = %peer_id,
                ours = ?ctx.network,
                theirs = ?net,
                "dropping peer on different network"
            );
            return Err("network mismatch".into());
        }
    }

    // Never peer with ourselves: if the handshake reveals our own node_id, the
    // address we dialed is really us (learned via gossip or our own advertised
    // address). Abort before registering — this is what produced phantom
    // "self" entries that inflated peer_count.
    if remote_node_id.as_ref() == Some(&ctx.node_address) {
        tracing::debug!(peer = %peer_id, "dropping self-connection (own node_id)");
        return Err("self-connection".into());
    }

    // Sign and send cookie response
    if let Some(cookie) = cookie_opt {
        let sig = burst_crypto::sign_message(&cookie, &ctx.node_private_key);
        let response = WireMessage::Handshake(HandshakeMsg {
            node_id: ctx.node_address.clone(),
            cookie: None,
            cookie_signature: Some(sig),
            params_hash: ctx.params_hash,
            network_id: ctx.network,
            peering_port: ctx.peering_port,
        });
        if let Ok(bytes) = bincode::serialize(&response) {
            let len_bytes = (bytes.len() as u32).to_be_bytes();
            let _ = write_half.write_all(&len_bytes).await;
            let _ = write_half.write_all(&bytes).await;
            let _ = write_half.flush().await;
            tracing::debug!(peer = %peer_id, "sent cookie response");
        }
    } else {
        tracing::warn!(peer = %peer_id, "no cookie challenge received");
    }

    let read_half = reader.into_inner();
    let now = unix_now_secs();

    // The peer's verified node_id (from the handshake) is the dedup key.
    // Without it we can't dedup deterministically, so require it.
    let peer_node_id = match remote_node_id {
        Some(id) => id.as_str().to_string(),
        None => {
            tracing::warn!(peer = %peer_id, "outbound handshake had no node_id — dropping");
            return Err("no peer node_id".into());
        }
    };
    let our_node_id = ctx.node_address.as_str().to_string();

    // Register with deterministic dedup keyed by node_id: if the peer already
    // dialed us (opposite direction), keep exactly one — the connection from
    // the lower node_id — so simultaneous connects don't churn. `None` means
    // this outbound lost the tie-break; close it.
    let conn_id = {
        let mut registry = ctx.connection_registry.write().await;
        match registry.register_dedup(
            peer_id.clone(),
            peer_node_id,
            Direction::Outbound,
            write_half,
            &our_node_id,
        ) {
            Some(id) => {
                ctx.metrics.peer_count.set(registry.len() as i64);
                id
            }
            None => {
                tracing::debug!(peer = %peer_id, "outbound superseded by peer's inbound (tie-break)");
                return Err("connection deduplicated".into());
            }
        }
    };

    // Register the peer address (address book, for gossip/dialing).
    {
        let mut pm = ctx.peer_manager.write().await;
        pm.add_peer(peer_addr.clone());
        pm.mark_connected(&peer_id, now);
    }

    // Spawn a read loop (no SYN cookie for outbound — already validated)
    spawn_peer_read_loop(
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
        None,
        ip.clone(),
        Arc::clone(&ctx.frontier),
        Arc::clone(&ctx.store),
        ctx.params_hash,
    );

    Ok(ConnectedPeer { peer_id, peer_addr })
}

/// Check if the peer is already connected by parsing the address string.
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
