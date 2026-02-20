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
use std::time::{SystemTime, UNIX_EPOCH};

/// SYN cookie challenge-response handshake.
///
/// Prevents connection flooding and identity spoofing:
/// 1. Node generates a random cookie for the connecting peer's IP
/// 2. Peer must sign the cookie with their node key
/// 3. Node verifies the signature matches the claimed identity
pub struct SynCookies {
    /// IP -> (cookie, timestamp, claimed_node_id)
    pending: HashMap<String, CookieEntry>,
    /// Maximum pending cookies (prevents memory exhaustion)
    max_pending: usize,
    /// Cookie validity period in seconds
    cookie_ttl_secs: u64,
    /// Rate limit: max cookies per IP per minute
    max_per_ip_per_min: u32,
    /// IP -> (count, window_start)
    rate_limits: HashMap<String, (u32, u64)>,
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
    /// - `max_pending` — maximum number of outstanding cookie challenges
    /// - `cookie_ttl_secs` — how long a cookie remains valid
    /// - `max_per_ip_per_min` — rate limit: max cookie generations per IP per 60s window
    pub fn new(max_pending: usize, cookie_ttl_secs: u64, max_per_ip_per_min: u32) -> Self {
        Self {
            pending: HashMap::new(),
            max_pending,
            cookie_ttl_secs,
            max_per_ip_per_min,
            rate_limits: HashMap::new(),
        }
    }

    /// Generate a cookie for an incoming connection.
    /// Returns `None` if rate limit exceeded for this IP or if at max capacity.
    pub fn generate(&mut self, peer_ip: &str) -> Option<[u8; 32]> {
        let now = unix_now();

        // Check rate limit
        if let Some((count, window_start)) = self.rate_limits.get_mut(peer_ip) {
            if now - *window_start < 60 {
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

        // Evict expired cookies
        self.cleanup_expired(now);

        if self.pending.len() >= self.max_pending {
            return None;
        }

        // Generate random cookie
        let mut cookie = [0u8; 32];
        getrandom::getrandom(&mut cookie).ok()?;

        self.pending.insert(
            peer_ip.to_string(),
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
        peer_ip: &str,
        claimed_id: &WalletAddress,
        signature: &Signature,
    ) -> bool {
        let entry = match self.pending.remove(peer_ip) {
            Some(e) => e,
            None => return false,
        };

        let now = unix_now();
        if now - entry.created_at > self.cookie_ttl_secs {
            return false;
        }

        // Verify signature of cookie against claimed identity
        if let Some(pubkey_bytes) = burst_crypto::decode_address(claimed_id.as_str()) {
            burst_crypto::verify_signature(
                &entry.cookie,
                signature,
                &PublicKey(pubkey_bytes),
            )
        } else {
            false
        }
    }

    /// Remove expired cookies and stale rate-limit windows.
    pub fn cleanup_expired(&mut self, now: u64) {
        self.pending
            .retain(|_, entry| now - entry.created_at <= self.cookie_ttl_secs);
        // Also clean up stale rate-limit windows (older than 60s)
        self.rate_limits
            .retain(|_, (_, window_start)| now - *window_start < 120);
    }

    /// Number of pending (outstanding) cookies.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_secs()
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

        let now = unix_now();
        sc.cleanup_expired(now);
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
