//! LMDB implementation of GovernanceStore.

use burst_store::governance::GovernanceStore;
use burst_store::StoreError;
use burst_types::{TxHash, WalletAddress};

pub struct LmdbGovernanceStore;

impl GovernanceStore for LmdbGovernanceStore {
    fn put_proposal(&self, _hash: &TxHash, _data: &[u8]) -> Result<(), StoreError> { todo!() }
    fn get_proposal(&self, _hash: &TxHash) -> Result<Vec<u8>, StoreError> { todo!() }
    fn list_active_proposals(&self) -> Result<Vec<TxHash>, StoreError> { todo!() }
    fn put_vote(&self, _proposal: &TxHash, _voter: &WalletAddress, _data: &[u8]) -> Result<(), StoreError> { todo!() }
    fn get_votes(&self, _proposal: &TxHash) -> Result<Vec<Vec<u8>>, StoreError> { todo!() }
    fn put_delegation(&self, _delegator: &WalletAddress, _data: &[u8]) -> Result<(), StoreError> { todo!() }
    fn get_delegation(&self, _delegator: &WalletAddress) -> Result<Option<Vec<u8>>, StoreError> { todo!() }
    fn put_constitution(&self, _data: &[u8]) -> Result<(), StoreError> { todo!() }
    fn get_constitution(&self) -> Result<Vec<u8>, StoreError> { todo!() }
}
