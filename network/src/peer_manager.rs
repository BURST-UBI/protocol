//! Peer discovery, keepalive, connection tracking, and peer scoring/banning.

use burst_messages::PeerAddress;
use rand::seq::SliceRandom;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::net::SocketAddrV4;

// ---------------------------------------------------------------------------
// Penalty / scoring types
// ---------------------------------------------------------------------------

/// Reasons a peer can be penalized. Each carries a fixed penalty value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PenaltyReason {
    InvalidBlock,
    InvalidVote,
    Spam,
    Timeout,
    ProtocolViolation,
}

impl PenaltyReason {
    /// Penalty points deducted for this reason (always negative).
    pub fn penalty(self) -> i32 {
        match self {
            Self::InvalidBlock => -100,
            Self::InvalidVote => -50,
            Self::Spam => -25,
            Self::Timeout => -10,
            Self::ProtocolViolation => -200,
        }
    }
}

// ---------------------------------------------------------------------------
// Peer state
// ---------------------------------------------------------------------------

/// Telemetry data received from a peer.
#[derive(Clone, Debug)]
pub struct PeerTelemetry {
    pub block_count: u64,
    pub cemented_count: u64,
    pub account_count: u64,
    pub peer_count: u32,
    pub protocol_version: u8,
    pub uptime: u64,
    pub major_version: u8,
    pub minor_version: u8,
    pub patch_version: u8,
    pub timestamp: u64,
}

/// Per-peer metadata tracked by the [`PeerManager`].
#[derive(Clone, Debug)]
pub struct PeerState {
    pub address: PeerAddress,
    pub connected: bool,
    pub last_seen_secs: u64,
    /// Reputation score. Starts at 0, clamped to [`SCORE_MIN`]..=[`SCORE_MAX`].
    pub score: i32,
    /// Whether the peer is currently banned.
    pub banned: bool,
    /// Unix timestamp (seconds) when the ban expires, if any.
    pub ban_until_secs: Option<u64>,
    /// Most recent telemetry data received from this peer.
    pub telemetry: Option<PeerTelemetry>,
    /// The address other nodes should use to connect to this peer.
    /// Learned from the peer's self-address in keepalive slot 0.
    /// Differs from `address` when the peer is behind NAT (the TCP
    /// `peer_addr` may use an ephemeral port).
    pub peering_addr: Option<PeerAddress>,
}

/// Score at or below which a peer is banned.
const BAN_THRESHOLD: i32 = -500;
/// Duration of a ban in seconds (1 hour).
const BAN_DURATION_SECS: u64 = 3600;
/// Minimum reputation score.
const SCORE_MIN: i32 = -1000;
/// Maximum reputation score.
const SCORE_MAX: i32 = 100;

// ---------------------------------------------------------------------------
// Peer manager
// ---------------------------------------------------------------------------

/// Maximum number of recent keepalive payloads to buffer for the reachout loop.
const LATEST_KEEPALIVES_CAPACITY: usize = 32;

/// Central registry for peer discovery, keepalive scheduling, scoring, and
/// ban management.
pub struct PeerManager {
    /// All known peers keyed by `"ip:port"`.
    peers: HashMap<String, PeerState>,
    /// Upper bound on the number of peers we track.
    max_peers: usize,
    /// Hardcoded bootstrap peers to connect to on startup.
    bootstrap_peers: Vec<String>,
    /// How often (in seconds) we send keepalive messages.
    keepalive_interval_secs: u64,
    /// Timestamp (seconds) of the last keepalive round, or `None` if no
    /// keepalive has been sent yet (triggers an immediate first keepalive).
    last_keepalive_secs: Option<u64>,
    /// Incrementally tracked count of connected peers — O(1) queries.
    num_connected: usize,
    /// External (public) address discovered via UPnP, if available.
    /// When set, keepalive messages advertise this address so peers behind
    /// NAT can be reached by others.
    external_address: Option<SocketAddrV4>,
    /// Ring buffer of recently received keepalive peer lists. The reachout
    /// loop pops random entries and attempts connections to discovered peers.
    latest_keepalives: VecDeque<Vec<PeerAddress>>,
}

impl PeerManager {
    /// Create a new `PeerManager` with the given peer limit.
    pub fn new(max_peers: usize) -> Self {
        Self {
            peers: HashMap::new(),
            max_peers,
            bootstrap_peers: Vec::new(),
            keepalive_interval_secs: 60,
            last_keepalive_secs: None,
            num_connected: 0,
            external_address: None,
            latest_keepalives: VecDeque::with_capacity(LATEST_KEEPALIVES_CAPACITY),
        }
    }

    /// Create a `PeerManager` with full configuration.
    pub fn with_config(
        max_peers: usize,
        bootstrap_peers: Vec<String>,
        keepalive_interval_secs: u64,
    ) -> Self {
        Self {
            peers: HashMap::new(),
            max_peers,
            bootstrap_peers,
            keepalive_interval_secs,
            last_keepalive_secs: None,
            num_connected: 0,
            external_address: None,
            latest_keepalives: VecDeque::with_capacity(LATEST_KEEPALIVES_CAPACITY),
        }
    }

    // -- Bootstrap -------------------------------------------------------------

    /// Return the configured bootstrap peer addresses.
    pub fn bootstrap_peers(&self) -> &[String] {
        &self.bootstrap_peers
    }

    // -- Peer lifecycle --------------------------------------------------------

    /// Build the canonical key for a [`PeerAddress`].
    fn peer_key(address: &PeerAddress) -> String {
        format!("{}:{}", address.ip, address.port)
    }

    /// Add a discovered peer. If at capacity, evicts the lowest-scoring
    /// peer when the new peer would score higher (new peers start at 0).
    /// Banned peers are never added.
    pub fn add_peer(&mut self, address: PeerAddress) {
        let key = Self::peer_key(&address);

        if let Some(existing) = self.peers.get(&key) {
            if existing.banned {
                return;
            }
        }

        if self.peers.contains_key(&key) {
            return;
        }

        if self.peers.len() >= self.max_peers {
            if let Some((worst_key, worst_score)) = self.find_worst_peer() {
                let new_score = 0i32;
                if new_score > worst_score {
                    tracing::debug!(
                        evicted = %worst_key,
                        score = worst_score,
                        "evicted lowest-scoring peer to make room"
                    );
                    self.peers.remove(&worst_key);
                } else {
                    return;
                }
            } else {
                return;
            }
        }

        self.peers.insert(
            key,
            PeerState {
                address,
                connected: false,
                last_seen_secs: 0,
                score: 0,
                banned: false,
                ban_until_secs: None,
                telemetry: None,
                peering_addr: None,
            },
        );
    }

    /// Find the peer with the lowest reputation score.
    fn find_worst_peer(&self) -> Option<(String, i32)> {
        self.peers
            .iter()
            .min_by_key(|(_, p)| p.score)
            .map(|(key, p)| (key.clone(), p.score))
    }

    /// Remove a peer entirely.
    pub fn remove_peer(&mut self, peer_id: &str) {
        if let Some(removed) = self.peers.remove(peer_id) {
            if removed.connected {
                self.num_connected = self.num_connected.saturating_sub(1);
            }
        }
    }

    /// Mark a peer as connected and update `last_seen_secs`.
    pub fn mark_connected(&mut self, peer_id: &str, now_secs: u64) {
        if let Some(peer) = self.peers.get_mut(peer_id) {
            if !peer.connected {
                self.num_connected += 1;
            }
            peer.connected = true;
            peer.last_seen_secs = now_secs;
        }
    }

    /// Update a peer's `last_seen_secs` timestamp. Called on every inbound
    /// message so idle detection works correctly.
    pub fn touch(&mut self, peer_id: &str, now_secs: u64) {
        if let Some(peer) = self.peers.get_mut(peer_id) {
            peer.last_seen_secs = now_secs;
        }
    }

    /// Mark a peer as disconnected.
    pub fn mark_disconnected(&mut self, peer_id: &str) {
        if let Some(peer) = self.peers.get_mut(peer_id) {
            if peer.connected {
                self.num_connected = self.num_connected.saturating_sub(1);
            }
            peer.connected = false;
        }
    }

    // -- Queries ---------------------------------------------------------------

    /// Number of currently connected peers — O(1).
    pub fn connected_count(&self) -> usize {
        self.num_connected
    }

    /// Check if a peer is currently connected by its key (ip:port).
    pub fn is_connected(&self, key: &str) -> bool {
        self.peers
            .get(key)
            .map(|p| p.connected && !p.banned)
            .unwrap_or(false)
    }

    /// Iterate over all connected (and not-banned) peers.
    pub fn iter_connected(&self) -> impl Iterator<Item = (&String, &PeerState)> {
        self.peers.iter().filter(|(_, p)| p.connected && !p.banned)
    }

    /// Return peers that are known but not currently connected and not banned.
    /// These are candidates for outbound connection attempts.
    pub fn get_connectable_peers(&self) -> Vec<&PeerState> {
        self.peers
            .values()
            .filter(|p| !p.connected && !p.banned)
            .collect()
    }

    /// Return up to `count` random *connected* peer addresses, suitable for
    /// inclusion in a keepalive message.
    ///
    /// Prefers `peering_addr` over the raw TCP address so that peers behind
    /// NAT advertise their externally reachable address.
    pub fn random_peers(&self, count: usize) -> Vec<PeerAddress> {
        let mut result = Vec::with_capacity(count);

        let connected: Vec<&PeerState> = self
            .peers
            .values()
            .filter(|p| p.connected && !p.banned)
            .collect();

        let mut rng = rand::thread_rng();
        let mut indices: Vec<usize> = (0..connected.len()).collect();
        indices.shuffle(&mut rng);

        for i in indices {
            if result.len() >= count {
                break;
            }
            let peer = connected[i];
            result.push(
                peer.peering_addr
                    .clone()
                    .unwrap_or_else(|| peer.address.clone()),
            );
        }

        result
    }

    /// Return up to `count` random *connected* peer addresses with this
    /// node's own external address in slot 0 (for self-advertisement).
    pub fn random_peers_with_self(&self, count: usize) -> Vec<PeerAddress> {
        let mut result = Vec::with_capacity(count);
        if let Some(self_addr) = self.self_peer_address() {
            result.push(self_addr);
        }
        let rest = self.random_peers(count.saturating_sub(result.len()));
        result.extend(rest);
        result.truncate(count);
        result
    }

    // -- External address (UPnP) -----------------------------------------------

    /// Set the node's external (public) address as discovered by UPnP.
    /// When set, keepalive messages will include this address so other
    /// peers know how to reach this node.
    pub fn set_external_address(&mut self, addr: SocketAddrV4) {
        self.external_address = Some(addr);
    }

    /// Clear the external address (e.g. when UPnP mapping expires or is removed).
    pub fn clear_external_address(&mut self) {
        self.external_address = None;
    }

    /// Returns the external address if UPnP mapping is active.
    pub fn external_address(&self) -> Option<SocketAddrV4> {
        self.external_address
    }

    /// Returns the address to advertise for this node in keepalive messages.
    /// Prefers the UPnP external address; falls back to `None` if unavailable.
    pub fn self_peer_address(&self) -> Option<PeerAddress> {
        self.external_address.map(|addr| PeerAddress {
            ip: addr.ip().to_string(),
            port: addr.port(),
        })
    }

    // -- Keepalive -------------------------------------------------------------

    /// Returns `true` if enough time has elapsed since the last keepalive
    /// round. Always returns `true` when no keepalive has been sent yet.
    pub fn should_keepalive(&self, now_secs: u64) -> bool {
        match self.last_keepalive_secs {
            None => true,
            Some(last) => now_secs.saturating_sub(last) >= self.keepalive_interval_secs,
        }
    }

    /// Record that we just sent a keepalive round.
    pub fn record_keepalive(&mut self, now_secs: u64) {
        self.last_keepalive_secs = Some(now_secs);
    }

    /// Process a received keepalive message: learn any new peer addresses
    /// and buffer the full list for the reachout loop.
    pub fn process_keepalive(&mut self, peers: Vec<PeerAddress>) {
        if !peers.is_empty() {
            if self.latest_keepalives.len() >= LATEST_KEEPALIVES_CAPACITY {
                self.latest_keepalives.pop_front();
            }
            self.latest_keepalives.push_back(peers.clone());
        }
        for addr in peers {
            self.add_peer(addr);
        }
    }

    /// Pop a random recently received keepalive peer list for the reachout
    /// loop. Returns `None` when the buffer is empty.
    pub fn pop_random_keepalive(&mut self) -> Option<Vec<PeerAddress>> {
        if self.latest_keepalives.is_empty() {
            return None;
        }
        let idx = rand::random::<usize>() % self.latest_keepalives.len();
        self.latest_keepalives.remove(idx)
    }

    /// Set a peer's reachable address (learned from keepalive self-advertisement).
    pub fn set_peering_addr(&mut self, peer_id: &str, addr: PeerAddress) {
        if let Some(peer) = self.peers.get_mut(peer_id) {
            peer.peering_addr = Some(addr);
        }
    }

    /// Return addresses of all currently connected peers (for cache persistence).
    pub fn connected_peer_addresses(&self) -> Vec<(String, u64)> {
        self.peers
            .iter()
            .filter(|(_, p)| p.connected && !p.banned)
            .map(|(key, p)| (key.clone(), p.last_seen_secs))
            .collect()
    }

    // -- Scoring / banning -----------------------------------------------------

    /// Penalize a peer for bad behaviour. Returns `true` if the peer was
    /// banned as a result.
    pub fn penalize(&mut self, peer_id: &str, reason: PenaltyReason, now_secs: u64) -> bool {
        let Some(peer) = self.peers.get_mut(peer_id) else {
            return false;
        };

        let should_ban = if reason == PenaltyReason::ProtocolViolation {
            true
        } else {
            peer.score = (peer.score + reason.penalty()).max(SCORE_MIN);
            peer.score <= BAN_THRESHOLD
        };

        if should_ban {
            if peer.connected {
                self.num_connected = self.num_connected.saturating_sub(1);
            }
            Self::ban_peer(peer, now_secs);
            return true;
        }

        false
    }

    /// Reward a peer for good behaviour (e.g. delivering a valid block).
    pub fn reward(&mut self, peer_id: &str, amount: i32) {
        if let Some(peer) = self.peers.get_mut(peer_id) {
            peer.score = (peer.score + amount).min(SCORE_MAX);
        }
    }

    /// Check whether a peer is currently banned.
    pub fn is_banned(&self, peer_id: &str) -> bool {
        self.peers.get(peer_id).is_some_and(|p| p.banned)
    }

    /// Unban peers whose ban has expired.
    pub fn check_bans(&mut self, now_secs: u64) {
        for peer in self.peers.values_mut() {
            if peer.banned {
                if let Some(until) = peer.ban_until_secs {
                    if now_secs >= until {
                        peer.banned = false;
                        peer.ban_until_secs = None;
                        peer.score = 0;
                    }
                }
            }
        }
    }

    /// Return peer IDs of connections that have been idle longer than
    /// `timeout_secs` and mark them disconnected. The caller should close
    /// the associated TCP streams.
    pub fn cleanup_idle(&mut self, now_secs: u64, timeout_secs: u64) -> Vec<String> {
        let cutoff = now_secs.saturating_sub(timeout_secs);
        let mut idle_peers = Vec::new();

        for (key, peer) in self.peers.iter_mut() {
            if peer.connected && !peer.banned && peer.last_seen_secs < cutoff {
                peer.connected = false;
                idle_peers.push(key.clone());
            }
        }

        idle_peers
    }

    /// Update a peer's telemetry data.
    pub fn update_telemetry(&mut self, peer_id: &str, telemetry: PeerTelemetry) {
        if let Some(peer) = self.peers.get_mut(peer_id) {
            peer.telemetry = Some(telemetry);
        }
    }

    /// Internal helper — mark a peer as banned.
    fn ban_peer(peer: &mut PeerState, now_secs: u64) {
        peer.banned = true;
        peer.connected = false;
        peer.ban_until_secs = Some(now_secs + BAN_DURATION_SECS);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(ip: &str, port: u16) -> PeerAddress {
        PeerAddress {
            ip: ip.to_string(),
            port,
        }
    }

    fn key(ip: &str, port: u16) -> String {
        format!("{ip}:{port}")
    }

    #[test]
    fn add_and_connect_peer() {
        let mut pm = PeerManager::new(10);
        pm.add_peer(addr("1.2.3.4", 7075));
        assert_eq!(pm.connected_count(), 0);

        pm.mark_connected(&key("1.2.3.4", 7075), 100);
        assert_eq!(pm.connected_count(), 1);

        pm.mark_disconnected(&key("1.2.3.4", 7075));
        assert_eq!(pm.connected_count(), 0);
    }

    #[test]
    fn add_peer_respects_max_when_scores_equal() {
        let mut pm = PeerManager::new(2);
        pm.add_peer(addr("1.0.0.1", 1));
        pm.add_peer(addr("1.0.0.2", 2));
        pm.add_peer(addr("1.0.0.3", 3));
        // All peers at score 0 — new peer (score 0) is not better, so rejected
        assert_eq!(pm.peers.len(), 2);
    }

    #[test]
    fn add_peer_evicts_worst_when_full() {
        let mut pm = PeerManager::new(2);
        pm.add_peer(addr("1.0.0.1", 1));
        pm.add_peer(addr("1.0.0.2", 2));
        // Penalize one peer so its score goes negative
        pm.penalize(&key("1.0.0.1", 1), PenaltyReason::Timeout, 0);
        assert_eq!(pm.peers[&key("1.0.0.1", 1)].score, -10);

        // New peer (score 0) is better than the worst (score -10), so it evicts
        pm.add_peer(addr("1.0.0.3", 3));
        assert_eq!(pm.peers.len(), 2);
        assert!(!pm.peers.contains_key(&key("1.0.0.1", 1)));
        assert!(pm.peers.contains_key(&key("1.0.0.3", 3)));
    }

    #[test]
    fn add_peer_ignores_banned() {
        let mut pm = PeerManager::new(10);
        pm.add_peer(addr("1.0.0.1", 1));
        pm.penalize(&key("1.0.0.1", 1), PenaltyReason::ProtocolViolation, 0);
        assert!(pm.is_banned(&key("1.0.0.1", 1)));

        pm.remove_peer(&key("1.0.0.1", 1));
        pm.add_peer(addr("1.0.0.1", 1));
        // Peer was removed so it's re-added fresh, not banned.
        assert!(!pm.is_banned(&key("1.0.0.1", 1)));
    }

    #[test]
    fn keepalive_timing() {
        let pm = PeerManager::with_config(10, vec![], 60);
        assert!(pm.should_keepalive(0));
        assert!(pm.should_keepalive(60));
    }

    #[test]
    fn keepalive_not_ready() {
        let mut pm = PeerManager::with_config(10, vec![], 60);
        pm.record_keepalive(100);
        assert!(!pm.should_keepalive(120));
        assert!(pm.should_keepalive(160));
    }

    #[test]
    fn process_keepalive_learns_peers() {
        let mut pm = PeerManager::new(10);
        pm.process_keepalive(vec![addr("5.5.5.5", 7075), addr("6.6.6.6", 7075)]);
        assert_eq!(pm.peers.len(), 2);
    }

    #[test]
    fn random_peers_returns_connected_only() {
        let mut pm = PeerManager::new(10);
        pm.add_peer(addr("1.0.0.1", 1));
        pm.add_peer(addr("1.0.0.2", 2));
        pm.add_peer(addr("1.0.0.3", 3));
        pm.mark_connected(&key("1.0.0.1", 1), 0);
        pm.mark_connected(&key("1.0.0.3", 3), 0);

        let random = pm.random_peers(10);
        assert_eq!(random.len(), 2);
    }

    #[test]
    fn get_connectable_excludes_connected_and_banned() {
        let mut pm = PeerManager::new(10);
        pm.add_peer(addr("1.0.0.1", 1));
        pm.add_peer(addr("1.0.0.2", 2));
        pm.add_peer(addr("1.0.0.3", 3));
        pm.mark_connected(&key("1.0.0.1", 1), 0);
        pm.penalize(&key("1.0.0.2", 2), PenaltyReason::ProtocolViolation, 0);

        let connectable = pm.get_connectable_peers();
        assert_eq!(connectable.len(), 1);
        assert_eq!(connectable[0].address.port, 3);
    }

    #[test]
    fn scoring_and_ban_threshold() {
        let mut pm = PeerManager::new(10);
        pm.add_peer(addr("1.0.0.1", 1));
        let id = key("1.0.0.1", 1);

        // 5 * -100 = -500 -> hits threshold
        for _ in 0..4 {
            assert!(!pm.penalize(&id, PenaltyReason::InvalidBlock, 0));
        }
        assert!(pm.penalize(&id, PenaltyReason::InvalidBlock, 0));
        assert!(pm.is_banned(&id));
    }

    #[test]
    fn protocol_violation_bans_immediately() {
        let mut pm = PeerManager::new(10);
        pm.add_peer(addr("1.0.0.1", 1));
        let id = key("1.0.0.1", 1);
        assert!(pm.penalize(&id, PenaltyReason::ProtocolViolation, 100));
        assert!(pm.is_banned(&id));
        assert_eq!(pm.peers[&id].ban_until_secs, Some(100 + 3600));
    }

    #[test]
    fn reward_clamps_to_max() {
        let mut pm = PeerManager::new(10);
        pm.add_peer(addr("1.0.0.1", 1));
        let id = key("1.0.0.1", 1);
        pm.reward(&id, 200);
        assert_eq!(pm.peers[&id].score, SCORE_MAX);
    }

    #[test]
    fn check_bans_unbans_expired() {
        let mut pm = PeerManager::new(10);
        pm.add_peer(addr("1.0.0.1", 1));
        let id = key("1.0.0.1", 1);
        pm.penalize(&id, PenaltyReason::ProtocolViolation, 1000);
        assert!(pm.is_banned(&id));

        pm.check_bans(1000 + 3599);
        assert!(pm.is_banned(&id));

        pm.check_bans(1000 + 3600);
        assert!(!pm.is_banned(&id));
        assert_eq!(pm.peers[&id].score, 0);
    }

    #[test]
    fn iter_connected_skips_banned() {
        let mut pm = PeerManager::new(10);
        pm.add_peer(addr("1.0.0.1", 1));
        pm.add_peer(addr("1.0.0.2", 2));
        pm.mark_connected(&key("1.0.0.1", 1), 0);
        pm.mark_connected(&key("1.0.0.2", 2), 0);
        pm.penalize(&key("1.0.0.2", 2), PenaltyReason::ProtocolViolation, 0);

        let connected: Vec<_> = pm.iter_connected().collect();
        assert_eq!(connected.len(), 1);
    }
}
