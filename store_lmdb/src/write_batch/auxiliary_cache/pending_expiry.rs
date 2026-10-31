//! Origin-time selection for pending expiry, scoped to a committed writer generation.
use super::*;
use burst_store::pending::PendingInfo;
use burst_utils::{SharedMap, SharedSet};

type PendingEntry = (WalletAddress, TxHash, PendingInfo);

#[derive(Clone, Default)]
pub(super) struct PendingSelection {
    origins: SharedSet<(Timestamp, Vec<u8>)>,
    by_key: SharedMap<Vec<u8>, Timestamp>,
    dirty: SharedSet<Vec<u8>>,
}

fn origin_time(info: &PendingInfo) -> Option<Timestamp> {
    if info.return_credit { None } else {
        info.provenance.first().map(|p| p.effective_origin_timestamp)
    }
}

impl PendingSelection {
    fn update(&mut self, key: Vec<u8>, info: Option<&PendingInfo>) {
        if let Some(at) = self.by_key.remove(&key) {
            self.origins.remove(&(at, key.clone()));
        }
        if let Some(at) = info.and_then(origin_time) {
            self.origins.insert((at, key.clone()));
            self.by_key.insert(key, at);
        }
    }
}

impl AuxiliaryCache {
    pub(crate) fn mark_pending_expiry(&mut self, table: UndoTable, key: &[u8]) {
        self.mark_pending_provenance(table, key);
        if table == UndoTable::Pending {
            if let Some(cache) = self.pending_expiry.as_mut() {
                cache.dirty.insert(key.to_vec());
            }
        }
    }

    pub(crate) fn clear_pending_expiry(&mut self, table: UndoTable) {
        if table == UndoTable::Pending {
            self.pending_expiry = None;
            self.pending_provenance = None;
        }
    }
}

impl LmdbWriteTransaction<'_> {
    /// Collect due ordinary claims in the same primary-key order as `all_pending`.
    /// Selection uses the first provenance entry's effective origin time; return
    /// credits and rows without provenance are excluded. A cold cache scans all
    /// pending rows. Warm reads refresh changed rows and decode every due claim.
    /// Results have no page limit, and this method does not remove claims.
    pub fn expired_pending(
        &mut self,
        expiry_secs: u64,
        now: Timestamp,
    ) -> Result<Vec<PendingEntry>, StoreError> {
        self.select_expired_pending(expiry_secs, now).map(|(entries, _)| entries)
    }

    fn select_expired_pending(
        &mut self,
        expiry_secs: u64,
        now: Timestamp,
    ) -> Result<(Vec<PendingEntry>, usize), StoreError> {
        let result = (|| {
            let previous = self.auxiliary_cache.get_mut().pending_expiry.take();
            let cold = previous.is_none();
            let mut cache = previous.unwrap_or_default();
            let mut decoded = 0;
            if cold {
                for row in self.env.pending_db.iter(&self.txn).map_err(LmdbError::from)? {
                    let (key, bytes) = row.map_err(LmdbError::from)?;
                    let (_, _, info) = crate::pending::decode_pending_entry(key, bytes)?;
                    decoded += 1;
                    cache.update(key.to_vec(), Some(&info));
                }
            } else {
                for key in std::mem::take(&mut cache.dirty).iter() {
                    let info = self.env.pending_db.get(&self.txn, key).map_err(LmdbError::from)?
                        .map(|bytes| crate::pending::decode_pending_entry(key, bytes))
                        .transpose()?;
                    decoded += usize::from(info.is_some());
                    cache.update(key.clone(), info.as_ref().map(|(_, _, info)| info));
                }
            }
            // has_expired uses saturating addition: at MAX every origin is due,
            // including those whose origin + governed duration would overflow.
            let cutoff = if now.as_secs() == u64::MAX { Some(u64::MAX) }
                else { now.as_secs().checked_sub(expiry_secs) };
            let mut keys = SharedSet::default();
            if let Some(cutoff) = cutoff {
                for (_, key) in cache.origins.iter().take_while(|(at, _)| at.as_secs() <= cutoff) {
                    keys.insert(key.clone());
                }
            }
            let mut entries = Vec::with_capacity(keys.len());
            for key in keys.iter() {
                let bytes = self.env.pending_db.get(&self.txn, key).map_err(LmdbError::from)?
                    .ok_or_else(|| StoreError::Corruption("pending expiry index has no primary row".into()))?;
                let entry = crate::pending::decode_pending_entry(key, bytes)?;
                decoded += 1;
                let at = origin_time(&entry.2);
                if at != cache.by_key.get(key).copied()
                    || !at.is_some_and(|at| at.has_expired(expiry_secs, now))
                {
                    return Err(StoreError::Corruption("pending expiry index disagrees with primary row".into()));
                }
                entries.push(entry);
            }
            self.auxiliary_cache.get_mut().pending_expiry = Some(cache);
            Ok((entries, decoded))
        })();
        if result.is_err() { self.failed = true; }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::{account::AccountStore, pending::{PendingProvenance, PendingStore}};
    use burst_types::TrstBasis;

    #[test]
    #[ignore = "explicit capped-host expiry selector scaling probe"]
    fn pending_expiry_selection_scaling() {
        use std::time::Instant;
        use burst_store::MetaStore;
        let selected_population = std::env::var("BURST_EXPIRY_PROBE_POPULATION")
            .ok().map(|value| value.parse::<usize>().unwrap());
        assert!(selected_population.is_none_or(|n| [100, 1_000, 10_000, 100_000].contains(&n)));
        for population in [100usize, 1_000, 10_000, 100_000] {
            if selected_population.is_some_and(|n| n != population) { continue; }
            let dir = tempfile::tempdir().unwrap();
            let store = crate::LmdbStore::open(dir.path(), 32, 512 << 20).unwrap();
            let address = |n: usize| WalletAddress::new(format!("brst_pending_scale_{n:08}"));
            let mut batch = store.tx_begin_write().unwrap();
            for n in 0..population {
                batch.put_pending(&address(n), hash(n as u64).as_bytes(),
                    &bincode::serialize(&info(10_000 + n as u64)).unwrap()).unwrap();
            }
            batch.commit().unwrap();
            let rss_before = probe_rss_kib();
            let mut batch = store.tx_begin_write().unwrap();
            let start = Instant::now();
            let (entries, decoded) = batch.select_expired_pending(100, Timestamp::new(1000)).unwrap();
            let cold_ns = start.elapsed().as_nanos();
            assert!(entries.is_empty());
            assert_eq!(decoded, population);
            {
                let auxiliary = batch.auxiliary_cache.borrow();
                let cache = auxiliary.pending_expiry.as_ref().unwrap();
                assert_eq!(cache.origins.len(), population);
                assert_eq!(cache.by_key.len(), population);
            }
            batch.commit().unwrap();
            let rss_warm = probe_rss_kib();
            for sample in 0..5 {
                let mut batch = store.tx_begin_write().unwrap();
                let start = Instant::now();
                let (entries, idle_decoded) = batch.select_expired_pending(100, Timestamp::new(1000)).unwrap();
                let idle_ns = start.elapsed().as_nanos();
                assert!(entries.is_empty());
                assert_eq!(idle_decoded, 0);
                let bytes = bincode::serialize(&info(0)).unwrap();
                let start = Instant::now();
                batch.put_pending(&address(0), hash(0).as_bytes(), &bytes).unwrap();
                let mutation_ns = start.elapsed().as_nanos();
                let start = Instant::now();
                let (entries, changed_decoded) = batch.select_expired_pending(100, Timestamp::new(1000)).unwrap();
                let changed_ns = start.elapsed().as_nanos();
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].0, address(0));
                assert_eq!(changed_decoded, 2);
                let commit_ns = if sample == 4 {
                    let start = Instant::now();
                    let [database_ns, auxiliary_ns, governance_ns] = probe_commit(batch);
                    let total_ns = start.elapsed().as_nanos();
                    println!("expiry_commit_stage population={population} sample={sample} database_ns={database_ns} auxiliary_ns={auxiliary_ns} governance_ns={governance_ns} total_ns={total_ns}");
                    total_ns
                } else { drop(batch); 0 };
                println!("expiry_scale kind=pending population={population} sample={sample} cold_ns={cold_ns} idle_ns={idle_ns} mutation_ns={mutation_ns} changed_ns={changed_ns} commit_ns={commit_ns} idle_decoded=0 changed_decoded=2 rss_before_kib={rss_before} rss_warm_kib={rss_warm}");
            }
            let mut batch = store.tx_begin_write().unwrap();
            batch.put_pending(&address(0), hash(0).as_bytes(), &bincode::serialize(&info(10_000)).unwrap()).unwrap();
            let (entries, decoded) = batch.select_expired_pending(100, Timestamp::new(1000)).unwrap();
            assert!(entries.is_empty());
            assert_eq!(decoded, 1);
            batch.commit().unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            let start = Instant::now();
            let (entries, decoded) = batch.select_expired_pending(100, Timestamp::new(10_100)).unwrap();
            let due_ns = start.elapsed().as_nanos();
            assert_eq!(entries.len(), 1);
            assert_eq!(decoded, 1);
            batch.commit().unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            let start = Instant::now();
            let (entries, decoded) = batch.select_expired_pending(100, Timestamp::new(1000)).unwrap();
            let backward_ns = start.elapsed().as_nanos();
            assert!(entries.is_empty());
            assert_eq!(decoded, 0);
            batch.commit().unwrap();
            // The sparse-due measurement above does not cover a deadline burst.
            // Measure selection before checking every returned primary row.
            let mut batch = store.tx_begin_write().unwrap();
            let start = Instant::now();
            let (entries, decoded) = batch.select_expired_pending(
                100, Timestamp::new(10_100 + population as u64),
            ).unwrap();
            let burst_ns = start.elapsed().as_nanos();
            let burst_rss = probe_rss_kib();
            assert_eq!(entries.len(), population);
            assert_eq!(decoded, population);
            for (n, (holder, tx, row)) in entries.iter().enumerate() {
                assert_eq!(*holder, address(n));
                assert_eq!(*tx, hash(n as u64));
                assert_eq!(bincode::serialize(row).unwrap(),
                    bincode::serialize(&info(10_000 + n as u64)).unwrap());
            }
            assert_eq!(entries.iter().map(|(_, _, row)| row.amount).sum::<u128>(),
                population as u128 * 10);
            drop(entries);
            batch.commit().unwrap();
            println!("expiry_scale_burst kind=pending population={population} burst_ns={burst_ns} decoded={decoded} rss_kib={burst_rss}");
            store.meta_store().put_meta("probe-external", b"invalidate").unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            let start = Instant::now();
            let (entries, decoded) = batch.select_expired_pending(100, Timestamp::new(1000)).unwrap();
            let invalidated_ns = start.elapsed().as_nanos();
            assert!(entries.is_empty());
            assert_eq!(decoded, population);
            println!("expiry_scale_boundary kind=pending population={population} due_ns={due_ns} due_decoded=1 backward_ns={backward_ns} invalidated_ns={invalidated_ns} invalidated_decoded={population}");
        }
    }

    fn destination(n: u64) -> WalletAddress {
        WalletAddress::new(format!("brst_pending_{:04}", 511 - n))
    }

    fn hash(n: u64) -> TxHash {
        let mut bytes = [0; 32];
        bytes[..8].copy_from_slice(&n.to_be_bytes());
        TxHash::new(bytes)
    }

    fn info(at: u64) -> PendingInfo {
        PendingInfo {
            source: WalletAddress::new("brst_source"),
            amount: 10,
            timestamp: Timestamp::new(2000),
            return_credit: false,
            provenance: vec![PendingProvenance {
                basis: TrstBasis::Single(hash(99)),
                amount: 10,
                origin: hash(99),
                origin_wallet: WalletAddress::new("brst_origin"),
                origin_timestamp: Timestamp::new(at),
                effective_origin_timestamp: Timestamp::new(at),
                revoked_amounts: Default::default(),
            }],
        }
    }

    fn compare(batch: &mut LmdbWriteTransaction<'_>, expiry_secs: u64, at: u64) -> usize {
        let now = Timestamp::new(at);
        let expected: Vec<_> = batch.all_pending().unwrap().into_iter()
            .filter(|(_, _, row)| !row.return_credit && row.provenance.first()
                .is_some_and(|p| p.effective_origin_timestamp.has_expired(expiry_secs, now)))
            .collect();
        let (actual, decoded) = batch.select_expired_pending(expiry_secs, now).unwrap();
        assert_eq!(bincode::serialize(&actual).unwrap(), bincode::serialize(&expected).unwrap());
        decoded
    }

    #[test]
    fn pending_expiry_selection_matches_scan_and_skips_unrelated_warm_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        for n in 0..512 {
            let mut row = info(1000 + n);
            if n == 0 { row.return_credit = true; }
            if n == 1 { row.provenance.clear(); }
            if n == 2 { row.provenance.push(info(0).provenance.remove(0)); }
            batch.put_pending(&destination(n), hash(n).as_bytes(), &bincode::serialize(&row).unwrap()).unwrap();
        }
        batch.put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &[1; 32]).unwrap();
        assert_eq!(compare(&mut batch, 100, 999), 512);
        batch.commit().unwrap();
        reserve_test_votes(&store);
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(compare(&mut batch, 100, 999), 0);
        for expiry_secs in [0, 100, u64::MAX] {
            for at in [0, 999, 1002, 1200, u64::MAX - 1, u64::MAX, 500] {
                compare(&mut batch, expiry_secs, at);
            }
        }
        batch.put_pending(&destination(7), hash(7).as_bytes(), &bincode::serialize(&info(0)).unwrap()).unwrap();
        assert_eq!(compare(&mut batch, 100, 999), 2); // one dirty row, one due row
        batch.commit().unwrap();
        {
            let mut batch = store.tx_begin_write().unwrap();
            batch.put_pending(&destination(8), hash(8).as_bytes(), &bincode::serialize(&info(0)).unwrap()).unwrap();
            assert_eq!(compare(&mut batch, 100, 999), 3);
            // Abort both primary mutation and updated index.
        }
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(compare(&mut batch, 100, 999), 1);
        batch.delete_pending(&destination(7), hash(7).as_bytes()).unwrap();
        assert_eq!(compare(&mut batch, 100, 999), 0);
        let mut credit = info(0);
        credit.return_credit = true;
        batch.put_pending(&destination(9), hash(9).as_bytes(), &bincode::serialize(&credit).unwrap()).unwrap();
        assert_eq!(compare(&mut batch, 100, 999), 1);
        batch.commit().unwrap();
        store.pending_store().put_pending(&destination(10), &hash(10), &info(0)).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(compare(&mut batch, 100, 999), 512); // 511 cold rows + one due
        batch.commit().unwrap();
        drop(store);
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(compare(&mut batch, 100, 999), 512);
        assert_eq!(compare(&mut batch, 100, 999), 1);
    }

    #[test]
    fn pending_expiry_undo_checkpoint_clear_and_corruption_are_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let owner = WalletAddress::new("brst_account");
        store.account_store().put_account(&AccountInfo::unopened(owner.clone())).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(compare(&mut batch, 100, 1000), 0);
        batch.commit().unwrap();
        let block = BlockHash::new([7; 32]);
        let mut batch = store.tx_begin_write().unwrap();
        batch.begin_block_undo(block, owner.clone(), 1).unwrap();
        batch.put_block_with_account(&block, b"pending-index-block", &owner, 1).unwrap();
        batch.put_frontier(&owner, &block).unwrap();
        let mut account = batch.get_account(&owner).unwrap();
        account.head = block;
        account.block_count = 1;
        batch.put_account_info(&account, false).unwrap();
        batch.put_pending(&destination(1), hash(1).as_bytes(), &bincode::serialize(&info(0)).unwrap()).unwrap();
        assert_eq!(compare(&mut batch, 100, 1000), 2);
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.rewind_block(&block).unwrap();
        assert_eq!(compare(&mut batch, 100, 1000), 0);
        batch.commit().unwrap();
        store.pending_store().put_pending(&destination(2), &hash(2), &info(0)).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(compare(&mut batch, 100, 1000), 2);
        batch.commit().unwrap();
        {
            let mut batch = store.tx_begin_write().unwrap();
            batch.clear_checkpoint_rows(UndoTable::Pending, None).unwrap();
            assert_eq!(compare(&mut batch, 100, 1000), 0);
        }
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(compare(&mut batch, 100, 1000), 1);
        batch.clear_checkpoint_rows(UndoTable::Pending, None).unwrap();
        batch.put_pending(&destination(3), hash(3).as_bytes(), &bincode::serialize(&info(900)).unwrap()).unwrap();
        assert_eq!(compare(&mut batch, 100, 1000), 2);
        batch.commit().unwrap();
        // An out-of-band corrupt primary write changes the writer generation.
        let mut batch = store.tx_begin_write().unwrap();
        batch.env.pending_db.put(&mut batch.txn, b"bad", b"corrupt").unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch.expired_pending(100, Timestamp::new(1000)).is_err());
        assert!(batch.commit().is_err());
    }
}
