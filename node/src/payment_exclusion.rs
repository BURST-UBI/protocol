//! Exclusion requires an atomically observed closed epoch and an unchanged,
//! canonical, fully confirmed account frontier, or a still-unopened account.
//! Missing block bodies alone are not exclusion.
use burst_store::{meta, StoreError};
use burst_store_lmdb::LmdbStore;
use burst_types::{
    payment_recovery::{ExclusionQuery, ExclusionReview},
    BlockHash, WalletAddress,
};

fn hash(value: &str) -> Result<BlockHash, String> {
    value
        .parse()
        .map_err(|error: burst_types::transaction_hash::ParseHashError| error.to_string())
}

/// Canonical account position and finality are read from one database snapshot.
/// Historical block bodies are not needed and cache entries are not consulted.
pub(crate) fn slot(
    store: &LmdbStore,
    query: burst_types::payment_recovery::SlotQuery,
) -> Result<burst_types::payment_recovery::SlotReview, String> {
    indexed_slot(store, query)?.ok_or_else(|| "confirmed slot history is unavailable".into())
}

/// Outer None means canonical indexes cannot answer. A returned review with
/// no successor means no confirmed successor was found for this query, not
/// that an unconfirmed successor is absent or an earlier payment is excluded.
pub(crate) fn indexed_slot(
    store: &LmdbStore,
    query: burst_types::payment_recovery::SlotQuery,
) -> Result<Option<burst_types::payment_recovery::SlotReview>, String> {
    burst_crypto::decode_address(&query.account).ok_or("invalid recovery account")?;
    let account = WalletAddress::new(&query.account);
    let previous = hash(&query.previous)?;
    let mut review = burst_types::payment_recovery::SlotReview {
        account: query.account,
        previous: previous.to_string(),
        successor: None,
    };
    let txn = store.tx_begin_read().map_err(|e| e.to_string())?;
    let info = match store.account_store().get_account_txn(&txn, &account) {
        Ok(info) => info,
        Err(StoreError::NotFound(_)) => return Ok(Some(review)),
        Err(error) => return Err(error.to_string()),
    };
    if info.address != account || info.confirmation_height > info.block_count {
        return Err("inconsistent recovery account position".into());
    }
    if info.confirmation_height == 0 {
        return Ok(Some(review));
    }
    let blocks = store.block_store();
    if blocks
        .block_at_height_txn(&txn, &account, info.block_count)
        .map_err(|e| e.to_string())?
        != Some(info.head)
    {
        return Err("recovery account head differs from canonical index".into());
    }
    let next = if previous.is_zero() {
        1
    } else {
        let Some(height) = blocks
            .height_of_block_txn(&txn, &previous)
            .map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        if height >= info.confirmation_height
            || blocks
                .block_at_height_txn(&txn, &account, height)
                .map_err(|e| e.to_string())?
                != Some(previous)
        {
            return Ok(Some(review));
        }
        height.checked_add(1).ok_or("recovery height overflow")?
    };
    let Some(successor) = blocks
        .block_at_height_txn(&txn, &account, next)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    if successor.is_zero() {
        return Err("invalid canonical confirmed successor".into());
    }
    review.successor = Some(successor.to_string());
    Ok(Some(review))
}

pub(crate) struct Reader {
    store: std::sync::Arc<LmdbStore>,
    authority: std::sync::Mutex<Option<(usize, Option<BlockHash>)>>,
    #[cfg(test)]
    validations: std::sync::atomic::AtomicUsize,
}

impl Reader {
    pub(crate) fn new(store: std::sync::Arc<LmdbStore>) -> Self {
        Self {
            store,
            authority: Default::default(),
            #[cfg(test)]
            validations: Default::default(),
        }
    }

    /// Review one closed-epoch payment position from a single snapshot. False
    /// can mean insufficient evidence, not proof that the payment was included.
    pub(crate) fn read(&self, query: ExclusionQuery) -> Result<ExclusionReview, String> {
        read_with_authority(&self.store, query, |txn| self.authority_epoch(txn))
    }

    fn authority_epoch(
        &self,
        txn: &burst_store_lmdb::LmdbReadTransaction<'_>,
    ) -> Result<Option<BlockHash>, String> {
        let identity = self.store.read_payment_authority_identity(txn);
        if let Some(identity) = identity {
            let cached = self.authority.lock().map_err(|error| error.to_string())?;
            if let Some((saved, epoch)) = *cached {
                if saved == identity {
                    return Ok(epoch);
                }
            }
        }
        // Validate outside the cache lock. Cache only the epoch, never the roster
        // or the per-query closure, account position and canonical index.
        #[cfg(test)]
        self.validations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let epoch = validated_epoch(&self.store, txn)?;
        if let Some(identity) = identity {
            *self.authority.lock().map_err(|error| error.to_string())? = Some((identity, epoch));
        }
        Ok(epoch)
    }

    #[cfg(test)]
    pub(crate) fn authority_epoch_for_test(
        &self,
        txn: &burst_store_lmdb::LmdbReadTransaction<'_>,
    ) -> Result<Option<BlockHash>, String> {
        self.authority_epoch(txn)
    }

    #[cfg(test)]
    pub(crate) fn validation_count(&self) -> usize {
        self.validations.load(std::sync::atomic::Ordering::Relaxed)
    }
}

fn validated_epoch(
    store: &LmdbStore,
    txn: &burst_store_lmdb::LmdbReadTransaction<'_>,
) -> Result<Option<BlockHash>, String> {
    let meta = store.meta_store();
    crate::payment_authority::PaymentAuthority::read_at(|key| meta.get_meta_txn(txn, key))
        .map(|authority| authority.map(|authority| authority.epoch))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
pub(crate) fn read(store: &LmdbStore, query: ExclusionQuery) -> Result<ExclusionReview, String> {
    read_with_authority(store, query, |txn| validated_epoch(store, txn))
}

fn read_with_authority(
    store: &LmdbStore,
    query: ExclusionQuery,
    authority: impl FnOnce(
        &burst_store_lmdb::LmdbReadTransaction<'_>,
    ) -> Result<Option<BlockHash>, String>,
) -> Result<ExclusionReview, String> {
    burst_crypto::decode_address(&query.account).ok_or("invalid recovery account")?;
    let account = WalletAddress::new(&query.account);
    let previous = hash(&query.previous)?;
    let epoch = hash(&query.payment_epoch)?;
    if epoch.is_zero() {
        return Err("payment recovery requires a nonzero epoch".into());
    }
    let txn = store.tx_begin_read().map_err(|e| e.to_string())?;
    let meta = store.meta_store();
    let get = |key: &str| meta.get_meta_txn(&txn, key);
    let installed = meta::decode_payment_epoch(
        &get(meta::PAYMENT_EPOCH_KEY)
            .map_err(|e| e.to_string())?
            .ok_or("missing installed payment epoch")?,
    )
    .map_err(|e| e.to_string())?;
    let mut review = ExclusionReview {
        account: query.account,
        previous: previous.to_string(),
        payment_epoch: epoch.to_string(),
        installed_epoch: installed.to_string(),
        excluded: false,
    };
    if installed == epoch {
        return Ok(review);
    }
    // Reuse the installed-authority validator, including its current-epoch fence
    // checks. Startup authenticates this descriptor and closure history from genesis.
    let Some(authority_epoch) = authority(&txn)? else {
        return Ok(review);
    };
    if authority_epoch != installed {
        return Err("inconsistent installed payment authority".into());
    }
    let Some(closed) = get(&meta::closed_payment_epoch_key(&epoch)).map_err(|e| e.to_string())?
    else {
        return Ok(review);
    };
    if closed.len() != 64 || closed[32..] != *epoch.as_bytes() {
        return Err("invalid closed payment epoch record".into());
    }
    let successor = BlockHash::new(
        closed[..32]
            .try_into()
            .map_err(|_| "invalid epoch successor")?,
    );
    if successor.is_zero() || successor == epoch {
        return Err("invalid closed payment epoch successor".into());
    }
    let info = match store.account_store().get_account_txn(&txn, &account) {
        Ok(info) => info,
        Err(StoreError::NotFound(_)) => {
            if previous.is_zero() {
                // Account absence and its canonical opening index must agree in
                // the same transaction as the installed closed-epoch fence.
                review.excluded = store
                    .block_store()
                    .block_at_height_txn(&txn, &account, 1)
                    .map_err(|e| e.to_string())?
                    .is_none();
            }
            return Ok(review);
        }
        Err(error) => return Err(error.to_string()),
    };
    if info.address != account {
        return Err("recovery account record differs from its key".into());
    }
    if info.head != previous
        || info.block_count == 0
        || info.block_count != info.confirmation_height
    {
        return Ok(review);
    }
    let canonical = store
        .block_store()
        .block_at_height_txn(&txn, &account, info.block_count)
        .map_err(|e| e.to_string())?;
    review.excluded = canonical == Some(previous);
    Ok(review)
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store::account::{AccountInfo, AccountStore};
    use burst_types::payment_recovery::SlotQuery;

    #[test]
    fn confirmed_slots_use_atomic_indexes_without_decoding_bodies() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
        let account =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[91; 32]).public);
        let other =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[92; 32]).public);
        let first = BlockHash::new([1; 32]);
        let second = BlockHash::new([2; 32]);
        let mut info = AccountInfo::unopened(account.clone());
        info.head = second;
        info.block_count = 2;
        info.confirmation_height = 1;
        let mut batch = store.tx_begin_write().unwrap();
        // Deliberately undecodable payloads ensure lookup uses position indexes.
        batch
            .put_block_with_account(&first, b"unavailable body", &account, 1)
            .unwrap();
        batch
            .put_block_with_account(&second, b"unavailable body", &account, 2)
            .unwrap();
        batch.put_account_info(&info, false).unwrap();
        let mut other_info = AccountInfo::unopened(other.clone());
        other_info.head = BlockHash::new([4; 32]);
        other_info.block_count = 2;
        other_info.confirmation_height = 2;
        batch
            .put_block_with_account(&BlockHash::new([3; 32]), b"unavailable body", &other, 1)
            .unwrap();
        batch
            .put_block_with_account(&other_info.head, b"unavailable body", &other, 2)
            .unwrap();
        batch.put_account_info(&other_info, false).unwrap();
        batch.commit().unwrap();
        let query = SlotQuery {
            account: account.to_string(),
            previous: first.to_string(),
        };
        assert_eq!(slot(&store, query.clone()).unwrap().successor, None);
        assert_eq!(
            slot(
                &store,
                SlotQuery {
                    previous: BlockHash::ZERO.to_string(),
                    ..query.clone()
                }
            )
            .unwrap()
            .successor,
            Some(first.to_string())
        );
        for previous in [second, BlockHash::new([3; 32])] {
            assert_eq!(
                slot(
                    &store,
                    SlotQuery {
                        previous: previous.to_string(),
                        ..query.clone()
                    }
                )
                .unwrap()
                .successor,
                None
            );
        }
        assert_eq!(
            slot(
                &store,
                SlotQuery {
                    account: other.to_string(),
                    ..query.clone()
                }
            )
            .unwrap()
            .successor,
            None
        );
        info.confirmation_height = 2;
        store.account_store().put_account(&info).unwrap();
        assert_eq!(
            slot(&store, query.clone()).unwrap().successor,
            Some(second.to_string())
        );
        drop(store);
        let store = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
        assert_eq!(
            slot(&store, query.clone()).unwrap().successor,
            Some(second.to_string())
        );
        info.head = first;
        store.account_store().put_account(&info).unwrap();
        assert!(slot(&store, query).is_err());
    }

    #[test]
    fn cached_absence_is_invalidated_by_orphaned_authority_dependencies() {
        let directory = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(LmdbStore::open(directory.path(), 32, 8 << 20).unwrap());
        let installed = BlockHash::new([31; 32]);
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_meta(meta::PAYMENT_EPOCH_KEY, installed.as_bytes())
            .unwrap();
        batch.commit().unwrap();
        let reader = Reader::new(store.clone());
        let account =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[92; 32]).public);
        let query = ExclusionQuery {
            account: account.to_string(),
            previous: BlockHash::ZERO.to_string(),
            payment_epoch: BlockHash::new([32; 32]).to_string(),
        };
        for _ in 0..2 {
            assert!(!reader.read(query.clone()).unwrap().excluded);
        }
        assert_eq!(reader.validation_count(), 1);
        let mut count = 1;
        for key in [
            crate::confirmed_replay::installation::CUTOFF_KEY,
            meta::PAYMENT_VOTE_LIMIT_KEY,
            meta::GOVERNANCE_COUNTING_LIMIT_KEY,
            crate::confirmed_governance::KEY,
            crate::confirmed_governance::HEADER,
        ] {
            let mut batch = store.tx_begin_write().unwrap();
            batch.put_meta(key, b"orphan").unwrap();
            batch.commit().unwrap();
            for _ in 0..2 {
                assert!(reader.read(query.clone()).is_err(), "{key}");
                count += 1;
                assert_eq!(reader.validation_count(), count);
            }
            let mut batch = store.tx_begin_write().unwrap();
            batch.delete_meta(key).unwrap();
            batch.commit().unwrap();
            assert!(!reader.read(query.clone()).unwrap().excluded);
            count += 1;
            assert_eq!(reader.validation_count(), count);
        }
    }
}
