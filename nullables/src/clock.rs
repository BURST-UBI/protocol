//! Manually controlled atomic timestamp for tests.

use burst_types::Timestamp;
use std::sync::atomic::{AtomicU64, Ordering};

/// A deterministic clock for testing.
///
/// Time changes only through set or advance. It does not drive Tokio timers
/// or replace the process wall clock.
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

    /// Add seconds, wrapping on u64 overflow.
    pub fn advance(&self, secs: u64) {
        self.current.fetch_add(secs, Ordering::SeqCst);
    }

    /// Set an arbitrary timestamp, including an earlier value.
    pub fn set(&self, secs: u64) {
        self.current.store(secs, Ordering::SeqCst);
    }

    /// Get the current time in seconds.
    pub fn current_secs(&self) -> u64 {
        self.current.load(Ordering::SeqCst)
    }
}

// Keep the clock shareable across test workers.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<NullClock>();
};

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
