use super::*;
use bincode::Options;
use burst_store_lmdb::block_undo::UndoTable;
use serde::{de::DeserializeOwned, Serialize};

fn read(
    batch: &LmdbWriteTransaction<'_>,
    table: UndoTable,
    key: &[u8],
) -> Result<Vec<u8>, StoreError> {
    let bytes = batch
        .undo_value(table, key)?
        .ok_or_else(|| invalid("missing applied engine snapshot"))?;
    if bytes.len() > 64 << 20 {
        return Err(invalid("applied engine snapshot exceeds limit"));
    }
    Ok(bytes)
}

fn compare<T: DeserializeOwned + Serialize>(bytes: &[u8], expected: &T) -> Result<(), StoreError> {
    let actual: T = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(invalid)?;
    equivalent(&actual, expected)
}

fn equivalent<T: Serialize>(actual: &T, expected: &T) -> Result<(), StoreError> {
    if super::super::canonical::encode(actual).map_err(invalid)?
        != super::super::canonical::encode(expected).map_err(invalid)?
    {
        return Err(invalid("persisted engine differs from applied replay"));
    }
    Ok(())
}

pub(super) fn verify(
    batch: &LmdbWriteTransaction<'_>,
    state: &crate::confirmed_replay::ConfirmedReplay,
) -> Result<(), StoreError> {
    compare(
        &read(batch, UndoTable::BrnMeta, b"protocol_params")?,
        &state.params,
    )?;
    if read(
        batch,
        UndoTable::BrnMeta,
        burst_verification::bootstrap::BootstrapPhase::KEY,
    )? != state.bootstrap_phase.encoding()
    {
        return Err(invalid(
            "persisted bootstrap phase differs from applied replay",
        ));
    }
    let bytes = read(
        batch,
        UndoTable::Meta,
        crate::endorsement_commit::SNAPSHOT_KEY.as_bytes(),
    )?;
    let actual = burst_verification::OrchestratorSnapshot::decode(&bytes).map_err(invalid)?;
    equivalent(&actual, &state.verification.snapshot())
        .map_err(|error| invalid(format!("verification snapshot: {error}")))?;
    verify_lifecycle(batch, &state.lifecycle_metadata)?;
    let governance = state
        .governance
        .as_ref()
        .ok_or_else(|| invalid("applied replay lacks governance state"))?;
    compare(
        &read(batch, UndoTable::BrnMeta, b"governance_engine")?,
        &governance.governance,
    )?;
    compare(
        &read(batch, UndoTable::Meta, b"consti_engine")?,
        &governance.constitution,
    )?;
    // The persisted delegation format contains the forward graph. Reverse
    // indexes are derived and key-ballot receipts are checked separately.
    let expected: burst_governance::delegation::DelegationSnapshot =
        bincode::deserialize(&governance.delegation.save_state()).map_err(invalid)?;
    compare(
        &read(
            batch,
            UndoTable::Meta,
            burst_governance::DelegationEngine::meta_key().as_bytes(),
        )?,
        &expected,
    )
}

fn verify_lifecycle(
    batch: &LmdbWriteTransaction<'_>,
    expected: &std::collections::HashMap<String, Vec<u8>>,
) -> Result<(), StoreError> {
    let mut count = 0usize;
    batch.visit_checkpoint_lifecycle(|key, bytes| {
        let key = std::str::from_utf8(key).map_err(invalid)?;
        if expected.get(key).map(Vec::as_slice) != Some(bytes) {
            return Err(invalid(format!(
                "persisted lifecycle entry differs from applied replay: {key}"
            )));
        }
        count += 1;
        Ok(())
    })?;
    if count
        != expected
            .keys()
            .filter(|key| key.as_str() != crate::endorsement_commit::SNAPSHOT_KEY)
            .count()
    {
        return Err(invalid(
            "persisted lifecycle inventory differs from applied replay",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_projection_streams_large_inventory_and_rejects_mismatches() {
        let directory = tempfile::tempdir().unwrap();
        let store = burst_store_lmdb::LmdbStore::open(directory.path(), 32, 64 << 20).unwrap();
        let mut expected = std::collections::HashMap::new();
        let mut batch = store.tx_begin_write().unwrap();
        for index in 0..131_073 {
            let key = format!("verification_receipt/{index:08}");
            batch.put_meta(&key, b"receipt").unwrap();
            expected.insert(key, b"receipt".to_vec());
        }
        // Intermediate orchestrator state is deliberately checked elsewhere.
        expected.insert(crate::endorsement_commit::SNAPSHOT_KEY.into(), vec![1]);
        batch
            .put_meta(crate::endorsement_commit::SNAPSHOT_KEY, &[2])
            .unwrap();
        batch
            .put_meta("checkpoint_round/v1/local", b"journal")
            .unwrap();
        verify_lifecycle(&batch, &expected).unwrap();
        let key = "verification_receipt/00000000";
        expected.insert(key.into(), b"changed".to_vec());
        assert!(verify_lifecycle(&batch, &expected).is_err());
        expected.remove(key);
        assert!(verify_lifecycle(&batch, &expected).is_err());
        expected.insert(key.into(), b"receipt".to_vec());
        expected.insert("verification_receipt/missing".into(), b"receipt".to_vec());
        assert!(verify_lifecycle(&batch, &expected).is_err());
        expected.remove("verification_receipt/missing");
        batch.commit().unwrap();
        store.force_sync().unwrap();
        drop(store);
        let store = burst_store_lmdb::LmdbStore::open(directory.path(), 32, 64 << 20).unwrap();
        verify_lifecycle(&store.tx_begin_write().unwrap(), &expected).unwrap();
    }
}
