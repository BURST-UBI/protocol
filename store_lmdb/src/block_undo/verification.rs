//! Build verification undo images and convert v1 target images to member rows.
//! These helpers do not write storage. Callers migrating records must commit
//! current rows and their journals together.
use super::*;
use burst_verification::orchestrator::records::{normalized, VerificationRecord};
use burst_verification::VerificationOrchestrator;

/// Select verification ownership against the other changes in one reconstructed
/// block. These are local storage images, not authority to replay or roll back.
/// Rule changes or a delta that exceeds the journal budget use complete snapshots
/// instead. Those snapshots must still fit the whole journal's size and row limits.
pub fn project_transition(
    base: &BlockUndo,
    before: &VerificationOrchestrator,
    after: &VerificationOrchestrator,
) -> Result<Vec<UndoChange>, StoreError> {
    if base.changes.iter().any(|row| {
        row.table == UndoTable::Meta
            && (row.key == b"verification_orchestrator"
                || row.key.starts_with(b"verification_state/"))
    }) {
        return Err(invalid("replayed verification has mixed journal ownership"));
    }
    before
        .validate_storage_state()
        .map_err(StoreError::Corruption)?;
    after
        .validate_storage_state()
        .map_err(StoreError::Corruption)?;
    let mut recorder = UndoRecorder::new(base.block, base.account.clone(), base.height);
    for row in &base.changes {
        recorder.record(
            row.table,
            &row.key,
            row.before.as_deref(),
            row.after.as_deref(),
        )?;
    }
    let mut aggregate = before
        .rules_record()
        .storage_entry()
        .map_err(StoreError::Corruption)?
        != after
            .rules_record()
            .storage_entry()
            .map_err(StoreError::Corruption)?;
    let mut pending = Vec::new();
    if !aggregate {
        let scoped = after.storage_replacement_scope(before);
        let mut budget = recorder.budget();
        normalized::visit_state_delta(before, &scoped, |key, old, new| {
            if !aggregate {
                if recorder.preview_meta_change(&mut budget, &key, old.as_deref(), new.as_deref()) {
                    pending.push(UndoChange {
                        table: UndoTable::Meta,
                        key: key.into_bytes(),
                        before: old,
                        after: new,
                    });
                } else {
                    aggregate = true;
                    pending.clear();
                }
            }
            Ok(())
        })
        .map_err(StoreError::Corruption)?;
    }
    if aggregate {
        let encode = |state: &VerificationOrchestrator| -> Result<Vec<u8>, StoreError> {
            let snapshot = state.snapshot();
            if snapshot
                .encoded_size()
                .map_err(|e| invalid(&e.to_string()))?
                > MAX_BYTES as u64
            {
                return Err(invalid("replayed verification snapshot exceeds byte limit"));
            }
            snapshot.encode().map_err(|e| invalid(&e.to_string()))
        };
        pending.push(UndoChange {
            table: UndoTable::Meta,
            key: b"verification_orchestrator".to_vec(),
            before: Some(encode(before)?),
            after: Some(encode(after)?),
        });
    }
    for row in &pending {
        recorder.record(
            row.table,
            &row.key,
            row.before.as_deref(),
            row.after.as_deref(),
        )?;
    }
    Ok(pending)
}

fn target_key(key: &[u8]) -> bool {
    key.starts_with(b"verification_state/v1/1/")
}
fn rows(key: &[u8], bytes: Option<&[u8]>) -> Result<BTreeMap<String, Vec<u8>>, StoreError> {
    let Some(bytes) = bytes else {
        return Ok(BTreeMap::new());
    };
    let key = std::str::from_utf8(key).map_err(|_| invalid("invalid target journal key"))?;
    let record =
        VerificationRecord::from_storage_entry(key, bytes).map_err(StoreError::Corruption)?;
    normalized::target_rows(&record).map_err(StoreError::Corruption)
}

/// Check a canonically decoded journal without expanding its physical changes.
/// The caller validates identity/size/order with BlockUndo::decode first. Retained
/// target images keep their existing 64 KiB codec bound and original conflict scope.
pub(crate) fn validate_retained(journal: &BlockUndo) -> Result<(), StoreError> {
    let has_targets = journal
        .changes
        .iter()
        .any(|c| c.table == UndoTable::Meta && target_key(&c.key));
    if !has_targets {
        return Ok(());
    }
    if journal.changes.iter().any(|c| {
        c.table == UndoTable::Meta
            && (c.key.starts_with(normalized::PREFIX.as_bytes())
                || c.key == b"verification_orchestrator")
    }) {
        return Err(invalid("mixed verification target migration journal"));
    }
    for change in &journal.changes {
        if change.table != UndoTable::Meta || !target_key(&change.key) {
            continue;
        }
        let key =
            std::str::from_utf8(&change.key).map_err(|_| invalid("invalid target journal key"))?;
        normalized::legacy_target_address(key).map_err(StoreError::Corruption)?;
        for bytes in [change.before.as_deref(), change.after.as_deref()]
            .into_iter()
            .flatten()
        {
            let record = VerificationRecord::from_storage_entry(key, bytes)
                .map_err(StoreError::Corruption)?;
            // Validate member identities and keys, retaining
            // only this bounded target's rows, never the expanded whole journal.
            normalized::target_rows(&record).map_err(StoreError::Corruption)?;
        }
    }
    Ok(())
}

/// Expand v1 target images into exact normalized member deltas. Other tables,
/// verifier/rules rows and aggregate journals retain their original bytes. No
/// ledger ordering or authenticated-cut claim follows from a successful conversion.
pub fn convert(journal: BlockUndo) -> Result<BlockUndo, StoreError> {
    if bincode::serialized_size(&journal).map_err(|e| StoreError::Serialization(e.to_string()))?
        > MAX_BYTES as u64
    {
        return Err(invalid("oversized verification migration journal"));
    }
    let bytes =
        bincode::serialize(&journal).map_err(|e| StoreError::Serialization(e.to_string()))?;
    let mut journal = BlockUndo::decode(&journal.block, &bytes)?;
    drop(bytes);
    let has_targets = journal
        .changes
        .iter()
        .any(|c| c.table == UndoTable::Meta && target_key(&c.key));
    if !has_targets {
        return Ok(journal);
    }
    if journal.changes.iter().any(|c| {
        c.table == UndoTable::Meta
            && (c.key.starts_with(normalized::PREFIX.as_bytes())
                || c.key == b"verification_orchestrator")
    }) {
        return Err(invalid("mixed verification target migration journal"));
    }
    let original = std::mem::take(&mut journal.changes);
    let mut encoded_bytes =
        bincode::serialized_size(&journal).map_err(|e| StoreError::Serialization(e.to_string()))?;
    let mut changes = BTreeMap::new();
    let mut add = |change: UndoChange| -> Result<(), StoreError> {
        let size = bincode::serialized_size(&change)
            .map_err(|e| StoreError::Serialization(e.to_string()))?;
        encoded_bytes = encoded_bytes
            .checked_add(size)
            .ok_or_else(|| invalid("converted journal size overflow"))?;
        if encoded_bytes > MAX_BYTES as u64 || changes.len() >= MAX_CHANGES {
            return Err(invalid(
                "converted verification journal exceeds existing limits",
            ));
        }
        if changes
            .insert((change.table, change.key.clone()), change)
            .is_some()
        {
            return Err(invalid("duplicate converted verification journal key"));
        }
        Ok(())
    };
    for change in original {
        if change.table != UndoTable::Meta || !target_key(&change.key) {
            add(change)?;
            continue;
        }
        let mut before = rows(&change.key, change.before.as_deref())?;
        let after = rows(&change.key, change.after.as_deref())?;
        for (key, value) in after {
            let old = before.remove(&key);
            if old.as_ref() != Some(&value) {
                add(UndoChange {
                    table: UndoTable::Meta,
                    key: key.into_bytes(),
                    before: old,
                    after: Some(value),
                })?;
            }
        }
        for (key, value) in before {
            add(UndoChange {
                table: UndoTable::Meta,
                key: key.into_bytes(),
                before: Some(value),
                after: None,
            })?;
        }
    }
    journal.changes = changes.into_values().collect();
    let bytes =
        bincode::serialize(&journal).map_err(|e| StoreError::Serialization(e.to_string()))?;
    BlockUndo::decode(&journal.block, &bytes)
}

#[cfg(test)]
mod tests;
