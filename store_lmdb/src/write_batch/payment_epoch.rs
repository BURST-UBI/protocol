use super::*;
use std::collections::HashSet;

const MAX_RESERVATIONS: usize = 131_072;

pub(crate) fn closed_key(epoch: &BlockHash) -> String {
    burst_store::meta::closed_payment_epoch_key(epoch)
}

fn invalid(message: &str) -> StoreError {
    StoreError::Corruption(message.into())
}

impl LmdbWriteTransaction<'_> {
    /// Read the installed, unclosed context from this writer's atomic view.
    pub fn payment_epoch(&self) -> Result<BlockHash, StoreError> {
        self.read_view().payment_epoch()
    }

    /// The storage portion of a certified handoff. The caller must authenticate
    /// the certificate, derive `included` by replay, and install its exact
    /// economic state and human authority in THIS transaction. This method does
    /// not validate a certificate or authorize an epoch from these hashes alone.
    /// Sync the committed transaction before publishing state or signing again.
    ///
    /// Return old-epoch final choices excluded by this cut. These may be minority
    /// votes, not network finality; the caller must reconcile the corresponding
    /// local tails without replacing cemented state. Every promise and fence is
    /// retained under its epoch, which can never be reopened.
    pub fn advance_payment_epoch(
        &mut self,
        expected: &BlockHash,
        checkpoint_parent: &BlockHash,
        next: &BlockHash,
        included: &HashSet<BlockHash>,
    ) -> Result<Vec<BlockHash>, StoreError> {
        self.advance_epoch(expected, checkpoint_parent, next, Some(included))
    }

    /// Close the reconciled local epoch during compact snapshot installation.
    /// The caller must authenticate descent and project complete state in this
    /// writer. Sparse membership cannot establish exclusion: return ALL retained
    /// choices without classifying any as minority or removing signer records.
    pub fn advance_snapshot_payment_epoch(
        &mut self,
        expected: &BlockHash,
        next: &BlockHash,
    ) -> Result<Vec<BlockHash>, StoreError> {
        self.advance_epoch(expected, expected, next, None)
    }

    fn advance_epoch(
        &mut self,
        expected: &BlockHash,
        checkpoint_parent: &BlockHash,
        next: &BlockHash,
        included: Option<&HashSet<BlockHash>>,
    ) -> Result<Vec<BlockHash>, StoreError> {
        let result = (|| {
            if self.failed
                || self.undo.is_some()
                || checkpoint_parent.is_zero()
                || next.is_zero()
                || next == expected
                || included.is_some_and(|hashes| hashes.len() > MAX_RESERVATIONS)
                || self.payment_epoch()? != *expected
                || self.get_meta(&closed_key(next))?.is_some()
            {
                return Err(invalid("invalid payment epoch handoff"));
            }
            if self
                .env
                .meta_db
                .prefix_iter(&self.txn, b"consensus_final/v1/")
                .map_err(LmdbError::from)?
                .next()
                .transpose()
                .map_err(LmdbError::from)?
                .is_some()
            {
                return Err(invalid(
                    "legacy final votes require explicit reconstruction",
                ));
            }
            let mut reservations = 0;
            let mut retired = HashSet::new();
            let prefix = format!("consensus_final/v2/{expected}/");
            for entry in self
                .env
                .meta_db
                .prefix_iter(&self.txn, prefix.as_bytes())
                .map_err(LmdbError::from)?
            {
                reservations += 1;
                if reservations > MAX_RESERVATIONS {
                    return Err(invalid("final vote handoff requires incremental replay"));
                }
                let (_, bytes) = entry.map_err(LmdbError::from)?;
                let hash = BlockHash::new(
                    bytes
                        .try_into()
                        .map_err(|_| invalid("invalid durable final vote"))?,
                );
                if hash.is_zero() {
                    return Err(invalid("invalid durable final vote"));
                }
                if included.is_none_or(|hashes| !hashes.contains(&hash)) {
                    retired.insert(hash);
                }
            }
            if self
                .env
                .meta_db
                .prefix_iter(&self.txn, b"consensus_fence/v1/")
                .map_err(LmdbError::from)?
                .next()
                .transpose()
                .map_err(LmdbError::from)?
                .is_some()
            {
                return Err(invalid(
                    "legacy consensus fence requires explicit reconstruction",
                ));
            }
            let prefix = format!("consensus_fence/v2/{expected}/");
            for (count, entry) in self
                .env
                .meta_db
                .prefix_iter(&self.txn, prefix.as_bytes())
                .map_err(LmdbError::from)?
                .enumerate()
            {
                if count >= MAX_RESERVATIONS {
                    return Err(invalid("vote fence handoff requires incremental replay"));
                }
                let (_, parent) = entry.map_err(LmdbError::from)?;
                if parent != checkpoint_parent.as_bytes() {
                    return Err(invalid("vote fence does not match the certified parent"));
                }
            }
            let mut closed = Vec::with_capacity(64);
            closed.extend_from_slice(next.as_bytes());
            closed.extend_from_slice(checkpoint_parent.as_bytes());
            self.put_meta(&closed_key(expected), &closed)?;
            self.put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, next.as_bytes())?;
            let mut retired: Vec<_> = retired.into_iter().collect();
            retired.sort_by_key(|hash| *hash.as_bytes());
            Ok(retired)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::meta::{MetaStore, PAYMENT_EPOCH_KEY};

    fn hash(value: u8) -> BlockHash {
        BlockHash::new([value; 32])
    }
    fn rep(value: u8) -> WalletAddress {
        WalletAddress::new(format!("brst_rep_{value}"))
    }
    fn open(path: &std::path::Path) -> LmdbEnvironment {
        LmdbEnvironment::open(path, 32, 16 << 20).unwrap()
    }
    fn initialize(store: &LmdbEnvironment) {
        store
            .meta_store()
            .put_meta(PAYMENT_EPOCH_KEY, hash(1).as_bytes())
            .unwrap();
    }
    fn vote(
        store: &LmdbEnvironment,
        epoch: u8,
        signer: u8,
        previous: BlockHash,
        choice: u8,
    ) -> Result<u64, StoreError> {
        store.meta_store().reserve_consensus_vote(
            &hash(epoch),
            &rep(signer),
            &WalletAddress::new("brst_account"),
            &previous,
            &hash(choice),
            true,
            0,
        )
    }

    #[test]
    fn epoch_handoff_is_atomic_and_keeps_final_promises_and_old_fences_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        initialize(&store);
        vote(&store, 1, 0, BlockHash::ZERO, 3).unwrap();
        vote(&store, 1, 1, hash(3), 4).unwrap();
        let included = HashSet::from([hash(3), hash(4)]);
        let fence = super::super::vote_fence::fence_key(&hash(1), &rep(0));
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .seal_consensus_votes(&rep(0), &hash(1), &hash(9), &included)
            .unwrap();
        batch.commit().unwrap();
        for commit in [false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            batch
                .advance_payment_epoch(&hash(1), &hash(9), &hash(2), &included)
                .unwrap();
            batch
                .put_meta("test/certified-state", b"new state")
                .unwrap();
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
                assert_eq!(store.meta_store().payment_epoch().unwrap(), hash(1));
                assert!(store.meta_store().get_meta("test/certified-state").is_err());
                assert!(store.meta_store().get_meta(&closed_key(&hash(1))).is_err());
                assert!(vote(&store, 1, 0, BlockHash::ZERO, 3).is_err());
            }
        }
        store.force_sync().unwrap();
        drop(store);
        let store = open(dir.path());
        assert_eq!(store.meta_store().payment_epoch().unwrap(), hash(2));
        assert_eq!(
            store.meta_store().get_meta(&fence).unwrap(),
            hash(9).as_bytes()
        );
        assert_eq!(
            store.meta_store().get_meta("test/certified-state").unwrap(),
            b"new state"
        );
        assert!(vote(&store, 1, 0, hash(4), 5).is_err());
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch
            .seal_consensus_votes(&rep(0), &hash(1), &hash(9), &included)
            .is_err());
        assert!(batch.commit().is_err());
        assert_eq!(vote(&store, 2, 0, hash(4), 5).unwrap(), 2);
        assert!(vote(&store, 2, 0, hash(4), 6).is_err());
        let key = super::super::vote_fence::final_key(
            &hash(1),
            &rep(0),
            &WalletAddress::new("brst_account"),
            &BlockHash::ZERO,
        );
        assert_eq!(
            store.meta_store().get_meta(&key).unwrap(),
            hash(3).as_bytes()
        );
    }

    #[test]
    fn snapshot_epoch_handoff_retains_every_promise_without_exclusion_claims() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        initialize(&store);
        vote(&store, 1, 0, BlockHash::ZERO, 3).unwrap();
        vote(&store, 1, 1, hash(3), 4).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .seal_consensus_after_certificate(&rep(0), &hash(1))
            .unwrap();
        batch.commit().unwrap();
        let before = store
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"consensus_")
            .unwrap();
        for commit in [false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            assert_eq!(
                batch
                    .advance_snapshot_payment_epoch(&hash(1), &hash(8))
                    .unwrap(),
                vec![hash(3), hash(4)]
            );
            assert_eq!(batch.checkpoint_metadata(b"consensus_").unwrap(), before);
            batch.put_meta("test/complete-snapshot", b"state").unwrap();
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
                assert_eq!(store.meta_store().payment_epoch().unwrap(), hash(1));
                assert!(store
                    .meta_store()
                    .get_meta("test/complete-snapshot")
                    .is_err());
            }
        }
        store.force_sync().unwrap();
        drop(store);
        let store = open(dir.path());
        assert_eq!(store.meta_store().payment_epoch().unwrap(), hash(8));
        assert_eq!(
            store
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"consensus_")
                .unwrap(),
            before
        );
        assert!(vote(&store, 1, 0, hash(4), 5).is_err());
        assert!(vote(&store, 8, 0, hash(4), 5).is_ok());
    }

    #[test]
    fn snapshot_handoff_stale_epoch_and_wrong_fences_abort_prior_writes() {
        for case in 0..3 {
            let dir = tempfile::tempdir().unwrap();
            let store = open(dir.path());
            initialize(&store);
            if case == 2 {
                let mut batch = store.tx_begin_write().unwrap();
                batch
                    .seal_consensus_votes(&rep(0), &hash(1), &hash(7), &HashSet::new())
                    .unwrap();
                batch.commit().unwrap();
            }
            let mut batch = store.tx_begin_write().unwrap();
            batch.put_meta("test/abort-snapshot", b"state").unwrap();
            let next = if case == 0 { hash(1) } else { hash(8) };
            let old = if case == 1 { hash(7) } else { hash(1) };
            assert!(batch.advance_snapshot_payment_epoch(&old, &next).is_err());
            assert!(batch.commit().is_err());
            assert_eq!(store.meta_store().payment_epoch().unwrap(), hash(1));
            assert!(store.meta_store().get_meta("test/abort-snapshot").is_err());
        }
    }

    #[test]
    fn wrong_parent_and_stale_context_poison_the_entire_handoff() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        initialize(&store);
        vote(&store, 1, 0, BlockHash::ZERO, 3).unwrap();
        vote(&store, 1, 1, hash(3), 4).unwrap();
        let included = HashSet::from([hash(3), hash(4)]);
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .seal_consensus_votes(&rep(0), &hash(1), &hash(9), &included)
            .unwrap();
        batch.commit().unwrap();
        for (expected, parent, next, cut) in [
            (hash(1), hash(8), hash(2), included.clone()),
            (hash(8), hash(9), hash(2), included.clone()),
            (hash(1), hash(9), hash(1), included.clone()),
            (hash(1), hash(9), BlockHash::ZERO, included.clone()),
        ] {
            let mut batch = store.tx_begin_write().unwrap();
            batch
                .put_meta("test/certified-state", b"must abort")
                .unwrap();
            assert!(batch
                .advance_payment_epoch(&expected, &parent, &next, &cut)
                .is_err());
            assert!(batch.commit().is_err());
            assert!(store.meta_store().get_meta("test/certified-state").is_err());
            assert_eq!(store.meta_store().payment_epoch().unwrap(), hash(1));
        }
    }

    #[test]
    fn observer_handoff_cannot_reopen_any_closed_epoch_even_after_metadata_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        initialize(&store);
        for (old, next) in [(1, 2), (2, 3)] {
            let mut batch = store.tx_begin_write().unwrap();
            batch
                .advance_payment_epoch(&hash(old), &hash(9), &hash(next), &HashSet::new())
                .unwrap();
            batch.commit().unwrap();
        }
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch
            .advance_payment_epoch(&hash(3), &hash(9), &hash(1), &HashSet::new())
            .is_err());
        assert!(batch.commit().is_err());
        store
            .meta_store()
            .put_meta(PAYMENT_EPOCH_KEY, hash(1).as_bytes())
            .unwrap();
        assert!(vote(&store, 1, 0, BlockHash::ZERO, 4).is_err());
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch
            .seal_consensus_votes(&rep(0), &hash(1), &hash(9), &HashSet::new())
            .is_err());
        assert!(batch.commit().is_err());
    }

    #[test]
    fn racing_old_epoch_vote_is_retired_by_handoff_or_rejected_after_it() {
        for _ in 0..16 {
            let dir = tempfile::tempdir().unwrap();
            let store = open(dir.path());
            initialize(&store);
            let barrier = std::sync::Barrier::new(2);
            let (voted, retired) = std::thread::scope(|scope| {
                let vote = scope.spawn(|| {
                    barrier.wait();
                    vote(&store, 1, 0, BlockHash::ZERO, 4).is_ok()
                });
                let handoff = scope.spawn(|| {
                    barrier.wait();
                    let mut batch = store.tx_begin_write().unwrap();
                    let retired = batch
                        .advance_payment_epoch(&hash(1), &hash(9), &hash(2), &HashSet::new())
                        .unwrap();
                    batch.commit().unwrap();
                    retired
                });
                (vote.join().unwrap(), handoff.join().unwrap())
            });
            assert_eq!(retired, if voted { vec![hash(4)] } else { vec![] });
            assert_eq!(store.meta_store().payment_epoch().unwrap(), hash(2));
            assert!(vote(&store, 1, 0, BlockHash::ZERO, 4).is_err());
        }
    }

    #[test]
    fn legacy_or_corrupt_reservations_and_block_undo_cannot_bypass_handoff_validation() {
        for corrupt in [0, 1, 2, 3] {
            let dir = tempfile::tempdir().unwrap();
            let store = open(dir.path());
            initialize(&store);
            let key = match corrupt {
                0 => format!("consensus_fence/v1/{}", rep(0)),
                1 => super::super::vote_fence::fence_key(&hash(1), &rep(0)),
                2 => format!(
                    "consensus_final/v1/{}/brst_account/{}",
                    rep(0),
                    BlockHash::ZERO
                ),
                _ => super::super::vote_fence::final_key(
                    &hash(1),
                    &rep(0),
                    &WalletAddress::new("brst_account"),
                    &BlockHash::ZERO,
                ),
            };
            store.meta_store().put_meta(&key, &[1]).unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            assert!(batch
                .advance_payment_epoch(&hash(1), &hash(9), &hash(2), &HashSet::new())
                .is_err());
            assert!(batch.commit().is_err());
            assert_eq!(store.meta_store().get_meta(&key).unwrap(), [1]);
        }
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        initialize(&store);
        let mut batch = store.tx_begin_write().unwrap();
        batch.begin_block_undo(hash(4), rep(0), 2).unwrap();
        assert!(batch
            .advance_payment_epoch(&hash(1), &hash(9), &hash(2), &HashSet::new())
            .is_err());
        assert!(batch.commit().is_err());
        assert_eq!(store.meta_store().payment_epoch().unwrap(), hash(1));
    }

    #[test]
    fn minority_final_promise_survives_but_cannot_strand_an_authenticated_new_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path());
        initialize(&store);
        vote(&store, 1, 0, BlockHash::ZERO, 4).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        // This signer cannot sign a checkpoint that omits its own final vote.
        assert!(batch
            .seal_consensus_votes(&rep(0), &hash(1), &hash(9), &HashSet::new())
            .is_err());
        assert!(batch.commit().is_err());
        // Certificate authentication belongs to the caller. Its old-roster
        // quorum may decide without this minority signer.
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(
            batch
                .advance_payment_epoch(&hash(1), &hash(9), &hash(2), &HashSet::new())
                .unwrap(),
            vec![hash(4)]
        );
        batch.commit().unwrap();
        assert!(vote(&store, 1, 0, BlockHash::ZERO, 5).is_err());
        assert_eq!(vote(&store, 2, 0, BlockHash::ZERO, 5).unwrap(), 2);
        assert!(vote(&store, 2, 0, BlockHash::ZERO, 6).is_err());
        for (epoch, choice) in [(1, 4), (2, 5)] {
            let key = super::super::vote_fence::final_key(
                &hash(epoch),
                &rep(0),
                &WalletAddress::new("brst_account"),
                &BlockHash::ZERO,
            );
            assert_eq!(
                store.meta_store().get_meta(&key).unwrap(),
                hash(choice).as_bytes()
            );
        }
    }
}

impl crate::read_view::LedgerReadView<'_> {
    pub fn payment_epoch(&self) -> Result<BlockHash, StoreError> {
        burst_store::meta::read_open_payment_epoch(|key| self.get_meta(key))
    }
}
