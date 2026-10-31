//! Best-effort queueing for all eligible peers or a random subset.
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
    pub queued: usize,
    /// Number of peers for which queueing failed (channel full / closed).
    pub failed: usize,
}

/// Queue-based broadcaster.
///
/// Attempts one nonblocking channel send per selected peer entry. A full or
/// closed channel increments the failure count; this helper does not retry.
/// Queue acceptance does not establish wire delivery or peer acknowledgment.
#[derive(Clone)]
pub struct Broadcaster {
    outbound_tx: mpsc::Sender<(String, Vec<u8>)>,
}

impl Broadcaster {
    /// Create a new broadcaster backed by the given outbound channel.
    pub fn new(outbound_tx: mpsc::Sender<(String, Vec<u8>)>) -> Self {
        Self { outbound_tx }
    }

    /// Attempt queueing for every connected, non-banned peer entry.
    pub async fn broadcast_to_all(&self, message: &[u8], peers: &[PeerState]) -> BroadcastResult {
        let mut result = BroadcastResult::default();

        for peer in peers.iter().filter(|p| p.connected && !p.banned) {
            let peer_id = peer.peer_id.clone();
            match self.outbound_tx.try_send((peer_id, message.to_vec())) {
                Ok(()) => result.queued += 1,
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
            let peer_id = peer.peer_id.clone();
            match self.outbound_tx.try_send((peer_id, message.to_vec())) {
                Ok(()) => result.queued += 1,
                Err(_) => result.failed += 1,
            }
        }

        result
    }

    /// Sample max(ceil(sqrt(eligible)), min_fanout), capped by eligible entries.
    /// Selection scans and shuffles in O(n) time with O(n) temporary storage.
    /// With a fixed minimum, message copies grow as O(sqrt(n)); their byte cost
    /// also depends on message size.
    pub async fn broadcast_with_fanout(
        &self,
        message: &[u8],
        peers: &[PeerState],
        min_fanout: usize,
    ) -> BroadcastResult {
        let eligible = peers.iter().filter(|p| p.connected && !p.banned).count();
        let count = ((eligible as f64).sqrt().ceil() as usize).max(min_fanout);
        self.broadcast_to_subset(message, peers, count).await
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_types::PeerAddress;

    fn make_peer(ip: &str, port: u16, connected: bool) -> PeerState {
        PeerState {
            peer_id: format!("{ip}:{port}"),
            address_is_dialable: true,
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
            peering_addr: None,
        }
    }

    #[tokio::test]
    async fn every_broadcast_variant_routes_by_identity_not_shared_endpoint() {
        let (tx, mut rx) = mpsc::channel(64);
        let broadcaster = Broadcaster::new(tx);
        let mut a = make_peer("1.2.3.4", 7076, true);
        let mut b = a.clone();
        a.peer_id = "node-a".into();
        b.peer_id = "node-b".into();
        let peers = [a, b];
        for variant in 0..3 {
            let result = match variant {
                0 => broadcaster.broadcast_to_all(b"hello", &peers).await,
                1 => broadcaster.broadcast_to_subset(b"hello", &peers, 2).await,
                _ => broadcaster.broadcast_with_fanout(b"hello", &peers, 2).await,
            };
            assert_eq!(result.queued, 2);
            let mut ids = vec![rx.recv().await.unwrap().0, rx.recv().await.unwrap().0];
            ids.sort();
            assert_eq!(ids, ["node-a", "node-b"]);
        }
    }

    #[tokio::test]
    async fn broadcast_to_all_queues_for_connected() {
        let (tx, mut rx) = mpsc::channel(64);
        let broadcaster = Broadcaster::new(tx);

        let peers = vec![
            make_peer("1.0.0.1", 1, true),
            make_peer("1.0.0.2", 2, false),
            make_peer("1.0.0.3", 3, true),
        ];

        let result = broadcaster.broadcast_to_all(b"hello", &peers).await;
        assert_eq!(result.queued, 2);
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
        assert_eq!(result.queued, 3);
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
        assert_eq!(result.queued, 1);

        let (id, _) = rx.recv().await.unwrap();
        assert_eq!(id, "1.0.0.2:2");
    }

    #[tokio::test]
    async fn broadcast_with_fanout_selects_sqrt_peers() {
        let (tx, mut rx) = mpsc::channel(64);
        let broadcaster = Broadcaster::new(tx);

        let peers: Vec<PeerState> = (0..16)
            .map(|i| {
                let mut p = make_peer(&format!("10.0.0.{i}"), 7075, true);
                p.score = i; // higher IP = higher score
                p
            })
            .collect();

        let result = broadcaster.broadcast_with_fanout(b"block", &peers, 2).await;
        // sqrt(16) = 4, min_fanout = 2, so fanout = max(4, 2) = 4
        assert_eq!(result.queued, 4);

        let mut received = Vec::new();
        while let Ok(item) = rx.try_recv() {
            received.push(item.0);
        }
        assert_eq!(received.len(), 4);
        let unique: std::collections::HashSet<_> = received.iter().collect();
        assert_eq!(unique.len(), 4);
        assert!(received.iter().all(|id| peers.iter().any(|peer| &peer.peer_id == id)));
    }

    #[tokio::test]
    async fn broadcast_with_fanout_respects_min_fanout() {
        let (tx, mut rx) = mpsc::channel(64);
        let broadcaster = Broadcaster::new(tx);

        // 4 peers: sqrt(4) = 2, but min_fanout = 3
        let peers: Vec<PeerState> = (0..4)
            .map(|i| make_peer(&format!("10.0.0.{i}"), 7075, true))
            .collect();

        let result = broadcaster.broadcast_with_fanout(b"block", &peers, 3).await;
        assert_eq!(result.queued, 3);

        let mut received = Vec::new();
        while let Ok(item) = rx.try_recv() {
            received.push(item.0);
        }
        assert_eq!(received.len(), 3);
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
        assert_eq!(result.queued + result.failed, 3);
        assert!(result.failed > 0);
    }
}
