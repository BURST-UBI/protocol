//! LMDB implementation of MetaStore.

use crate::shared_env::SharedEnv;

use heed::types::Bytes;
use heed::Database;

use burst_store::meta::MetaStore;
use burst_store::StoreError;
use burst_types::{BlockHash, WalletAddress};

use crate::LmdbError;

const SCHEMA_VERSION_KEY: &[u8] = b"schema_version";

pub struct LmdbMetaStore {
    pub(crate) env: SharedEnv,
    pub(crate) meta_db: Database<Bytes, Bytes>,
}

impl LmdbMetaStore {
    /// Visit borrowed rows in one pinned read view. Bounds are checked before
    /// invoking the callback; callback errors stop traversal immediately.
    pub fn visit_prefix_bounded_txn(
        &self,
        txn: &crate::LmdbReadTransaction<'_>,
        prefix: &str,
        max_rows: usize,
        max_bytes: usize,
        mut visit: impl FnMut(&str, &[u8]) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let mut used = 0usize;
        for (count, entry) in self
            .meta_db
            .prefix_iter(&txn.txn, prefix.as_bytes())
            .map_err(LmdbError::from)?
            .enumerate()
        {
            let (key, bytes) = entry.map_err(LmdbError::from)?;
            used = used
                .checked_add(key.len())
                .and_then(|size| size.checked_add(bytes.len()))
                .filter(|size| *size <= max_bytes && count < max_rows)
                .ok_or_else(|| StoreError::Corruption("metadata scan exceeds limit".into()))?;
            let key = std::str::from_utf8(key)
                .map_err(|error| StoreError::Corruption(error.to_string()))?;
            visit(key, bytes)?;
        }
        Ok(())
    }

    pub fn prefix_page_txn(
        &self,
        txn: &crate::LmdbReadTransaction<'_>,
        prefix: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        self.prefix_page_bounded_txn(txn, prefix, cursor, limit, usize::MAX)
    }

    /// Bound copied key/value bytes as well as row count, within one read view.
    /// A row that exceeds the budget rejects the page before it is copied.
    pub fn prefix_page_bounded_txn(
        &self,
        txn: &crate::LmdbReadTransaction<'_>,
        prefix: &str,
        cursor: Option<&str>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        use std::ops::Bound;
        if limit > 101 || cursor.is_some_and(|key| !key.starts_with(prefix)) {
            return Err(StoreError::Serialization("invalid metadata page".into()));
        }
        let lower = cursor.map_or(Bound::Included(prefix.as_bytes()), |key| {
            Bound::Excluded(key.as_bytes())
        });
        let bounds = (lower, Bound::<&[u8]>::Unbounded);
        let mut rows = Vec::new();
        let mut used = 0usize;
        for entry in self
            .meta_db
            .range(&txn.txn, &bounds)
            .map_err(LmdbError::from)?
            .take(limit)
        {
            let (key, bytes) = entry.map_err(LmdbError::from)?;
            if !key.starts_with(prefix.as_bytes()) {
                break;
            }
            used = used
                .checked_add(key.len())
                .and_then(|size| size.checked_add(bytes.len()))
                .filter(|size| *size <= max_bytes)
                .ok_or_else(|| StoreError::Corruption("metadata page exceeds byte limit".into()))?;
            rows.push((
                String::from_utf8(key.to_vec())
                    .map_err(|e| StoreError::Corruption(e.to_string()))?,
                bytes.to_vec(),
            ));
        }
        Ok(rows)
    }

    pub fn get_meta_txn(
        &self,
        txn: &crate::LmdbReadTransaction<'_>,
        key: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        self.meta_db
            .get(&txn.txn, key.as_bytes())
            .map(|bytes| bytes.map(<[u8]>::to_vec))
            .map_err(LmdbError::from)
            .map_err(Into::into)
    }

    /// Reserve one checkpoint per signer and height, across ALL parents. Keying
    /// by parent would allow two conflicting branches at the same height.
    /// Call only after validating the parent, ledger cut and future-round rules.
    /// There is no timeout unlock: safe view changes need a separate protocol.
    pub fn reserve_checkpoint_vote(
        &self,
        network: burst_types::NetworkId,
        genesis: &BlockHash,
        representative: &WalletAddress,
        height: u64,
        checkpoint_hash: &BlockHash,
    ) -> Result<(), StoreError> {
        if height == 0 || *genesis == BlockHash::ZERO || *checkpoint_hash == BlockHash::ZERO {
            return Err(StoreError::Serialization(
                "invalid checkpoint vote identity".into(),
            ));
        }
        let key = format!(
            "checkpoint_vote/v1/{}/{genesis}/{representative}/{height}",
            network.as_str()
        );
        let mut txn = self.env.write_txn().map_err(LmdbError::from)?;
        if let Some(choice) = self
            .meta_db
            .get(&txn, key.as_bytes())
            .map_err(LmdbError::from)?
        {
            if choice.len() != 32 {
                return Err(StoreError::Corruption(
                    "invalid durable checkpoint vote".into(),
                ));
            }
            if choice != checkpoint_hash.as_bytes() {
                return Err(StoreError::Duplicate("conflicting checkpoint vote".into()));
            }
        } else {
            self.meta_db
                .put(&mut txn, key.as_bytes(), checkpoint_hash.as_bytes())
                .map_err(LmdbError::from)?;
        }
        txn.commit().map_err(LmdbError::from)?;
        self.env.force_sync().map_err(LmdbError::from)?;
        Ok(())
    }

    /// Reserve a sequence and, for final votes, an irrevocable choice before
    /// signing. The account qualifies zero predecessors (opening blocks).
    /// Neither a restart nor a later soft vote may contradict a choice in the
    /// same epoch. Only a certified handoff can close that context.
    #[allow(clippy::too_many_arguments)]
    pub fn reserve_consensus_vote(
        &self,
        payment_epoch: &BlockHash,
        representative: &WalletAddress,
        account: &WalletAddress,
        previous: &BlockHash,
        hash: &BlockHash,
        is_final: bool,
        sequence_floor: u64,
    ) -> Result<u64, StoreError> {
        self.reserve_consensus_vote_at(
            payment_epoch,
            representative,
            account,
            previous,
            hash,
            is_final,
            sequence_floor,
            None,
        )
    }

    /// Check the certified timestamp limit inside the same writer transaction
    /// as the irrevocable choice. Missing timestamps cannot bypass a limit.
    #[allow(clippy::too_many_arguments)]
    pub fn reserve_consensus_vote_at(
        &self,
        payment_epoch: &BlockHash,
        representative: &WalletAddress,
        account: &WalletAddress,
        previous: &BlockHash,
        hash: &BlockHash,
        is_final: bool,
        sequence_floor: u64,
        timestamp: Option<u64>,
    ) -> Result<u64, StoreError> {
        self.reserve_consensus_vote_with_governance_limit(
            payment_epoch,
            representative,
            account,
            previous,
            hash,
            is_final,
            sequence_floor,
            timestamp,
            None,
        )
    }

    /// A final governance promise installs its deadline atomically with its
    /// choice. A previously promised later timestamp makes that promise invalid.
    #[allow(clippy::too_many_arguments)]
    pub fn reserve_consensus_vote_with_governance_limit(
        &self,
        payment_epoch: &BlockHash,
        representative: &WalletAddress,
        account: &WalletAddress,
        previous: &BlockHash,
        hash: &BlockHash,
        is_final: bool,
        sequence_floor: u64,
        timestamp: Option<u64>,
        governance_limit: Option<u64>,
    ) -> Result<u64, StoreError> {
        let sequence_key = format!("consensus_sequence/v1/{representative}");
        let final_key = crate::write_batch::vote_fence::final_key(
            payment_epoch,
            representative,
            account,
            previous,
        );
        let mut txn = self.env.write_txn().map_err(LmdbError::from)?;
        let installed = self
            .meta_db
            .get(&txn, burst_store::meta::PAYMENT_EPOCH_KEY.as_bytes())
            .map_err(LmdbError::from)?
            .ok_or_else(|| StoreError::Corruption("payment epoch is not initialized".into()))?;
        if burst_store::meta::decode_payment_epoch(installed)? != *payment_epoch {
            return Err(StoreError::Corruption(
                "vote payment epoch is not installed".into(),
            ));
        }
        let time_key = format!("consensus_time/v1/{payment_epoch}/{representative}");
        let limit_key = format!("consensus_governance/v1/{payment_epoch}/{representative}");
        let read_time = |key: &str| -> Result<Option<u64>, StoreError> {
            self.meta_db
                .get(&txn, key.as_bytes())
                .map_err(LmdbError::from)?
                .map(|bytes| {
                    bytes.try_into().map(u64::from_be_bytes).map_err(|_| {
                        StoreError::Corruption("invalid durable vote timestamp".into())
                    })
                })
                .transpose()
        };
        let previous_time = read_time(&time_key)?;
        let previous_limit = read_time(&limit_key)?;
        if governance_limit.is_some() && (!is_final || timestamp.is_none()) {
            return Err(StoreError::Corruption(
                "governance deadline requires a timestamped final promise".into(),
            ));
        }
        if governance_limit.is_some_and(|limit| previous_time.is_some_and(|at| at > limit)) {
            return Err(StoreError::Backend(
                "governance deadline precedes an earlier final promise".into(),
            ));
        }
        let next_limit = previous_limit.into_iter().chain(governance_limit).min();
        if next_limit.is_some_and(|limit| timestamp.is_none_or(|at| at > limit)) {
            return Err(StoreError::Backend(
                "ordinary vote crosses a promised governance deadline".into(),
            ));
        }
        if is_final
            && timestamp.is_none()
            && self
                .meta_db
                .get(
                    &txn,
                    burst_store::meta::CERTIFIED_PAYMENT_AUTHORITY_KEY.as_bytes(),
                )
                .map_err(LmdbError::from)?
                .is_some()
        {
            return Err(StoreError::Corruption(
                "certified final promise requires its timestamp".into(),
            ));
        }
        if is_final && timestamp.is_some() && previous_time.is_none() {
            let prefix =
                crate::write_batch::vote_fence::final_prefix(payment_epoch, representative);
            if self
                .meta_db
                .prefix_iter(&txn, prefix.as_bytes())
                .map_err(LmdbError::from)?
                .next()
                .transpose()
                .map_err(LmdbError::from)?
                .is_some()
            {
                return Err(StoreError::Corruption(
                    "existing final promises have no timestamp high-water mark".into(),
                ));
            }
        }
        for (key, message) in [
            (
                burst_store::meta::PAYMENT_VOTE_LIMIT_KEY,
                "ordinary vote crosses certified governance activation",
            ),
            (
                burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY,
                "ordinary vote crosses confirmed governance counting boundary",
            ),
        ] {
            match self
                .meta_db
                .get(&txn, key.as_bytes())
                .map_err(LmdbError::from)?
            {
                Some(bytes) => {
                    if let Some(limit) =
                        burst_store::meta::decode_payment_vote_limit(bytes, *payment_epoch)?
                    {
                        if timestamp.is_none_or(|timestamp| timestamp > limit) {
                            return Err(StoreError::Backend(message.into()));
                        }
                    }
                }
                None if self
                    .meta_db
                    .get(
                        &txn,
                        burst_store::meta::CERTIFIED_PAYMENT_AUTHORITY_KEY.as_bytes(),
                    )
                    .map_err(LmdbError::from)?
                    .is_some() =>
                {
                    return Err(StoreError::Corruption(
                        "certified payment vote limit is missing".into(),
                    ));
                }
                None => {}
            }
        }
        if self
            .meta_db
            .get(
                &txn,
                format!("consensus_final/v1/{representative}/{account}/{previous}").as_bytes(),
            )
            .map_err(LmdbError::from)?
            .is_some()
        {
            return Err(StoreError::Corruption(
                "legacy final vote requires explicit reconstruction".into(),
            ));
        }
        if self
            .meta_db
            .get(
                &txn,
                crate::write_batch::payment_epoch::closed_key(payment_epoch).as_bytes(),
            )
            .map_err(LmdbError::from)?
            .is_some()
        {
            return Err(StoreError::Corruption(
                "installed payment epoch is already closed".into(),
            ));
        }
        if self
            .meta_db
            .get(
                &txn,
                format!("consensus_fence/v1/{representative}").as_bytes(),
            )
            .map_err(LmdbError::from)?
            .is_some()
        {
            return Err(StoreError::Corruption(
                "legacy consensus fence requires explicit reconstruction".into(),
            ));
        }
        let fence_key = crate::write_batch::vote_fence::fence_key(payment_epoch, representative);
        if let Some(fence) = self
            .meta_db
            .get(&txn, fence_key.as_bytes())
            .map_err(LmdbError::from)?
        {
            if fence.len() != 32 || fence.iter().all(|byte| *byte == 0) {
                return Err(StoreError::Corruption(
                    "invalid consensus vote fence".into(),
                ));
            }
            return Err(StoreError::Backend(
                "ordinary votes are sealed pending certified payment-epoch installation".into(),
            ));
        }
        if let Some(choice) = self
            .meta_db
            .get(&txn, final_key.as_bytes())
            .map_err(LmdbError::from)?
        {
            if choice.len() != 32 {
                return Err(StoreError::Corruption("invalid durable final vote".into()));
            }
            if choice != hash.as_bytes() {
                return Err(StoreError::Duplicate(format!(
                    "conflicting vote after final choice for {account}/{previous}"
                )));
            }
        }
        let previous_sequence = match self
            .meta_db
            .get(&txn, sequence_key.as_bytes())
            .map_err(LmdbError::from)?
        {
            Some(bytes) => u64::from_be_bytes(
                bytes
                    .try_into()
                    .map_err(|_| StoreError::Corruption("invalid durable vote sequence".into()))?,
            ),
            None => 0,
        };
        let sequence = previous_sequence
            .max(sequence_floor)
            .checked_add(1)
            .ok_or_else(|| StoreError::Backend("consensus vote sequence exhausted".into()))?;
        self.meta_db
            .put(&mut txn, sequence_key.as_bytes(), &sequence.to_be_bytes())
            .map_err(LmdbError::from)?;
        if is_final {
            self.meta_db
                .put(&mut txn, final_key.as_bytes(), hash.as_bytes())
                .map_err(LmdbError::from)?;
            if let Some(at) = timestamp {
                self.meta_db
                    .put(
                        &mut txn,
                        time_key.as_bytes(),
                        &previous_time
                            .map_or(at, |previous| previous.max(at))
                            .to_be_bytes(),
                    )
                    .map_err(LmdbError::from)?;
            }
            if let Some(limit) = next_limit {
                self.meta_db
                    .put(&mut txn, limit_key.as_bytes(), &limit.to_be_bytes())
                    .map_err(LmdbError::from)?;
            }
        }
        txn.commit().map_err(LmdbError::from)?;
        self.env.force_sync().map_err(LmdbError::from)?;
        Ok(sequence)
    }
}

impl MetaStore for LmdbMetaStore {
    fn put_meta(&self, key: &str, value: &[u8]) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.meta_db
            .put(&mut wtxn, key.as_bytes(), value)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_meta(&self, key: &str) -> Result<Vec<u8>, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let val = self
            .meta_db
            .get(&rtxn, key.as_bytes())
            .map_err(LmdbError::from)?
            .ok_or_else(|| LmdbError::NotFound(format!("meta key '{}'", key)))?;
        Ok(val.to_vec())
    }

    fn delete_meta(&self, key: &str) -> Result<(), StoreError> {
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.meta_db
            .delete(&mut wtxn, key.as_bytes())
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }

    fn get_schema_version(&self) -> Result<u32, StoreError> {
        let rtxn = self.env.read_txn().map_err(LmdbError::from)?;
        let val = self
            .meta_db
            .get(&rtxn, SCHEMA_VERSION_KEY)
            .map_err(LmdbError::from)?;
        match val {
            Some(bytes) if bytes.len() == 4 => {
                let arr: [u8; 4] = bytes.try_into().expect("checked length");
                Ok(u32::from_le_bytes(arr))
            }
            Some(_) => Err(LmdbError::Serialization(
                "schema_version has unexpected byte length".to_string(),
            ))?,
            None => Ok(0),
        }
    }

    fn set_schema_version(&self, version: u32) -> Result<(), StoreError> {
        let bytes = version.to_le_bytes();
        let mut wtxn = self.env.write_txn().map_err(LmdbError::from)?;
        self.meta_db
            .put(&mut wtxn, SCHEMA_VERSION_KEY, &bytes)
            .map_err(LmdbError::from)?;
        wtxn.commit().map_err(LmdbError::from)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LmdbEnvironment;
    use std::sync::Arc;

    #[test]
    fn metadata_visitor_borrows_pinned_values_and_enforces_both_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let meta = store.meta_store();
        meta.put_meta("a/1", b"1234").unwrap();
        meta.put_meta("a/2", b"5678").unwrap();
        meta.put_meta("ab/1", b"outside").unwrap();
        let txn = store.tx_begin_read().unwrap();
        let pointer = meta
            .meta_db
            .get(&txn.txn, b"a/1")
            .unwrap()
            .unwrap()
            .as_ptr();
        meta.put_meta("a/1", b"changed").unwrap();
        let mut seen = Vec::new();
        meta.visit_prefix_bounded_txn(&txn, "a/", 2, 14, |key, bytes| {
            if key == "a/1" {
                assert_eq!(bytes.as_ptr(), pointer);
                assert_eq!(bytes, b"1234");
            }
            seen.push(key.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, ["a/1", "a/2"]);
        for (rows, bytes, expected_calls) in [(1, 14, 1), (2, 13, 1), (2, 6, 0)] {
            let mut calls = 0;
            assert!(meta
                .visit_prefix_bounded_txn(&txn, "a/", rows, bytes, |_, _| {
                    calls += 1;
                    Ok(())
                })
                .is_err());
            assert_eq!(calls, expected_calls);
        }
        let mut calls = 0;
        assert!(meta
            .visit_prefix_bounded_txn(&txn, "a/", 2, 14, |_, _| {
                calls += 1;
                Err(StoreError::Corruption("stop".into()))
            })
            .is_err());
        assert_eq!(calls, 1);
        meta.visit_prefix_bounded_txn(&txn, "absent/", 0, 0, |_, _| panic!("absent"))
            .unwrap();
    }

    #[test]
    fn metadata_page_byte_budget_includes_keys_and_all_values() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let meta = store.meta_store();
        meta.put_meta("a/1", b"1234").unwrap();
        meta.put_meta("a/2", b"5678").unwrap();
        let txn = store.tx_begin_read().unwrap();
        assert!(meta
            .prefix_page_bounded_txn(&txn, "a/", None, 2, 13)
            .is_err());
        assert_eq!(
            meta.prefix_page_bounded_txn(&txn, "a/", None, 2, 14)
                .unwrap()
                .len(),
            2
        );
        assert!(meta
            .prefix_page_bounded_txn(&txn, "a/", None, 1, 6)
            .is_err());
        assert_eq!(
            meta.prefix_page_bounded_txn(&txn, "a/", Some("a/1"), 1, 7)
                .unwrap()[0]
                .0,
            "a/2"
        );
        assert!(meta
            .prefix_page_bounded_txn(&txn, "absent/", None, 1, 0)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn prefix_pages_are_bounded_exclusive_and_snapshot_consistent() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let meta = store.meta_store();
        for index in 0..53 {
            meta.put_meta(&format!("delegation/{index:03}"), &[index])
                .unwrap();
        }
        meta.put_meta("other/000", b"outside").unwrap();
        let txn = store.tx_begin_read().unwrap();
        let first = meta.prefix_page_txn(&txn, "delegation/", None, 50).unwrap();
        assert_eq!(first.len(), 50);
        let second = meta
            .prefix_page_txn(&txn, "delegation/", Some(&first[49].0), 50)
            .unwrap();
        assert_eq!(second.len(), 3);
        assert_eq!(second[0].0, "delegation/050");
        assert!(meta
            .prefix_page_txn(&txn, "delegation/", Some("other/000"), 50)
            .is_err());
        assert!(meta
            .prefix_page_txn(&txn, "delegation/", None, 102)
            .is_err());
        meta.put_meta("delegation/053", b"new").unwrap();
        assert_eq!(
            meta.prefix_page_txn(&txn, "delegation/", Some(&first[49].0), 50)
                .unwrap()
                .len(),
            3
        );
    }

    fn open(path: &std::path::Path) -> LmdbEnvironment {
        let env = LmdbEnvironment::open(path, 32, 16 * 1024 * 1024).unwrap();
        if matches!(
            env.meta_store().payment_epoch(),
            Err(StoreError::NotFound(_))
        ) {
            env.meta_store()
                .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &[1; 32])
                .unwrap();
        }
        env
    }

    #[test]
    fn metadata_snapshot_does_not_mix_chain_head_and_new_records() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let meta = store.meta_store();
        meta.put_meta("checkpoint_chain/test/head", &1u64.to_be_bytes())
            .unwrap();
        let snapshot = store.tx_begin_read().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_meta("checkpoint_chain/test/2", b"certificate")
            .unwrap();
        batch
            .put_meta("checkpoint_chain/test/head", &2u64.to_be_bytes())
            .unwrap();
        batch.commit().unwrap();
        assert_eq!(
            meta.get_meta_txn(&snapshot, "checkpoint_chain/test/head")
                .unwrap()
                .unwrap(),
            1u64.to_be_bytes()
        );
        assert!(meta
            .get_meta_txn(&snapshot, "checkpoint_chain/test/2")
            .unwrap()
            .is_none());
        drop(snapshot);
        let current = store.tx_begin_read().unwrap();
        assert_eq!(
            meta.get_meta_txn(&current, "checkpoint_chain/test/head")
                .unwrap()
                .unwrap(),
            2u64.to_be_bytes()
        );
        assert_eq!(
            meta.get_meta_txn(&current, "checkpoint_chain/test/2")
                .unwrap()
                .unwrap(),
            b"certificate"
        );
    }

    fn checkpoint(store: &LmdbMetaStore, height: u64, choice: u8) -> Result<(), StoreError> {
        store.reserve_checkpoint_vote(
            burst_types::NetworkId::Dev,
            &BlockHash::new([1; 32]),
            &WalletAddress::new("brst_rep"),
            height,
            &BlockHash::new([choice; 32]),
        )
    }

    #[test]
    fn checkpoint_choice_survives_restart_and_allows_only_identical_retry() {
        let dir = tempfile::tempdir().unwrap();
        {
            let env = open(dir.path());
            checkpoint(&env.meta_store(), 1, 3).unwrap();
        }
        let env = open(dir.path());
        let store = env.meta_store();
        checkpoint(&store, 1, 3).unwrap();
        assert!(matches!(
            checkpoint(&store, 1, 4),
            Err(StoreError::Duplicate(_))
        ));
        checkpoint(&store, 2, 4).unwrap();
        assert!(checkpoint(&store, 0, 4).is_err());
        assert!(checkpoint(&store, 3, 0).is_err());
    }

    #[test]
    fn concurrent_checkpoint_choices_have_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let env = open(dir.path());
        let store = Arc::new(env.meta_store());
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = [3, 4]
            .into_iter()
            .map(|choice| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    checkpoint(&store, 1, choice).is_ok()
                })
            })
            .collect();
        assert_eq!(
            threads
                .into_iter()
                .filter_map(|t| t.join().unwrap().then_some(()))
                .count(),
            1
        );
    }

    #[test]
    fn corrupt_checkpoint_reservation_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let env = open(dir.path());
        let store = env.meta_store();
        let key = format!(
            "checkpoint_vote/v1/dev/{}/brst_rep/1",
            BlockHash::new([1; 32])
        );
        store.put_meta(&key, &[1, 2]).unwrap();
        assert!(matches!(
            checkpoint(&store, 1, 3),
            Err(StoreError::Corruption(_))
        ));
        assert_eq!(store.get_meta(&key).unwrap(), [1, 2]);
    }

    #[test]
    fn checkpoint_reservations_are_network_and_genesis_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let env = open(dir.path());
        let store = env.meta_store();
        checkpoint(&store, 1, 3).unwrap();
        for (network, genesis) in [
            (burst_types::NetworkId::Test, 1),
            (burst_types::NetworkId::Dev, 2),
        ] {
            store
                .reserve_checkpoint_vote(
                    network,
                    &BlockHash::new([genesis; 32]),
                    &WalletAddress::new("brst_rep"),
                    1,
                    &BlockHash::new([4; 32]),
                )
                .unwrap();
        }
    }

    fn reserve(
        store: &LmdbMetaStore,
        account: &str,
        hash: u8,
        is_final: bool,
    ) -> Result<u64, StoreError> {
        store.reserve_consensus_vote(
            &burst_types::BlockHash::new([1; 32]),
            &WalletAddress::new("brst_rep"),
            &WalletAddress::new(account),
            &BlockHash::ZERO,
            &BlockHash::new([hash; 32]),
            is_final,
            0,
        )
    }

    #[test]
    fn payment_epoch_failure_does_not_reserve_sequence_or_final_choice() {
        let dir = tempfile::tempdir().unwrap();
        let env = LmdbEnvironment::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        let store = env.meta_store();
        let key = burst_store::meta::PAYMENT_EPOCH_KEY;
        assert!(reserve(&store, "brst_alice", 7, true).is_err());
        for invalid in [vec![], vec![1; 31], vec![1; 33], vec![0; 32], vec![2; 32]] {
            store.put_meta(key, &invalid).unwrap();
            assert!(reserve(&store, "brst_alice", 7, true).is_err());
            assert!(reserve(&store, "brst_alice", 7, false).is_err());
            assert!(matches!(
                store.get_meta("consensus_sequence/v1/brst_rep"),
                Err(StoreError::NotFound(_))
            ));
            assert!(matches!(
                store.get_meta(&format!(
                    "consensus_final/v2/{}/brst_rep/brst_alice/{}",
                    BlockHash::new([1; 32]),
                    BlockHash::ZERO
                )),
                Err(StoreError::NotFound(_))
            ));
        }
        store.put_meta(key, &[1; 32]).unwrap();
        assert_eq!(reserve(&store, "brst_alice", 8, true).unwrap(), 1);
        store.put_meta(key, &[2; 32]).unwrap();
        assert!(reserve(&store, "brst_alice", 8, true).is_err());
        store.put_meta(key, &[1; 32]).unwrap();
        assert_eq!(reserve(&store, "brst_alice", 8, true).unwrap(), 2);
        assert!(reserve(&store, "brst_alice", 7, true).is_err());
    }

    #[test]
    fn certified_vote_limit_is_atomic_durable_and_cannot_be_bypassed_without_a_timestamp() {
        use burst_store::meta::{encode_payment_vote_limit, PAYMENT_VOTE_LIMIT_KEY};
        let dir = tempfile::tempdir().unwrap();
        let epoch = BlockHash::new([1; 32]);
        {
            let env = open(dir.path());
            env.meta_store()
                .put_meta(
                    PAYMENT_VOTE_LIMIT_KEY,
                    &encode_payment_vote_limit(epoch, Some(100)),
                )
                .unwrap();
        }
        let env = open(dir.path());
        let store = env.meta_store();
        let vote = |timestamp, final_vote| {
            store.reserve_consensus_vote_at(
                &epoch,
                &WalletAddress::new("brst_rep"),
                &WalletAddress::new("brst_alice"),
                &BlockHash::ZERO,
                &BlockHash::new([7; 32]),
                final_vote,
                0,
                timestamp,
            )
        };
        for final_vote in [false, true] {
            assert!(vote(Some(101), final_vote).is_err());
            assert!(vote(None, final_vote).is_err());
            assert!(reserve(&store, "brst_alice", 7, final_vote).is_err());
        }
        assert!(matches!(
            store.get_meta("consensus_sequence/v1/brst_rep"),
            Err(StoreError::NotFound(_))
        ));
        assert_eq!(vote(Some(100), true).unwrap(), 1);
        assert!(vote(Some(101), true).is_err());
        assert_eq!(vote(Some(100), true).unwrap(), 2);
        let correct = encode_payment_vote_limit(epoch, Some(100));
        for invalid in [
            vec![],
            correct[..32].to_vec(),
            [correct.clone(), vec![0]].concat(),
            encode_payment_vote_limit(BlockHash::new([2; 32]), None),
        ] {
            store.put_meta(PAYMENT_VOTE_LIMIT_KEY, &invalid).unwrap();
            assert!(vote(Some(100), false).is_err());
        }
        store.delete_meta(PAYMENT_VOTE_LIMIT_KEY).unwrap();
        store
            .put_meta(burst_store::meta::CERTIFIED_PAYMENT_AUTHORITY_KEY, &[1])
            .unwrap();
        assert!(vote(Some(100), true).is_err());
        store.put_meta(PAYMENT_VOTE_LIMIT_KEY, &correct).unwrap();
        store
            .put_meta(
                burst_store::meta::GOVERNANCE_COUNTING_LIMIT_KEY,
                &encode_payment_vote_limit(epoch, None),
            )
            .unwrap();
        assert_eq!(vote(Some(100), true).unwrap(), 3);
    }

    #[test]
    fn counting_limit_rejects_votes_without_consuming_sequence_or_final_choice() {
        use burst_store::meta::{encode_payment_vote_limit, GOVERNANCE_COUNTING_LIMIT_KEY};
        let dir = tempfile::tempdir().unwrap();
        let epoch = BlockHash::new([1; 32]);
        {
            let env = open(dir.path());
            env.meta_store()
                .put_meta(
                    GOVERNANCE_COUNTING_LIMIT_KEY,
                    &encode_payment_vote_limit(epoch, Some(90)),
                )
                .unwrap();
        }
        let env = open(dir.path());
        let store = env.meta_store();
        let vote = |timestamp, final_vote| {
            store.reserve_consensus_vote_at(
                &epoch,
                &WalletAddress::new("brst_rep"),
                &WalletAddress::new("brst_alice"),
                &BlockHash::ZERO,
                &BlockHash::new([7; 32]),
                final_vote,
                0,
                timestamp,
            )
        };
        for final_vote in [false, true] {
            for timestamp in [None, Some(91)] {
                assert!(vote(timestamp, final_vote).is_err());
            }
        }
        assert!(matches!(
            store.get_meta("consensus_sequence/v1/brst_rep"),
            Err(StoreError::NotFound(_))
        ));
        assert_eq!(vote(Some(90), true).unwrap(), 1);
        assert!(vote(Some(91), true).is_err());
        store
            .put_meta(
                GOVERNANCE_COUNTING_LIMIT_KEY,
                &encode_payment_vote_limit(epoch, None),
            )
            .unwrap();
        assert_eq!(vote(Some(91), true).unwrap(), 2);
        // Removing a deadline must not erase an earlier irrevocable promise.
        assert!(reserve(&store, "brst_alice", 8, true).is_err());
    }

    #[test]
    fn final_choice_and_sequence_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let env = open(dir.path());
            let store = env.meta_store();
            assert_eq!(reserve(&store, "brst_alice", 1, false).unwrap(), 1);
            assert_eq!(reserve(&store, "brst_alice", 2, false).unwrap(), 2);
            assert_eq!(reserve(&store, "brst_alice", 2, true).unwrap(), 3);
        }
        let env = open(dir.path());
        let store = env.meta_store();
        assert!(reserve(&store, "brst_alice", 1, true).is_err());
        assert!(reserve(&store, "brst_alice", 1, false).is_err());
        assert_eq!(reserve(&store, "brst_alice", 2, true).unwrap(), 4);
        assert_eq!(reserve(&store, "brst_alice", 2, false).unwrap(), 5);
    }

    #[test]
    fn opening_roots_and_representatives_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let env = open(dir.path());
        let store = env.meta_store();
        assert_eq!(reserve(&store, "brst_alice", 1, true).unwrap(), 1);
        assert_eq!(reserve(&store, "brst_bob", 2, true).unwrap(), 2);
        assert_eq!(
            store
                .reserve_consensus_vote(
                    &burst_types::BlockHash::new([1; 32]),
                    &WalletAddress::new("brst_other_rep"),
                    &WalletAddress::new("brst_alice"),
                    &BlockHash::ZERO,
                    &BlockHash::new([2; 32]),
                    true,
                    0
                )
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .reserve_consensus_vote(
                    &burst_types::BlockHash::new([1; 32]),
                    &WalletAddress::new("brst_rep"),
                    &WalletAddress::new("brst_alice"),
                    &BlockHash::new([1; 32]),
                    &BlockHash::new([3; 32]),
                    true,
                    100
                )
                .unwrap(),
            101
        );
    }

    #[test]
    fn promised_limits_only_tighten_and_high_water_never_moves_backwards() {
        let dir = tempfile::tempdir().unwrap();
        let epoch = BlockHash::new([1; 32]);
        {
            let env = open(dir.path());
            let reserve_at = |hash, at, limit| {
                env.meta_store()
                    .reserve_consensus_vote_with_governance_limit(
                        &epoch,
                        &WalletAddress::new("brst_rep"),
                        &WalletAddress::new("brst_alice"),
                        &BlockHash::new([hash; 32]),
                        &BlockHash::new([hash; 32]),
                        true,
                        0,
                        Some(at),
                        limit,
                    )
            };
            reserve_at(1, 30, Some(100)).unwrap();
            reserve_at(2, 10, Some(90)).unwrap();
            assert!(reserve_at(3, 20, Some(29)).is_err());
            assert!(reserve_at(4, 91, None).is_err());
        }
        let env = open(dir.path());
        assert_eq!(
            env.meta_store()
                .get_meta(&format!("consensus_time/v1/{epoch}/brst_rep"))
                .unwrap(),
            30u64.to_be_bytes()
        );
        assert_eq!(
            env.meta_store()
                .get_meta(&format!("consensus_governance/v1/{epoch}/brst_rep"))
                .unwrap(),
            90u64.to_be_bytes()
        );
        assert_eq!(
            env.meta_store()
                .get_meta("consensus_sequence/v1/brst_rep")
                .unwrap(),
            2u64.to_be_bytes()
        );
    }

    #[test]
    fn concurrent_governance_deadline_and_later_final_have_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let env = open(dir.path());
        let epoch = BlockHash::new([1; 32]);
        let reserve_at = |account: &str, hash, timestamp, limit| {
            env.meta_store()
                .reserve_consensus_vote_with_governance_limit(
                    &epoch,
                    &WalletAddress::new("brst_rep"),
                    &WalletAddress::new(account),
                    &BlockHash::ZERO,
                    &BlockHash::new([hash; 32]),
                    true,
                    0,
                    Some(timestamp),
                    limit,
                )
        };
        let barrier = std::sync::Barrier::new(2);
        let results = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                reserve_at("brst_support", 1, 10, Some(100))
            });
            let second = scope.spawn(|| {
                barrier.wait();
                reserve_at("brst_payment", 2, 101, None)
            });
            [first.join().unwrap(), second.join().unwrap()]
        });
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            env.meta_store()
                .get_meta("consensus_sequence/v1/brst_rep")
                .unwrap(),
            1u64.to_be_bytes()
        );
    }

    #[test]
    fn missing_final_timestamp_history_cannot_be_silently_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let env = open(dir.path());
        let store = env.meta_store();
        reserve(&store, "brst_old", 1, true).unwrap();
        assert!(store
            .reserve_consensus_vote_with_governance_limit(
                &BlockHash::new([1; 32]),
                &WalletAddress::new("brst_rep"),
                &WalletAddress::new("brst_support"),
                &BlockHash::ZERO,
                &BlockHash::new([2; 32]),
                true,
                0,
                Some(10),
                Some(100)
            )
            .is_err());
        assert_eq!(
            store.get_meta("consensus_sequence/v1/brst_rep").unwrap(),
            1u64.to_be_bytes()
        );
    }

    #[test]
    fn concurrent_conflicting_final_reservations_have_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let env = open(dir.path());
        let results = std::thread::scope(|scope| {
            let first = scope.spawn(|| reserve(&env.meta_store(), "brst_alice", 1, true));
            let second = scope.spawn(|| reserve(&env.meta_store(), "brst_alice", 2, true));
            [first.join().unwrap(), second.join().unwrap()]
        });
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|r| r.is_err()).count(), 1);
    }

    #[test]
    fn corrupt_or_exhausted_sequence_never_reserves_a_final_choice() {
        let dir = tempfile::tempdir().unwrap();
        let env = open(dir.path());
        let store = env.meta_store();
        for bytes in [vec![0], u64::MAX.to_be_bytes().to_vec()] {
            store
                .put_meta("consensus_sequence/v1/brst_rep", &bytes)
                .unwrap();
            assert!(reserve(&store, "brst_alice", 1, true).is_err());
            assert!(matches!(
                store.get_meta(&format!(
                    "consensus_final/v2/{}/brst_rep/brst_alice/{}",
                    BlockHash::new([1; 32]),
                    BlockHash::ZERO
                )),
                Err(StoreError::NotFound(_))
            ));
        }
        store
            .put_meta("consensus_sequence/v1/brst_rep", &10u64.to_be_bytes())
            .unwrap();
        assert_eq!(reserve(&store, "brst_alice", 2, true).unwrap(), 11);
    }

    #[test]
    fn corrupt_final_choice_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let env = open(dir.path());
        let store = env.meta_store();
        store
            .put_meta(
                &format!(
                    "consensus_final/v2/{}/brst_rep/brst_alice/{}",
                    BlockHash::new([1; 32]),
                    BlockHash::ZERO
                ),
                &[1],
            )
            .unwrap();
        assert!(matches!(
            reserve(&store, "brst_alice", 1, true),
            Err(StoreError::Corruption(_))
        ));
        assert!(matches!(
            reserve(&store, "brst_alice", 1, false),
            Err(StoreError::Corruption(_))
        ));
        assert!(matches!(
            store.get_meta("consensus_sequence/v1/brst_rep"),
            Err(StoreError::NotFound(_))
        ));
    }
}
