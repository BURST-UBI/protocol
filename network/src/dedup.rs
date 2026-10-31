//! Rolling hash set for network-layer message deduplication.
//!
//! Tracks BLAKE2b-256 digests up to a fixed capacity. Callers choose whether
//! duplicates remain suppressed until eviction or may retry after a time window.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Default dedup capacity: track the last 65 536 message hashes.
pub const DEFAULT_DEDUP_CAPACITY: usize = 65_536;

/// Rolling hash set for message deduplication.
///
/// Entries retain their original insertion order, including when a timed entry
/// expires and is admitted again. New hashes evict the oldest entry at capacity.
pub struct MessageDedup {
    capacity: usize,
    hashes: HashMap<[u8; 32], Option<Instant>>,
    order: VecDeque<[u8; 32]>,
}

impl MessageDedup {
    /// Create a new dedup tracker with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            hashes: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
        }
    }

    /// Return true while an existing entry suppresses this hash. Otherwise,
    /// record it without a deadline and return false. Capacity zero records
    /// nothing. An existing unexpired timed entry keeps its original deadline.
    pub fn apply_digest(&mut self, hash: &[u8; 32]) -> bool {
        self.apply_digest_inner(hash, None, Instant::now())
    }

    /// Suppress a hash until its deadline, then allow a retry and renew the
    /// window. Duplicate arrivals do not extend it. Existing entries without
    /// deadlines remain suppressed until eviction; dependencies are not checked.
    pub fn apply_digest_with_window(
        &mut self,
        hash: &[u8; 32],
        window: Duration,
        now: Instant,
    ) -> bool {
        self.apply_digest_inner(hash, Some(now + window), now)
    }

    fn apply_digest_inner(
        &mut self,
        hash: &[u8; 32],
        expires: Option<Instant>,
        now: Instant,
    ) -> bool {
        if let Some(previous) = self.hashes.get_mut(hash) {
            if previous.is_none_or(|deadline| now < deadline) {
                return true;
            }
            *previous = expires;
            return false;
        }
        if self.capacity == 0 {
            return false;
        }
        if self.hashes.len() >= self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.hashes.remove(&old);
            }
        }
        self.hashes.insert(*hash, expires);
        self.order.push_back(*hash);
        false
    }

    /// Check suppression at the current time without inserting or refreshing.
    pub fn contains(&self, hash: &[u8; 32]) -> bool {
        self.hashes
            .get(hash)
            .is_some_and(|deadline| deadline.is_none_or(|expires| Instant::now() < expires))
    }

    /// Compute the Blake2b-256 digest of the supplied bytes.
    pub fn hash_message(data: &[u8]) -> [u8; 32] {
        burst_crypto::blake2b_256(data)
    }

    /// Stored entries, including expired entries not yet evicted.
    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    /// Whether there are no stored entries, including expired ones.
    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }
}

impl Default for MessageDedup {
    fn default() -> Self {
        Self::new(DEFAULT_DEDUP_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_does_not_insert_refresh_or_prevent_eviction() {
        let mut cache = MessageDedup::new(1);
        let first = [1; 32];
        assert!(!cache.contains(&first));
        assert!(cache.is_empty());
        cache.apply_digest(&first);
        assert!(cache.contains(&first));
        cache.apply_digest(&[2; 32]);
        assert!(!cache.contains(&first));
        assert_eq!(cache.len(), 1);
        let expired = [3; 32];
        cache.apply_digest_with_window(&expired, Duration::ZERO, Instant::now());
        assert!(!cache.contains(&expired));
    }

    #[test]
    fn retry_window_does_not_slide_or_grow_the_cache() {
        let mut dedup = MessageDedup::new(2);
        let now = Instant::now();
        let window = Duration::from_secs(1);
        let block = [1; 32];
        let vote = [2; 32];
        assert!(!dedup.apply_digest_with_window(&block, window, now));
        assert!(!dedup.apply_digest(&vote));
        assert!(dedup.apply_digest_with_window(&block, window, now + window / 2));
        for step in 1..100 {
            assert!(!dedup.apply_digest_with_window(&block, window, now + window * step));
            assert_eq!(dedup.len(), 2);
            assert_eq!(dedup.order.len(), 2);
            assert!(dedup.apply_digest(&vote));
        }
        assert!(!dedup.apply_digest(&[3; 32]));
        assert_eq!(dedup.len(), 2);
        assert!(!dedup.hashes.contains_key(&block));
    }

    #[test]
    fn zero_capacity_retains_no_messages() {
        let mut dedup = MessageDedup::new(0);
        assert!(!dedup.apply_digest(&[1; 32]));
        assert!(!dedup.apply_digest_with_window(&[2; 32], Duration::from_secs(1), Instant::now()));
        assert!(dedup.is_empty());
        assert!(dedup.order.is_empty());
    }

    #[test]
    fn new_dedup_is_empty() {
        let dedup = MessageDedup::new(100);
        assert!(dedup.is_empty());
        assert_eq!(dedup.len(), 0);
    }

    #[test]
    fn first_message_is_not_duplicate() {
        let mut dedup = MessageDedup::new(100);
        let hash = MessageDedup::hash_message(b"hello");
        assert!(!dedup.apply_digest(&hash));
    }

    #[test]
    fn same_message_is_duplicate() {
        let mut dedup = MessageDedup::new(100);
        let hash = MessageDedup::hash_message(b"hello");
        assert!(!dedup.apply_digest(&hash));
        assert!(dedup.apply_digest(&hash));
    }

    #[test]
    fn different_messages_are_not_duplicates() {
        let mut dedup = MessageDedup::new(100);
        let h1 = MessageDedup::hash_message(b"hello");
        let h2 = MessageDedup::hash_message(b"world");
        assert!(!dedup.apply_digest(&h1));
        assert!(!dedup.apply_digest(&h2));
        assert_eq!(dedup.len(), 2);
    }

    #[test]
    fn evicts_oldest_when_at_capacity() {
        let mut dedup = MessageDedup::new(3);
        let h1 = MessageDedup::hash_message(b"msg1");
        let h2 = MessageDedup::hash_message(b"msg2");
        let h3 = MessageDedup::hash_message(b"msg3");
        let h4 = MessageDedup::hash_message(b"msg4");

        assert!(!dedup.apply_digest(&h1));
        assert!(!dedup.apply_digest(&h2));
        assert!(!dedup.apply_digest(&h3));
        assert_eq!(dedup.len(), 3);

        // Inserting h4 should evict h1
        assert!(!dedup.apply_digest(&h4));
        assert_eq!(dedup.len(), 3);

        // h1 should no longer be recognized as a duplicate
        assert!(!dedup.apply_digest(&h1));
        // h2 should have been evicted to make room for h1
        assert!(!dedup.apply_digest(&h2));
    }

    #[test]
    fn hash_message_is_deterministic() {
        let h1 = MessageDedup::hash_message(b"test data");
        let h2 = MessageDedup::hash_message(b"test data");
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_message_differs_for_different_input() {
        let h1 = MessageDedup::hash_message(b"alpha");
        let h2 = MessageDedup::hash_message(b"beta");
        assert_ne!(h1, h2);
    }

    #[test]
    fn default_uses_standard_capacity() {
        let dedup = MessageDedup::default();
        assert_eq!(dedup.capacity, DEFAULT_DEDUP_CAPACITY);
    }

    #[test]
    fn duplicate_does_not_increase_len() {
        let mut dedup = MessageDedup::new(100);
        let hash = MessageDedup::hash_message(b"same");
        dedup.apply_digest(&hash);
        dedup.apply_digest(&hash);
        dedup.apply_digest(&hash);
        assert_eq!(dedup.len(), 1);
    }
}
