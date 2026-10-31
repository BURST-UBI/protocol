//! Peer metadata, discovery candidates, keepalive scheduling and score helpers.
//! The transport owns sockets and authentication. Stored flags and advertisements
//! do not independently prove a peer is live, reachable or trustworthy.

use burst_types::PeerAddress;
use rand::seq::SliceRandom;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::net::SocketAddrV4;

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
    /// Live routing key (handshake identity); discovered candidates use ip:port.
    pub peer_id: String,
    pub address: PeerAddress,
    /// Whether `address` may be used as a dial target. Discovery candidates set
    /// this before a successful dial; inbound source ports leave it false.
    pub address_is_dialable: bool,
    pub connected: bool,
    pub last_seen_secs: u64,
    /// Local score, initially zero. Penalty and reward helpers apply separate caps.
    pub score: i32,
    /// Whether the peer is currently banned.
    pub banned: bool,
    /// Unix timestamp (seconds) when the ban expires, if any.
    pub ban_until_secs: Option<u64>,
    /// Most recent telemetry data received from this peer.
    pub telemetry: Option<PeerTelemetry>,
    /// Advertised listening endpoint from keepalive slot zero. Its IP must match
    /// the transport record, but this does not prove the port is reachable.
    pub peering_addr: Option<PeerAddress>,
}

impl PeerState {
    fn dialable_address(&self) -> Option<&PeerAddress> {
        self.peering_addr
            .as_ref()
            .or_else(|| self.address_is_dialable.then_some(&self.address))
    }
}

/// Score at or below which a peer is banned.
const BAN_THRESHOLD: i32 = -500;
/// Duration of a ban in seconds (1 hour).
const BAN_DURATION_SECS: u64 = 3600;
/// Minimum reputation score.
const SCORE_MIN: i32 = -1000;
/// Maximum reputation score.
const SCORE_MAX: i32 = 100;

/// Maximum number of recent keepalive payloads to buffer for the reachout loop.
const LATEST_KEEPALIVES_CAPACITY: usize = 32;

/// Central registry for peer discovery, keepalive scheduling, scoring, and
/// ban management.
pub struct PeerManager {
    /// Discovered candidates keyed by ip:port; live peers by handshake identity.
    peers: HashMap<String, PeerState>,
    /// Upper bound on the number of peers we track.
    max_peers: usize,
    /// Configured bootstrap endpoints for initial discovery.
    bootstrap_peers: Vec<String>,
    /// How often (in seconds) we send keepalive messages.
    keepalive_interval_secs: u64,
    /// Timestamp (seconds) of the last keepalive round, or `None` if no
    /// keepalive has been sent yet (triggers an immediate first keepalive).
    last_keepalive_secs: Option<u64>,
    /// Bookkeeping count; public queries use the live peer map.
    num_connected: usize,
    /// Caller-supplied external address advertised in keepalives. Mapping
    /// lifecycle and reachability checks belong to the caller.
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

    /// Add an unconnected candidate at score zero. At capacity, only a lower-
    /// scoring disconnected entry can be evicted. A retained ban for this key
    /// prevents insertion; removing the record also removes that ban.
    pub fn add_peer(&mut self, address: PeerAddress) {
        let key = Self::peer_key(&address);

        if self.is_connected(&key) {
            return;
        }

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
            key.clone(),
            PeerState {
                peer_id: key.clone(),
                address,
                address_is_dialable: true,
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

    /// Register a live connection by identity, independently of its socket.
    /// Multiple nodes behind one NAT can advertise the same listening port.
    pub fn register_connection(
        &mut self,
        peer_id: String,
        address: PeerAddress,
        outbound: bool,
        now: u64,
    ) -> bool {
        let candidate = Self::peer_key(&address);
        if self.is_banned(&peer_id) || (outbound && self.is_banned(&candidate)) {
            return false;
        }
        if !self.peers.contains_key(&peer_id)
            && !(outbound && self.peers.contains_key(&candidate))
            && self.peers.len() >= self.max_peers
        {
            let victim = self
                .peers
                .iter()
                .filter(|(_, p)| !p.connected && !p.banned)
                .min_by_key(|(_, p)| p.score)
                .map(|(id, _)| id.clone());
            if let Some(id) = victim {
                self.peers.remove(&id);
            } else {
                return false;
            }
        }
        let previous = self.peers.remove(&peer_id).or_else(|| {
            if outbound {
                self.peers.remove(&candidate)
            } else {
                None
            }
        });
        let mut peer = previous.unwrap_or(PeerState {
            peer_id: peer_id.clone(),
            address: address.clone(),
            address_is_dialable: outbound,
            connected: false,
            last_seen_secs: now,
            score: 0,
            banned: false,
            ban_until_secs: None,
            telemetry: None,
            peering_addr: None,
        });
        peer.peer_id = peer_id.clone();
        // Preserve a previously dialed listening endpoint through a simultaneous
        // inbound/outbound tie-break, but never preserve an ephemeral source.
        if outbound || !peer.address_is_dialable {
            peer.address = address;
            peer.address_is_dialable = outbound;
        }
        peer.connected = true;
        peer.last_seen_secs = now;
        self.peers.insert(peer_id, peer);
        self.num_connected = self.iter_connected().count();
        true
    }

    /// Find the lowest-scoring disconnected record, including banned records.
    fn find_worst_peer(&self) -> Option<(String, i32)> {
        self.peers
            .iter()
            .filter(|(_, p)| !p.connected)
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

    /// A completed outbound handshake proves this endpoint reaches the live
    /// identity even when deterministic dedup keeps its inbound connection.
    /// Remember that association so bootstrap does not redial it forever.
    pub fn record_dialed_address(&mut self, peer_id: &str, address: PeerAddress) {
        if let Some(peer) = self.peers.get_mut(peer_id) {
            peer.address = address;
            peer.address_is_dialable = true;
        }
    }

    /// Replace last-seen time for an existing peer; the caller chooses when.
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

    /// Number of currently connected peers.
    /// Counts the live map, excluding banned peers.
    pub fn connected_count(&self) -> usize {
        self.iter_connected().count()
    }

    /// Check either a live identity or a dialable endpoint.
    pub fn is_connected(&self, key: &str) -> bool {
        self.peers
            .get(key)
            .map(|p| p.connected && !p.banned)
            .unwrap_or(false)
            || self.iter_connected().any(|(_, p)| {
                (p.address_is_dialable && Self::peer_key(&p.address) == key)
                    || p.peering_addr
                        .as_ref()
                        .is_some_and(|a| Self::peer_key(a) == key)
            })
    }

    /// Iterate over all connected (and not-banned) peers.
    pub fn iter_connected(&self) -> impl Iterator<Item = (&String, &PeerState)> {
        self.peers.iter().filter(|(_, p)| p.connected && !p.banned)
    }

    /// Return peer IDs of all connected peers from this IP.
    /// Multiple identities sharing one NAT IP remain separate peers.
    pub fn connected_peer_ids_from_ip(&self, ip: &str) -> Vec<String> {
        self.peers
            .iter()
            .filter(|(_, p)| p.address.ip == ip && p.connected && !p.banned)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Return peers that are known but not currently connected and not banned.
    /// These are candidates for outbound connection attempts.
    pub fn get_connectable_peers(&self) -> Vec<&PeerState> {
        self.peers
            .values()
            .filter(|p| !p.connected && !p.banned && p.dialable_address().is_some())
            .collect()
    }

    /// Dialable `ip:port` strings for known-but-unconnected peers, preferring
    /// each peer's advertised listening address (`peering_addr`) over its raw
    /// socket address, and excluding our own external address.
    ///
    /// The reachout loop attempts these endpoints. Returning an address does
    /// not establish reachability or guarantee a particular mesh topology.
    pub fn reachout_candidates(&self) -> Vec<String> {
        let self_ip_port = self
            .external_address
            .map(|address| (address.ip().to_string(), address.port()));
        self.peers
            .values()
            .filter(|peer| !peer.connected && !peer.banned)
            .filter_map(|peer| {
                let address = peer.dialable_address()?;
                if self.is_connected(&Self::peer_key(address)) {
                    return None;
                }
                if address.ip.is_empty() || address.ip == "0.0.0.0" || address.ip == "::" {
                    return None;
                }
                if let Some((self_ip, self_port)) = &self_ip_port {
                    if &address.ip == self_ip && address.port == *self_port {
                        return None;
                    }
                }
                Some(format!("{}:{}", address.ip, address.port))
            })
            .collect()
    }

    /// Return up to `count` random *connected* peer addresses, suitable for
    /// inclusion in a keepalive message.
    ///
    /// Prefers the advertised listening address over a dialable transport
    /// address. Reachability is not checked here.
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
            if let Some(addr) = peer.dialable_address() {
                result.push(addr.clone());
            }
        }

        result
    }

    /// Return up to `count` random *known* (connected or not) peer addresses.
    ///
    /// Includes unconnected discovery candidates with dialable addresses;
    /// excludes banned records. Entries are sampled without checking reachability.
    pub fn random_known_peers(&self, count: usize) -> Vec<PeerAddress> {
        let mut result = Vec::with_capacity(count);

        let known: Vec<&PeerState> = self.peers.values().filter(|p| !p.banned).collect();

        let mut rng = rand::thread_rng();
        let mut indices: Vec<usize> = (0..known.len()).collect();
        indices.shuffle(&mut rng);

        for i in indices {
            if result.len() >= count {
                break;
            }
            let peer = known[i];
            if let Some(addr) = peer.dialable_address() {
                result.push(addr.clone());
            }
        }

        result
    }

    /// Return up to `count` random *known* peer addresses with this
    /// node's own external address in slot 0 (for self-advertisement).
    pub fn random_peers_with_self(&self, count: usize) -> Vec<PeerAddress> {
        let mut result = Vec::with_capacity(count);
        if count == 0 {
            return result;
        }
        // Slot zero is ALWAYS self, never an unrelated discovered peer.
        result.push(self.self_peer_address().unwrap_or(PeerAddress {
            ip: "0.0.0.0".into(),
            port: 0,
        }));
        let rest = self.random_known_peers(count.saturating_sub(result.len()));
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

    /// Return the stored advertisement without checking router mapping state.
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
            if addr.port != 0 && addr.ip != "0.0.0.0" && addr.ip != "::" && !addr.ip.is_empty() {
                self.add_peer(addr);
            }
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

    /// Store a nonzero advertised port only when its IP matches the peer record.
    pub fn set_peering_addr(&mut self, peer_id: &str, addr: PeerAddress) {
        if let Some(peer) = self.peers.get_mut(peer_id) {
            // Slot zero may withdraw a mapping. Never reinterpret a NAT source
            // port or another peer's address as this node's listening endpoint.
            peer.peering_addr = if addr.port != 0 && addr.ip == peer.address.ip {
                Some(addr)
            } else {
                None
            };
        }
    }

    /// Return addresses of all currently connected peers (for cache persistence).
    pub fn connected_peer_addresses(&self) -> Vec<(String, u64)> {
        self.peers
            .iter()
            .filter(|(_, p)| p.connected && !p.banned)
            .filter_map(|(_, p)| {
                p.dialable_address()
                    .map(|a| (Self::peer_key(a), p.last_seen_secs))
            })
            .collect()
    }

    // -- Scoring / banning -----------------------------------------------------

    /// Apply a caller-reported penalty; returns true when it marks a ban.
    /// ProtocolViolation bans immediately. This helper does not detect offenses
    /// or close sockets, and current node code does not call it.
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

    /// Add caller-supplied score, capped above at SCORE_MAX. Current transport
    /// calls this after enqueueing block traffic, before block validation.
    pub fn reward(&mut self, peer_id: &str, amount: i32) {
        if let Some(peer) = self.peers.get_mut(peer_id) {
            peer.score = (peer.score + amount).min(SCORE_MAX);
        }
    }

    /// Read the stored ban flag; expiry is applied only by `check_bans`.
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
                self.num_connected = self.num_connected.saturating_sub(1);
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
    fn shared_nat_identities_route_separately_without_gossiping_source_ports() {
        let mut pm = PeerManager::new(10);
        assert!(pm.register_connection("node-a".into(), addr("1.2.3.4", 41000), false, 100));
        assert!(pm.register_connection("node-b".into(), addr("1.2.3.4", 42000), false, 100));
        assert_eq!(pm.connected_count(), 2);
        assert_eq!(pm.connected_peer_ids_from_ip("1.2.3.4").len(), 2);
        assert!(pm.random_known_peers(8).is_empty());
        assert!(pm.connected_peer_addresses().is_empty());
        assert_eq!(pm.random_peers_with_self(8)[0].port, 0);
        pm.set_peering_addr("node-a", addr("1.2.3.4", 17076));
        assert_eq!(
            pm.connected_peer_addresses(),
            vec![("1.2.3.4:17076".into(), 100)]
        );
        pm.mark_disconnected("node-b");
        assert!(pm.reachout_candidates().is_empty());
        pm.mark_disconnected("node-a");
        assert_eq!(pm.reachout_candidates(), vec!["1.2.3.4:17076"]);
        pm.set_peering_addr("node-a", addr("0.0.0.0", 0));
        assert!(pm.reachout_candidates().is_empty());
    }

    #[test]
    fn identity_replacement_keeps_one_peer_and_preserves_dialable_endpoint() {
        let mut pm = PeerManager::new(2);
        pm.add_peer(addr("1.2.3.4", 7076));
        assert!(pm.register_connection("node-a".into(), addr("1.2.3.4", 7076), true, 100));
        assert!(pm.is_connected("1.2.3.4:7076"));
        assert!(pm.register_connection("node-a".into(), addr("1.2.3.4", 41000), false, 101));
        assert_eq!(pm.connected_count(), 1);
        assert_eq!(
            pm.connected_peer_addresses(),
            vec![("1.2.3.4:7076".into(), 101)]
        );
        assert!(pm.register_connection("node-b".into(), addr("1.2.3.4", 42000), false, 101));
        assert!(!pm.register_connection("node-c".into(), addr("1.2.3.4", 43000), false, 101));
        pm.penalize("node-a", PenaltyReason::ProtocolViolation, 102);
        assert!(!pm.register_connection("node-a".into(), addr("1.2.3.4", 44000), false, 103));
    }

    #[test]
    fn keepalive_slot_zero_is_never_an_unrelated_peer() {
        let mut pm = PeerManager::new(10);
        pm.add_peer(addr("1.2.3.4", 7076));
        let peers = pm.random_peers_with_self(8);
        assert_eq!(peers[0].ip, "0.0.0.0");
        assert_eq!(peers.len(), 2);
        pm.process_keepalive(peers);
        assert_eq!(pm.peers.len(), 1);
        assert!(pm.random_peers_with_self(0).is_empty());
    }

    #[test]
    fn losing_outbound_dial_associates_endpoint_with_inbound_identity() {
        let mut pm = PeerManager::new(10);
        pm.register_connection("node-a".into(), addr("1.2.3.4", 41000), false, 100);
        assert!(!pm.is_connected("1.2.3.4:7076"));
        pm.record_dialed_address("node-a", addr("1.2.3.4", 7076));
        assert!(pm.is_connected("1.2.3.4:7076"));
        pm.set_peering_addr("node-a", addr("1.2.3.4", 17076));
        assert!(pm.is_connected("1.2.3.4:7076"));
        assert!(pm.is_connected("1.2.3.4:17076"));
        assert_eq!(pm.connected_count(), 1);
    }

    #[test]
    fn reachout_candidates_prefer_peering_addr_exclude_connected_and_self() {
        use std::net::{Ipv4Addr, SocketAddrV4};
        let mut pm = PeerManager::new(10);
        pm.set_external_address(SocketAddrV4::new(Ipv4Addr::new(9, 9, 9, 9), 7076));

        // Connected peer — excluded.
        pm.add_peer(addr("1.1.1.1", 7076));
        pm.mark_connected(&key("1.1.1.1", 7076), 100);
        // Disconnected, no peering_addr — dialed by its raw address.
        pm.add_peer(addr("2.2.2.2", 7076));
        // Disconnected, known only by an ephemeral source port but with an
        // advertised listening address — must be dialed on the listening port.
        pm.add_peer(addr("3.3.3.3", 40000));
        pm.set_peering_addr(&key("3.3.3.3", 40000), addr("3.3.3.3", 7076));
        // A record of ourselves — excluded.
        pm.add_peer(addr("9.9.9.9", 7076));

        let cands = pm.reachout_candidates();
        assert!(cands.contains(&"2.2.2.2:7076".to_string()));
        assert!(cands.contains(&"3.3.3.3:7076".to_string()));
        assert!(!cands.contains(&"3.3.3.3:40000".to_string()));
        assert!(!cands.iter().any(|c| c.starts_with("1.1.1.1")));
        assert!(!cands.contains(&"9.9.9.9:7076".to_string()));
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
    fn random_known_peers_returns_all_unbanned() {
        let mut pm = PeerManager::new(10);
        pm.add_peer(addr("1.0.0.1", 1));
        pm.add_peer(addr("1.0.0.2", 2));
        pm.add_peer(addr("1.0.0.3", 3));
        pm.mark_connected(&key("1.0.0.1", 1), 0);
        // 1.0.0.2 is not connected, 1.0.0.3 is not connected
        pm.penalize(&key("1.0.0.3", 3), PenaltyReason::ProtocolViolation, 0);

        let known = pm.random_known_peers(10);
        // Should return 1.0.0.1 and 1.0.0.2 (not 1.0.0.3 which is banned)
        assert_eq!(known.len(), 2);
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

    #[test]
    fn cleanup_idle_disconnects_peers() {
        let mut pm = PeerManager::new(10);
        pm.add_peer(addr("1.0.0.1", 1));
        pm.add_peer(addr("1.0.0.2", 2));
        pm.mark_connected(&key("1.0.0.1", 1), 100);
        pm.mark_connected(&key("1.0.0.2", 2), 100);
        assert_eq!(pm.connected_count(), 2);

        // At t=500, peers last_seen at 100 are idle (>300 sec threshold)
        let idle = pm.cleanup_idle(500, 300);
        assert_eq!(idle.len(), 2);
        assert_eq!(pm.connected_count(), 0);
    }
}
