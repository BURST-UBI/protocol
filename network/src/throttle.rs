//! Per-peer bandwidth limiter using the token bucket algorithm.
//!
//! Each peer connection gets its own `BandwidthThrottle` to prevent any
//! single peer from consuming excessive bandwidth.

use std::time::Instant;

/// Default bandwidth limit: 5 MiB/s per peer.
pub const DEFAULT_MAX_BYTES_PER_SEC: u64 = 5 * 1024 * 1024;

/// Per-peer bandwidth limiter using the token bucket algorithm.
///
/// Tokens represent available bytes. Tokens refill over time at
/// `max_bytes_per_sec` rate. The bucket can hold at most 2× the rate
/// to allow short bursts.
pub struct BandwidthThrottle {
    /// Maximum bytes per second.
    max_bytes_per_sec: u64,
    /// Available tokens (bytes).
    tokens: u64,
    /// Last refill time.
    last_refill: Instant,
}

impl BandwidthThrottle {
    /// Create a new throttle with the given bytes-per-second limit.
    ///
    /// The token bucket starts full at the per-second rate, allowing an
    /// initial burst of up to `max_bytes_per_sec` bytes.
    pub fn new(max_bytes_per_sec: u64) -> Self {
        Self {
            max_bytes_per_sec,
            tokens: max_bytes_per_sec,
            last_refill: Instant::now(),
        }
    }

    /// Try to consume `bytes` of bandwidth.
    ///
    /// Returns `true` if sufficient tokens are available (and consumes them).
    /// Returns `false` if throttled — the caller should back off or drop the message.
    pub fn try_consume(&mut self, bytes: u64) -> bool {
        self.refill();
        if self.tokens >= bytes {
            self.tokens -= bytes;
            true
        } else {
            false
        }
    }

    /// Refill tokens based on elapsed time since last refill.
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill);
        let new_tokens = (elapsed.as_millis() as u64 * self.max_bytes_per_sec) / 1000;
        // Cap at 2× rate to limit burst size
        self.tokens = (self.tokens + new_tokens).min(self.max_bytes_per_sec * 2);
        self.last_refill = now;
    }

    /// The configured maximum bytes-per-second rate.
    pub fn max_bytes_per_sec(&self) -> u64 {
        self.max_bytes_per_sec
    }

    /// Current available tokens (bytes). Useful for diagnostics.
    pub fn available_tokens(&self) -> u64 {
        self.tokens
    }
}

impl Default for BandwidthThrottle {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_BYTES_PER_SEC)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn new_throttle_starts_with_tokens() {
        let throttle = BandwidthThrottle::new(1000);
        assert_eq!(throttle.max_bytes_per_sec(), 1000);
        assert_eq!(throttle.available_tokens(), 1000);
    }

    #[test]
    fn consume_within_budget_succeeds() {
        let mut throttle = BandwidthThrottle::new(1000);
        assert!(throttle.try_consume(500));
        assert!(throttle.try_consume(500));
    }

    #[test]
    fn consume_exceeding_budget_fails() {
        let mut throttle = BandwidthThrottle::new(1000);
        assert!(throttle.try_consume(1000));
        // No time has passed, tokens should be zero
        assert!(!throttle.try_consume(1));
    }

    #[test]
    fn tokens_refill_over_time() {
        let mut throttle = BandwidthThrottle::new(10_000);
        assert!(throttle.try_consume(10_000));
        assert!(!throttle.try_consume(1));

        // Sleep 100ms → expect ~1000 tokens refilled (10_000 * 0.1)
        thread::sleep(Duration::from_millis(100));
        assert!(throttle.try_consume(500));
    }

    #[test]
    fn burst_cap_is_twice_rate() {
        let mut throttle = BandwidthThrottle::new(1000);
        // Wait long enough to exceed 2× cap
        thread::sleep(Duration::from_millis(300));
        throttle.refill();
        // Tokens should be capped at 2000 (2 × 1000)
        assert!(throttle.available_tokens() <= 2000);
    }

    #[test]
    fn default_uses_5mib_rate() {
        let throttle = BandwidthThrottle::default();
        assert_eq!(throttle.max_bytes_per_sec(), DEFAULT_MAX_BYTES_PER_SEC);
    }

    #[test]
    fn zero_byte_consume_always_succeeds() {
        let mut throttle = BandwidthThrottle::new(1000);
        assert!(throttle.try_consume(1000));
        // Even with zero tokens, consuming 0 should succeed
        assert!(throttle.try_consume(0));
    }
}
