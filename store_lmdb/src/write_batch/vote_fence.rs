use super::*;
use std::collections::HashSet;

const MAX_FINAL_VOTES: usize = 131_072;

pub(crate) fn fence_key(epoch: &BlockHash, representative: &WalletAddress) -> String {
    format!("consensus_fence/v2/{epoch}/{representative}")
}

pub(crate) fn final_prefix(epoch: &BlockHash, representative: &WalletAddress) -> String {
    format!("consensus_final/v2/{epoch}/{representative}/")
}

pub(crate) fn final_key(
    epoch: &BlockHash,
    representative: &WalletAddress,
    account: &WalletAddress,
    previous: &BlockHash,
) -> String {
    format!(
        "{}{account}/{previous}",
        final_prefix(epoch, representative)
    )
}

impl LmdbWriteTransaction<'_> {
    fn checked_vote_fence_key(
        &self,
        representative: &WalletAddress,
        expected_epoch: &BlockHash,
        parent: &BlockHash,
    ) -> Result<String, StoreError> {
        if self.failed || self.undo.is_some() || parent.is_zero() {
            return Err(StoreError::Corruption(
                "invalid consensus vote fence".into(),
            ));
        }
        let epoch = self.payment_epoch()?;
        if epoch != *expected_epoch {
            return Err(StoreError::Corruption(
                "checkpoint signing payment epoch is not installed".into(),
            ));
        }
        if self
            .get_meta(&format!("consensus_fence/v1/{representative}"))?
            .is_some()
        {
            return Err(StoreError::Corruption(
                "legacy consensus fence requires explicit reconstruction".into(),
            ));
        }
        let key = fence_key(&epoch, representative);
        if self
            .get_meta(&key)?
            .is_some_and(|saved| saved.as_slice() != parent.as_bytes())
        {
            return Err(StoreError::Corruption(
                "consensus vote fence parent mismatch".into(),
            ));
        }
        Ok(key)
    }

    /// Stop old-epoch voting after learning an authenticated quorum certificate.
    /// Unlike signing a candidate, observing a decision does not require this
    /// observer's minority final promises to be in the cut. They remain intact.
    /// The caller must authenticate the certificate under this epoch's roster
    /// and commit its decision journal in the same transaction before publishing.
    /// This operation does not authorize a checkpoint signature or install state.
    pub fn seal_consensus_after_certificate(
        &mut self,
        representative: &WalletAddress,
        epoch: &BlockHash,
    ) -> Result<(), StoreError> {
        let result = self
            .checked_vote_fence_key(representative, epoch, epoch)
            .and_then(|key| self.put_meta(&key, epoch.as_bytes()));
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Freeze this signer's installed payment epoch in the same transaction as
    /// its checkpoint journal. The caller must derive `included` from replayed
    /// signed history, not a peer's assertion that these blocks were included.
    ///
    /// Every previously reserved final vote must be in the cut. Competing rounds
    /// at the same parent may change the cut, but cannot remove the fence.
    /// A later epoch leaves this fence and all final choices intact.
    pub fn seal_consensus_votes(
        &mut self,
        representative: &WalletAddress,
        expected_epoch: &BlockHash,
        parent: &BlockHash,
        included: &HashSet<BlockHash>,
    ) -> Result<(), StoreError> {
        let result = (|| {
            self.check_consensus_vote_coverage(representative, expected_epoch, parent, included)?;
            let key = self.checked_vote_fence_key(representative, expected_epoch, parent)?;
            self.put_meta(&key, parent.as_bytes())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Read-only preflight under this writer. It neither fences payment voting
    /// nor replaces the atomic check required when committing a signer journal.
    pub fn check_consensus_vote_coverage(
        &self,
        representative: &WalletAddress,
        expected_epoch: &BlockHash,
        parent: &BlockHash,
        included: &HashSet<BlockHash>,
    ) -> Result<(), StoreError> {
        if !self.covers_consensus_votes(representative, expected_epoch, parent, included)? {
            return Err(StoreError::Corruption("checkpoint cut omits a durable final vote".into()));
        }
        Ok(())
    }

    /// Return false at the first final vote missing from `included`. Malformed
    /// records encountered before that point return an error; later records are
    /// not inspected. This read does not change the journal or install a fence.
    pub fn covers_consensus_votes(
        &self, representative: &WalletAddress, expected_epoch: &BlockHash,
        parent: &BlockHash, included: &HashSet<BlockHash>,
    ) -> Result<bool, StoreError> {
        if included.len() > MAX_FINAL_VOTES {
            return Err(StoreError::Corruption(
                "invalid consensus vote fence".into(),
            ));
        }
        self.checked_vote_fence_key(representative, expected_epoch, parent)?;
        let prefixes = [
            format!("consensus_final/v1/{representative}/"),
            final_prefix(expected_epoch, representative),
        ];
        let mut count = 0;
        for prefix in prefixes {
            for entry in self
                .env
                .meta_db
                .prefix_iter(&self.txn, prefix.as_bytes())
                .map_err(LmdbError::from)?
            {
                if count >= MAX_FINAL_VOTES {
                    return Err(StoreError::Corruption(
                        "final vote scan requires incremental replay".into(),
                    ));
                }
                count += 1;
                let (_, bytes) = entry.map_err(LmdbError::from)?;
                let hash =
                    BlockHash::new(bytes.try_into().map_err(|_| {
                        StoreError::Corruption("invalid durable final vote".into())
                    })?);
                if hash.is_zero() {
                    return Err(StoreError::Corruption("invalid durable final vote".into()));
                }
                if !included.contains(&hash) { return Ok(false); }
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::meta::MetaStore;

    fn reserve(
        store: &LmdbEnvironment,
        representative: &WalletAddress,
        hash: u8,
        final_vote: bool,
    ) -> Result<u64, StoreError> {
        if matches!(
            store.meta_store().payment_epoch(),
            Err(StoreError::NotFound(_))
        ) {
            store
                .meta_store()
                .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &[1; 32])?;
        }
        store.meta_store().reserve_consensus_vote(
            &burst_types::BlockHash::new([1; 32]),
            representative,
            &WalletAddress::new("brst_account"),
            &BlockHash::ZERO,
            &BlockHash::new([hash; 32]),
            final_vote,
            0,
        )
    }

    #[test]
    fn certificate_fence_retains_minority_promises_and_commits_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let rep = WalletAddress::new("brst_rep");
        let epoch = BlockHash::new([1; 32]);
        let minority = BlockHash::new([7; 32]);
        reserve(&store, &rep, 7, true).unwrap();
        {
            let mut batch = store.tx_begin_write().unwrap();
            batch
                .seal_consensus_after_certificate(&rep, &epoch)
                .unwrap();
            batch.put_meta("test/certificate", b"aborted").unwrap();
        }
        assert_eq!(reserve(&store, &rep, 7, true).unwrap(), 2);
        assert!(matches!(
            store.meta_store().get_meta("test/certificate"),
            Err(StoreError::NotFound(_))
        ));
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .seal_consensus_after_certificate(&rep, &epoch)
            .unwrap();
        batch.put_meta("test/certificate", b"accepted").unwrap();
        batch.commit().unwrap();
        assert!(reserve(&store, &rep, 7, true).is_err());
        assert!(reserve(&store, &rep, 8, false).is_err());
        assert_eq!(
            store
                .meta_store()
                .get_meta(&final_key(
                    &epoch,
                    &rep,
                    &WalletAddress::new("brst_account"),
                    &BlockHash::ZERO
                ))
                .unwrap(),
            minority.as_bytes()
        );
        let next = BlockHash::new([2; 32]);
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(
            batch
                .advance_payment_epoch(&epoch, &epoch, &next, &HashSet::new())
                .unwrap(),
            vec![minority]
        );
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta("test/certificate", b"stale").unwrap();
        assert!(batch
            .seal_consensus_after_certificate(&rep, &epoch)
            .is_err());
        assert!(batch.commit().is_err());
        assert_eq!(
            store.meta_store().get_meta("test/certificate").unwrap(),
            b"accepted"
        );
        assert!(matches!(
            store.meta_store().get_meta(&fence_key(&next, &rep)),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn vote_fence_rejects_missing_final_without_committing_journal_or_fence() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let rep = WalletAddress::new("brst_rep");
        reserve(&store, &rep, 1, true).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_meta("test/checkpoint-journal", b"uncommitted")
            .unwrap();
        assert!(batch
            .seal_consensus_votes(
                &rep,
                &BlockHash::new([1; 32]),
                &BlockHash::new([9; 32]),
                &HashSet::new()
            )
            .is_err());
        assert!(batch.commit().is_err());
        assert!(matches!(
            store.meta_store().get_meta("test/checkpoint-journal"),
            Err(StoreError::NotFound(_))
        ));
        assert!(matches!(
            store
                .meta_store()
                .get_meta(&fence_key(&BlockHash::new([1; 32]), &rep)),
            Err(StoreError::NotFound(_))
        ));
        assert_eq!(reserve(&store, &rep, 1, true).unwrap(), 2);
    }

    #[test]
    fn vote_fence_survives_restart_blocks_soft_and_final_and_preserves_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let rep = WalletAddress::new("brst_rep");
        let parent = BlockHash::new([9; 32]);
        let included = HashSet::from([BlockHash::new([1; 32])]);
        {
            let store = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
            reserve(&store, &rep, 1, true).unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            batch
                .seal_consensus_votes(&rep, &BlockHash::new([1; 32]), &parent, &included)
                .unwrap();
            batch
                .put_meta("test/checkpoint-journal", b"committed")
                .unwrap();
            batch.commit().unwrap();
            store.force_sync().unwrap();
        }
        let store = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        for final_vote in [false, true] {
            for hash in [1, 2] {
                assert!(reserve(&store, &rep, hash, final_vote).is_err());
            }
        }
        let meta = store.meta_store();
        assert_eq!(
            meta.get_meta("consensus_sequence/v1/brst_rep").unwrap(),
            1u64.to_be_bytes()
        );
        assert_eq!(
            meta.get_meta("test/checkpoint-journal").unwrap(),
            b"committed"
        );
        assert_eq!(
            reserve(&store, &WalletAddress::new("brst_other"), 2, true).unwrap(),
            1
        );
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .seal_consensus_votes(&rep, &BlockHash::new([1; 32]), &parent, &included)
            .unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch
            .seal_consensus_votes(
                &rep,
                &BlockHash::new([1; 32]),
                &BlockHash::new([8; 32]),
                &included
            )
            .is_err());
        assert!(batch.commit().is_err());
        assert_eq!(
            meta.get_meta(&fence_key(&BlockHash::new([1; 32]), &rep))
                .unwrap(),
            parent.as_bytes()
        );
    }

    #[test]
    fn vote_fence_and_outside_final_reservation_cannot_both_commit() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        store
            .meta_store()
            .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &[1; 32])
            .unwrap();
        for index in 0..16 {
            let rep = WalletAddress::new(format!("brst_rep_{index}"));
            let barrier = std::sync::Barrier::new(2);
            let results = std::thread::scope(|scope| {
                let vote = scope.spawn(|| {
                    barrier.wait();
                    reserve(&store, &rep, 1, true).is_ok()
                });
                let fence = scope.spawn(|| {
                    barrier.wait();
                    let mut batch = store.tx_begin_write().unwrap();
                    if batch
                        .seal_consensus_votes(
                            &rep,
                            &BlockHash::new([1; 32]),
                            &BlockHash::new([9; 32]),
                            &HashSet::new(),
                        )
                        .is_err()
                    {
                        return false;
                    }
                    batch
                        .put_meta(&format!("test/journal/{index}"), b"saved")
                        .unwrap();
                    batch.commit().unwrap();
                    true
                });
                (vote.join().unwrap(), fence.join().unwrap())
            });
            assert_ne!(results.0, results.1);
            assert_eq!(
                store
                    .meta_store()
                    .get_meta(&format!("test/journal/{index}"))
                    .is_ok(),
                results.1
            );
        }
    }

    #[test]
    fn vote_fence_abort_and_corrupt_records_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        store
            .meta_store()
            .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &[1; 32])
            .unwrap();
        let rep = WalletAddress::new("brst_rep");
        let parent = BlockHash::new([9; 32]);
        {
            let mut batch = store.tx_begin_write().unwrap();
            batch
                .seal_consensus_votes(&rep, &BlockHash::new([1; 32]), &parent, &HashSet::new())
                .unwrap();
            // Dropping the uncommitted transaction discards the fence.
        }
        assert_eq!(reserve(&store, &rep, 1, true).unwrap(), 1);
        let meta = store.meta_store();
        meta.put_meta(
            &format!("consensus_final/v1/{rep}/brst_bad/{}", BlockHash::ZERO),
            &[1],
        )
        .unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch
            .seal_consensus_votes(
                &rep,
                &BlockHash::new([1; 32]),
                &parent,
                &HashSet::from([BlockHash::new([1; 32])])
            )
            .is_err());
        assert!(batch.commit().is_err());
        assert!(meta
            .get_meta(&fence_key(&BlockHash::new([1; 32]), &rep))
            .is_err());
        meta.put_meta(&fence_key(&BlockHash::new([1; 32]), &rep), &[1])
            .unwrap();
        assert!(reserve(&store, &rep, 1, false).is_err());
        assert!(reserve(&store, &rep, 1, true).is_err());
        assert_eq!(
            meta.get_meta(&fence_key(&BlockHash::new([1; 32]), &rep))
                .unwrap(),
            [1]
        );
        assert_eq!(
            meta.get_meta("consensus_sequence/v1/brst_rep").unwrap(),
            1u64.to_be_bytes()
        );
    }
}
