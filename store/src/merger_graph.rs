//! Merger graph storage trait.

use crate::StoreError;
use burst_types::TxHash;

/// Trait for persisting the merger graph.
pub trait MergerGraphStore {
    /// Store a merge relationship: origin → merge_tx.
    fn put_origin_merge(&self, origin: &TxHash, merge_tx: &TxHash) -> Result<(), StoreError>;

    /// Get all merge transactions that consumed a given origin.
    fn get_merges_for_origin(&self, origin: &TxHash) -> Result<Vec<TxHash>, StoreError>;

    /// Store a downstream merge relationship: parent_merge → child_merge.
    fn put_downstream(&self, parent: &TxHash, child: &TxHash) -> Result<(), StoreError>;

    /// Get downstream merges of a merge.
    fn get_downstream(&self, parent: &TxHash) -> Result<Vec<TxHash>, StoreError>;

    /// Store merge node data (serialized).
    fn put_merge_node(&self, merge_tx: &TxHash, data: &[u8]) -> Result<(), StoreError>;

    /// Retrieve merge node data.
    fn get_merge_node(&self, merge_tx: &TxHash) -> Result<Vec<u8>, StoreError>;
}
