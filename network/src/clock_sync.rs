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
    /// Current estimated offset from true UTC.
    pub offset_ms: i64,
}

impl ClockSync {
    pub fn new(max_drift_ms: i64) -> Self {
        Self {
            max_drift_ms,
            offset_ms: 0,
        }
    }

    /// Sync with NTP servers to determine clock offset.
    pub async fn sync_ntp(&mut self) -> Result<(), NetworkError> {
        todo!("query NTP servers, compute offset")
    }

    /// Sync with a peer by comparing timestamps.
    pub async fn sync_with_peer(&mut self, _peer_timestamp: Timestamp) -> Result<(), NetworkError> {
        todo!("compare peer timestamp with local, update offset")
    }

    /// Get the current adjusted timestamp.
    pub fn now(&self) -> Timestamp {
        let raw = Timestamp::now();
        Timestamp::new((raw.as_secs() as i64 + self.offset_ms / 1000) as u64)
    }

    /// Validate that a timestamp is within acceptable drift.
    pub fn validate_timestamp(&self, ts: Timestamp) -> bool {
        let now = self.now();
        let diff = (now.as_secs() as i64 - ts.as_secs() as i64).abs();
        diff * 1000 <= self.max_drift_ms
    }
}
