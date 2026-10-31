//! Verification state read and staged within an admission or recovery transaction.
use burst_store::StoreError;
use burst_store_lmdb::{block_undo::UndoTable, LmdbWriteTransaction};
use burst_verification::{OrchestratorSnapshot, VerificationOrchestrator};

pub(crate) fn load(
    batch: &LmdbWriteTransaction<'_>,
) -> Result<VerificationOrchestrator, StoreError> {
    if batch.uses_verification_records()? {
        return batch.read_verification_records();
    }
    let bytes = batch
        .undo_value_ref(UndoTable::Meta, crate::endorsement_commit::SNAPSHOT_KEY.as_bytes())?
        .ok_or_else(|| StoreError::Corruption(
            "missing persisted verification orchestrator; authenticated replay required".into(),
        ))?;
    OrchestratorSnapshot::decode(bytes)
        .map(VerificationOrchestrator::restore)
        .map_err(|error| StoreError::Corruption(format!(
            "invalid persisted verification orchestrator: {error}",
        )))
}

pub(crate) fn load_view(
    batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
) -> Result<VerificationOrchestrator, StoreError> {
    if batch.uses_verification_records()? {
        return batch.read_verification_records();
    }
    let bytes = batch
        .undo_value_ref(UndoTable::Meta, crate::endorsement_commit::SNAPSHOT_KEY.as_bytes())?
        .ok_or_else(|| StoreError::Corruption(
            "missing persisted verification orchestrator; authenticated replay required".into(),
        ))?;
    OrchestratorSnapshot::decode(bytes)
        .map(VerificationOrchestrator::restore)
        .map_err(|error| StoreError::Corruption(format!(
            "invalid persisted verification orchestrator: {error}",
        )))
}

/// Stage changes without committing. The caller aborts the transaction on error.
pub(crate) fn persist(
    batch: &mut LmdbWriteTransaction<'_>,
    before: &VerificationOrchestrator,
    after: &VerificationOrchestrator,
) -> Result<(), StoreError> {
    if batch.uses_verification_records()? {
        batch.apply_verification_record_delta(before, after)
    } else {
        let bytes = after.snapshot().encode()
            .map_err(|error| StoreError::Serialization(error.to_string()))?;
        batch.put_meta(crate::endorsement_commit::SNAPSHOT_KEY, &bytes)
    }
}

/// Expand aggregate undo images into row overrides for recovery reads.
/// The budget counts encoded inputs and cumulative changed key/value bytes,
/// not decoded heap use. Failure leaves the overlay and byte counter unchanged.
pub(crate) fn overlay_snapshot_delta(
    normalized: bool,
    after: Option<&[u8]>,
    before: Option<&[u8]>,
    values: &mut std::collections::BTreeMap<(UndoTable, Vec<u8>), Option<Vec<u8>>>,
    restored_bytes: &mut usize,
    max_bytes: usize,
) -> Result<(), StoreError> {
    fn rows(bytes: Option<&[u8]>, limit: usize, normalized: bool)
        -> Result<std::collections::BTreeMap<String, Vec<u8>>, StoreError>
    {
        let mut rows = std::collections::BTreeMap::new();
        if let Some(bytes) = bytes {
            if bytes.len() > limit.min(64 << 20) {
                return Err(StoreError::Corruption("verification undo snapshot exceeds limit".into()));
            }
            let snapshot = OrchestratorSnapshot::decode(bytes)
                .map_err(|error| StoreError::Corruption(error.to_string()))?;
            let engine = VerificationOrchestrator::restore(snapshot);
            let mut insert = |key, bytes| { rows.insert(key, bytes); Ok(()) };
            if normalized {
                burst_verification::orchestrator::records::normalized::visit_state_rows(&engine, insert)
            } else {
                engine.visit_records(|record| {
                    let (key, bytes) = record.storage_entry()?;
                    insert(key, bytes)
                })
            }.map_err(StoreError::Corruption)?;
        }
        Ok(rows)
    }
    let old = rows(after, max_bytes, normalized)?;
    let new = rows(before, max_bytes, normalized)?;
    apply_overlay_rows(old, new, values, restored_bytes, max_bytes)
}

fn apply_overlay_rows(
    old: std::collections::BTreeMap<String, Vec<u8>>,
    mut new: std::collections::BTreeMap<String, Vec<u8>>,
    values: &mut std::collections::BTreeMap<(UndoTable, Vec<u8>), Option<Vec<u8>>>,
    restored_bytes: &mut usize, max_bytes: usize,
) -> Result<(), StoreError> {
    let mut delta = Vec::new();
    for (key, bytes) in old {
        let value = new.remove(&key);
        if value.as_ref() != Some(&bytes) {
            delta.push((key, value));
        }
    }
    delta.extend(new.into_iter().map(|(key, value)| (key, Some(value))));
    let mut size = *restored_bytes;
    for (key, value) in &delta {
        size = size.checked_add(key.len())
            .and_then(|size| size.checked_add(value.as_ref().map_or(0, Vec::len)))
            .ok_or_else(|| StoreError::Corruption("verification overlay size overflow".into()))?;
        if size > max_bytes {
            return Err(StoreError::Corruption("rollback verification overlay exceeds byte limit".into()));
        }
    }
    for (key, value) in delta {
        values.insert((UndoTable::Meta, key.into_bytes()), value);
    }
    *restored_bytes = size;
    Ok(())
}

/// A retained v1 target journal owns only its target, not the whole state.
pub(crate) fn overlay_target_delta(
    key: &str, after: Option<&[u8]>, before: Option<&[u8]>,
    values: &mut std::collections::BTreeMap<(UndoTable, Vec<u8>), Option<Vec<u8>>>,
    restored_bytes: &mut usize, max_bytes: usize,
) -> Result<(), StoreError> {
    use burst_verification::orchestrator::records::{normalized, VerificationRecord};
    normalized::legacy_target_address(key).map_err(StoreError::Corruption)?;
    let rows = |value: Option<&[u8]>| {
        value.map(|bytes| {
            let record = VerificationRecord::from_storage_entry(key, bytes)?;
            normalized::target_rows(&record)
        }).transpose().map(Option::unwrap_or_default).map_err(StoreError::Corruption)
    };
    apply_overlay_rows(rows(after)?, rows(before)?, values, restored_bytes, max_bytes)
}

/// Run after authenticated recovery and before any admission worker starts.
/// A sync error after commit does not roll back the format conversion.
pub(crate) fn migrate(store: &burst_store_lmdb::LmdbStore) -> Result<bool, StoreError> {
    let mut batch = store.tx_begin_write()?;
    let migrated = batch.normalize_verification_records()?;
    // Commit the validated cache even when the database was already migrated.
    // The first admitted block can then share its durable roots.
    batch.read_verification_records()?;
    batch.commit()?;
    if migrated {
        store.force_sync().map_err(|error| StoreError::Corruption(error.to_string()))?;
    }
    Ok(migrated)
}

pub(crate) fn is_state_key(key: &str) -> bool {
    key == crate::endorsement_commit::SNAPSHOT_KEY
        || key == "verification_records_format"
        || key.starts_with(burst_verification::orchestrator::records::VerificationRecord::STORAGE_PREFIX)
        || key.starts_with(burst_verification::orchestrator::records::normalized::PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn aggregate_overlay_reconstructs_rows_and_enforces_budget_atomically() {
        for normalized in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = burst_store_lmdb::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut before = VerificationOrchestrator::new();
        let first = crate::genesis_key::genesis_address(burst_types::NetworkId::Dev);
        before.initialize_genesis(&first);
        let before_bytes = before.snapshot().encode().unwrap();
        let mut after = before.storage_clone();
        let second = burst_crypto::derive_address(&burst_types::PublicKey([31; 32]));
        after.initialize_genesis(&second);
        let after_bytes = after.snapshot().encode().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(crate::endorsement_commit::SNAPSHOT_KEY, &after_bytes).unwrap();
        batch.migrate_verification_records().unwrap();
        if normalized { batch.normalize_verification_records().unwrap(); }
        let mut overlay = BTreeMap::new();
        let mut size = 0;
        overlay_snapshot_delta(normalized, Some(&after_bytes), Some(&before_bytes), &mut overlay, &mut size, 64 << 20).unwrap();
        assert!(size > 0);
        assert_eq!(batch.verification_undo_snapshot_with_overlay(&overlay, 64 << 20).unwrap(), Some(before_bytes.clone()));
        // Reversing the change updates the same overlay keys.
        overlay_snapshot_delta(normalized, Some(&before_bytes), Some(&after_bytes), &mut overlay, &mut size, 64 << 20).unwrap();
        assert_eq!(batch.verification_undo_snapshot_with_overlay(&overlay, 64 << 20).unwrap(), Some(after_bytes.clone()));
        let saved = overlay.clone();
        let saved_size = size;
        assert!(overlay_snapshot_delta(normalized, Some(&after_bytes), Some(&before_bytes), &mut overlay, &mut size, saved_size).is_err());
        assert_eq!(overlay, saved);
        assert_eq!(size, saved_size);
        assert!(overlay_snapshot_delta(normalized, Some(b"invalid"), None, &mut overlay, &mut size, 64 << 20).is_err());
        assert_eq!(overlay, saved);
        assert_eq!(size, saved_size);
        }
    }
}
