//! LMDB implementation of VerificationStore.
//!
//! Endorsements and votes use composite keys `target_bytes ++ actor_bytes`
//! so each entry is its own LMDB key/value pair. Listing all entries for a
//! target is a prefix range-scan.

use std::ops::Bound;
use std::sync::Arc;

use heed::types::Bytes;
use heed::{Database, Env};

use burst_store::verification::VerificationStore;
use burst_store::StoreError;
use burst_types::WalletAddress;

use crate::block::increment_prefix;
use crate::LmdbError;

pub struct LmdbVerificationStore {
    pub(crate) env: Arc<Env>,
    pub(crate) endorsements_db: Database<Bytes, Bytes>,
    pub(crate) verification_votes_db: Database<Bytes, Bytes>,
    pub(crate) challenges_db: Database<Bytes, Bytes>,
}

/// Build composite key `target_bytes ++ actor_bytes`.
fn composite_key(target: &WalletAddress, actor: &WalletAddress) -> Vec<u8> {
    let t = target.as_str().as_bytes();
    let a = actor.as_str().as_bytes();
    let mut key = Vec::with_capacity(t.len() + a.len());
    key.extend_from_slice(t);
    key.extend_from_slice(a);
    key
}

/// Prefix range-scan: collect all values whose key starts with `prefix`.
fn range_scan_values(
    db: &Database<Bytes, Bytes>,
    env: &Env,
    prefix: &[u8],
) -> Result<Vec<Vec<u8>>, LmdbError> {
    let rtxn = env.read_txn()?;
    let mut upper = prefix.to_vec();
    increment_prefix(&mut upper);
    let bounds = (
        Bound::Included(prefix),
        Bound::Excluded(upper.as_slice()),
    );
    let iter = db.range(&rtxn, &bounds)?;
    let mut results = Vec::new();
    for result in iter {
        let (_key, val) = result?;
        results.push(val.to_vec());
    }
    Ok(results)
}

impl VerificationStore for LmdbVerificationStore {
    fn put_endorsement(
        &self,
        target: &WalletAddress,
        endorser: &WalletAddress,
        data: &[u8],
    ) -> Result<(), StoreError> {
        let key = composite_key(target, endorser);
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.endorsements_db
            .put(&mut wtxn, &key, data)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_endorsements(&self, target: &WalletAddress) -> Result<Vec<Vec<u8>>, StoreError> {
        range_scan_values(&self.endorsements_db, &self.env, target.as_str().as_bytes())
            .map_err(StoreError::from)
    }

    fn put_verification_vote(
        &self,
        target: &WalletAddress,
        voter: &WalletAddress,
        data: &[u8],
    ) -> Result<(), StoreError> {
        let key = composite_key(target, voter);
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.verification_votes_db
            .put(&mut wtxn, &key, data)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_verification_votes(
        &self,
        target: &WalletAddress,
    ) -> Result<Vec<Vec<u8>>, StoreError> {
        range_scan_values(
            &self.verification_votes_db,
            &self.env,
            target.as_str().as_bytes(),
        )
        .map_err(StoreError::from)
    }

    fn put_challenge(&self, target: &WalletAddress, data: &[u8]) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.challenges_db
            .put(&mut wtxn, target.as_str().as_bytes(), data)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_challenge(&self, target: &WalletAddress) -> Result<Option<Vec<u8>>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let val = self
            .challenges_db
            .get(&rtxn, target.as_str().as_bytes())
            .map_err(LmdbError::from)?
            .map(|b| b.to_vec());
        Ok(val)
    }
}
