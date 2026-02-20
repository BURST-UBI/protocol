//! LMDB implementation of GovernanceStore.
//!
//! Votes use composite key `proposal(32) ++ voter_bytes` → vote data.
//! `get_votes` performs a prefix range-scan on the 32-byte proposal prefix.
//! No length-prefixed blobs or dual writes.

use std::ops::Bound;
use std::sync::Arc;

use heed::types::Bytes;
use heed::{Database, Env};

use burst_store::governance::GovernanceStore;
use burst_store::StoreError;
use burst_types::{TxHash, WalletAddress};

use crate::block::increment_prefix;
use crate::LmdbError;

pub struct LmdbGovernanceStore {
    pub(crate) env: Arc<Env>,
    pub(crate) proposals_db: Database<Bytes, Bytes>,
    pub(crate) votes_db: Database<Bytes, Bytes>,
    pub(crate) delegations_db: Database<Bytes, Bytes>,
    pub(crate) constitution_db: Database<Bytes, Bytes>,
}

/// Build the composite key `proposal(32) ++ voter_bytes`.
fn vote_key(proposal: &TxHash, voter: &WalletAddress) -> Vec<u8> {
    let mut key = Vec::with_capacity(32 + voter.as_str().len());
    key.extend_from_slice(proposal.as_bytes());
    key.extend_from_slice(voter.as_str().as_bytes());
    key
}

impl GovernanceStore for LmdbGovernanceStore {
    fn put_proposal(&self, hash: &TxHash, data: &[u8]) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.proposals_db
            .put(&mut wtxn, hash.as_bytes().as_slice(), data)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_proposal(&self, hash: &TxHash) -> Result<Vec<u8>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let val = self
            .proposals_db
            .get(&rtxn, hash.as_bytes().as_slice())
            .map_err(LmdbError::from)?
            .ok_or_else(|| LmdbError::NotFound(format!("proposal {:?}", hash)))?;
        Ok(val.to_vec())
    }

    fn list_active_proposals(&self) -> Result<Vec<TxHash>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let mut proposals = Vec::new();
        let iter = self.proposals_db.iter(&rtxn).map_err(LmdbError::from)?;
        for result in iter {
            let (key, _val) = result.map_err(LmdbError::from)?;
            if key.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(key);
                proposals.push(TxHash::new(arr));
            }
        }
        Ok(proposals)
    }

    fn put_vote(
        &self,
        proposal: &TxHash,
        voter: &WalletAddress,
        data: &[u8],
    ) -> Result<(), StoreError> {
        let key = vote_key(proposal, voter);
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.votes_db
            .put(&mut wtxn, &key, data)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_vote(&self, proposal: &TxHash, voter: &WalletAddress) -> Result<Vec<u8>, StoreError> {
        let key = vote_key(proposal, voter);
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let val = self
            .votes_db
            .get(&rtxn, &key)
            .map_err(LmdbError::from)?
            .ok_or_else(|| {
                LmdbError::NotFound(format!("vote for {:?} by {:?}", proposal, voter))
            })?;
        Ok(val.to_vec())
    }

    fn get_votes(&self, proposal: &TxHash) -> Result<Vec<Vec<u8>>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let prefix = proposal.as_bytes().as_slice();
        let mut upper = prefix.to_vec();
        increment_prefix(&mut upper);

        let bounds = (
            Bound::Included(prefix),
            Bound::Excluded(upper.as_slice()),
        );
        let iter = self.votes_db.range(&rtxn, &bounds).map_err(LmdbError::from)?;
        let mut results = Vec::new();
        for result in iter {
            let (_key, val) = result.map_err(LmdbError::from)?;
            results.push(val.to_vec());
        }
        Ok(results)
    }

    fn put_delegation(&self, delegator: &WalletAddress, data: &[u8]) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.delegations_db
            .put(&mut wtxn, delegator.as_str().as_bytes(), data)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_delegation(&self, delegator: &WalletAddress) -> Result<Option<Vec<u8>>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let val = self
            .delegations_db
            .get(&rtxn, delegator.as_str().as_bytes())
            .map_err(LmdbError::from)?
            .map(|b| b.to_vec());
        Ok(val)
    }

    fn put_constitution(&self, data: &[u8]) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.constitution_db
            .put(&mut wtxn, "current".as_bytes(), data)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_constitution(&self) -> Result<Vec<u8>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let val = self
            .constitution_db
            .get(&rtxn, "current".as_bytes())
            .map_err(LmdbError::from)?
            .ok_or_else(|| LmdbError::NotFound("constitution".to_string()))?;
        Ok(val.to_vec())
    }
}
