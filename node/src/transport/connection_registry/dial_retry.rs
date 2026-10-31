//! Per-address retry delays: 2, 4, 8, 16, 32, then 60 seconds.
//! Failure records expire after five minutes; a full table blocks new addresses
//! until expiry or a successful attempt frees an entry.
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

const MAX_ENTRIES: usize = 4096;
const RETENTION: Duration = Duration::from_secs(300);

#[derive(Default)]
pub(super) struct DialRetry {
    failed: HashMap<String, Failure>,
}

struct Failure {
    count: u32,
    retry_at: Instant,
    expires: Instant,
}

impl DialRetry {
    pub(super) fn ready(&mut self, address: &str, now: Instant) -> bool {
        self.failed.retain(|_, failure| failure.expires > now);
        match self.failed.get(address) {
            Some(failure) => now >= failure.retry_at,
            None => self.failed.len() < MAX_ENTRIES,
        }
    }

    pub(super) fn finish(&mut self, address: &str, success: bool, now: Instant) {
        if success {
            self.failed.remove(address);
            return;
        }
        if !self.failed.contains_key(address) && self.failed.len() >= MAX_ENTRIES {
            return;
        }
        let failure = self.failed.entry(address.to_owned()).or_insert(Failure {
            count: 0,
            retry_at: now,
            expires: now,
        });
        failure.count = (failure.count + 1).min(6);
        failure.retry_at = now + Duration::from_secs((1u64 << failure.count).min(60));
        failure.expires = now + RETENTION;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dial_retry_backs_off_resumes_and_resets_on_success() {
        let mut retry = DialRetry::default();
        let mut now = Instant::now();
        for seconds in [2, 4, 8, 16, 32, 60, 60] {
            assert!(retry.ready("peer", now));
            retry.finish("peer", false, now);
            assert!(!retry.ready("peer", now + Duration::from_secs(seconds - 1)));
            assert!(retry.ready("other", now));
            now += Duration::from_secs(seconds);
        }
        assert!(retry.ready("peer", now));
        retry.finish("peer", true, now);
        assert!(retry.ready("peer", now));
        retry.finish("peer", false, now);
        assert!(retry.ready("peer", now + Duration::from_secs(2)));
    }

    #[test]
    fn dial_retry_bounds_failed_inventory_and_expires_it() {
        let mut retry = DialRetry::default();
        let now = Instant::now();
        for index in 0..MAX_ENTRIES {
            let peer = index.to_string();
            assert!(retry.ready(&peer, now));
            retry.finish(&peer, false, now);
        }
        assert!(!retry.ready("new", now));
        retry.finish("overflow", false, now);
        assert_eq!(retry.failed.len(), MAX_ENTRIES);
        assert!(retry.ready("0", now + Duration::from_secs(2)));
        assert!(retry.ready("new", now + RETENTION));
        assert!(retry.failed.is_empty());
    }
}
