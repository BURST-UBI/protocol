//! LMDB implementation of MergerGraphStore.

use burst_store::merger_graph::MergerGraphStore;
use burst_store::StoreError;
use burst_types::TxHash;

pub struct LmdbMergerGraphStore;

impl MergerGraphStore for LmdbMergerGraphStore {
    fn put_origin_merge(&self, _origin: &TxHash, _merge_tx: &TxHash) -> Result<(), StoreError> { todo!() }
    fn get_merges_for_origin(&self, _origin: &TxHash) -> Result<Vec<TxHash>, StoreError> { todo!() }
    fn put_downstream(&self, _parent: &TxHash, _child: &TxHash) -> Result<(), StoreError> { todo!() }
    fn get_downstream(&self, _parent: &TxHash) -> Result<Vec<TxHash>, StoreError> { todo!() }
    fn put_merge_node(&self, _merge_tx: &TxHash, _data: &[u8]) -> Result<(), StoreError> { todo!() }
    fn get_merge_node(&self, _merge_tx: &TxHash) -> Result<Vec<u8>, StoreError> { todo!() }
}
