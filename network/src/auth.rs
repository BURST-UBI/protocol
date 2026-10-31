//! Caller-maintained peer/key registry. It performs no cryptographic checks.
//! The running node authenticates handshakes in its transport layer; this
//! standalone registry is not that authentication boundary.

use std::collections::HashMap;

use burst_types::PublicKey;

/// Records keys under caller-chosen peer identifiers. Callers must verify
/// identity before insertion and remove entries when their session ends.
pub struct PeerAuth {
    /// Opaque peer identifier to recorded public key.
    authenticated_peers: HashMap<String, PublicKey>,
}

impl PeerAuth {
    /// Create a new empty authentication tracker.
    pub fn new() -> Self {
        Self {
            authenticated_peers: HashMap::new(),
        }
    }

    /// Insert or replace a key without verifying it.
    pub fn authenticate(&mut self, peer_id: &str, node_id: PublicKey) {
        self.authenticated_peers.insert(peer_id.to_owned(), node_id);
    }

    /// Whether the caller has recorded this identifier.
    pub fn is_authenticated(&self, peer_id: &str) -> bool {
        self.authenticated_peers.contains_key(peer_id)
    }

    /// Return the key recorded for this identifier.
    pub fn get_node_id(&self, peer_id: &str) -> Option<&PublicKey> {
        self.authenticated_peers.get(peer_id)
    }

    /// Remove authentication when a peer disconnects.
    pub fn deauthenticate(&mut self, peer_id: &str) {
        self.authenticated_peers.remove(peer_id);
    }

    /// Number of recorded identifiers.
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
