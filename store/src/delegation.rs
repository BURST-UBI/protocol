use burst_types::{Timestamp, WalletAddress};
use crate::StoreError;

/// Record of an active delegation relationship.
#[derive(Clone, Debug)]
pub struct DelegationRecord {
    pub delegator: WalletAddress,
    pub delegate: WalletAddress,
    pub delegation_public_key: [u8; 32],
    pub created_at: Timestamp,
    pub revoked: bool,
}

pub trait DelegationStore {
    fn put_delegation(&self, record: &DelegationRecord) -> Result<(), StoreError>;
    fn get_delegation_by_delegator(&self, delegator: &WalletAddress) -> Result<Option<DelegationRecord>, StoreError>;
    fn get_delegation_by_pubkey(&self, pubkey: &[u8; 32]) -> Result<Option<DelegationRecord>, StoreError>;
    fn revoke_delegation(&self, delegator: &WalletAddress) -> Result<(), StoreError>;
}
