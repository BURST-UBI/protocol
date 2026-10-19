//! Clock synchronization — required for BRN computation.
//!
//! BRN is a function of time, so nodes must agree on the current time.
//! Unauthenticated NTP and peer observations are diagnostic estimates, not a
//! trusted clock-error bound for pre-beacon checkpoint signatures.

use crate::NetworkError;
use burst_types::Timestamp;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const NTP_EPOCH_OFFSET: u64 = 2_208_988_800;
const NTP_SCALE: i128 = 1 << 32;

fn ntp_error(message: impl ToString) -> NetworkError {
    NetworkError::ConnectionFailed(format!("NTP: {}", message.to_string()))
}

fn system_time() -> Result<Duration, NetworkError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(ntp_error)
}

fn ntp_timestamp(time: Duration) -> u64 {
    let seconds = time.as_secs().wrapping_add(NTP_EPOCH_OFFSET) as u32;
    let fraction = ((u64::from(time.subsec_nanos()) << 32) / 1_000_000_000) as u32;
    (u64::from(seconds) << 32) | u64::from(fraction)
}

fn ntp_difference(a: u64, b: u64) -> i128 {
    // Signed modular subtraction also handles the NTP seconds rollover in 2036.
    i128::from(a.wrapping_sub(b) as i64)
}

fn response_offset(response: &[u8], sent: u64, received: u64) -> Result<i64, NetworkError> {
    if response.len() != 48 {
        return Err(ntp_error("unexpected response length"));
    }
    let version = (response[0] >> 3) & 7;
    if response[0] >> 6 == 3
        || !matches!(version, 3 | 4)
        || response[0] & 7 != 4
        || !(1..=15).contains(&response[1])
    {
        return Err(ntp_error("unsynchronized server or invalid header"));
    }
    let stamp = |start: usize| u64::from_be_bytes(response[start..start + 8].try_into().unwrap());
    if sent == 0 || stamp(24) != sent || stamp(32) == 0 || stamp(40) == 0 {
        return Err(ntp_error("unmatched or missing timestamps"));
    }
    let elapsed = ntp_difference(received, sent);
    let processing = ntp_difference(stamp(40), stamp(32));
    if !(0..=5 * NTP_SCALE).contains(&elapsed)
        || processing < 0
        || processing > elapsed + NTP_SCALE / 1000
    {
        return Err(ntp_error("inconsistent response timing"));
    }
    let offset = (ntp_difference(stamp(32), sent) + ntp_difference(stamp(40), received)) * 1000
        / (2 * NTP_SCALE);
    i64::try_from(offset).map_err(ntp_error)
}

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
    /// Queries pool.ntp.org with one bounded SNTP exchange. A failed or rejected
    /// observation leaves the previous estimate and its freshness unchanged.
    pub async fn sync_ntp(&mut self) -> Result<(), NetworkError> {
        let offset = Self::query_ntp_offset("pool.ntp.org:123").await?;
        self.accept_offset(offset, Timestamp::now(), self.sync_count == 0)?;
        Ok(())
    }

    async fn query_ntp_offset(server: &str) -> Result<i64, NetworkError> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let address = tokio::net::lookup_host(server)
                .await
                .map_err(ntp_error)?
                .next()
                .ok_or_else(|| ntp_error("server has no addresses"))?;
            let bind = if address.is_ipv4() {
                "0.0.0.0:0"
            } else {
                "[::]:0"
            };
            let socket = tokio::net::UdpSocket::bind(bind).await.map_err(ntp_error)?;
            // A connected UDP socket filters replies from other endpoints.
            socket.connect(address).await.map_err(ntp_error)?;
            let mut request = [0u8; 48];
            request[0] = 0x23;
            let before = system_time()?;
            let elapsed = Instant::now();
            let sent = ntp_timestamp(before);
            request[40..48].copy_from_slice(&sent.to_be_bytes());
            socket.send(&request).await.map_err(ntp_error)?;
            // The extra byte detects oversized packets instead of accepting a
            // truncated header as a complete response.
            let mut response = [0u8; 49];
            let count = socket.recv(&mut response).await.map_err(ntp_error)?;
            let after = system_time()?;
            let wall_elapsed = after
                .checked_sub(before)
                .ok_or_else(|| ntp_error("local clock moved backwards"))?;
            if wall_elapsed.abs_diff(elapsed.elapsed()) > Duration::from_millis(250) {
                return Err(ntp_error("local clock changed during exchange"));
            }
            response_offset(&response[..count], sent, ntp_timestamp(after))
        })
        .await
        .map_err(|_| ntp_error("timeout"))?
    }

    fn accept_offset(
        &mut self,
        offset: i64,
        now: Timestamp,
        first: bool,
    ) -> Result<(), NetworkError> {
        if self.max_drift_ms < 0 || i128::from(offset).abs() > i128::from(self.max_drift_ms) {
            return Err(NetworkError::ClockDrift {
                drift_ms: offset,
                max_ms: self.max_drift_ms,
            });
        }
        self.offset_ms = if first {
            offset
        } else {
            ((i128::from(self.offset_ms) * 7 + i128::from(offset)) / 8) as i64
        };
        self.last_sync_secs = now.as_secs();
        self.sync_count = self.sync_count.saturating_add(1);
        Ok(())
    }

    /// Sync with a peer by comparing timestamps.
    ///
    /// Computes the drift between local time and the peer's reported time,
    /// then updates the offset using an exponential moving average (EMA)
    /// with a weight of 7/8 on the old value, 1/8 on the new observation.
    /// This smooths out individual peer discrepancies while still tracking
    /// the reported offset over time. Peers cannot establish trusted UTC.
    ///
    /// Returns an error if the peer's drift exceeds `max_drift_ms`.
    pub async fn sync_with_peer(&mut self, peer_timestamp: Timestamp) -> Result<(), NetworkError> {
        let local_now = Timestamp::now();
        let drift = (i128::from(peer_timestamp.as_secs()) - i128::from(local_now.as_secs())) * 1000;
        let offset = i64::try_from(drift).map_err(|_| ntp_error("peer timestamp out of range"))?;
        self.accept_offset(offset, local_now, false)
    }

    /// Get the current adjusted timestamp.
    pub fn now(&self) -> Timestamp {
        let raw = Timestamp::now();
        Timestamp::new(raw.as_secs().saturating_add_signed(self.offset_ms / 1000))
    }

    /// Validate that a timestamp is within acceptable drift.
    pub fn validate_timestamp(&self, ts: Timestamp) -> bool {
        let now = self.now();
        let diff_secs = now.as_secs().abs_diff(ts.as_secs());
        self.max_drift_ms >= 0 && u128::from(diff_secs) * 1000 <= self.max_drift_ms as u128
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

    fn response(sent: u64, receive: u64, transmit: u64) -> [u8; 48] {
        let mut packet = [0; 48];
        packet[0] = 0x24;
        packet[1] = 2;
        packet[24..32].copy_from_slice(&sent.to_be_bytes());
        packet[32..40].copy_from_slice(&receive.to_be_bytes());
        packet[40..48].copy_from_slice(&transmit.to_be_bytes());
        packet
    }

    #[tokio::test]
    async fn ntp_exchange_uses_matching_endpoint_and_full_timestamps() {
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = server.local_addr().unwrap().to_string();
        let responder = tokio::spawn(async move {
            let mut request = [0; 48];
            let (length, sender) = server.recv_from(&mut request).await.unwrap();
            assert_eq!(length, 48);
            assert_eq!(request[0], 0x23);
            let sent = u64::from_be_bytes(request[40..48].try_into().unwrap());
            assert_ne!(sent, 0);
            let receive = ntp_timestamp(system_time().unwrap() + Duration::from_millis(200));
            let transmit = ntp_timestamp(system_time().unwrap() + Duration::from_millis(200));
            server
                .send_to(&response(sent, receive, transmit), sender)
                .await
                .unwrap();
        });
        let offset = ClockSync::query_ntp_offset(&address).await.unwrap();
        responder.await.unwrap();
        assert!((150..=250).contains(&offset), "offset: {offset}");
    }

    #[test]
    fn ntp_offset_preserves_fractions_and_era_rollover() {
        for seconds in [1_791_680_000, u64::from(u32::MAX) - NTP_EPOCH_OFFSET] {
            let start = Duration::from_secs(seconds) + Duration::from_millis(900);
            let sent = ntp_timestamp(start);
            let receive = ntp_timestamp(start + Duration::from_millis(300));
            let transmit = ntp_timestamp(start + Duration::from_millis(350));
            let received = ntp_timestamp(start + Duration::from_millis(250));
            let offset =
                response_offset(&response(sent, receive, transmit), sent, received).unwrap();
            assert!((199..=200).contains(&offset), "offset: {offset}");
        }
    }

    #[test]
    fn malformed_and_unmatched_ntp_packets_are_rejected() {
        let sent = ntp_timestamp(Duration::from_secs(1_791_680_000));
        let receive = sent + (NTP_SCALE / 10) as u64;
        let transmit = receive + (NTP_SCALE / 100) as u64;
        let received = sent + (NTP_SCALE / 5) as u64;
        let valid = response(sent, receive, transmit);
        assert!(response_offset(&valid, sent, received).is_ok());
        for (index, value) in [(0, 0xe4), (0, 0x23), (0, 0x14), (1, 0), (1, 16), (24, 0)] {
            let mut invalid = valid;
            invalid[index] = value;
            assert!(
                response_offset(&invalid, sent, received).is_err(),
                "byte {index}: {value}"
            );
        }
        for (receive, transmit) in [
            (0, transmit),
            (receive, 0),
            (transmit, receive),
            (receive, received + NTP_SCALE as u64),
        ] {
            assert!(response_offset(&response(sent, receive, transmit), sent, received).is_err());
        }
        assert!(response_offset(&valid[..47], sent, received).is_err());
        assert!(response_offset(&[0; 49], sent, received).is_err());
        assert!(response_offset(&valid, sent, sent - 1).is_err());
        assert!(response_offset(&valid, sent, sent + 6 * NTP_SCALE as u64).is_err());
    }

    #[test]
    fn only_accepted_observations_update_offset_and_freshness() {
        let mut sync = ClockSync::new(5000);
        sync.accept_offset(200, Timestamp::new(1), true).unwrap();
        assert_eq!(sync.offset_ms, 200);
        sync.accept_offset(280, Timestamp::new(2), false).unwrap();
        assert_eq!(sync.offset_ms, 210);
        for rejected in [5001, -5001, i64::MAX, i64::MIN] {
            assert!(sync
                .accept_offset(rejected, Timestamp::new(3), false)
                .is_err());
            assert_eq!(
                (sync.offset_ms, sync.last_sync_secs, sync.sync_count),
                (210, 2, 2)
            );
        }
        sync.sync_count = u32::MAX;
        sync.accept_offset(210, Timestamp::new(4), false).unwrap();
        assert_eq!(sync.sync_count, u32::MAX);
        assert!(!sync.validate_timestamp(Timestamp::new(u64::MAX)));
        sync.offset_ms = i64::MIN;
        assert_eq!(sync.now(), Timestamp::new(0));
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

        assert_eq!(
            (sync.offset_ms, sync.sync_count, sync.last_sync_secs),
            (0, 0, 0)
        );
        assert!(sync.sync_with_peer(Timestamp::new(u64::MAX)).await.is_err());
        assert_eq!(
            (sync.offset_ms, sync.sync_count, sync.last_sync_secs),
            (0, 0, 0)
        );
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
