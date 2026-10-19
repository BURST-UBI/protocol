//! TRST holdings read and written inside the caller's economic transaction.
//! Row-format activation is separate from decoding and ordinary persistence.

use burst_store::StoreError;
use burst_store_lmdb::block_undo::UndoTable;
use burst_store_lmdb::LmdbWriteTransaction;
use burst_trst::{engine::WalletRecord, TrstEngine};

pub(crate) const FORMAT_KEY: &str = "trst_state_format";

fn invalid(reason: impl ToString) -> StoreError {
    StoreError::Corruption(reason.to_string())
}

pub(crate) fn row_format(batch: &LmdbWriteTransaction<'_>) -> Result<bool, StoreError> {
    match batch.get_meta(FORMAT_KEY)?.as_deref() {
        None => {
            if let Some(row) = batch
                .checkpoint_metadata_entries(WalletRecord::STORAGE_PREFIX.as_bytes())?
                .next()
            {
                row?;
                return Err(invalid("TRST rows exist without a format marker"));
            }
            Ok(false)
        }
        Some([1]) => {
            if batch
                .undo_value_ref(UndoTable::Meta, TrstEngine::meta_key().as_bytes())?
                .is_some()
            {
                return Err(invalid("mixed aggregate and row TRST state"));
            }
            Ok(true)
        }
        Some(_) => Err(invalid("unsupported TRST storage format")),
    }
}

pub(crate) fn present(batch: &LmdbWriteTransaction<'_>) -> Result<bool, StoreError> {
    Ok(row_format(batch)?
        || batch
            .undo_value_ref(UndoTable::Meta, TrstEngine::meta_key().as_bytes())?
            .is_some())
}

pub(crate) fn load(
    batch: &LmdbWriteTransaction<'_>,
    expiry_secs: u64,
) -> Result<TrstEngine, StoreError> {
    if !row_format(batch)? {
        let bytes = batch
            .undo_value_ref(UndoTable::Meta, TrstEngine::meta_key().as_bytes())?
            .ok_or_else(|| invalid("missing persisted TRST holdings"))?;
        return TrstEngine::load_wallets(bytes, expiry_secs).map_err(invalid);
    }
    let rows = batch.checkpoint_metadata_entries(WalletRecord::STORAGE_PREFIX.as_bytes())?;
    TrstEngine::from_wallet_records(
        rows.map(|row| {
            let (key, bytes) = row.map_err(|error| error.to_string())?;
            let key = std::str::from_utf8(key).map_err(|error| error.to_string())?;
            WalletRecord::from_storage_entry(key, bytes)
        }),
        expiry_secs,
    )
    .map_err(invalid)
}

pub(crate) fn persist(
    batch: &mut LmdbWriteTransaction<'_>,
    engine: &TrstEngine,
) -> Result<(), StoreError> {
    if !row_format(batch)? {
        return batch.put_meta(TrstEngine::meta_key(), &engine.save_wallets());
    }
    replace_rows(batch, engine)
}

/// The caller must abort its transaction on any error, as for full persistence.
pub(crate) fn persist_delta(
    batch: &mut LmdbWriteTransaction<'_>,
    before: &TrstEngine,
    after: &TrstEngine,
) -> Result<(), StoreError> {
    if !row_format(batch)? {
        return persist(batch, after);
    }
    after.visit_wallet_record_delta(before, |old, new| {
        let old = old
            .map(|record| record.storage_entry())
            .transpose()
            .map_err(invalid)?;
        let new = new
            .map(|record| record.storage_entry())
            .transpose()
            .map_err(invalid)?;
        let key = new
            .as_ref()
            .or(old.as_ref())
            .ok_or_else(|| invalid("empty TRST delta"))?
            .0
            .as_str();
        if old
            .as_ref()
            .zip(new.as_ref())
            .is_some_and(|(a, b)| a.0 != b.0)
        {
            return Err(invalid("TRST delta changes its primary key"));
        }
        if batch.get_meta(key)?.as_deref() != old.as_ref().map(|(_, bytes)| bytes.as_slice()) {
            return Err(invalid("TRST delta conflicts with persisted state"));
        }
        if let Some((_, bytes)) = &new {
            batch.put_meta(key, bytes)
        } else {
            batch.delete_meta(key)
        }
    })
}

/// Upgrade holdings and every provisional journal together, after startup has
/// authenticated/recovered the ledger. No runtime engine or finality is changed.
pub(crate) fn migrate(
    store: &burst_store_lmdb::LmdbStore,
    expiry_secs: u64,
) -> Result<bool, StoreError> {
    let mut batch = store.tx_begin_write()?;
    if row_format(&batch)? {
        return Ok(false);
    }
    let engine = load(&batch, expiry_secs)?;
    let mut records = Vec::new();
    let mut bytes = 0u64;
    for account in batch.account_entries()? {
        let account = account?;
        if account.confirmation_height > account.block_count {
            return Err(invalid("TRST migration has invalid confirmation height"));
        }
        for previous_height in account.confirmation_height..account.block_count {
            if records.len() >= 131_072 {
                return Err(invalid("TRST migration exceeds provisional journal budget"));
            }
            let hash = batch
                .block_at_height(&account.address, previous_height + 1)?
                .ok_or_else(|| invalid("TRST migration requires complete provisional indexes"))?;
            let record = batch
                .block_undo(&hash)?
                .ok_or_else(|| invalid("TRST migration requires recovered provisional journals"))?;
            let converted = convert_undo(record)?;
            bytes = bytes
                .checked_add(bincode::serialized_size(&converted).map_err(invalid)?)
                .filter(|bytes| *bytes <= 64 << 20)
                .ok_or_else(|| invalid("TRST migration exceeds provisional journal byte budget"))?;
            records.push(converted);
        }
    }
    // The existing installer checks completeness, ledger identity and immutable
    // metadata. Its failure aborts this entire transaction, including the rows.
    replace_rows(&mut batch, &engine)?;
    batch.delete_meta(TrstEngine::meta_key())?;
    batch.put_meta(FORMAT_KEY, &[1])?;
    batch.replace_checkpoint_undo(&records)?;
    batch.commit()?;
    store.force_sync().map_err(invalid)?;
    Ok(true)
}

// Full replacement is used by replay/installation as well as the initial
// integration. Only changed rows reach the undo journal. Account-scoped
// preparation is a separate requirement; this still visits the whole engine.
fn replace_rows(
    batch: &mut LmdbWriteTransaction<'_>,
    engine: &TrstEngine,
) -> Result<(), StoreError> {
    replace_records(batch, |visit| engine.visit_wallet_records(visit))
}

/// Certified recovery replaces holdings and all provisional journals in one
/// transaction. Ordinary publication must preserve surviving journal promises.
pub(crate) fn stage_replayed(
    batch: &mut LmdbWriteTransaction<'_>,
    engine: &TrstEngine,
) -> Result<(), StoreError> {
    row_format(batch)?;
    for row in batch.checkpoint_metadata_entries(WalletRecord::STORAGE_PREFIX.as_bytes())? {
        let (key, _) = row?;
        std::str::from_utf8(key).map_err(invalid)?;
    }
    batch.clear_checkpoint_token_rows(WalletRecord::STORAGE_PREFIX.as_bytes())?;
    engine.visit_wallet_records(|record| {
        let (key, bytes) = record.storage_entry().map_err(invalid)?;
        batch.put_meta(&key, &bytes)
    })?;
    batch.delete_meta(TrstEngine::meta_key())?;
    batch.put_meta(FORMAT_KEY, &[1])
}

fn replace_records(
    batch: &mut LmdbWriteTransaction<'_>,
    visit: impl FnOnce(&mut dyn FnMut(WalletRecord) -> Result<(), StoreError>) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let mut old_keys = std::collections::BTreeSet::new();
    for row in batch.checkpoint_metadata_entries(WalletRecord::STORAGE_PREFIX.as_bytes())? {
        let (key, _) = row?;
        old_keys.insert(std::str::from_utf8(key).map_err(invalid)?.to_owned());
    }
    visit(&mut |record| {
        let (key, bytes) = record.storage_entry().map_err(invalid)?;
        old_keys.remove(&key);
        if batch.get_meta(&key)?.as_deref() != Some(bytes.as_slice()) {
            batch.put_meta(&key, &bytes)?;
        }
        Ok::<_, StoreError>(())
    })?;
    for key in old_keys {
        batch.delete_meta(&key)?;
    }
    Ok(())
}

/// Translate only the aggregate holdings images. Ledger identity, graph,
/// lifecycle and finality fields are preserved and validated by the installer.
pub(crate) fn convert_undo(
    mut record: burst_store_lmdb::block_undo::BlockUndo,
) -> Result<burst_store_lmdb::block_undo::BlockUndo, StoreError> {
    use burst_store_lmdb::block_undo::UndoChange;
    use std::collections::BTreeMap;
    fn entries(bytes: Option<&[u8]>) -> Result<BTreeMap<String, Vec<u8>>, StoreError> {
        let mut entries = BTreeMap::new();
        if let Some(bytes) = bytes {
            TrstEngine::visit_wallet_snapshot_records(bytes, |record| {
                let (key, value) = record.storage_entry()?;
                if entries.insert(key, value).is_some() {
                    return Err("duplicate TRST migration key".into());
                }
                Ok(())
            })
            .map_err(invalid)?;
        }
        Ok(entries)
    }
    let mut changes = BTreeMap::new();
    for change in record.changes {
        let expanded =
            if change.table == UndoTable::Meta && change.key == TrstEngine::meta_key().as_bytes() {
                let mut before = entries(change.before.as_deref())?;
                let after = entries(change.after.as_deref())?;
                let mut expanded = Vec::new();
                for (key, value) in after {
                    let previous = before.remove(&key);
                    if previous.as_ref() != Some(&value) {
                        expanded.push(UndoChange {
                            table: UndoTable::Meta,
                            key: key.into_bytes(),
                            before: previous,
                            after: Some(value),
                        });
                    }
                }
                expanded.extend(before.into_iter().map(|(key, value)| UndoChange {
                    table: UndoTable::Meta,
                    key: key.into_bytes(),
                    before: Some(value),
                    after: None,
                }));
                expanded
            } else {
                vec![change]
            };
        for change in expanded {
            if changes
                .insert((change.table, change.key.clone()), change)
                .is_some()
            {
                return Err(invalid("conflicting aggregate and row undo changes"));
            }
        }
    }
    record.changes = changes.into_values().collect();
    Ok(record)
}

#[cfg(test)]
pub(crate) fn stage_rows_for_test(
    batch: &mut LmdbWriteTransaction<'_>,
    engine: &TrstEngine,
) -> Result<(), StoreError> {
    replace_rows(batch, engine)?;
    batch.delete_meta(TrstEngine::meta_key())?;
    batch.put_meta(FORMAT_KEY, &[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_store_lmdb::LmdbStore;
    use burst_types::{BlockHash, Timestamp, TxHash, WalletAddress};

    fn fixture() -> TrstEngine {
        let mut engine = TrstEngine::with_expiry(100);
        let owner = WalletAddress::new("brst_owner");
        let token = engine
            .mint(
                TxHash::new([1; 32]),
                owner.clone(),
                50,
                owner,
                Timestamp::new(100),
            )
            .unwrap();
        engine.track_token(token);
        engine
    }

    // Not a production migration: historical aggregate undo images still need
    // conversion before row-format activation can be exposed to operators.
    fn stage_fixture_conversion(
        batch: &mut LmdbWriteTransaction<'_>,
        engine: &TrstEngine,
    ) -> Result<(), StoreError> {
        stage_rows_for_test(batch, engine)
    }

    #[test]
    fn row_conversion_and_replacement_are_atomic_and_reopenable() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let engine = fixture();
        let expected = engine.save_wallets();
        let mut batch = store.tx_begin_write().unwrap();
        persist(&mut batch, &engine).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        stage_fixture_conversion(&mut batch, &engine).unwrap();
        assert_eq!(load(&batch, 100).unwrap().save_wallets(), expected);
        drop(batch);
        let mut batch = store.tx_begin_write().unwrap();
        assert!(!row_format(&batch).unwrap());
        stage_fixture_conversion(&mut batch, &engine).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let hash = BlockHash::new([9; 32]);
        batch
            .begin_block_undo(hash, WalletAddress::new("brst_owner"), 1)
            .unwrap();
        let mut account =
            burst_store::account::AccountInfo::unopened(WalletAddress::new("brst_owner"));
        account.head = hash;
        account.block_count = 1;
        batch
            .put_block_with_account(&hash, b"fixture ledger entry", &account.address, 1)
            .unwrap();
        batch.put_frontier(&account.address, &hash).unwrap();
        batch.put_account_info(&account, false).unwrap();
        let mut expired = engine.clone();
        expired.flush_all_expired(Timestamp::new(201));
        persist(&mut batch, &expired).unwrap();
        batch.commit().unwrap();
        let batch = store.tx_begin_write().unwrap();
        let undo = batch.block_undo(&hash).unwrap().unwrap();
        assert_eq!(
            undo.changes
                .iter()
                .filter(|change| change
                    .key
                    .starts_with(WalletRecord::STORAGE_PREFIX.as_bytes()))
                .count(),
            2,
            "only the portfolio cache and holding changed"
        );
        assert_eq!(
            load(&batch, 100).unwrap().save_wallets(),
            expired.save_wallets()
        );
        drop(batch);
        drop(store);
        let reopened = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let batch = reopened.tx_begin_write().unwrap();
        assert_eq!(
            load(&batch, 100).unwrap().save_wallets(),
            expired.save_wallets()
        );
        drop(batch);
        // An independent holder changes after the journaled transition.
        let other = WalletAddress::new("brst_other");
        let token = expired
            .mint(
                TxHash::new([2; 32]),
                other.clone(),
                70,
                other,
                Timestamp::new(201),
            )
            .unwrap();
        expired.track_token(token.clone());
        let mut batch = reopened.tx_begin_write().unwrap();
        persist(&mut batch, &expired).unwrap();
        batch.commit().unwrap();
        let mut batch = reopened.tx_begin_write().unwrap();
        batch.rewind_block(&hash).unwrap();
        let mut expected = engine;
        expected
            .mint(
                token.id,
                token.origin_wallet.clone(),
                token.amount,
                token.holder.clone(),
                token.origin_timestamp,
            )
            .unwrap();
        expected.track_token(token);
        assert_eq!(
            load(&batch, 100).unwrap().save_wallets(),
            expected.save_wallets()
        );
        batch.commit().unwrap();
    }

    #[test]
    fn mixed_or_unmarked_rows_are_not_silently_ignored() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let engine = fixture();
        let mut batch = store.tx_begin_write().unwrap();
        persist(&mut batch, &engine).unwrap();
        replace_rows(&mut batch, &engine).unwrap();
        assert!(load(&batch, 100).is_err());
        batch.put_meta(FORMAT_KEY, &[1]).unwrap();
        assert!(load(&batch, 100).is_err());
        batch.delete_meta(TrstEngine::meta_key()).unwrap();
        assert!(load(&batch, 100).is_ok());
        batch.put_meta(FORMAT_KEY, &[2]).unwrap();
        assert!(load(&batch, 100).is_err());
    }

    #[test]
    fn delta_writes_only_changed_rows_and_rejects_a_stale_baseline() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let mut before = fixture();
        let unrelated = WalletAddress::new("brst_unrelated");
        before
            .wallets
            .insert(unrelated.clone(), burst_trst::Shared::default());
        let (key, original) = WalletRecord::Portfolio {
            holder: unrelated,
            cached_transferable: 0,
            earliest_expiry: None,
        }
        .storage_entry()
        .unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        stage_rows_for_test(&mut batch, &before).unwrap();
        // A sentinel proves the delta neither reads nor rewrites an untouched
        // portfolio. Restore it before validating the whole stored state.
        batch.put_meta(&key, b"untouched").unwrap();
        batch.commit().unwrap();
        let mut after = before.clone();
        after.flush_all_expired(Timestamp::new(201));
        let hash = BlockHash::new([77; 32]);
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .begin_block_undo(hash, WalletAddress::new("brst_owner"), 1)
            .unwrap();
        persist_delta(&mut batch, &before, &after).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.block_undo(&hash).unwrap().unwrap().changes.len(), 2);
        assert_eq!(
            batch.get_meta(&key).unwrap().as_deref(),
            Some(b"untouched".as_slice())
        );
        batch.put_meta(&key, &original).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert!(persist_delta(&mut batch, &before, &after).is_err());
        drop(batch);
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(
            load(&batch, 100).unwrap().save_wallets(),
            after.save_wallets()
        );
    }

    #[test]
    fn legacy_journal_conversion_preserves_rollback_across_governed_expiry() {
        let directory = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let before = fixture();
        let mut batch = store.tx_begin_write().unwrap();
        persist(&mut batch, &before).unwrap();
        batch.commit().unwrap();
        let hash = BlockHash::new([8; 32]);
        let owner = WalletAddress::new("brst_owner");
        let mut batch = store.tx_begin_write().unwrap();
        batch.begin_block_undo(hash, owner.clone(), 1).unwrap();
        let mut account = burst_store::account::AccountInfo::unopened(owner);
        account.head = hash;
        account.block_count = 1;
        batch
            .put_block_with_account(&hash, b"fixture ledger entry", &account.address, 1)
            .unwrap();
        batch.put_frontier(&account.address, &hash).unwrap();
        batch.put_account_info(&account, false).unwrap();
        let mut after = before.clone();
        after.set_expiry_period(200, Timestamp::new(150));
        persist(&mut batch, &after).unwrap();
        batch.commit().unwrap();
        let batch = store.tx_begin_write().unwrap();
        let original = batch.block_undo(&hash).unwrap().unwrap();
        let converted = convert_undo(original.clone()).unwrap();
        let unchanged = |record: &burst_store_lmdb::block_undo::BlockUndo| {
            record
                .changes
                .iter()
                .filter(|change| change.table != UndoTable::Meta)
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(unchanged(&original), unchanged(&converted));
        assert!(!converted
            .changes
            .iter()
            .any(|change| change.key == TrstEngine::meta_key().as_bytes()));
        drop(batch);
        {
            let mut batch = store.tx_begin_write().unwrap();
            assert!(!row_format(&batch).unwrap());
            stage_fixture_conversion(&mut batch, &after).unwrap();
            batch
                .replace_checkpoint_undo(std::slice::from_ref(&converted))
                .unwrap();
            assert_eq!(
                load(&batch, 200).unwrap().save_wallets(),
                after.save_wallets()
            );
            drop(batch);
            let batch = store.tx_begin_write().unwrap();
            assert_eq!(batch.block_undo(&hash).unwrap().unwrap(), original);
            assert_eq!(
                load(&batch, 200).unwrap().save_wallets(),
                after.save_wallets()
            );
        }
        assert!(migrate(&store, 200).unwrap());
        assert!(!migrate(&store, 200).unwrap(), "migration is idempotent");
        drop(store);
        let reopened = LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
        let mut batch = reopened.tx_begin_write().unwrap();
        batch.rewind_block(&hash).unwrap();
        assert_eq!(
            load(&batch, 100).unwrap().save_wallets(),
            before.save_wallets()
        );
        assert!(
            load(&batch, 200).is_err(),
            "old cache must not be rewritten to the new period"
        );
        batch.commit().unwrap();
    }
}
