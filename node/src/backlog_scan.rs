//! Rediscover accepted blocks after restart or election expiry, without restoring votes.
use burst_ledger::StateBlock;
use burst_store::StoreError;
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, WalletAddress};

#[derive(Default)]
pub(crate) struct BacklogScan {
    cursor: Option<WalletAddress>,
}

impl BacklogScan {
    /// Read at most limit accounts in one snapshot and return each account's
    /// first unconfirmed block. The limit counts accounts, not returned blocks
    /// or bytes. A successful short page resets the cursor; errors leave it
    /// unchanged. Each call opens a fresh snapshot.
    pub(crate) fn next(
        &mut self,
        store: &LmdbStore,
        limit: usize,
    ) -> Result<Vec<StateBlock>, StoreError> {
        let txn = store.tx_begin_read()?;
        let accounts =
            store
                .account_store()
                .accounts_page_txn(&txn, self.cursor.as_ref(), limit)?;
        let blocks = store.block_store();
        let mut pending = Vec::new();
        for account in &accounts {
            if account.confirmation_height > account.block_count {
                return Err(StoreError::Corruption(
                    "backlog confirmation exceeds account height".into(),
                ));
            }
            if account.confirmation_height == account.block_count {
                continue;
            }
            let height = account.confirmation_height + 1;
            let hash = blocks
                .block_at_height_txn(&txn, &account.address, height)?
                .ok_or_else(|| StoreError::Corruption("backlog block index missing".into()))?;
            let block: StateBlock = bincode::deserialize(&blocks.get_block_txn(&txn, &hash)?)
                .map_err(|error| StoreError::Serialization(error.to_string()))?;
            let previous = if height == 1 {
                BlockHash::ZERO
            } else {
                blocks
                    .block_at_height_txn(&txn, &account.address, height - 1)?
                    .ok_or_else(|| {
                        StoreError::Corruption("backlog predecessor index missing".into())
                    })?
            };
            if block.hash != hash
                || block.compute_hash() != hash
                || block.account != account.address
                || block.previous != previous
            {
                return Err(StoreError::Corruption(
                    "backlog candidate identity mismatch".into(),
                ));
            }
            pending.push(block);
        }
        self.cursor = if accounts.len() < limit {
            None
        } else {
            accounts.last().map(|account| account.address.clone())
        };
        Ok(pending)
    }
}

/// The caller holds the election writer through this fresh ledger read and
/// registration. Cementation retires roots under the same election writer.
/// The caller supplies a hash-validated block; this checks its current ledger
/// position. True means registration succeeded, including an existing candidate.
pub(crate) fn register_candidate(
    store: &LmdbStore,
    elections: &mut burst_consensus::ActiveElections,
    block: &StateBlock,
    now: burst_types::Timestamp,
) -> Result<bool, StoreError> {
    let txn = store.tx_begin_read()?;
    let blocks = store.block_store();
    let Some(height) = blocks.height_of_block_txn(&txn, &block.hash)? else {
        return Ok(false);
    };
    let account = store
        .account_store()
        .get_account_txn(&txn, &block.account)?;
    if height > account.block_count
        || account.confirmation_height > account.block_count
        || blocks.block_at_height_txn(&txn, &block.account, height)? != Some(block.hash)
    {
        return Err(StoreError::Corruption(
            "scheduler candidate account index mismatch".into(),
        ));
    }
    if height <= account.confirmation_height
        || store
            .meta_store()
            .confirmation_pending_txn(&txn, &block.hash)?
    {
        return Ok(false);
    }
    elections
        .register_candidate(crate::ledger_updater::election_root(block), block.hash, now)
        .map_err(|error| StoreError::Backend(error.to_string()))?;
    Ok(true)
}

/// Pop at most min(initial free election slots, 256) entries, counting stale
/// or corrupt entries against the limit. Failed entries are not requeued here.
/// Return successful registrations, which may include existing elections.
/// Release the scheduler lock before ledger reads or election-lock waits.
pub(crate) async fn drain_scheduler(
    store: &LmdbStore,
    scheduler: &tokio::sync::Mutex<burst_consensus::PriorityScheduler>,
    elections: &tokio::sync::RwLock<burst_consensus::ActiveElections>,
    max_elections: usize,
    now: burst_types::Timestamp,
) -> usize {
    use burst_store::block::BlockStore;
    let capacity = max_elections.saturating_sub(elections.read().await.election_count());
    let mut started = 0;
    for _ in 0..capacity.min(256) {
        let entry = scheduler.lock().await.pop();
        let Some((hash, _)) = entry else { break; };
        let block = store.block_store().get_block(&hash).ok()
            .and_then(|bytes| bincode::deserialize::<StateBlock>(&bytes).ok());
        let Some(block) = block.filter(|block| block.hash == hash && block.compute_hash() == hash) else {
            tracing::warn!(%hash, "scheduler candidate missing or corrupt");
            continue;
        };
        let mut elections = elections.write().await;
        match register_candidate(store, &mut elections, &block, now) {
            Ok(true) => started += 1,
            Ok(false) => {},
            Err(error) => tracing::warn!(%hash, %error, "scheduler candidate deferred"),
        }
    }
    started
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::{account::AccountStore, block::BlockStore};
    use burst_types::{NetworkId, ProtocolParams, Timestamp};

    fn chain(store: &LmdbStore, seed: u8) -> Vec<StateBlock> {
        let mut block = crate::genesis_state::block(NetworkId::Dev, &ProtocolParams::default());
        block.account =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[seed; 32]).public);
        block.representative = block.account.clone();
        let mut batch = store.tx_begin_write().unwrap();
        let mut blocks = Vec::new();
        for height in 1..=3 {
            block.timestamp = Timestamp::new(height);
            block.hash = block.compute_hash();
            batch
                .put_block_with_account(
                    &block.hash,
                    &bincode::serialize(&block).unwrap(),
                    &block.account,
                    height,
                )
                .unwrap();
            blocks.push(block.clone());
            block.previous = block.hash;
            block.block_type = burst_ledger::BlockType::ChangeRepresentative;
        }
        let mut account =
            crate::genesis_state::account_to_write(None, block.account, blocks[2].hash).unwrap();
        account.block_count = 3;
        account.confirmation_height = 1;
        batch.put_account_info(&account, false).unwrap();
        batch.commit().unwrap();
        blocks
    }

    #[tokio::test]
    async fn scheduler_drain_bounds_stale_work_and_preserves_next_page() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let blocks = chain(&store, 1);
        let mut queue = burst_consensus::PriorityScheduler::new(300);
        for i in 0..257u32 {
            let mut bytes = [0; 32];
            bytes[..4].copy_from_slice(&i.to_le_bytes());
            queue.push(BlockHash::new(bytes), blocks[0].account.clone(), 100);
        }
        queue.push(blocks[1].hash, blocks[1].account.clone(), 1);
        let queue = tokio::sync::Mutex::new(queue);
        let elections = tokio::sync::RwLock::new(burst_consensus::ActiveElections::new(5000, 1000));
        assert_eq!(drain_scheduler(&store, &queue, &elections, 5000, Timestamp::new(100)).await, 0);
        assert_eq!(queue.lock().await.len(), 2);
        assert_eq!(drain_scheduler(&store, &queue, &elections, 5000, Timestamp::new(100)).await, 1);
        assert!(queue.lock().await.is_empty());
        assert!(elections.read().await.candidate_root(&blocks[1].hash).is_some());
    }

    #[tokio::test]
    async fn waiting_for_election_writer_does_not_hold_scheduler_queue() {
        use std::{sync::Arc, time::Duration};
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap());
        let blocks = chain(&store, 1);
        let queue = Arc::new(tokio::sync::Mutex::new(burst_consensus::PriorityScheduler::new(4)));
        queue.lock().await.push(blocks[1].hash, blocks[1].account.clone(), 100);
        let elections = Arc::new(tokio::sync::RwLock::new(burst_consensus::ActiveElections::new(4, 1000)));
        let reader = elections.read().await;
        let task = tokio::spawn({
            let (store, queue, elections) = (store.clone(), queue.clone(), elections.clone());
            async move { drain_scheduler(&store, &queue, &elections, 4, Timestamp::new(100)).await }
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if queue.lock().await.is_empty() { break; }
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        assert!(!task.is_finished());
        queue.try_lock().expect("scheduler must remain available during election wait")
            .push(blocks[2].hash, blocks[2].account.clone(), 1);
        drop(reader);
        assert_eq!(tokio::time::timeout(Duration::from_secs(1), task).await.unwrap().unwrap(), 2);
    }

    #[test]
    fn bounded_scan_cycles_accounts_and_advances_only_after_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let mut chains = vec![chain(&store, 1), chain(&store, 2), chain(&store, 3)];
        chains.sort_by(|a, b| a[0].account.as_str().cmp(b[0].account.as_str()));
        let mut scan = BacklogScan::default();
        for blocks in &chains {
            let pending = scan.next(&store, 1).unwrap();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].hash, blocks[1].hash);
            assert_eq!(
                store
                    .account_store()
                    .get_account(&blocks[0].account)
                    .unwrap()
                    .confirmation_height,
                1
            );
        }
        assert!(scan.next(&store, 1).unwrap().is_empty());
        store
            .account_store()
            .advance_confirmation_height(&chains[0][0].account, 2)
            .unwrap();
        store
            .account_store()
            .advance_confirmation_height(&chains[1][0].account, 3)
            .unwrap();
        let pending = scan.next(&store, 10).unwrap();
        assert_eq!(
            pending.iter().map(|block| block.hash).collect::<Vec<_>>(),
            vec![chains[0][2].hash, chains[2][1].hash]
        );
    }

    #[test]
    fn saturated_tracking_cache_does_not_lose_durable_work_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut expected = Vec::new();
        {
            let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
            let mut cache = crate::bounded_backlog::BoundedBacklog::new(1);
            for seed in 1..=3 {
                let blocks = chain(&store, seed);
                let block = &blocks[1];
                if cache.can_accept(&block.account) {
                    cache.insert(block.hash, block.account.clone(), block.work, 0);
                }
                expected.push(block.hash);
            }
            assert_eq!(cache.len(), 1);
        }
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let mut scan = BacklogScan::default();
        let mut recovered: Vec<_> = scan
            .next(&store, 10)
            .unwrap()
            .into_iter()
            .map(|block| block.hash)
            .collect();
        expected.sort_by_key(|hash| hash.to_string());
        recovered.sort_by_key(|hash| hash.to_string());
        assert_eq!(recovered, expected);
        // Neither scanning nor loss of the cache advances confirmation.
        let mut again: Vec<_> = scan
            .next(&store, 10)
            .unwrap()
            .into_iter()
            .map(|block| block.hash)
            .collect();
        again.sort_by_key(|hash| hash.to_string());
        assert_eq!(again, expected);
    }

    #[test]
    fn corrupt_candidate_cannot_advance_the_scan_cursor_or_create_a_vote() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let blocks = chain(&store, 1);
        let mut corrupt = blocks[1].clone();
        corrupt.brn_spent += 1;
        store
            .block_store()
            .put_block(&blocks[1].hash, &bincode::serialize(&corrupt).unwrap())
            .unwrap();
        let mut scan = BacklogScan::default();
        assert!(scan.next(&store, 1).is_err());
        assert!(scan.cursor.is_none());
        store
            .block_store()
            .put_block(&blocks[1].hash, &bincode::serialize(&blocks[1]).unwrap())
            .unwrap();
        assert_eq!(scan.next(&store, 1).unwrap()[0].hash, blocks[1].hash);
    }

    #[test]
    fn stale_scan_cannot_restart_an_election_after_durable_handoff_or_cementation() {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let blocks = chain(&store, 1);
        let candidate = BacklogScan::default().next(&store, 10).unwrap().remove(0);
        let mut elections = burst_consensus::ActiveElections::new(10, 1);
        assert!(
            register_candidate(&store, &mut elections, &candidate, Timestamp::new(10)).unwrap()
        );
        elections.remove_root(&crate::ledger_updater::election_root(&candidate));
        store
            .meta_store()
            .enqueue_confirmation(&candidate.hash)
            .unwrap();
        assert!(
            !register_candidate(&store, &mut elections, &candidate, Timestamp::new(11)).unwrap()
        );
        assert_eq!(elections.election_count(), 0);

        let mut batch = store.tx_begin_write().unwrap();
        batch
            .advance_confirmation_height(&candidate.account, 2)
            .unwrap();
        batch.acknowledge_confirmation(&candidate.hash).unwrap();
        batch.commit().unwrap();
        assert!(
            !register_candidate(&store, &mut elections, &candidate, Timestamp::new(12)).unwrap()
        );
        assert_eq!(elections.election_count(), 0);
        assert!(
            register_candidate(&store, &mut elections, &blocks[2], Timestamp::new(12)).unwrap()
        );
        assert_eq!(elections.election_count(), 1);
    }
}
