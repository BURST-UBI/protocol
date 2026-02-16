//! Nullable clock — deterministic time for testing.

use burst_types::Timestamp;
use std::cell::Cell;

/// A deterministic clock for testing.
///
/// Time only advances when you tell it to.
pub struct NullClock {
    current: Cell<u64>,
}

impl NullClock {
    pub fn new(initial_secs: u64) -> Self {
        Self {
            current: Cell::new(initial_secs),
        }
    }

    /// Get the current time.
    pub fn now(&self) -> Timestamp {
        Timestamp::new(self.current.get())
    }

    /// Advance time by a number of seconds.
    pub fn advance(&self, secs: u64) {
        self.current.set(self.current.get() + secs);
    }

    /// Set the time to a specific value.
    pub fn set(&self, secs: u64) {
        self.current.set(secs);
    }
}
