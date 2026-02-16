//! Timestamp type used throughout the protocol.
//!
//! Timestamps are Unix epoch seconds (UTC). BRN computation requires
//! clock synchronization between nodes (NTP or equivalent).

use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

/// A Unix timestamp in seconds since epoch (UTC).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Timestamp(u64);

impl Timestamp {
    /// The epoch (time zero).
    pub const EPOCH: Self = Self(0);

    pub fn new(secs: u64) -> Self {
        Self(secs)
    }

    /// Get the current system time as a `Timestamp`.
    pub fn now() -> Self {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_secs();
        Self(secs)
    }

    pub fn as_secs(&self) -> u64 {
        self.0
    }

    /// Seconds elapsed since this timestamp (relative to `now`).
    pub fn elapsed_since(&self, now: Timestamp) -> u64 {
        now.0.saturating_sub(self.0)
    }

    /// Whether this timestamp + duration has passed relative to `now`.
    pub fn has_expired(&self, duration_secs: u64, now: Timestamp) -> bool {
        now.0 >= self.0.saturating_add(duration_secs)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}s", self.0)
    }
}
