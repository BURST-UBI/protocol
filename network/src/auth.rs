//! Connection authentication — tracks verified node IDs for connected peers.
//!
//! This provides node-ID-level authentication without full transport encryption.
//! Peers prove ownership of their node key during the handshake; this module
//! records which peers have been successfully authenticated.

use std::collections::HashMap;

use burst_types::PublicKey;

/// Tracks authenticated node IDs for connected peers.
///
/// After a successful handshake, a peer's socket address is mapped to
/// the `PublicKey` they proved ownership of. Messages from unauthenticated
/// peers can be rejected.
pub struct PeerAuth {
    /// Map of peer_id (socket address string) to their verified node ID.
    authenticated_peers: HashMap<String, PublicKey>,
}

impl PeerAuth {
    /// Create a new empty authentication tracker.
    pub fn new() -> Self {
        Self {
            authenticated_peers: HashMap::new(),
        }
    }

    /// Record a successful handshake authentication.
    pub fn authenticate(&mut self, peer_id: &str, node_id: PublicKey) {
        self.authenticated_peers.insert(peer_id.to_owned(), node_id);
    }

    /// Check if a peer has been authenticated.
    pub fn is_authenticated(&self, peer_id: &str) -> bool {
        self.authenticated_peers.contains_key(peer_id)
    }

    /// Get a peer's verified node ID, if authenticated.
    pub fn get_node_id(&self, peer_id: &str) -> Option<&PublicKey> {
        self.authenticated_peers.get(peer_id)
    }

    /// Remove authentication when a peer disconnects.
    pub fn deauthenticate(&mut self, peer_id: &str) {
        self.authenticated_peers.remove(peer_id);
    }

    /// Number of currently authenticated peers.
    pub fn count(&self) -> usize {
        self.authenticated_peers.len()
    }
}

impl Default for PeerAuth {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key(byte: u8) -> PublicKey {
        PublicKey([byte; 32])
    }

    #[test]
    fn new_auth_is_empty() {
        let auth = PeerAuth::new();
        assert_eq!(auth.count(), 0);
    }

    #[test]
    fn authenticate_and_query() {
        let mut auth = PeerAuth::new();
        let key = test_key(0xAA);
        auth.authenticate("192.168.1.1:8000", key.clone());

        assert!(auth.is_authenticated("192.168.1.1:8000"));
        assert_eq!(auth.get_node_id("192.168.1.1:8000"), Some(&test_key(0xAA)));
        assert_eq!(auth.count(), 1);
    }

    #[test]
    fn unauthenticated_peer_returns_false() {
        let auth = PeerAuth::new();
        assert!(!auth.is_authenticated("10.0.0.1:9000"));
        assert_eq!(auth.get_node_id("10.0.0.1:9000"), None);
    }

    #[test]
    fn deauthenticate_removes_peer() {
        let mut auth = PeerAuth::new();
        auth.authenticate("peer1:8000", test_key(1));
        assert!(auth.is_authenticated("peer1:8000"));

        auth.deauthenticate("peer1:8000");
        assert!(!auth.is_authenticated("peer1:8000"));
        assert_eq!(auth.count(), 0);
    }

    #[test]
    fn deauthenticate_nonexistent_is_noop() {
        let mut auth = PeerAuth::new();
        auth.deauthenticate("nobody:1234");
        assert_eq!(auth.count(), 0);
    }

    #[test]
    fn multiple_peers() {
        let mut auth = PeerAuth::new();
        auth.authenticate("peer1:8000", test_key(1));
        auth.authenticate("peer2:8000", test_key(2));
        auth.authenticate("peer3:8000", test_key(3));
        assert_eq!(auth.count(), 3);

        assert_eq!(auth.get_node_id("peer2:8000"), Some(&test_key(2)));
    }

    #[test]
    fn re_authenticate_updates_key() {
        let mut auth = PeerAuth::new();
        auth.authenticate("peer1:8000", test_key(1));
        auth.authenticate("peer1:8000", test_key(2));
        assert_eq!(auth.count(), 1);
        assert_eq!(auth.get_node_id("peer1:8000"), Some(&test_key(2)));
    }

    #[test]
    fn default_is_empty() {
        let auth = PeerAuth::default();
        assert_eq!(auth.count(), 0);
    }
}
