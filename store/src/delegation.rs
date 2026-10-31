use crate::StoreError;
use burst_types::{Timestamp, WalletAddress};

/// Delegation record, including its revocation flag.
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
    fn get_delegation_by_delegator(
        &self,
        delegator: &WalletAddress,
    ) -> Result<Option<DelegationRecord>, StoreError>;
    fn get_delegation_by_pubkey(
        &self,
        pubkey: &[u8; 32],
    ) -> Result<Option<DelegationRecord>, StoreError>;
    fn revoke_delegation(&self, delegator: &WalletAddress) -> Result<(), StoreError>;
}

use std::{collections::HashMap, sync::Mutex};

/// In-memory delegation records with separately locked record and key maps.
/// Replacing a record leaves its old public-key mapping in place. Lookups return
/// revoked records too; callers must check the key and revocation flag.
pub struct InMemoryDelegationStore {
    by_delegator: Mutex<HashMap<String, DelegationRecord>>,
    pubkey_index: Mutex<HashMap<[u8; 32], String>>,
}

impl InMemoryDelegationStore {
    pub fn new() -> Self {
        Self {
            by_delegator: Mutex::new(HashMap::new()),
            pubkey_index: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryDelegationStore {
    fn default() -> Self {
        Self::new()
    }
}

impl DelegationStore for InMemoryDelegationStore {
    fn put_delegation(&self, record: &DelegationRecord) -> Result<(), StoreError> {
        let key = record.delegator.to_string();
        self.pubkey_index
            .lock()
            .unwrap()
            .insert(record.delegation_public_key, key.clone());
        self.by_delegator
            .lock()
            .unwrap()
            .insert(key, record.clone());
        Ok(())
    }

    fn get_delegation_by_delegator(
        &self,
        delegator: &WalletAddress,
    ) -> Result<Option<DelegationRecord>, StoreError> {
        Ok(self
            .by_delegator
            .lock()
            .unwrap()
            .get(delegator.as_str())
            .cloned())
    }

    fn get_delegation_by_pubkey(
        &self,
        pubkey: &[u8; 32],
    ) -> Result<Option<DelegationRecord>, StoreError> {
        let index = self.pubkey_index.lock().unwrap();
        match index.get(pubkey) {
            Some(delegator_key) => {
                let records = self.by_delegator.lock().unwrap();
                Ok(records.get(delegator_key).cloned())
            }
            None => Ok(None),
        }
    }

    fn revoke_delegation(&self, delegator: &WalletAddress) -> Result<(), StoreError> {
        let mut records = self.by_delegator.lock().unwrap();
        if let Some(record) = records.get_mut(delegator.as_str()) {
            record.revoked = true;
            Ok(())
        } else {
            Err(StoreError::NotFound(delegator.to_string()))
        }
    }
}
