//! Peer discovery and connection management.

use crate::NetworkError;
use burst_messages::PeerAddress;
use std::collections::HashMap;

/// Manages known peers and active connections.
pub struct PeerManager {
    /// Known peers and their connection state.
    peers: HashMap<String, PeerState>,
    /// Maximum number of outbound connections.
    max_peers: usize,
}

#[derive(Clone, Debug)]
pub struct PeerState {
    pub address: PeerAddress,
    pub connected: bool,
    pub last_seen_secs: u64,
}

impl PeerManager {
    pub fn new(max_peers: usize) -> Self {
        Self {
            peers: HashMap::new(),
            max_peers,
        }
    }

    /// Add a discovered peer.
    pub fn add_peer(&mut self, address: PeerAddress) {
        let key = format!("{}:{}", address.ip, address.port);
        self.peers.entry(key).or_insert(PeerState {
            address,
            connected: false,
            last_seen_secs: 0,
        });
    }

    /// Connect to a peer.
    pub async fn connect(&mut self, _address: &PeerAddress) -> Result<(), NetworkError> {
        todo!("establish TCP connection, perform handshake")
    }

    /// Broadcast a message to all connected peers.
    pub async fn broadcast(&self, _message: &[u8]) -> Result<(), NetworkError> {
        todo!("send message to all connected peers")
    }

    /// Get the number of connected peers.
    pub fn connected_count(&self) -> usize {
        self.peers.values().filter(|p| p.connected).count()
    }
}
