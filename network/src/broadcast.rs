//! Flood-based message broadcasting (Nano-style).
//!
//! The [`Broadcaster`] does not write directly to TCP streams. Instead it
//! pushes `(peer_id, message_bytes)` tuples onto an `mpsc` channel that the
//! connection layer drains.

use crate::peer_manager::PeerState;
use rand::seq::SliceRandom;
use tokio::sync::mpsc;

/// Outcome of a broadcast attempt.
#[derive(Clone, Debug, Default)]
pub struct BroadcastResult {
    /// Number of peers the message was successfully queued for.
    pub sent: usize,
    /// Number of peers for which queueing failed (channel full / closed).
    pub failed: usize,
}

/// Queue-based broadcaster.
///
/// Each call places one `(peer_id, message_bytes)` entry per target peer onto
/// the outbound channel. The connection layer is responsible for actually
/// writing to the wire.
#[derive(Clone)]
pub struct Broadcaster {
    outbound_tx: mpsc::Sender<(String, Vec<u8>)>,
}

impl Broadcaster {
    /// Create a new broadcaster backed by the given outbound channel.
    pub fn new(outbound_tx: mpsc::Sender<(String, Vec<u8>)>) -> Self {
        Self { outbound_tx }
    }

    /// Broadcast a serialised message to **all** connected peers.
    ///
    /// Uses flood-based propagation: every connected peer receives the message.
    pub async fn broadcast_to_all(&self, message: &[u8], peers: &[PeerState]) -> BroadcastResult {
        let mut result = BroadcastResult::default();

        for peer in peers.iter().filter(|p| p.connected && !p.banned) {
            let peer_id = format!("{}:{}", peer.address.ip, peer.address.port);
            match self.outbound_tx.try_send((peer_id, message.to_vec())) {
                Ok(()) => result.sent += 1,
                Err(_) => result.failed += 1,
            }
        }

        result
    }

    /// Broadcast a serialised message to a random subset of connected peers.
    ///
    /// At most `count` peers are chosen uniformly at random from the connected
    /// (and not-banned) peers.
    pub async fn broadcast_to_subset(
        &self,
        message: &[u8],
        peers: &[PeerState],
        count: usize,
    ) -> BroadcastResult {
        let eligible: Vec<&PeerState> = peers.iter().filter(|p| p.connected && !p.banned).collect();

        let mut rng = rand::thread_rng();
        let mut indices: Vec<usize> = (0..eligible.len()).collect();
        indices.shuffle(&mut rng);
        indices.truncate(count);

        let mut result = BroadcastResult::default();

        for &i in &indices {
            let peer = eligible[i];
            let peer_id = format!("{}:{}", peer.address.ip, peer.address.port);
            match self.outbound_tx.try_send((peer_id, message.to_vec())) {
                Ok(()) => result.sent += 1,
                Err(_) => result.failed += 1,
            }
        }

        result
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use burst_messages::PeerAddress;

    fn make_peer(ip: &str, port: u16, connected: bool) -> PeerState {
        PeerState {
            address: PeerAddress {
                ip: ip.to_string(),
                port,
            },
            connected,
            last_seen_secs: 0,
            score: 0,
            banned: false,
            ban_until_secs: None,
            telemetry: None,
        }
    }

    #[tokio::test]
    async fn broadcast_to_all_sends_to_connected() {
        let (tx, mut rx) = mpsc::channel(64);
        let broadcaster = Broadcaster::new(tx);

        let peers = vec![
            make_peer("1.0.0.1", 1, true),
            make_peer("1.0.0.2", 2, false),
            make_peer("1.0.0.3", 3, true),
        ];

        let result = broadcaster.broadcast_to_all(b"hello", &peers).await;
        assert_eq!(result.sent, 2);
        assert_eq!(result.failed, 0);

        let (id1, msg1) = rx.recv().await.unwrap();
        assert_eq!(msg1, b"hello");
        assert!(id1 == "1.0.0.1:1" || id1 == "1.0.0.3:3");

        let (id2, _) = rx.recv().await.unwrap();
        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn broadcast_to_subset_limits_count() {
        let (tx, mut rx) = mpsc::channel(64);
        let broadcaster = Broadcaster::new(tx);

        let peers: Vec<PeerState> = (0..10)
            .map(|i| make_peer(&format!("10.0.0.{i}"), 7075, true))
            .collect();

        let result = broadcaster.broadcast_to_subset(b"vote", &peers, 3).await;
        assert_eq!(result.sent, 3);
        assert_eq!(result.failed, 0);

        let mut received = Vec::new();
        while let Ok(item) = rx.try_recv() {
            received.push(item);
        }
        assert_eq!(received.len(), 3);
    }

    #[tokio::test]
    async fn broadcast_skips_banned_peers() {
        let (tx, mut rx) = mpsc::channel(64);
        let broadcaster = Broadcaster::new(tx);

        let mut banned_peer = make_peer("1.0.0.1", 1, true);
        banned_peer.banned = true;

        let peers = vec![banned_peer, make_peer("1.0.0.2", 2, true)];

        let result = broadcaster.broadcast_to_all(b"block", &peers).await;
        assert_eq!(result.sent, 1);

        let (id, _) = rx.recv().await.unwrap();
        assert_eq!(id, "1.0.0.2:2");
    }

    #[tokio::test]
    async fn broadcast_handles_full_channel() {
        let (tx, _rx) = mpsc::channel(1);
        let broadcaster = Broadcaster::new(tx);

        let peers = vec![
            make_peer("1.0.0.1", 1, true),
            make_peer("1.0.0.2", 2, true),
            make_peer("1.0.0.3", 3, true),
        ];

        let result = broadcaster.broadcast_to_all(b"data", &peers).await;
        assert_eq!(result.sent + result.failed, 3);
        assert!(result.failed > 0);
    }
}
