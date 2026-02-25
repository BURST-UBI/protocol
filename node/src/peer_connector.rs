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
use burst_types::BlockHash;

use crate::connection_registry::{spawn_peer_read_loop, ConnectionRegistry};
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

    // Sign and send cookie response
    if let Some(cookie) = cookie_opt {
        let sig = burst_crypto::sign_message(&cookie, &ctx.node_private_key);
        let response = WireMessage::Handshake(HandshakeMsg {
            node_id: ctx.node_address.clone(),
            cookie: None,
            cookie_signature: Some(sig),
            params_hash: ctx.params_hash,
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

    // Register write half in the connection registry
    {
        let mut registry = ctx.connection_registry.write().await;
        registry.insert(peer_id.clone(), write_half);
    }

    // Register the peer
    {
        let mut pm = ctx.peer_manager.write().await;
        pm.add_peer(peer_addr.clone());
        pm.mark_connected(&peer_id, now);
        ctx.metrics.peer_count.set(pm.connected_count() as i64);
    }

    // Spawn a read loop (no SYN cookie for outbound — already validated)
    spawn_peer_read_loop(
        peer_id.clone(),
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
