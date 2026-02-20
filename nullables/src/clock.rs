//! Nullable clock — deterministic time for testing.
//!
//! Thread-safe: uses AtomicU64 so it can be shared across async tasks.

use burst_types::Timestamp;
use std::sync::atomic::{AtomicU64, Ordering};

/// A deterministic clock for testing.
///
/// Time only advances when you tell it to. Safe to share across threads.
pub struct NullClock {
    current: AtomicU64,
}

impl NullClock {
    pub fn new(initial_secs: u64) -> Self {
        Self {
            current: AtomicU64::new(initial_secs),
        }
    }

    /// Get the current time.
    pub fn now(&self) -> Timestamp {
        Timestamp::new(self.current.load(Ordering::SeqCst))
    }

    /// Advance time by a number of seconds.
    pub fn advance(&self, secs: u64) {
        self.current.fetch_add(secs, Ordering::SeqCst);
    }

    /// Set the time to a specific value.
    pub fn set(&self, secs: u64) {
        self.current.store(secs, Ordering::SeqCst);
    }

    /// Get the current time in seconds.
    pub fn current_secs(&self) -> u64 {
        self.current.load(Ordering::SeqCst)
    }
}

// NullClock is Send + Sync thanks to AtomicU64 (no RefCell/Cell).
unsafe impl Send for NullClock {}
unsafe impl Sync for NullClock {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_null_clock_advance() {
        let clock = NullClock::new(1000);
        assert_eq!(clock.now().as_secs(), 1000);
        clock.advance(60);
        assert_eq!(clock.now().as_secs(), 1060);
    }

    #[test]
    fn test_null_clock_set() {
        let clock = NullClock::new(0);
        clock.set(5000);
        assert_eq!(clock.now().as_secs(), 5000);
    }
}
