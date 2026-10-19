//! SYN cookie challenge-response handshake.
//!
//! Prevents connection flooding and identity spoofing:
//! 1. Node generates a random cookie for the connecting peer's IP
//! 2. Peer must sign the cookie with their node key
//! 3. Node verifies the signature matches the claimed identity
//!
//! Inspired by rsnano's SYN cookie mechanism. Rate-limits per IP and caps
//! total pending cookies to prevent memory exhaustion.

use burst_types::{PublicKey, Signature, WalletAddress};
use std::collections::HashMap;
use std::time::Instant;

/// SYN cookie challenge-response handshake.
///
/// Prevents connection flooding and identity spoofing:
/// 1. Node generates a random cookie for the connecting peer's IP
/// 2. Peer must sign the cookie with their node key
/// 3. Node verifies the signature matches the claimed identity
pub struct SynCookies {
    /// Connection ID -> cookie. Rate limiting remains per IP, not per port.
    pending: HashMap<String, CookieEntry>,
    /// Maximum pending cookies (prevents memory exhaustion)
    max_pending: usize,
    /// Cookie validity period in seconds
    cookie_ttl_secs: u64,
    /// Rate limit: max cookies per IP per minute
    max_per_ip_per_min: u32,
    /// IP -> (count, window_start), bounded by `max_pending` independently of
    /// challenge consumption. New IPs wait for stale windows when this is full.
    rate_limits: HashMap<String, (u32, u64)>,
    started_at: Instant,
}

struct CookieEntry {
    cookie: [u8; 32],
    created_at: u64,
    #[allow(dead_code)]
    peer_ip: String,
}

impl SynCookies {
    /// Create a new SYN cookie manager.
    ///
    /// # Arguments
    /// - `max_pending` — maximum outstanding challenges and tracked IP windows
    /// - `cookie_ttl_secs` — how long a cookie remains valid
    /// - `max_per_ip_per_min` — rate limit: max cookie generations per IP per 60s window
    pub fn new(max_pending: usize, cookie_ttl_secs: u64, max_per_ip_per_min: u32) -> Self {
        Self {
            pending: HashMap::new(),
            max_pending,
            cookie_ttl_secs,
            max_per_ip_per_min,
            rate_limits: HashMap::new(),
            started_at: Instant::now(),
        }
    }

    /// Generate a cookie for an incoming connection.
    /// Returns `None` if rate limit exceeded for this IP or if at max capacity.
    pub fn generate(&mut self, peer_ip: &str) -> Option<[u8; 32]> {
        self.generate_for_connection(peer_ip, peer_ip)
    }

    /// Challenge a distinct TCP connection without replacing challenges for
    /// other peers sharing its IP (localhost or a NAT gateway).
    pub fn generate_for_connection(
        &mut self,
        peer_ip: &str,
        connection_id: &str,
    ) -> Option<[u8; 32]> {
        self.generate_for_connection_at(peer_ip, connection_id, self.started_at.elapsed().as_secs())
    }

    fn generate_for_connection_at(
        &mut self,
        peer_ip: &str,
        connection_id: &str,
        now: u64,
    ) -> Option<[u8; 32]> {
        self.cleanup_expired_at(now);
        if self.max_per_ip_per_min == 0
            || (!self.rate_limits.contains_key(peer_ip)
                && self.rate_limits.len() >= self.max_pending)
        {
            return None;
        }

        // Check rate limit
        if let Some((count, window_start)) = self.rate_limits.get_mut(peer_ip) {
            if now.saturating_sub(*window_start) < 60 {
                if *count >= self.max_per_ip_per_min {
                    return None;
                }
                *count += 1;
            } else {
                *window_start = now;
                *count = 1;
            }
        } else {
            self.rate_limits.insert(peer_ip.to_string(), (1, now));
        }

        if self.pending.len() >= self.max_pending {
            return None;
        }

        // Generate random cookie
        let mut cookie = [0u8; 32];
        getrandom::getrandom(&mut cookie).ok()?;

        self.pending.insert(
            connection_id.to_string(),
            CookieEntry {
                cookie,
                created_at: now,
                peer_ip: peer_ip.to_string(),
            },
        );

        Some(cookie)
    }

    /// Verify a peer's response to a cookie challenge.
    /// The peer must sign the cookie with their private key, proving they
    /// own the claimed identity.
    pub fn verify(
        &mut self,
        connection_id: &str,
        claimed_id: &WalletAddress,
        signature: &Signature,
    ) -> bool {
        let Some(cookie) = self.take_cookie(connection_id) else {
            return false;
        };
        if let Some(pubkey_bytes) = burst_crypto::decode_address(claimed_id.as_str()) {
            burst_crypto::verify_signature(&cookie, signature, &PublicKey(pubkey_bytes))
        } else {
            false
        }
    }

    /// Consume an unexpired challenge once, for a context-bound handshake proof.
    pub fn take_cookie(&mut self, connection_id: &str) -> Option<[u8; 32]> {
        let entry = self.pending.remove(connection_id)?;

        let now = self.started_at.elapsed().as_secs();
        if now.saturating_sub(entry.created_at) > self.cookie_ttl_secs {
            return None;
        }
        Some(entry.cookie)
    }

    /// Remove expired cookies and stale rate-limit windows.
    pub fn cleanup_expired(&mut self) {
        self.cleanup_expired_at(self.started_at.elapsed().as_secs());
    }

    fn cleanup_expired_at(&mut self, now: u64) {
        self.pending
            .retain(|_, entry| now.saturating_sub(entry.created_at) <= self.cookie_ttl_secs);
        // A completed window no longer constrains its IP and can release its slot.
        self.rate_limits
            .retain(|_, (_, window_start)| now.saturating_sub(*window_start) < 60);
    }

    /// Number of pending (outstanding) cookies.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cookies() -> SynCookies {
        SynCookies::new(100, 30, 5)
    }

    #[test]
    fn generate_returns_cookie() {
        let mut sc = make_cookies();
        let cookie = sc.generate("192.168.1.1");
        assert!(cookie.is_some());
        assert_eq!(sc.pending_count(), 1);
    }

    #[test]
    fn concurrent_connections_from_same_ip_keep_independent_challenges() {
        let mut sc = make_cookies();
        let a = sc
            .generate_for_connection("127.0.0.1", "127.0.0.1:40001")
            .unwrap();
        let b = sc
            .generate_for_connection("127.0.0.1", "127.0.0.1:40002")
            .unwrap();
        assert_eq!(sc.pending_count(), 2);
        for (id, cookie) in [("127.0.0.1:40001", a), ("127.0.0.1:40002", b)] {
            let kp = burst_crypto::generate_keypair();
            let address = burst_crypto::derive_address(&kp.public);
            let signature = burst_crypto::sign_message(&cookie, &kp.private);
            assert!(sc.verify(id, &address, &signature));
            assert!(
                !sc.verify(id, &address, &signature),
                "cookie must be single-use"
            );
        }
        assert_eq!(sc.pending_count(), 0);
    }

    #[test]
    fn distinct_connections_still_share_the_ip_rate_limit() {
        let mut sc = SynCookies::new(100, 30, 2);
        assert!(sc
            .generate_for_connection("127.0.0.1", "127.0.0.1:40001")
            .is_some());
        assert!(sc
            .generate_for_connection("127.0.0.1", "127.0.0.1:40002")
            .is_some());
        assert!(sc
            .generate_for_connection("127.0.0.1", "127.0.0.1:40003")
            .is_none());
        assert!(sc
            .generate_for_connection("127.0.0.2", "127.0.0.2:40003")
            .is_some());
    }

    #[test]
    fn consumed_challenges_do_not_allow_unbounded_ip_accounting() {
        let mut sc = SynCookies::new(2, 30, 5);
        for ip in ["192.0.2.1", "192.0.2.2"] {
            let cookie = sc.generate_for_connection_at(ip, ip, 0).unwrap();
            assert_eq!(sc.take_cookie(ip), Some(cookie));
        }
        assert_eq!(sc.pending_count(), 0);
        for number in 0..10_000 {
            assert!(sc
                .generate_for_connection_at(&format!("new-{number}"), "connection", 1)
                .is_none());
        }
        assert_eq!(sc.rate_limits.len(), 2);
        assert_eq!(sc.pending_count(), 0);
        assert!(sc
            .generate_for_connection_at("192.0.2.1", "reconnect", 1)
            .is_some());
        assert!(sc
            .generate_for_connection_at("192.0.2.3", "fresh", 60)
            .is_some());
        assert_eq!(sc.rate_limits.len(), 1);
        assert_eq!(sc.pending_count(), 1);
    }

    #[test]
    fn rate_window_reopens_without_relaxing_same_ip_limits() {
        let mut sc = SynCookies::new(8, 30, 2);
        for connection in ["a", "b"] {
            assert!(sc
                .generate_for_connection_at("192.0.2.1", connection, 0)
                .is_some());
            assert!(sc.take_cookie(connection).is_some());
        }
        assert!(sc
            .generate_for_connection_at("192.0.2.1", "c", 59)
            .is_none());
        assert!(sc
            .generate_for_connection_at("192.0.2.1", "d", 60)
            .is_some());
        assert!(sc
            .generate_for_connection_at("192.0.2.1", "e", 60)
            .is_some());
        assert!(sc
            .generate_for_connection_at("192.0.2.1", "f", 60)
            .is_none());
    }

    #[test]
    fn zero_capacity_or_rate_stays_closed_without_allocating_windows() {
        for mut sc in [SynCookies::new(0, 30, 5), SynCookies::new(8, 30, 0)] {
            assert!(sc.generate("192.0.2.1").is_none());
            assert!(sc.rate_limits.is_empty());
            assert_eq!(sc.pending_count(), 0);
        }
    }

    #[test]
    fn generate_unique_per_call() {
        let mut sc = make_cookies();
        let c1 = sc.generate("10.0.0.1").unwrap();
        let c2 = sc.generate("10.0.0.2").unwrap();
        // Overwhelmingly likely to be different
        assert_ne!(c1, c2);
    }

    #[test]
    fn generate_replaces_for_same_ip() {
        let mut sc = make_cookies();
        let _c1 = sc.generate("10.0.0.1").unwrap();
        let _c2 = sc.generate("10.0.0.1").unwrap();
        // Same IP overwrites, so still just 1 pending
        assert_eq!(sc.pending_count(), 1);
    }

    #[test]
    fn rate_limit_blocks_excessive_requests() {
        let mut sc = SynCookies::new(100, 30, 3);
        assert!(sc.generate("10.0.0.1").is_some()); // 1
        assert!(sc.generate("10.0.0.1").is_some()); // 2
        assert!(sc.generate("10.0.0.1").is_some()); // 3
        assert!(sc.generate("10.0.0.1").is_none()); // 4 -> rate limited
    }

    #[test]
    fn rate_limit_does_not_affect_other_ips() {
        let mut sc = SynCookies::new(100, 30, 1);
        assert!(sc.generate("10.0.0.1").is_some());
        assert!(sc.generate("10.0.0.1").is_none()); // rate limited
        assert!(sc.generate("10.0.0.2").is_some()); // different IP is fine
    }

    #[test]
    fn max_pending_cap() {
        let mut sc = SynCookies::new(2, 30, 100);
        assert!(sc.generate("10.0.0.1").is_some());
        assert!(sc.generate("10.0.0.2").is_some());
        assert!(sc.generate("10.0.0.3").is_none()); // at capacity
    }

    #[test]
    fn cleanup_removes_expired() {
        let mut sc = make_cookies();
        // Manually insert an expired entry
        sc.pending.insert(
            "old_peer".to_string(),
            CookieEntry {
                cookie: [0xAA; 32],
                created_at: 0, // epoch = very old
                peer_ip: "old_peer".to_string(),
            },
        );
        assert_eq!(sc.pending_count(), 1);

        sc.cleanup_expired_at(31);
        assert_eq!(sc.pending_count(), 0);
    }

    #[test]
    fn verify_missing_cookie_returns_false() {
        let mut sc = make_cookies();
        let addr = WalletAddress::new("brst_test1234567890");
        let sig = Signature([0u8; 64]);
        assert!(!sc.verify("10.0.0.1", &addr, &sig));
    }

    #[test]
    fn verify_expired_cookie_returns_false() {
        let mut sc = make_cookies();
        sc.started_at = Instant::now() - std::time::Duration::from_secs(31);
        // Insert an already-expired cookie
        sc.pending.insert(
            "10.0.0.1".to_string(),
            CookieEntry {
                cookie: [0xBB; 32],
                created_at: 0,
                peer_ip: "10.0.0.1".to_string(),
            },
        );
        let addr = WalletAddress::new("brst_test1234567890");
        let sig = Signature([0u8; 64]);
        assert!(!sc.verify("10.0.0.1", &addr, &sig));
    }

    #[test]
    fn verify_with_valid_signature() {
        let mut sc = make_cookies();
        let cookie = sc.generate("10.0.0.5").unwrap();

        // Generate a real keypair and sign the cookie
        let kp = burst_crypto::generate_keypair();
        let sig = burst_crypto::sign_message(&cookie, &kp.private);
        let address = burst_crypto::derive_address(&kp.public);

        assert!(sc.verify("10.0.0.5", &address, &sig));
        // Cookie is consumed after verification
        assert_eq!(sc.pending_count(), 0);
    }

    #[test]
    fn verify_with_wrong_signature_fails() {
        let mut sc = make_cookies();
        let _cookie = sc.generate("10.0.0.6").unwrap();

        // Sign something different
        let kp = burst_crypto::generate_keypair();
        let wrong_sig = burst_crypto::sign_message(b"wrong data", &kp.private);
        let address = burst_crypto::derive_address(&kp.public);

        assert!(!sc.verify("10.0.0.6", &address, &wrong_sig));
    }

    #[test]
    fn verify_with_wrong_identity_fails() {
        let mut sc = make_cookies();
        let cookie = sc.generate("10.0.0.7").unwrap();

        // Sign with one key but claim a different identity
        let kp1 = burst_crypto::generate_keypair();
        let kp2 = burst_crypto::generate_keypair();
        let sig = burst_crypto::sign_message(&cookie, &kp1.private);
        let wrong_address = burst_crypto::derive_address(&kp2.public);

        assert!(!sc.verify("10.0.0.7", &wrong_address, &sig));
    }
}
