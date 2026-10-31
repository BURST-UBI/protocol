//! TRST index storage traits for origin and expiry lookups.

use crate::StoreError;
use burst_types::{Timestamp, TxHash};

/// Trait for TRST secondary indexes.
///
/// The origin index groups transaction hashes by origin. The expiry index
/// groups them by expiry time. Both query methods collect all matching hashes;
/// their cost grows with the result size. These indexes do not apply revocations.
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
    /// The caller need not supply the origin or expiry. LMDB uses a reverse
    /// mapping to locate the entries; no expiry-table scan is required.
    fn delete_token(&self, tx_hash: &TxHash) -> Result<(), StoreError>;
}
