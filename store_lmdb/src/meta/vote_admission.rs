use super::*;
use std::{collections::{HashMap, BTreeMap}, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, time::Instant};

type Key = (BlockHash, WalletAddress);

#[derive(Default)]
pub(crate) struct DeferredVotes {
    active: AtomicBool,
    holds: Mutex<HashMap<Key, BTreeMap<Instant, usize>>>,
}

/// Temporarily defer new soft votes for one store instance, epoch and representative.
/// Dropping this guard removes only its hold; other unexpired holds still apply.
/// Reservations ignore elapsed deadlines even if the guard remains alive.
/// This in-memory guard does not change durable promises or vote fences.
#[must_use]
pub struct VoteDeferral {
    state: Arc<DeferredVotes>,
    key: Key,
    deadline: Instant,
}

impl DeferredVotes {
    pub(super) fn contains(&self, epoch: &BlockHash, representative: &WalletAddress) -> Result<bool, StoreError> {
        self.contains_at(epoch, representative, Instant::now())
    }

    fn contains_at(&self, epoch: &BlockHash, representative: &WalletAddress, now: Instant) -> Result<bool, StoreError> {
        if !self.active.load(Ordering::Acquire) { return Ok(false); }
        Ok(self.holds.lock().map_err(|_| StoreError::Backend("vote deferral lock poisoned".into()))?
            .get(&(*epoch, representative.clone()))
            .and_then(|holds| holds.last_key_value()).is_some_and(|(deadline, _)| *deadline > now))
    }

    fn acquire(self: &Arc<Self>, key: Key, deadline: Instant) -> Result<VoteDeferral, StoreError> {
        let mut state = self.holds.lock().map_err(|_| StoreError::Backend("vote deferral lock poisoned".into()))?;
        let holds = state.entry(key.clone()).or_default();
        let count = holds.get(&deadline).copied().unwrap_or(0).checked_add(1)
            .ok_or_else(|| StoreError::Backend("vote deferral count exhausted".into()))?;
        holds.insert(deadline, count);
        self.active.store(true, Ordering::Release);
        Ok(VoteDeferral { state: self.clone(), key, deadline })
    }
}

impl Drop for VoteDeferral {
    fn drop(&mut self) {
        let mut state = self.state.holds.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(holds) = state.get_mut(&self.key) {
            if let Some(count) = holds.get_mut(&self.deadline) {
                *count -= 1;
                if *count == 0 { holds.remove(&self.deadline); }
            }
            if holds.is_empty() { state.remove(&self.key); }
        }
        if state.is_empty() { self.state.active.store(false, Ordering::Release); }
    }
}

impl VoteDeferral {
    pub fn deadline(&self) -> Instant { self.deadline }
}

impl LmdbMetaStore {
    /// Serialize the start of a temporary deferral with in-flight reservations.
    /// Only an authenticated local checkpoint-preparation owner should acquire
    /// this guard. It is not a signature authorization or a durable vote fence.
    /// Final votes (including completion of soft-quorum elections) and validated
    /// cached responses remain possible;
    /// every normal epoch, deadline, conflict and durable-fence check still runs.
    pub fn defer_new_soft_votes(
        &self, epoch: &BlockHash, representative: &WalletAddress, deadline: Instant,
    ) -> Result<VoteDeferral, StoreError> {
        let txn = self.env.write_txn().map_err(LmdbError::from)?;
        let installed = self.meta_db.get(&txn, burst_store::meta::PAYMENT_EPOCH_KEY.as_bytes())
            .map_err(LmdbError::from)?
            .ok_or_else(|| StoreError::Corruption("payment epoch is not initialized".into()))?;
        if epoch.is_zero() || burst_store::meta::decode_payment_epoch(installed)? != *epoch {
            return Err(StoreError::Backend("vote deferral epoch is not installed".into()));
        }
        if deadline <= Instant::now() {
            return Err(StoreError::Backend("vote deferral deadline has elapsed".into()));
        }
        let guard = self.deferred_votes.acquire((*epoch, representative.clone()), deadline)?;
        drop(txn);
        Ok(guard)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn deferral_drains_final_votes_and_preserves_cached_responses_but_defers_new_soft_votes() {
        let dir = tempfile::tempdir().unwrap();
        let env = crate::LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let meta = env.meta_store();
        let epoch = BlockHash::new([1; 32]);
        let rep = WalletAddress::new("brst_rep");
        let other = WalletAddress::new("brst_other");
        let first = BlockHash::new([2; 32]);
        let next = BlockHash::new([3; 32]);
        meta.put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, epoch.as_bytes()).unwrap();
        let reserve = |representative: &WalletAddress, hash: &BlockHash, previous: &BlockHash, final_vote| {
            meta.reserve_consensus_vote_at(&epoch, representative, &rep, previous, hash,
                final_vote, 0, Some(100))
        };
        let soft = reserve(&rep, &first, &BlockHash::ZERO, false).unwrap();
        let final_sequence = reserve(&rep, &first, &BlockHash::ZERO, true).unwrap();
        let revision = env.env().info().last_txn_id;
        let hold = meta.defer_new_soft_votes(&epoch, &rep, Instant::now() + Duration::from_secs(60)).unwrap();
        assert_eq!(env.env().info().last_txn_id, revision);
        let nested = env.meta_store().defer_new_soft_votes(&epoch, &rep, Instant::now() + Duration::from_secs(60)).unwrap();
        assert!(reserve(&rep, &next, &first, false).is_err());
        let drained = reserve(&rep, &BlockHash::new([4; 32]), &BlockHash::new([5; 32]), true).unwrap();
        meta.validate_reserved_soft_vote(&epoch, &rep, &rep, &BlockHash::ZERO, &first, soft, 100).unwrap();
        meta.validate_reserved_final_vote(&epoch, &rep, &rep, &BlockHash::ZERO, &first, final_sequence, 100).unwrap();
        let repeated = reserve(&rep, &first, &BlockHash::ZERO, true).unwrap();
        assert_eq!(repeated, drained + 1);
        assert!(reserve(&rep, &next, &BlockHash::ZERO, true).is_err());
        reserve(&other, &next, &first, true).unwrap();
        let request = |hash, previous| ConsensusVoteReservation {
            payment_epoch: &epoch, representative: &rep, account: &rep, previous, hash,
            is_final: true, sequence_floor: 0, timestamp: Some(100), governance_limit: None,
        };
        let mut fresh_soft = request(&next, &first);
        fresh_soft.is_final = false;
        assert!(meta.reserve_consensus_votes(&[request(&first, &BlockHash::ZERO), fresh_soft]).is_err());
        assert_eq!(reserve(&rep, &first, &BlockHash::ZERO, true).unwrap(), repeated + 1,
            "failed batch must not consume the first request's sequence");
        drop(hold);
        assert!(reserve(&rep, &next, &first, false).is_err());
        drop(nested);
        reserve(&rep, &next, &first, false).unwrap();
    }

    #[test]
    fn deferral_drops_on_unwind_and_never_overrides_a_durable_fence() {
        let dir = tempfile::tempdir().unwrap();
        let env = crate::LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let meta = env.meta_store();
        let epoch = BlockHash::new([1; 32]);
        let rep = WalletAddress::new("brst_rep");
        let hash = BlockHash::new([2; 32]);
        meta.put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, epoch.as_bytes()).unwrap();
        assert!(meta.defer_new_soft_votes(&BlockHash::new([9; 32]), &rep, Instant::now() + Duration::from_secs(60)).is_err());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _hold = meta.defer_new_soft_votes(&epoch, &rep, Instant::now() + Duration::from_secs(60)).unwrap();
            panic!("cancel preparation");
        }));
        assert!(result.is_err());
        let sequence = meta.reserve_consensus_vote_at(&epoch, &rep, &rep, &BlockHash::ZERO,
            &hash, true, 0, Some(100)).unwrap();
        let hold = meta.defer_new_soft_votes(&epoch, &rep, Instant::now() + Duration::from_secs(60)).unwrap();
        let mut txn = env.tx_begin_write().unwrap();
        txn.seal_consensus_votes(&rep, &epoch, &epoch, &[hash].into_iter().collect()).unwrap();
        txn.commit().unwrap();
        assert!(meta.validate_reserved_final_vote(&epoch, &rep, &rep, &BlockHash::ZERO,
            &hash, sequence, 100).is_err());
        drop(hold);
        assert!(meta.reserve_consensus_vote_at(&epoch, &rep, &rep, &BlockHash::ZERO,
            &hash, true, 0, Some(100)).is_err());
    }
    #[test]
    fn expired_owner_cannot_keep_votes_deferred_and_nested_deadlines_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let env = crate::LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let meta = env.meta_store();
        let epoch = BlockHash::new([1; 32]);
        let rep = WalletAddress::new("brst_rep");
        meta.put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, epoch.as_bytes()).unwrap();
        let now = Instant::now();
        assert!(meta.defer_new_soft_votes(&epoch, &rep, now).is_err());
        // Inject an elapsed lease without a sleep, keeping its owner alive.
        let expired = meta.deferred_votes.acquire((epoch, rep.clone()), now).unwrap();
        meta.reserve_consensus_vote_at(&epoch, &rep, &rep, &BlockHash::ZERO,
            &BlockHash::new([2; 32]), false, 0, Some(100)).unwrap();
        let short = meta.deferred_votes.acquire((epoch, rep.clone()), now + Duration::from_secs(10)).unwrap();
        let long = meta.deferred_votes.acquire((epoch, rep.clone()), now + Duration::from_secs(20)).unwrap();
        assert!(meta.deferred_votes.contains_at(&epoch, &rep, now + Duration::from_secs(15)).unwrap());
        drop(long);
        assert!(!meta.deferred_votes.contains_at(&epoch, &rep, now + Duration::from_secs(15)).unwrap());
        assert!(meta.deferred_votes.contains_at(&epoch, &rep, now + Duration::from_secs(5)).unwrap());
        drop(short);
        assert!(!meta.deferred_votes.contains_at(&epoch, &rep, now).unwrap());
        drop(expired);
    }

}
