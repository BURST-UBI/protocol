//! TRST index storage traits for origin and expiry lookups.

use crate::StoreError;
use burst_types::{Timestamp, TxHash};

/// Trait for TRST secondary indexes.
///
/// Two indexes are maintained:
/// - **Origin index**: maps an origin burn hash to all TRST transaction hashes
///   descended from it. Enables O(1) revocation of an entire sybil lineage.
/// - **Expiry index**: maps `(expiry_timestamp, tx_hash)` pairs. Enables efficient
///   range scans to find all TRST that have expired before a given cutoff time.
pub trait TrstIndexStore {
    /// Record that `tx_hash` is derived from the given origin burn.
    fn put_origin_index(&self, origin_hash: &TxHash, tx_hash: &TxHash) -> Result<(), StoreError>;

    /// Get all TRST transaction hashes derived from the given origin.
    fn get_by_origin(&self, origin_hash: &TxHash) -> Result<Vec<TxHash>, StoreError>;

    /// Delete the entire origin index entry (e.g. after full revocation).
    fn delete_origin_index(&self, origin_hash: &TxHash) -> Result<(), StoreError>;

    /// Record that `tx_hash` expires at the given timestamp.
    fn put_expiry_index(&self, expiry: Timestamp, tx_hash: &TxHash) -> Result<(), StoreError>;

    /// Get all TRST transaction hashes that expire before the given cutoff.
    fn get_expired_before(&self, cutoff: Timestamp) -> Result<Vec<TxHash>, StoreError>;

    /// Delete a specific expiry index entry.
    fn delete_expiry_index(&self, expiry: Timestamp, tx_hash: &TxHash) -> Result<(), StoreError>;

    /// Delete a token from all secondary indexes (origin + expiry).
    ///
    /// Used by the pruning engine to remove a fully expired or revoked
    /// token from the TRST index. Implementations scan the expiry index
    /// for matching entries since the expiry timestamp may not be known
    /// to the caller.
    fn delete_token(&self, tx_hash: &TxHash) -> Result<(), StoreError>;
}
