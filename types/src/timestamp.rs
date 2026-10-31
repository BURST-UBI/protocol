//! Timestamp type used throughout the protocol.
//!
//! Values are Unix epoch seconds. This type neither synchronizes clocks nor
//! establishes an agreed protocol time.

use serde::{Deserialize, Serialize};
use std::fmt;
#[cfg(not(target_arch = "wasm32"))]
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

    /// Read the local wall clock, rounded down to seconds.
    /// Native builds panic if the system clock precedes the Unix epoch.
    pub fn now() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_secs();
        #[cfg(target_arch = "wasm32")]
        let secs = (js_sys::Date::now() / 1000.0).floor() as u64;
        Self(secs)
    }

    pub fn as_secs(&self) -> u64 {
        self.0
    }

    /// Seconds until `now`, or zero when `now` precedes this timestamp.
    pub fn elapsed_secs_until(&self, now: Timestamp) -> u64 {
        now.0.saturating_sub(self.0)
    }

    /// Whether `now` is at or after the deadline. Deadline addition saturates
    /// at u64::MAX.
    pub fn has_expired(&self, duration_secs: u64, now: Timestamp) -> bool {
        now.0 >= self.0.saturating_add(duration_secs)
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}s", self.0)
    }
}
