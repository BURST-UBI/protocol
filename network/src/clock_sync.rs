//! Clock synchronization — required for BRN computation.
//!
//! BRN is a function of time, so nodes must agree on the current time.
//! Uses NTP or peer-to-peer time comparison with a tolerance threshold.

use crate::NetworkError;
use burst_types::Timestamp;

/// Clock synchronization service.
pub struct ClockSync {
    /// Maximum acceptable clock drift in milliseconds.
    pub max_drift_ms: i64,
    /// Current estimated offset from true UTC in milliseconds.
    pub offset_ms: i64,
    /// Unix timestamp (seconds) of the last successful sync.
    pub last_sync_secs: u64,
    /// Number of sync operations performed.
    pub sync_count: u32,
}

impl ClockSync {
    pub fn new(max_drift_ms: i64) -> Self {
        Self {
            max_drift_ms,
            offset_ms: 0,
            last_sync_secs: 0,
            sync_count: 0,
        }
    }

    /// Sync with NTP servers to determine clock offset.
    ///
    /// Queries pool.ntp.org using a minimal SNTPv4 implementation.
    /// Falls back to zero offset if the query fails (graceful degradation).
    pub async fn sync_ntp(&mut self) -> Result<(), NetworkError> {
        match Self::query_ntp_offset().await {
            Ok(offset_ms) => {
                // Apply EMA: blend new NTP offset with existing
                if self.sync_count == 0 {
                    self.offset_ms = offset_ms;
                } else {
                    self.offset_ms = (self.offset_ms * 7 + offset_ms) / 8;
                }
            }
            Err(_) => {
                // Graceful degradation: keep existing offset
                tracing::warn!(
                    "NTP sync failed, keeping existing offset of {}ms",
                    self.offset_ms
                );
            }
        }
        self.last_sync_secs = Timestamp::now().as_secs();
        self.sync_count += 1;
        Ok(())
    }

    /// Query NTP server and return offset in milliseconds.
    ///
    /// Implements a minimal SNTPv4 client (RFC 4330):
    /// - Sends a 48-byte request to pool.ntp.org:123
    /// - Parses the transmit timestamp from the response
    /// - Computes offset as (server_time - local_time)
    async fn query_ntp_offset() -> Result<i64, NetworkError> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let socket = tokio::net::UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| NetworkError::ConnectionFailed(format!("NTP bind failed: {e}")))?;

        // NTP uses epoch of 1900-01-01; Unix epoch is 1970-01-01
        // Difference: 70 years = 2208988800 seconds
        const NTP_EPOCH_OFFSET: u64 = 2_208_988_800;

        // Build SNTPv4 request: 48 bytes, LI=0, VN=4, Mode=3 (client)
        let mut request = [0u8; 48];
        request[0] = 0x23; // LI=0, VN=4, Mode=3

        // Record local time before sending
        let t1 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();

        socket
            .send_to(&request, "pool.ntp.org:123")
            .await
            .map_err(|e| NetworkError::ConnectionFailed(format!("NTP send failed: {e}")))?;

        let mut response = [0u8; 48];
        let recv_future = socket.recv_from(&mut response);
        let nbytes =
            match tokio::time::timeout(std::time::Duration::from_secs(5), recv_future).await {
                Ok(Ok((n, _addr))) => n,
                Ok(Err(e)) => {
                    return Err(NetworkError::ConnectionFailed(format!(
                        "NTP recv failed: {e}"
                    )));
                }
                Err(_elapsed) => {
                    return Err(NetworkError::ConnectionFailed("NTP timeout".into()));
                }
            };

        if nbytes < 48 {
            return Err(NetworkError::ConnectionFailed(
                "NTP response too short".into(),
            ));
        }

        // Parse transmit timestamp (bytes 40-47): seconds since NTP epoch (big-endian u32)
        let ntp_secs = u32::from_be_bytes([response[40], response[41], response[42], response[43]]);
        if (ntp_secs as u64) < NTP_EPOCH_OFFSET {
            return Err(NetworkError::ConnectionFailed(
                "NTP response contains invalid timestamp".into(),
            ));
        }
        let server_unix_secs = ntp_secs as u64 - NTP_EPOCH_OFFSET;

        // Record local time after receiving
        let t4 = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();

        // Simple offset calculation: server_time - midpoint_of_local_times
        let local_midpoint_ms = ((t1.as_millis() + t4.as_millis()) / 2) as i64;
        let server_ms = (server_unix_secs * 1000) as i64;
        let offset = server_ms - local_midpoint_ms;

        Ok(offset)
    }

    /// Sync with a peer by comparing timestamps.
    ///
    /// Computes the drift between local time and the peer's reported time,
    /// then updates the offset using an exponential moving average (EMA)
    /// with a weight of 7/8 on the old value, 1/8 on the new observation.
    /// This smooths out individual peer discrepancies while still tracking
    /// the true offset over time.
    ///
    /// Returns an error if the peer's drift exceeds `max_drift_ms`.
    pub async fn sync_with_peer(&mut self, peer_timestamp: Timestamp) -> Result<(), NetworkError> {
        let local_now = Timestamp::now();
        let peer_secs = peer_timestamp.as_secs() as i64;
        let local_secs = local_now.as_secs() as i64;
        let drift = peer_secs - local_secs;

        // Update offset using exponential moving average
        self.offset_ms = (self.offset_ms * 7 + drift * 1000) / 8;
        self.last_sync_secs = local_now.as_secs();
        self.sync_count += 1;

        if drift.abs() * 1000 > self.max_drift_ms {
            return Err(NetworkError::ClockDrift {
                drift_ms: drift * 1000,
                max_ms: self.max_drift_ms,
            });
        }
        Ok(())
    }

    /// Get the current adjusted timestamp.
    pub fn now(&self) -> Timestamp {
        let raw = Timestamp::now();
        Timestamp::new((raw.as_secs() as i64 + self.offset_ms / 1000) as u64)
    }

    /// Validate that a timestamp is within acceptable drift.
    pub fn validate_timestamp(&self, ts: Timestamp) -> bool {
        let now = self.now();
        let diff_secs = if now.as_secs() > ts.as_secs() {
            now.as_secs() - ts.as_secs()
        } else {
            ts.as_secs() - now.as_secs()
        };
        (diff_secs as i64 * 1000) <= self.max_drift_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_timestamp_within_drift() {
        let sync = ClockSync::new(5000); // 5 second drift tolerance
        let now = sync.now();
        let within_drift = Timestamp::new(now.as_secs() - 3); // 3 seconds ago
        assert!(sync.validate_timestamp(within_drift));
    }

    #[test]
    fn test_validate_timestamp_exceeds_drift() {
        let sync = ClockSync::new(5000); // 5 second drift tolerance
        let now = sync.now();
        let too_old = Timestamp::new(now.as_secs() - 10); // 10 seconds ago
        assert!(!sync.validate_timestamp(too_old));
    }

    #[test]
    fn test_validate_timestamp_future_within_drift() {
        let sync = ClockSync::new(5000); // 5 second drift tolerance
        let now = sync.now();
        let future = Timestamp::new(now.as_secs() + 3); // 3 seconds in future
        assert!(sync.validate_timestamp(future));
    }

    #[test]
    fn test_validate_timestamp_future_exceeds_drift() {
        let sync = ClockSync::new(5000); // 5 second drift tolerance
        let now = sync.now();
        let too_future = Timestamp::new(now.as_secs() + 10); // 10 seconds in future
        assert!(!sync.validate_timestamp(too_future));
    }

    #[test]
    fn test_validate_timestamp_exact_boundary() {
        let sync = ClockSync::new(5000); // 5 second drift tolerance
        let now = sync.now();
        let boundary = Timestamp::new(now.as_secs() - 5); // exactly 5 seconds ago
        assert!(sync.validate_timestamp(boundary)); // should be valid (<=)
    }

    #[test]
    fn test_validate_timestamp_with_offset() {
        let mut sync = ClockSync::new(5000);
        sync.offset_ms = 2000; // 2 second offset
        let now = sync.now();
        // Timestamp 3 seconds ago relative to adjusted time
        let ts = Timestamp::new(now.as_secs() - 3);
        assert!(sync.validate_timestamp(ts));
    }

    #[test]
    fn test_now_with_offset() {
        let mut sync = ClockSync::new(5000);
        sync.offset_ms = 5000; // 5 second offset
        let now1 = sync.now();
        let now2 = sync.now();
        // Both should be consistent (within 1 second of each other)
        let diff = if now1.as_secs() > now2.as_secs() {
            now1.as_secs() - now2.as_secs()
        } else {
            now2.as_secs() - now1.as_secs()
        };
        assert!(diff <= 1);
    }

    // --- New tests for sync_ntp and sync_with_peer ---

    #[tokio::test]
    async fn test_sync_ntp_updates_fields() {
        let mut sync = ClockSync::new(5000);
        sync.offset_ms = 9999; // some stale offset

        // sync_ntp always succeeds (graceful degradation on NTP failure)
        sync.sync_ntp().await.unwrap();

        // sync_count and last_sync_secs should be updated regardless
        assert!(sync.last_sync_secs > 0);
        assert_eq!(sync.sync_count, 1);
    }

    #[tokio::test]
    async fn test_sync_ntp_increments_count() {
        let mut sync = ClockSync::new(5000);

        sync.sync_ntp().await.unwrap();
        sync.sync_ntp().await.unwrap();
        sync.sync_ntp().await.unwrap();

        assert_eq!(sync.sync_count, 3);
    }

    #[tokio::test]
    async fn test_sync_with_peer_small_drift() {
        let mut sync = ClockSync::new(5000); // 5 second tolerance

        // Peer reports time 2 seconds ahead
        let peer_time = Timestamp::new(Timestamp::now().as_secs() + 2);
        let result = sync.sync_with_peer(peer_time).await;
        assert!(result.is_ok());

        // Offset should be updated: (0 * 7 + 2 * 1000) / 8 = 250
        assert_eq!(sync.offset_ms, 250);
        assert_eq!(sync.sync_count, 1);
        assert!(sync.last_sync_secs > 0);
    }

    #[tokio::test]
    async fn test_sync_with_peer_excessive_drift_returns_error() {
        let mut sync = ClockSync::new(5000); // 5 second tolerance

        // Peer reports time 10 seconds ahead — drift of 10_000ms > max 5_000ms
        let peer_time = Timestamp::new(Timestamp::now().as_secs() + 10);
        let result = sync.sync_with_peer(peer_time).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            NetworkError::ClockDrift { drift_ms, max_ms } => {
                assert!(drift_ms.abs() >= 9000); // ~10_000, allow 1s timing slack
                assert_eq!(max_ms, 5000);
            }
            other => panic!("expected ClockDrift, got {:?}", other),
        }

        // Even on error, offset and sync_count are updated (EMA was applied)
        assert_eq!(sync.sync_count, 1);
    }

    #[tokio::test]
    async fn test_sync_with_peer_ema_convergence() {
        let mut sync = ClockSync::new(60_000); // generous tolerance for this test

        let now_secs = Timestamp::now().as_secs();

        // Repeatedly report peer 3 seconds ahead; EMA should converge toward 3000ms
        for _ in 0..20 {
            let peer_time = Timestamp::new(now_secs + 3);
            sync.sync_with_peer(peer_time).await.unwrap();
        }

        // After many iterations, offset should be close to 3000ms
        // (EMA converges: each step offset = offset*7/8 + 3000/8)
        assert!(
            sync.offset_ms > 2500 && sync.offset_ms < 3500,
            "expected offset near 3000ms, got {}ms",
            sync.offset_ms
        );
    }

    #[test]
    fn test_new_initializes_fields() {
        let sync = ClockSync::new(10_000);
        assert_eq!(sync.max_drift_ms, 10_000);
        assert_eq!(sync.offset_ms, 0);
        assert_eq!(sync.last_sync_secs, 0);
        assert_eq!(sync.sync_count, 0);
    }
}
