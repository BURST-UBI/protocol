//! Normalized confirmed schedules and a generation-pinned counting-deadline index.
use super::*;
use crate::confirmed_governance::{
    Cached, Schedule, Snapshot, HEADER, KEY, MAX_BYTES, MAX_PROPOSALS, ROW_PREFIX,
};
use burst_utils::{SharedMap, SharedSet};

fn invalid(message: &str) -> StoreError {
    StoreError::Corruption(message.into())
}
fn size<T: serde::Serialize>(value: &T) -> Result<u64, StoreError> {
    bincode::serialized_size(value).map_err(|e| StoreError::Serialization(e.to_string()))
}
fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, StoreError> {
    bincode::serialize(value).map_err(|e| StoreError::Serialization(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, StoreError> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("confirmed governance exceeds byte limit"));
    }
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_BYTES)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|e| StoreError::Serialization(e.to_string()))
}
fn row_key(hash: &[u8; 32]) -> String {
    format!("{ROW_PREFIX}{}", BlockHash::new(*hash))
}
fn deadlines(snapshot: &Snapshot) -> Result<SharedSet<(u64, [u8; 32])>, StoreError> {
    let mut result = SharedSet::default();
    for (hash, proposal) in &snapshot.proposals {
        if let Some(at) = proposal.counting_limit(&snapshot.params)? {
            result.insert((at, *hash));
        }
    }
    Ok(result)
}
fn load(env: &LmdbEnvironment, txn: &heed::RoTxn<'_>) -> Result<Cached, StoreError> {
    let legacy = env
        .meta_db
        .get(txn, KEY.as_bytes())
        .map_err(LmdbError::from)?;
    let header = env
        .meta_db
        .get(txn, HEADER.as_bytes())
        .map_err(LmdbError::from)?;
    let mut rows = env.meta_db.prefix_iter(txn, b"confirmed_governance/").map_err(LmdbError::from)?
        .filter(|row| !matches!(row, Ok((key, _)) if *key == KEY.as_bytes() || *key == HEADER.as_bytes()));
    let Some(header) = header else {
        if rows.next().transpose().map_err(LmdbError::from)?.is_some() {
            return Err(invalid("orphaned confirmed governance rows"));
        }
        let bytes = legacy.ok_or_else(|| invalid("confirmed governance schedule is missing"))?;
        let snapshot = Snapshot::decode(bytes)?;
        return Ok(Cached {
            deadlines: deadlines(&snapshot)?,
            snapshot,
            logical_bytes: bytes.len() as u64,
            uses_row_storage: false,
        });
    };
    if legacy.is_some() {
        return Err(invalid("mixed confirmed governance formats"));
    }
    let (epoch, params, count, expected_bytes): (BlockHash, burst_types::ProtocolParams, u64, u64) =
        decode(header)?;
    if count > MAX_PROPOSALS as u64 || expected_bytes > MAX_BYTES {
        return Err(invalid("invalid confirmed governance header"));
    }
    let mut logical_bytes = size(&(epoch, &params, 0u64))?;
    let mut proposals = SharedMap::default();
    for row in rows {
        let (key, bytes) = row.map_err(LmdbError::from)?;
        let (hash, schedule): ([u8; 32], Schedule) = decode(bytes)?;
        if key != row_key(&hash).as_bytes() || proposals.len() as u64 >= count {
            return Err(invalid("invalid confirmed governance row identity/count"));
        }
        logical_bytes = logical_bytes
            .checked_add(bytes.len() as u64)
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(|| invalid("confirmed governance exceeds byte limit"))?;
        proposals.insert(hash, schedule);
    }
    if proposals.len() as u64 != count || logical_bytes != expected_bytes {
        return Err(invalid("confirmed governance manifest mismatch"));
    }
    let snapshot = Snapshot {
        epoch,
        params,
        proposals,
    };
    Ok(Cached {
        deadlines: deadlines(&snapshot)?,
        snapshot,
        logical_bytes,
        uses_row_storage: true,
    })
}

impl LmdbWriteTransaction<'_> {
    pub fn has_confirmed_governance_storage(&self) -> Result<bool, StoreError> {
        Ok(self
            .env
            .meta_db
            .prefix_iter(&self.txn, b"confirmed_governance/")
            .map_err(LmdbError::from)?
            .next()
            .transpose()
            .map_err(LmdbError::from)?
            .is_some())
    }

    pub fn read_confirmed_governance(&self) -> Result<Snapshot, StoreError> {
        let cached = match self.auxiliary_cache.borrow().confirmed_governance.clone() {
            Some(cached) => cached,
            None => load(self.env, &self.txn)?,
        };
        if cached.snapshot.epoch != self.payment_epoch()? {
            return Err(invalid("confirmed governance has an invalid epoch"));
        }
        if self
            .get_meta(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY)?
            .as_deref()
            != Some(cached.limit_bytes().as_slice())
        {
            return Err(invalid(
                "confirmed governance counting limit differs from schedule",
            ));
        }
        let snapshot = cached.snapshot.clone();
        self.auxiliary_cache.borrow_mut().confirmed_governance = Some(cached);
        Ok(snapshot)
    }

    /// Stage a caller-authenticated schedule and its derived counting deadline.
    /// The caller must install the matching payment epoch in this transaction.
    /// Preflight errors leave rows unchanged; errors after staging starts prevent
    /// commit. A provisional undo journal is also rejected and prevents commit.
    pub fn write_confirmed_governance(&mut self, snapshot: Snapshot) -> Result<(), StoreError> {
        if self.undo.is_some() {
            self.failed = true;
            return Err(invalid("confirmed governance cannot join provisional undo"));
        }
        if snapshot.proposals.len() > MAX_PROPOSALS {
            return Err(invalid("confirmed governance exceeds proposal limit"));
        }
        let previous = self.auxiliary_cache.borrow().confirmed_governance.clone();
        if previous.is_none() {
            // A full replacement clears known proposal rows, but must not leave
            // an unsupported namespace hidden underneath its newly valid cache.
            for row in self.checkpoint_metadata_entries(b"confirmed_governance/")? {
                let (key, _) = row?;
                if key != KEY.as_bytes()
                    && key != HEADER.as_bytes()
                    && !key.starts_with(ROW_PREFIX.as_bytes())
                {
                    return Err(invalid("unsupported confirmed governance namespace"));
                }
            }
        }
        let delta = previous
            .as_ref()
            .filter(|old| old.uses_row_storage && old.snapshot.params == snapshot.params);
        let keys: Vec<_> = match delta {
            Some(old) => snapshot
                .proposals
                .changed_keys(&old.snapshot.proposals)
                .copied()
                .collect(),
            None => snapshot.proposals.keys().copied().collect(),
        };
        let mut logical_bytes = match delta {
            Some(old) => old.logical_bytes,
            None => size(&(snapshot.epoch, &snapshot.params, 0u64))?,
        };
        let mut deadlines = delta.map_or_else(SharedSet::default, |old| old.deadlines.clone());
        let mut encoded = Vec::new();
        // Validate all deltas before writing either the projection or its vote fence.
        for hash in &keys {
            if let Some(old) = delta.and_then(|old| old.snapshot.proposals.get(hash)) {
                logical_bytes = logical_bytes
                    .checked_sub(size(&(hash, old))?)
                    .ok_or_else(|| invalid("confirmed governance size underflow"))?;
                if let Some(at) = old.counting_limit(&snapshot.params)? {
                    deadlines.remove(&(at, *hash));
                }
            }
        }
        for hash in &keys {
            let bytes = if let Some(row) = snapshot.proposals.get(hash) {
                let row_size = size(&(hash, row))?;
                logical_bytes = logical_bytes
                    .checked_add(row_size)
                    .ok_or_else(|| invalid("confirmed governance size overflow"))?;
                if logical_bytes > MAX_BYTES {
                    return Err(invalid("confirmed governance exceeds byte limit"));
                }
                if let Some(at) = row.counting_limit(&snapshot.params)? {
                    deadlines.insert((at, *hash));
                }
                Some(encode(&(hash, row))?)
            } else {
                None
            };
            encoded.push((row_key(hash), bytes));
        }
        if logical_bytes > MAX_BYTES {
            return Err(invalid("confirmed governance exceeds byte limit"));
        }
        let header = encode(&(
            snapshot.epoch,
            &snapshot.params,
            snapshot.proposals.len() as u64,
            logical_bytes,
        ))?;
        let cached = Cached {
            snapshot,
            logical_bytes,
            deadlines,
            uses_row_storage: true,
        };
        let limit = cached.limit_bytes();
        let result = (|| {
            if delta.is_none() {
                self.clear_checkpoint_rows(UndoTable::Meta, Some(ROW_PREFIX.as_bytes()))?;
            }
            for (key, bytes) in encoded {
                match bytes {
                    Some(bytes) => self.put_meta(&key, &bytes)?,
                    None => self.delete_meta(&key)?,
                }
            }
            self.delete_meta(KEY)?;
            self.put_meta(HEADER, &header)?;
            self.put_meta(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY, &limit)?;
            self.auxiliary_cache.get_mut().confirmed_governance = Some(cached);
            Ok(())
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
    use burst_store::MetaStore;

    fn fixture() -> (tempfile::TempDir, crate::LmdbStore, Snapshot) {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let params = burst_types::ProtocolParams {
            governance_proposal_endorsements: 1,
            ..burst_types::ProtocolParams::burst_defaults()
        };
        let schedule = Schedule {
            proposer: WalletAddress::new("brst_schedule"),
            created_at: 100,
            timing: burst_governance::GovernanceTiming {
                proposal_secs: 10,
                voting_secs: 20,
                buffer_secs: 1,
            },
            endorsements: 1,
            last_endorsement: Some(101),
            withdrawn_at: None,
            exploration_started_at: None,
        };
        let mut proposals = SharedMap::default();
        for n in 0u32..1024 {
            let mut hash = [0; 32];
            hash[..4].copy_from_slice(&n.to_be_bytes());
            proposals.insert(hash, schedule.clone());
        }
        let snapshot = Snapshot {
            epoch: BlockHash::new([1; 32]),
            params,
            proposals,
        };
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_meta(
                burst_store::meta::PAYMENT_EPOCH_KEY,
                snapshot.epoch.as_bytes(),
            )
            .unwrap();
        batch.commit().unwrap();
        (dir, store, snapshot)
    }

    #[test]
    fn confirmed_governance_rejects_unknown_namespaces_after_cached_and_cold_reads() {
        for normalized in [false, true] {
            let (_dir, store, snapshot) = fixture();
            let mut batch = store.tx_begin_write().unwrap();
            if normalized {
                batch.write_confirmed_governance(snapshot.clone()).unwrap();
            } else {
                batch.put_meta(KEY, &encode(&snapshot).unwrap()).unwrap();
                batch
                    .put_meta(
                        burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY,
                        &snapshot.limit_bytes().unwrap(),
                    )
                    .unwrap();
            }
            batch.commit().unwrap();
            let unknown = encode(&([9u8; 32], snapshot.proposals.get(&[0; 32]).unwrap())).unwrap();
            for key in [
                "confirmed_governance/v4/unknown",
                "confirmed_governance/v3/unknown",
                "confirmed_governance/v1",
                "confirmed_governance/v2/unknown",
            ] {
                let mut batch = store.tx_begin_write().unwrap();
                assert_eq!(batch.read_confirmed_governance().unwrap(), snapshot);
                batch.put_meta(key, &unknown).unwrap();
                assert!(batch.read_confirmed_governance().is_err(), "cached {key}");
                let before = batch.checkpoint_metadata(b"confirmed_governance/").unwrap();
                assert!(
                    batch.write_confirmed_governance(snapshot.clone()).is_err(),
                    "replacement {key}"
                );
                assert_eq!(
                    batch.checkpoint_metadata(b"confirmed_governance/").unwrap(),
                    before
                );
                assert!(batch.read_confirmed_governance().is_err());
                drop(batch);
                assert_eq!(
                    store
                        .tx_begin_write()
                        .unwrap()
                        .read_confirmed_governance()
                        .unwrap(),
                    snapshot
                );
                store.meta_store().put_meta(key, &unknown).unwrap();
                let mut batch = store.tx_begin_write().unwrap();
                assert!(batch.read_confirmed_governance().is_err(), "cold {key}");
                assert!(batch.write_confirmed_governance(snapshot.clone()).is_err());
                drop(batch);
                store.meta_store().delete_meta(key).unwrap();
                assert_eq!(
                    store
                        .tx_begin_write()
                        .unwrap()
                        .read_confirmed_governance()
                        .unwrap(),
                    snapshot
                );
            }
            store
                .meta_store()
                .put_meta("confirmed_governance-other", b"unrelated")
                .unwrap();
            assert_eq!(
                store
                    .tx_begin_write()
                    .unwrap()
                    .read_confirmed_governance()
                    .unwrap(),
                snapshot
            );
        }
    }

    #[test]
    fn confirmed_governance_rows_convert_and_update_only_changed_proposals() {
        let (dir, store, snapshot) = fixture();
        let legacy: std::collections::BTreeMap<_, _> = snapshot
            .proposals
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        let bytes = encode(&(snapshot.epoch, &snapshot.params, legacy)).unwrap();
        assert_eq!(bytes, encode(&snapshot).unwrap());
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(KEY, &bytes).unwrap();
        batch
            .put_meta(
                burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY,
                &snapshot.limit_bytes().unwrap(),
            )
            .unwrap();
        batch.commit().unwrap();
        for commit in [false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            let loaded = batch.read_confirmed_governance().unwrap();
            assert_eq!(loaded, snapshot);
            batch.write_confirmed_governance(loaded).unwrap();
            assert!(batch.get_meta(KEY).unwrap().is_none());
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
                assert_eq!(store.meta_store().get_meta(KEY).unwrap(), bytes);
            }
        }
        let mut batch = store.tx_begin_write().unwrap();
        let before = batch.checkpoint_metadata(ROW_PREFIX.as_bytes()).unwrap();
        let mut changed = batch.read_confirmed_governance().unwrap();
        changed.proposals.get_mut(&[0; 32]).unwrap().withdrawn_at = Some(102);
        batch.write_confirmed_governance(changed.clone()).unwrap();
        let after = batch.checkpoint_metadata(ROW_PREFIX.as_bytes()).unwrap();
        assert_eq!(before.iter().zip(&after).filter(|(a, b)| a != b).count(), 1);
        assert_eq!(
            batch
                .get_meta(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY)
                .unwrap()
                .unwrap(),
            snapshot.limit_bytes().unwrap()
        );
        assert_eq!(
            batch
                .auxiliary_cache
                .borrow()
                .confirmed_governance
                .as_ref()
                .unwrap()
                .deadlines
                .len(),
            1023
        );
        batch.commit().unwrap();
        assert!(store
            .tx_begin_write()
            .unwrap()
            .read_confirmed_governance()
            .unwrap()
            .proposals
            .shares_storage_with(&changed.proposals));
        drop(store);
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.read_confirmed_governance().unwrap(), changed);
    }

    #[test]
    fn confirmed_governance_rows_validate_epoch_fence_manifest_and_abort() {
        let (_dir, store, snapshot) = fixture();
        let mut batch = store.tx_begin_write().unwrap();
        batch.write_confirmed_governance(snapshot.clone()).unwrap();
        batch.commit().unwrap();
        for key in [
            KEY,
            HEADER,
            burst_store::meta::PAYMENT_EPOCH_KEY,
            burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY,
        ] {
            let mut batch = store.tx_begin_write().unwrap();
            batch.put_meta(key, b"corrupt").unwrap();
            assert!(batch.read_confirmed_governance().is_err(), "{key}");
            drop(batch);
            assert_eq!(
                store
                    .tx_begin_write()
                    .unwrap()
                    .read_confirmed_governance()
                    .unwrap(),
                snapshot
            );
        }
        let mut batch = store.tx_begin_write().unwrap();
        batch.delete_meta(HEADER).unwrap();
        assert!(batch.read_confirmed_governance().is_err());
        drop(batch);
        for (count, bytes) in [
            (1023, size(&snapshot).unwrap()),
            (1024, 1),
            (MAX_PROPOSALS as u64 + 1, 1),
            (1024, MAX_BYTES + 1),
        ] {
            let mut batch = store.tx_begin_write().unwrap();
            batch
                .put_meta(
                    HEADER,
                    &encode(&(snapshot.epoch, &snapshot.params, count, bytes)).unwrap(),
                )
                .unwrap();
            assert!(batch.read_confirmed_governance().is_err());
        }
        let mut batch = store.tx_begin_write().unwrap();
        let mut invalid_snapshot = batch.read_confirmed_governance().unwrap();
        invalid_snapshot
            .proposals
            .get_mut(&[0; 32])
            .unwrap()
            .timing
            .voting_secs = u64::MAX;
        assert!(batch.write_confirmed_governance(invalid_snapshot).is_err());
        batch.commit().unwrap();
        assert_eq!(
            store
                .tx_begin_write()
                .unwrap()
                .read_confirmed_governance()
                .unwrap(),
            snapshot
        );
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .begin_block_undo(
                BlockHash::new([2; 32]),
                WalletAddress::new("brst_schedule"),
                1,
            )
            .unwrap();
        assert!(batch.write_confirmed_governance(snapshot.clone()).is_err());
        assert!(batch.commit().is_err());
        store.meta_store().put_meta(HEADER, b"corrupt").unwrap();
        assert!(store
            .tx_begin_write()
            .unwrap()
            .read_confirmed_governance()
            .is_err());
    }

    #[test]
    fn confirmed_governance_deadlines_follow_deletion_and_global_rule_changes() {
        let (_dir, store, mut snapshot) = fixture();
        snapshot.proposals.get_mut(&[0; 32]).unwrap().created_at = 90;
        let mut batch = store.tx_begin_write().unwrap();
        batch.write_confirmed_governance(snapshot.clone()).unwrap();
        batch.commit().unwrap();
        for commit in [false, true] {
            let mut batch = store.tx_begin_write().unwrap();
            let mut changed = batch.read_confirmed_governance().unwrap();
            changed.proposals.remove(&[0; 32]);
            batch.write_confirmed_governance(changed.clone()).unwrap();
            assert_eq!(
                batch
                    .get_meta(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY)
                    .unwrap()
                    .unwrap(),
                changed.limit_bytes().unwrap()
            );
            if commit {
                batch.commit().unwrap();
            } else {
                drop(batch);
                assert_eq!(
                    store
                        .tx_begin_write()
                        .unwrap()
                        .read_confirmed_governance()
                        .unwrap(),
                    snapshot
                );
            }
        }
        let mut batch = store.tx_begin_write().unwrap();
        let mut changed = batch.read_confirmed_governance().unwrap();
        changed.proposals.clear();
        batch.write_confirmed_governance(changed.clone()).unwrap();
        assert!(batch
            .checkpoint_metadata(ROW_PREFIX.as_bytes())
            .unwrap()
            .is_empty());
        assert_eq!(batch.read_confirmed_governance().unwrap(), changed);
        changed.params.governance_proposal_endorsements = 0;
        let mut proposal = snapshot.proposals[&[0; 32]].clone();
        proposal.endorsements = 0;
        changed.proposals.insert([3; 32], proposal);
        batch.write_confirmed_governance(changed.clone()).unwrap();
        assert_eq!(batch.read_confirmed_governance().unwrap(), changed);
        assert_eq!(
            batch
                .get_meta(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY)
                .unwrap()
                .unwrap(),
            changed.limit_bytes().unwrap()
        );
        batch.clear_checkpoint_rows(UndoTable::Meta, None).unwrap();
        assert!(batch.read_confirmed_governance().is_err());
    }
}

impl crate::read_view::LedgerReadView<'_> {
    pub fn read_confirmed_governance(&self) -> Result<Snapshot, StoreError> {
        let cached = load(self.env, self.txn)?;
        if cached.snapshot.epoch != self.payment_epoch()? {
            return Err(invalid("confirmed governance has an invalid epoch"));
        }
        if self
            .get_meta(burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY)?
            .as_deref()
            != Some(cached.limit_bytes().as_slice())
        {
            return Err(invalid(
                "confirmed governance counting limit differs from schedule",
            ));
        }
        Ok(cached.snapshot)
    }
}
