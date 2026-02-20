//! Rolling hash set for network-layer message deduplication.
//!
//! Prevents the same message from being processed or relayed twice by
//! maintaining a bounded set of recently-seen Blake2b-256 message hashes.

use std::collections::HashSet;
use std::collections::VecDeque;

/// Default dedup capacity: track the last 65 536 message hashes.
pub const DEFAULT_DEDUP_CAPACITY: usize = 65_536;

/// Rolling hash set for message deduplication.
///
/// Keeps track of the last `capacity` message hashes in insertion order.
/// When a new message arrives, check if its hash is already in the set.
/// Oldest entries are evicted when capacity is reached.
pub struct MessageDedup {
    capacity: usize,
    hashes: HashSet<[u8; 32]>,
    order: VecDeque<[u8; 32]>,
}

impl MessageDedup {
    /// Create a new dedup tracker with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            hashes: HashSet::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
        }
    }

    /// Check if a message hash has been seen before.
    ///
    /// Returns `true` if the hash is a duplicate (already seen).
    /// Returns `false` if the hash is new — it is recorded for future checks.
    pub fn is_duplicate(&mut self, hash: &[u8; 32]) -> bool {
        if self.hashes.contains(hash) {
            return true;
        }
        // Evict oldest if at capacity
        if self.hashes.len() >= self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.hashes.remove(&old);
            }
        }
        self.hashes.insert(*hash);
        self.order.push_back(*hash);
        false
    }

    /// Compute the Blake2b-256 hash of raw message bytes.
    ///
    /// Delegates to `burst_crypto::blake2b_256` for consistency with the
    /// rest of the protocol.
    pub fn hash_message(data: &[u8]) -> [u8; 32] {
        burst_crypto::blake2b_256(data)
    }

    /// Number of tracked hashes.
    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    /// Whether the tracker is empty.
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
    fn new_dedup_is_empty() {
        let dedup = MessageDedup::new(100);
        assert!(dedup.is_empty());
        assert_eq!(dedup.len(), 0);
    }

    #[test]
    fn first_message_is_not_duplicate() {
        let mut dedup = MessageDedup::new(100);
        let hash = MessageDedup::hash_message(b"hello");
        assert!(!dedup.is_duplicate(&hash));
    }

    #[test]
    fn same_message_is_duplicate() {
        let mut dedup = MessageDedup::new(100);
        let hash = MessageDedup::hash_message(b"hello");
        assert!(!dedup.is_duplicate(&hash));
        assert!(dedup.is_duplicate(&hash));
    }

    #[test]
    fn different_messages_are_not_duplicates() {
        let mut dedup = MessageDedup::new(100);
        let h1 = MessageDedup::hash_message(b"hello");
        let h2 = MessageDedup::hash_message(b"world");
        assert!(!dedup.is_duplicate(&h1));
        assert!(!dedup.is_duplicate(&h2));
        assert_eq!(dedup.len(), 2);
    }

    #[test]
    fn evicts_oldest_when_at_capacity() {
        let mut dedup = MessageDedup::new(3);
        let h1 = MessageDedup::hash_message(b"msg1");
        let h2 = MessageDedup::hash_message(b"msg2");
        let h3 = MessageDedup::hash_message(b"msg3");
        let h4 = MessageDedup::hash_message(b"msg4");

        assert!(!dedup.is_duplicate(&h1));
        assert!(!dedup.is_duplicate(&h2));
        assert!(!dedup.is_duplicate(&h3));
        assert_eq!(dedup.len(), 3);

        // Inserting h4 should evict h1
        assert!(!dedup.is_duplicate(&h4));
        assert_eq!(dedup.len(), 3);

        // h1 should no longer be recognized as a duplicate
        assert!(!dedup.is_duplicate(&h1));
        // h2 should have been evicted to make room for h1
        assert!(!dedup.is_duplicate(&h2));
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
        dedup.is_duplicate(&hash);
        dedup.is_duplicate(&hash);
        dedup.is_duplicate(&hash);
        assert_eq!(dedup.len(), 1);
    }
}
