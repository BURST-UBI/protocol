//! Persistent peer cache storage trait.
//!
//! Stores recently seen peers so the node can reconnect on restart without
//! relying solely on bootstrap peers. Modeled after rsnano's peer cache.

use crate::StoreError;

/// Trait for persisting peer addresses across restarts.
///
/// Keys are peer address strings (`"ip:port"`), values are the last-seen
/// Unix timestamp (seconds). Implementations must be safe for concurrent use
/// from a single writer (the peer cache task).
pub trait PeerStore {
    /// Insert or update a peer's last-seen timestamp.
    fn put_peer(&self, addr: &str, timestamp: u64) -> Result<(), StoreError>;

    /// Get a peer's last-seen timestamp.
    fn get_peer(&self, addr: &str) -> Result<Option<u64>, StoreError>;

    /// Remove a peer from the cache.
    fn delete_peer(&self, addr: &str) -> Result<(), StoreError>;

    /// Iterate over all cached peers, returning `(address, timestamp)` pairs.
    fn iter_peers(&self) -> Result<Vec<(String, u64)>, StoreError>;

    /// Remove all peers whose timestamp is older than `cutoff_secs`.
    fn purge_older_than(&self, cutoff_secs: u64) -> Result<usize, StoreError>;
}
