//! LMDB implementation of VerificationStore.

use burst_store::verification::VerificationStore;
use burst_store::StoreError;
use burst_types::WalletAddress;

pub struct LmdbVerificationStore;

impl VerificationStore for LmdbVerificationStore {
    fn put_endorsement(&self, _target: &WalletAddress, _data: &[u8]) -> Result<(), StoreError> { todo!() }
    fn get_endorsements(&self, _target: &WalletAddress) -> Result<Vec<Vec<u8>>, StoreError> { todo!() }
    fn put_verification_vote(&self, _target: &WalletAddress, _data: &[u8]) -> Result<(), StoreError> { todo!() }
    fn get_verification_votes(&self, _target: &WalletAddress) -> Result<Vec<Vec<u8>>, StoreError> { todo!() }
    fn put_challenge(&self, _target: &WalletAddress, _data: &[u8]) -> Result<(), StoreError> { todo!() }
    fn get_challenge(&self, _target: &WalletAddress) -> Result<Option<Vec<u8>>, StoreError> { todo!() }
}
