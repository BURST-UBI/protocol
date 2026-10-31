//! Derived selection, published only with the writer's committed generation.
use super::*;
use burst_trst::{TrstEngine, WalletPortfolio};
use burst_utils::{Shared, SharedMap, SharedSet};

#[derive(Clone)]
pub(super) struct CounterSelection {
    wallets: SharedMap<WalletAddress, Shared<WalletPortfolio>>,
    dirty: SharedSet<WalletAddress>,
    deadlines: SharedSet<(Timestamp, WalletAddress)>,
    next: SharedMap<WalletAddress, Timestamp>,
    now: Timestamp,
    expiry_secs: u64,
}

impl AuxiliaryCache {
    pub(crate) fn mark_counter_account(&mut self, table: UndoTable, key: &[u8]) {
        if table != UndoTable::Accounts {
            return;
        }
        if let Some(cache) = self.expiry_counters.as_mut() {
            match std::str::from_utf8(key) {
                Ok(address) => {
                    cache.dirty.insert(WalletAddress::new(address));
                }
                Err(_) => self.expiry_counters = None,
            }
        }
    }

    pub(crate) fn clear_counter_accounts(&mut self, table: UndoTable) {
        if table == UndoTable::Accounts {
            self.expiry_counters = None;
        }
    }
}

impl LmdbWriteTransaction<'_> {
    /// Refresh only changed accounts/portfolios and crossed counter deadlines.
    /// A cold or invalidated writer generation, backward clock or changed expiry
    /// rule rebuilds selection from all portfolios. The return value counts selected
    /// account reads, excluding reads performed while updating those accounts.
    pub fn refresh_expiry_counters(
        &mut self,
        trst: &TrstEngine,
        now: Timestamp,
    ) -> Result<usize, StoreError> {
        let result = (|| {
            let previous = self.auxiliary_cache.get_mut().expiry_counters.take();
            let mut cache = previous
                .filter(|cache| now >= cache.now && cache.expiry_secs == trst.expiry_secs)
                .unwrap_or_else(|| CounterSelection {
                    wallets: SharedMap::default(),
                    dirty: SharedSet::default(),
                    deadlines: SharedSet::default(),
                    next: SharedMap::default(),
                    now,
                    expiry_secs: trst.expiry_secs,
                });
            let mut selected = std::mem::take(&mut cache.dirty);
            for holder in trst.wallets.changed_keys(&cache.wallets) {
                selected.insert(holder.clone());
            }
            for (_, holder) in cache.deadlines.iter().take_while(|(at, _)| *at <= now) {
                selected.insert(holder.clone());
            }
            let mut reads = 0;
            for holder in selected.iter() {
                if let Some(at) = cache.next.remove(holder) {
                    cache.deadlines.remove(&(at, holder.clone()));
                }
                if trst.get_portfolio(holder).is_none() {
                    continue;
                }
                let mut account = self.get_account(holder)?;
                reads += 1;
                let ((expired, revoked), next) = trst
                    .token_counter_window(holder, now)
                    .map_err(StoreError::Corruption)?;
                if (account.expired_trst, account.revoked_trst) != (expired, revoked) {
                    account.expired_trst = expired;
                    account.revoked_trst = revoked;
                    self.put_account_info(&account, false)?;
                }
                if let Some(at) = next {
                    cache.next.insert(holder.clone(), at);
                    cache.deadlines.insert((at, holder.clone()));
                }
            }
            cache.wallets = trst.wallets.clone();
            cache.now = now;
            self.auxiliary_cache.get_mut().expiry_counters = Some(cache);
            Ok(reads)
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
    use burst_store::account::AccountStore;
    use burst_types::TrstState;

    #[test]
    #[ignore = "explicit capped-host expiry selector scaling probe"]
    fn expiry_counter_selection_scaling() {
        use burst_store::MetaStore;
        use std::time::Instant;
        for population in [100usize, 1_000, 10_000, 100_000] {
            let dir = tempfile::tempdir().unwrap();
            let store = crate::LmdbStore::open(dir.path(), 32, 512 << 20).unwrap();
            let mut engine = TrstEngine::with_expiry(100);
            let mut batch = store.tx_begin_write().unwrap();
            for n in 0..population {
                let mut hash = [0; 32];
                hash[..8].copy_from_slice(&(n as u64).to_be_bytes());
                let token = engine
                    .mint(
                        TxHash::new(hash),
                        holder(n),
                        10,
                        holder(n),
                        Timestamp::new(10_000 + n as u64),
                    )
                    .unwrap();
                engine.track_token(token);
                batch
                    .put_account_info(&AccountInfo::unopened(holder(n)), false)
                    .unwrap();
            }
            batch.commit().unwrap();
            let rss_before = probe_rss_kib();
            let mut batch = store.tx_begin_write().unwrap();
            let start = Instant::now();
            let reads = batch
                .refresh_expiry_counters(&engine, Timestamp::new(1000))
                .unwrap();
            let cold_ns = start.elapsed().as_nanos();
            assert_eq!(reads, population);
            {
                let auxiliary = batch.auxiliary_cache.borrow();
                let cache = auxiliary.expiry_counters.as_ref().unwrap();
                assert_eq!(cache.deadlines.len(), population);
                assert_eq!(cache.next.len(), population);
            }
            batch.commit().unwrap();
            let rss_warm = probe_rss_kib();
            for sample in 0..5 {
                let mut batch = store.tx_begin_write().unwrap();
                let start = Instant::now();
                let idle_reads = batch
                    .refresh_expiry_counters(&engine, Timestamp::new(1000))
                    .unwrap();
                let idle_ns = start.elapsed().as_nanos();
                assert_eq!(idle_reads, 0);
                let mut account = batch.get_account(&holder(0)).unwrap();
                account.expired_trst = 999;
                let start = Instant::now();
                batch.put_account_info(&account, false).unwrap();
                let mutation_ns = start.elapsed().as_nanos();
                let start = Instant::now();
                let changed_reads = batch
                    .refresh_expiry_counters(&engine, Timestamp::new(1000))
                    .unwrap();
                let changed_ns = start.elapsed().as_nanos();
                assert_eq!(changed_reads, 1);
                assert_eq!(batch.get_account(&holder(0)).unwrap().expired_trst, 0);
                let commit_ns = if sample == 4 {
                    let start = Instant::now();
                    batch.commit().unwrap();
                    start.elapsed().as_nanos()
                } else {
                    drop(batch);
                    0
                };
                println!("expiry_scale kind=counter population={population} sample={sample} cold_ns={cold_ns} idle_ns={idle_ns} mutation_ns={mutation_ns} changed_ns={changed_ns} commit_ns={commit_ns} idle_reads=0 changed_reads=1 rss_before_kib={rss_before} rss_warm_kib={rss_warm}");
            }
            let mut batch = store.tx_begin_write().unwrap();
            let start = Instant::now();
            assert_eq!(
                batch
                    .refresh_expiry_counters(&engine, Timestamp::new(10_100))
                    .unwrap(),
                1
            );
            let due_ns = start.elapsed().as_nanos();
            batch.commit().unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            let start = Instant::now();
            assert_eq!(
                batch
                    .refresh_expiry_counters(&engine, Timestamp::new(1000))
                    .unwrap(),
                population
            );
            let backward_ns = start.elapsed().as_nanos();
            batch.commit().unwrap();
            store
                .meta_store()
                .put_meta("probe-external", b"invalidate")
                .unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            let start = Instant::now();
            assert_eq!(
                batch
                    .refresh_expiry_counters(&engine, Timestamp::new(1000))
                    .unwrap(),
                population
            );
            let invalidated_ns = start.elapsed().as_nanos();
            println!("expiry_scale_boundary kind=counter population={population} due_ns={due_ns} due_reads=1 backward_ns={backward_ns} invalidated_ns={invalidated_ns} invalidated_reads={population}");
        }
    }

    fn holder(n: usize) -> WalletAddress {
        WalletAddress::new(format!("brst_counter_selection_{n:04}"))
    }

    fn refresh(store: &crate::LmdbStore, engine: &TrstEngine, at: u64) -> usize {
        let mut batch = store.tx_begin_write().unwrap();
        let reads = batch
            .refresh_expiry_counters(engine, Timestamp::new(at))
            .unwrap();
        batch.commit().unwrap();
        reads
    }

    #[test]
    fn selection_tracks_deadlines_mutations_abort_external_writes_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut engine = TrstEngine::with_expiry(100);
        let mut batch = store.tx_begin_write().unwrap();
        for n in 0..300 {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&(n as u64).to_be_bytes());
            let mut token = engine
                .mint(
                    TxHash::new(bytes),
                    holder(n),
                    10,
                    holder(n),
                    Timestamp::new(1000 + n as u64),
                )
                .unwrap();
            // A stored Expired flag must not hide a future counter deadline.
            if n == 0 {
                token.state = TrstState::Expired;
            }
            engine.track_token(token);
            batch
                .put_account_info(&AccountInfo::unopened(holder(n)), false)
                .unwrap();
        }
        batch
            .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &[1; 32])
            .unwrap();
        batch.commit().unwrap();
        assert_eq!(refresh(&store, &engine, 1050), 300);
        reserve_test_votes(&store);
        assert_eq!(refresh(&store, &engine, 1051), 0);
        assert_eq!(refresh(&store, &engine, 1100), 1);
        assert_eq!(
            store
                .account_store()
                .get_account(&holder(0))
                .unwrap()
                .expired_trst,
            10
        );
        assert_eq!(refresh(&store, &engine, 1100), 0);
        {
            let mut batch = store.tx_begin_write().unwrap();
            let mut row = batch.get_account(&holder(7)).unwrap();
            row.expired_trst = 99;
            batch.put_account_info(&row, false).unwrap();
            assert_eq!(
                batch
                    .refresh_expiry_counters(&engine, Timestamp::new(1100))
                    .unwrap(),
                1
            );
            // Aborting must publish neither the row nor the derived selection.
        }
        assert_eq!(refresh(&store, &engine, 1100), 0);
        let mut batch = store.tx_begin_write().unwrap();
        let mut row = batch.get_account(&holder(7)).unwrap();
        row.expired_trst = 99;
        batch.put_account_info(&row, false).unwrap();
        batch.commit().unwrap();
        assert_eq!(refresh(&store, &engine, 1100), 1);
        assert_eq!(
            store
                .account_store()
                .get_account(&holder(7))
                .unwrap()
                .expired_trst,
            0
        );
        engine.get_portfolio_mut(&holder(8)).unwrap().tokens[0].amount = 23;
        assert_eq!(refresh(&store, &engine, 1100), 1);
        assert_eq!(refresh(&store, &engine, 1108), 8);
        assert_eq!(
            store
                .account_store()
                .get_account(&holder(8))
                .unwrap()
                .expired_trst,
            23
        );
        assert_eq!(refresh(&store, &engine, 1050), 300);
        assert_eq!(
            store
                .account_store()
                .get_account(&holder(8))
                .unwrap()
                .expired_trst,
            0
        );
        let mut external = store.account_store().get_account(&holder(9)).unwrap();
        external.revoked_trst = 999;
        store.account_store().put_account(&external).unwrap();
        reserve_test_votes(&store); // Must not bless the now-stale cache.
        assert_eq!(refresh(&store, &engine, 1050), 300);
        assert_eq!(
            store
                .account_store()
                .get_account(&holder(9))
                .unwrap()
                .revoked_trst,
            0
        );
        assert_eq!(refresh(&store, &engine, 1050), 0);
        engine.set_expiry_period(50, Timestamp::new(1050)).unwrap();
        assert_eq!(refresh(&store, &engine, 1050), 300);
        assert_eq!(
            store
                .account_store()
                .get_account(&holder(0))
                .unwrap()
                .expired_trst,
            10
        );
        drop(store);
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        assert_eq!(refresh(&store, &engine, 1050), 300);
        assert_eq!(refresh(&store, &engine, 1050), 0);
    }

    #[test]
    fn selection_invalidates_on_undo_checkpoint_clear_and_failed_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut engine = TrstEngine::with_expiry(100);
        let token = engine
            .mint(
                TxHash::new([1; 32]),
                holder(1),
                10,
                holder(1),
                Timestamp::new(1000),
            )
            .unwrap();
        engine.track_token(token);
        store
            .account_store()
            .put_account(&AccountInfo::unopened(holder(1)))
            .unwrap();
        assert_eq!(refresh(&store, &engine, 1000), 1);
        let hash = BlockHash::new([2; 32]);
        let mut batch = store.tx_begin_write().unwrap();
        batch.begin_block_undo(hash, holder(1), 1).unwrap();
        batch
            .put_block_with_account(&hash, b"counter-selection-block", &holder(1), 1)
            .unwrap();
        batch.put_frontier(&holder(1), &hash).unwrap();
        let mut row = batch.get_account(&holder(1)).unwrap();
        row.expired_trst = 10;
        row.head = hash;
        row.block_count = 1;
        batch.put_account_info(&row, false).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.rewind_block(&hash).unwrap();
        batch.commit().unwrap();
        assert_eq!(refresh(&store, &engine, 1000), 1);
        assert_eq!(refresh(&store, &engine, 1000), 0);
        {
            let mut batch = store.tx_begin_write().unwrap();
            batch
                .clear_checkpoint_rows(UndoTable::Accounts, None)
                .unwrap();
            assert!(batch
                .refresh_expiry_counters(&engine, Timestamp::new(1000))
                .is_err());
            assert!(batch.commit().is_err());
        }
        assert_eq!(refresh(&store, &engine, 1000), 0);
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .clear_checkpoint_rows(UndoTable::Accounts, None)
            .unwrap();
        batch
            .put_account_info(&AccountInfo::unopened(holder(1)), false)
            .unwrap();
        assert_eq!(
            batch
                .refresh_expiry_counters(&engine, Timestamp::new(1000))
                .unwrap(),
            1
        );
        batch.commit().unwrap();
        assert_eq!(refresh(&store, &engine, 1000), 0);
    }
}
