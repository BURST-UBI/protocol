use super::*;

/// One ordinary vote intent. Storage guards run in input order within one writer,
/// including promises installed by earlier entries. The caller validates the
/// candidate and representative eligibility before requesting a reservation.
pub struct ConsensusVoteReservation<'a> {
    pub payment_epoch: &'a BlockHash,
    pub representative: &'a WalletAddress,
    pub account: &'a WalletAddress,
    pub previous: &'a BlockHash,
    pub hash: &'a BlockHash,
    pub is_final: bool,
    pub sequence_floor: u64,
    pub timestamp: Option<u64>,
    pub governance_limit: Option<u64>,
}

impl LmdbMetaStore {
    pub const MAX_CONSENSUS_RESERVATIONS: usize = 256;

    /// Return sequences only after the whole batch commits and synchronization
    /// succeeds. Staging failures abort the batch. A later sync error can leave
    /// committed reservations, but returns no sequences for the caller to sign.
    pub fn reserve_consensus_votes(
        &self,
        votes: &[ConsensusVoteReservation<'_>],
    ) -> Result<Vec<u64>, StoreError> {
        if votes.len() > Self::MAX_CONSENSUS_RESERVATIONS {
            return Err(StoreError::Backend("consensus reservation batch exceeds limit".into()));
        }
        if votes.is_empty() { return Ok(Vec::new()); }
        let mut txn = self.env.write_txn().map_err(LmdbError::from)?;
        let mut observed = self.body_changes.begin();
        let mut sequences = Vec::with_capacity(votes.len());
        for vote in votes {
            sequences.push(self.stage_consensus_vote_reservation(&mut txn,
                vote.payment_epoch, vote.representative, vote.account, vote.previous,
                vote.hash, vote.is_final, vote.sequence_floor, vote.timestamp,
                vote.governance_limit, None, &mut observed)?);
        }
        self.commit_vote_reservation(txn, votes.iter().all(|vote| !vote.is_final), observed)?;
        self.env.sync_after_commit().map_err(LmdbError::from)?;
        Ok(sequences)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_reservations_commit_once_and_reopen_with_ordered_sequences() {
        let dir = tempfile::tempdir().unwrap();
        let epoch = BlockHash::new([1; 32]);
        let rep = WalletAddress::new("brst_group_rep");
        let account = WalletAddress::new("brst_group_account");
        let first = BlockHash::new([2; 32]);
        let second = BlockHash::new([3; 32]);
        let env = crate::LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let meta = env.meta_store();
        meta.put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, epoch.as_bytes()).unwrap();
        let request = |hash, previous, floor| ConsensusVoteReservation {
            payment_epoch: &epoch, representative: &rep, account: &account,
            previous, hash, is_final: true, sequence_floor: floor,
            timestamp: Some(100), governance_limit: None,
        };
        let mut observer = env.tx_begin_write().unwrap().watch_body_changes(16, 8192).unwrap();
        let generation = env.env().info().last_txn_id;
        let mut soft = request(&first, &BlockHash::ZERO, 4);
        soft.is_final = false;
        assert_eq!(meta.reserve_consensus_votes(&[
            soft, request(&first, &BlockHash::ZERO, 4), request(&second, &first, 4),
        ]).unwrap(), vec![5, 6, 7]);
        assert_eq!(env.env().info().last_txn_id, generation + 1);
        let mut observed = Vec::new();
        observer.visit(&env, |row| { observed.push(row.clone()); Ok(()) }).unwrap();
        let finals: Vec<_> = observed.iter().filter(|row| row.key.starts_with(b"consensus_final/")).collect();
        assert_eq!(finals.len(), 2);
        assert_eq!(finals[0].after.as_deref(), Some(first.as_bytes().as_slice()));
        assert_eq!(finals[1].after.as_deref(), Some(second.as_bytes().as_slice()));
        assert!(observed.iter().all(|row| !row.key.starts_with(b"consensus_sequence/")));
        drop(observer);
        meta.validate_reserved_final_vote(&epoch, &rep, &account, &BlockHash::ZERO,
            &first, 6, 100).unwrap();
        meta.validate_reserved_final_vote(&epoch, &rep, &account, &first,
            &second, 7, 100).unwrap();
        drop(meta);
        drop(env);
        let env = crate::LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let meta = env.meta_store();
        assert_eq!(meta.reserve_consensus_vote_at(&epoch, &rep, &account, &second,
            &BlockHash::new([4; 32]), false, 0, Some(100)).unwrap(), 8);
        assert!(meta.reserve_consensus_vote_at(&epoch, &rep, &account, &BlockHash::ZERO,
            &second, true, 0, Some(100)).is_err());
    }

    #[test]
    fn growth_pressure_aborts_vote_promises_and_sequences_before_retry() {
        let dir = tempfile::tempdir().unwrap();
        let env = crate::LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let meta = env.meta_store();
        let epoch = BlockHash::new([1; 32]);
        let rep = WalletAddress::new("brst_pressure_rep");
        let hash = BlockHash::new([2; 32]);
        meta.put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, epoch.as_bytes()).unwrap();
        let accounts: Vec<_> = (0..256).map(|n| WalletAddress::new(format!("brst_pressure_account_{n}"))).collect();
        let votes: Vec<_> = accounts.iter().map(|account| ConsensusVoteReservation {
            payment_epoch: &epoch, representative: &rep, account,
            previous: &BlockHash::ZERO, hash: &hash, is_final: true,
            sequence_floor: 0, timestamp: Some(100), governance_limit: Some(100),
        }).collect();
        let mut writer = env.tx_begin_write().unwrap();
        let limit = writer.limit_file_extent(env.env().real_disk_size().unwrap() as usize).unwrap();
        let watch = limit.watch();
        let mut observer = writer.watch_body_changes(2048, 1 << 20).unwrap();
        let reader = env.tx_begin_read().unwrap();
        drop(writer);
        let generation = env.env().info().last_txn_id;
        assert!(matches!(meta.reserve_consensus_votes(&votes), Err(StoreError::GrowthLimit(_))));
        assert!(watch.was_exceeded());
        assert_eq!(env.env().info().last_txn_id, generation);
        drop(reader); // Inspection helpers open their own same-thread read transactions.
        observer.visit(&env, |_| panic!("rejected reservation published observations")).unwrap();
        for account in &accounts {
            assert!(meta.validate_reserved_final_vote(&epoch, &rep, account,
                &BlockHash::ZERO, &hash, 1, 100).is_err());
        }
        drop(limit);
        assert!(watch.wait_released(std::time::Duration::ZERO));
        assert_eq!(meta.reserve_consensus_votes(&votes).unwrap(), (1..=256).collect::<Vec<u64>>());
        assert_eq!(env.env().info().last_txn_id, generation + 1);
        for (index, account) in accounts.iter().enumerate() {
            meta.validate_reserved_final_vote(&epoch, &rep, account,
                &BlockHash::ZERO, &hash, index as u64 + 1, 100).unwrap();
        }
        assert_eq!(env.env().info().map_size, 16 << 20);
    }

    #[test]
    fn grouped_conflict_or_governance_limit_aborts_every_reservation() {
        let dir = tempfile::tempdir().unwrap();
        let env = crate::LmdbEnvironment::open(dir.path(), 32, 16 << 20).unwrap();
        let meta = env.meta_store();
        let epoch = BlockHash::new([1; 32]);
        let rep = WalletAddress::new("brst_group_rep");
        let first = BlockHash::new([2; 32]);
        let second = BlockHash::new([3; 32]);
        meta.put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, epoch.as_bytes()).unwrap();
        let mut observer = env.tx_begin_write().unwrap().watch_body_changes(16, 8192).unwrap();
        let generation = env.env().info().last_txn_id;
        for conflict in [true, false] {
            let request = |hash, previous, timestamp, limit| ConsensusVoteReservation {
                payment_epoch: &epoch, representative: &rep, account: &rep,
                previous, hash, is_final: true, sequence_floor: 0,
                timestamp: Some(timestamp), governance_limit: limit,
            };
            assert!(meta.reserve_consensus_votes(&[
                request(&first, &BlockHash::ZERO, 100, Some(100)),
                request(&second, if conflict { &BlockHash::ZERO } else { &first },
                    if conflict { 100 } else { 101 }, None),
            ]).is_err());
            assert_eq!(env.env().info().last_txn_id, generation);
            observer.visit(&env, |_| panic!("failed vote batch published observations")).unwrap();
            assert!(meta.validate_reserved_final_vote(&epoch, &rep, &rep,
                &BlockHash::ZERO, &first, 1, 100).is_err());
        }
        assert!(meta.reserve_consensus_votes(&[]).unwrap().is_empty());
        let too_many: Vec<_> = (0..257).map(|_| ConsensusVoteReservation {
            payment_epoch: &epoch, representative: &rep, account: &rep,
            previous: &BlockHash::ZERO, hash: &first, is_final: false,
            sequence_floor: 0, timestamp: Some(100), governance_limit: None,
        }).collect();
        assert!(meta.reserve_consensus_votes(&too_many).is_err());
        assert_eq!(env.env().info().last_txn_id, generation);
        // A failed group leaves no promise, limit, or sequence for singleton fallback.
        assert_eq!(meta.reserve_consensus_vote_at(&epoch, &rep, &rep, &BlockHash::ZERO,
            &second, true, 0, Some(200)).unwrap(), 1);
    }
}
