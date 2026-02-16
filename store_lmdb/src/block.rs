//! LMDB implementation of BlockStore.

use burst_store::block::BlockStore;
use burst_store::StoreError;
use burst_types::{BlockHash, WalletAddress};

pub struct LmdbBlockStore;

impl BlockStore for LmdbBlockStore {
    fn put_block(&self, _hash: &BlockHash, _block_bytes: &[u8]) -> Result<(), StoreError> { todo!() }
    fn get_block(&self, _hash: &BlockHash) -> Result<Vec<u8>, StoreError> { todo!() }
    fn exists(&self, _hash: &BlockHash) -> Result<bool, StoreError> { todo!() }
    fn delete_block(&self, _hash: &BlockHash) -> Result<(), StoreError> { todo!() }
    fn get_account_blocks(&self, _address: &WalletAddress) -> Result<Vec<BlockHash>, StoreError> { todo!() }
    fn block_count(&self) -> Result<u64, StoreError> { todo!() }
}
