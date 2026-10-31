//! Rate-format migration after startup has authenticated and recovered economics.
use bincode::Options;
use burst_brn::{rate_storage, RateHistory};
use burst_store::StoreError;
use burst_store_lmdb::{block_undo::{BlockUndo, UndoChange, UndoTable}, LmdbStore};
use std::collections::BTreeMap;

fn invalid(reason: impl ToString) -> StoreError { StoreError::Corruption(reason.to_string()) }

fn decode(bytes: Option<&[u8]>) -> Result<Option<RateHistory>, StoreError> {
    bytes.map(|bytes| {
        bincode::DefaultOptions::new().with_fixint_encoding().with_limit(64 << 20)
            .reject_trailing_bytes().deserialize(bytes).map_err(invalid)
    }).transpose()
}

fn convert_undo(mut record: BlockUndo) -> Result<BlockUndo, StoreError> {
    let mut changes = BTreeMap::new();
    for change in record.changes {
        if change.table == UndoTable::BrnMeta && change.key.starts_with(b"rate_history/") {
            return Err(invalid("aggregate BRN migration contains rate row images"));
        }
        let expanded = if change.table == UndoTable::BrnMeta && change.key == b"rate_history" {
            let before = decode(change.before.as_deref())?;
            let after = decode(change.after.as_deref())?;
            let mut old = before.as_ref().map(rate_storage::encode).transpose().map_err(invalid)?
                .into_iter().flatten();
            let mut new = after.as_ref().map(rate_storage::encode).transpose().map_err(invalid)?
                .into_iter().flatten();
            let mut expanded = Vec::new();
            let before_count = before.as_ref().map(|rates| (rates.segments().len() as u64).to_be_bytes().to_vec());
            let after_count = after.as_ref().map(|rates| (rates.segments().len() as u64).to_be_bytes().to_vec());
            if before_count != after_count {
                expanded.push(UndoChange { table: UndoTable::BrnMeta, key: rate_storage::HEADER.to_vec(),
                    before: before_count, after: after_count });
            }
            loop {
                let previous = old.next();
                let next = new.next();
                let Some(key) = previous.as_ref().or(next.as_ref()).map(|row| row.0.clone()) else { break };
                if next.as_ref().is_some_and(|row| row.0 != key) { return Err(invalid("BRN migration row order differs")); }
                let before = previous.map(|row| row.1.to_vec());
                let after = next.map(|row| row.1.to_vec());
                if before != after { expanded.push(UndoChange { table: UndoTable::BrnMeta, key, before, after }); }
            }
            expanded
        } else { vec![change] };
        for change in expanded {
            if changes.insert((change.table, change.key.clone()), change).is_some() {
                return Err(invalid("conflicting BRN migration undo images"));
            }
        }
    }
    record.changes = changes.into_values().collect();
    Ok(record)
}

/// Complete legacy rate images can be converted without guessing cross-account
/// order. Finality is unchanged; finalized records are never made reversible.
/// A sync failure is reported after commit and does not undo the conversion.
pub(crate) fn migrate(store: &LmdbStore) -> Result<bool, StoreError> {
    let mut batch = store.tx_begin_write()?;
    if batch.uses_brn_rate_records()? { return Ok(false); }
    let engine = batch.read_brn_engine(usize::MAX, usize::MAX)?;
    let mut records = Vec::new();
    let mut bytes = 0u64;
    for account in batch.account_entries()? {
        let account = account?;
        if account.confirmation_height > account.block_count {
            return Err(invalid("BRN migration has invalid confirmation height"));
        }
        for previous in account.confirmation_height..account.block_count {
            if records.len() >= 131_072 { return Err(invalid("BRN migration exceeds provisional journal budget")); }
            let hash = batch.block_at_height(&account.address, previous + 1)?
                .ok_or_else(|| invalid("BRN migration requires complete provisional indexes"))?;
            let record = batch.block_undo(&hash)?
                .ok_or_else(|| invalid("BRN migration requires recovered provisional journals"))?;
            let record = convert_undo(record)?;
            bytes = bytes.checked_add(bincode::serialized_size(&record).map_err(invalid)?)
                .filter(|bytes| *bytes <= 64 << 20)
                .ok_or_else(|| invalid("BRN migration exceeds provisional journal byte budget"))?;
            records.push(record);
        }
    }
    if records.is_empty() { batch.verify_checkpoint_undo_encoding()?; }
    batch.put_brn_rate_history(&engine.rate_history)?;
    if !records.is_empty() { batch.replace_checkpoint_undo(&records)?; }
    batch.commit()?;
    store.force_sync().map_err(invalid)?;
    Ok(true)
}

#[cfg(test)]
mod tests;
