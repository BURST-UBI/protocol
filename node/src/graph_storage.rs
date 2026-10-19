//! Graph rows share the economic writer and its block undo journal.
use burst_store::StoreError;
use burst_store_lmdb::{block_undo::UndoTable, LmdbWriteTransaction};
use burst_trst::{merger_graph::GraphRecord, MergerGraph};
use std::collections::{BTreeSet, HashMap};
mod delta;
pub(crate) use delta::persist_delta;

pub(crate) const FORMAT_KEY: &str = "trst_graph_format";
const LEGACY_KEY: &str = "merger_graph";

fn invalid(reason: impl ToString) -> StoreError {
    StoreError::Corruption(reason.to_string())
}

pub(crate) fn row_format(batch: &LmdbWriteTransaction<'_>) -> Result<bool, StoreError> {
    match batch.get_meta(FORMAT_KEY)?.as_deref() {
        None => {
            if let Some(row) = batch
                .checkpoint_metadata_entries(GraphRecord::STORAGE_PREFIX.as_bytes())?
                .next()
            {
                row?;
                return Err(invalid("graph rows exist without a format marker"));
            }
            Ok(false)
        }
        Some([1]) => {
            if batch
                .undo_value_ref(UndoTable::Meta, LEGACY_KEY.as_bytes())?
                .is_some()
            {
                return Err(invalid("mixed aggregate and row graph state"));
            }
            Ok(true)
        }
        Some(_) => Err(invalid("unsupported merger graph storage format")),
    }
}

pub(crate) fn load(batch: &LmdbWriteTransaction<'_>) -> Result<MergerGraph, StoreError> {
    if !row_format(batch)? {
        let bytes = batch
            .undo_value_ref(UndoTable::Meta, LEGACY_KEY.as_bytes())?
            .ok_or_else(|| invalid("missing persisted merger graph"))?;
        return MergerGraph::from_bytes(bytes).map_err(invalid);
    }
    MergerGraph::from_records(
        batch
            .checkpoint_metadata_entries(GraphRecord::STORAGE_PREFIX.as_bytes())?
            .map(|row| {
                let (key, bytes) = row.map_err(|error| error.to_string())?;
                let key = std::str::from_utf8(key).map_err(|error| error.to_string())?;
                GraphRecord::from_storage_entry(key, bytes)
            }),
    )
    .map_err(invalid)
}

pub(crate) fn present(batch: &LmdbWriteTransaction<'_>) -> Result<bool, StoreError> {
    Ok(row_format(batch)?
        || batch
            .undo_value_ref(UndoTable::Meta, LEGACY_KEY.as_bytes())?
            .is_some())
}

fn edge(record: &GraphRecord) -> Option<(String, u64)> {
    match record {
        GraphRecord::OriginEdge {
            origin, position, ..
        } => Some((
            format!("{}3/{origin}/", GraphRecord::STORAGE_PREFIX),
            *position,
        )),
        GraphRecord::DownstreamEdge {
            merge, position, ..
        } => Some((
            format!("{}4/{merge}/", GraphRecord::STORAGE_PREFIX),
            *position,
        )),
        _ => None,
    }
}

fn set_position(record: &mut GraphRecord, value: u64) {
    match record {
        GraphRecord::OriginEdge { position, .. } | GraphRecord::DownstreamEdge { position, .. } => {
            *position = value
        }
        _ => unreachable!("position belongs to a reverse edge"),
    }
}

/// Ordinary graph publication preserves durable ordering slots for every
/// surviving reverse edge. Removing one consumer must not renumber another
/// block's journal. The caller aborts the whole writer on any error.
pub(crate) fn persist(
    batch: &mut LmdbWriteTransaction<'_>,
    graph: &MergerGraph,
) -> Result<(), StoreError> {
    if !row_format(batch)? {
        return batch.put_meta(LEGACY_KEY, &graph.to_bytes());
    }
    write_rows(batch, graph, true)
}

/// Certified replay replaces the graph and every provisional journal in the
/// same transaction. Fresh ordering slots must match the replay-derived rows;
/// this must not be used for ordinary publication with surviving journals.
pub(crate) fn stage_replayed(
    batch: &mut LmdbWriteTransaction<'_>,
    graph: &MergerGraph,
) -> Result<(), StoreError> {
    row_format(batch)?;
    for row in batch.checkpoint_metadata_entries(GraphRecord::STORAGE_PREFIX.as_bytes())? {
        let (key, bytes) = row?;
        let key = std::str::from_utf8(key).map_err(invalid)?;
        GraphRecord::from_storage_entry(key, bytes).map_err(invalid)?;
    }
    batch.clear_checkpoint_token_rows(GraphRecord::STORAGE_PREFIX.as_bytes())?;
    graph.visit_records(|record| {
        let (key, bytes) = record.storage_entry().map_err(invalid)?;
        batch.put_meta(&key, &bytes)
    })?;
    batch.delete_meta(LEGACY_KEY)?;
    batch.put_meta(FORMAT_KEY, &[1])
}

fn write_rows(
    batch: &mut LmdbWriteTransaction<'_>,
    graph: &MergerGraph,
    preserve_slots: bool,
) -> Result<(), StoreError> {
    let mut old_keys = BTreeSet::new();
    let mut highest = HashMap::<String, u64>::new();
    for row in batch.checkpoint_metadata_entries(GraphRecord::STORAGE_PREFIX.as_bytes())? {
        let (key, bytes) = row?;
        let key = std::str::from_utf8(key).map_err(invalid)?;
        let record = GraphRecord::from_storage_entry(key, bytes).map_err(invalid)?;
        if let Some((group, position)) = edge(&record) {
            highest
                .entry(group)
                .and_modify(|old| *old = (*old).max(position))
                .or_insert(position);
        }
        old_keys.insert(key.to_owned());
    }
    let mut previous = HashMap::<String, u64>::new();
    graph.visit_records(|mut record| {
        let (key, _) = record.storage_entry().map_err(invalid)?;
        let old = batch.get_meta(&key)?;
        if preserve_slots {
            if let Some((group, _)) = edge(&record) {
                let position = if let Some(bytes) = &old {
                    let old = GraphRecord::from_storage_entry(&key, bytes).map_err(invalid)?;
                    edge(&old)
                        .ok_or_else(|| invalid("graph edge changed record type"))?
                        .1
                } else {
                    highest.get(&group).map_or(Ok(0), |position| {
                        position
                            .checked_add(1)
                            .ok_or_else(|| invalid("graph edge ordering exhausted"))
                    })?
                };
                if previous
                    .insert(group.clone(), position)
                    .is_some_and(|prior| prior >= position)
                {
                    return Err(invalid("ordinary graph write reorders existing consumers"));
                }
                highest
                    .entry(group)
                    .and_modify(|old| *old = (*old).max(position))
                    .or_insert(position);
                set_position(&mut record, position);
            }
        }
        let (_, bytes) = record.storage_entry().map_err(invalid)?;
        old_keys.remove(&key);
        if old.as_deref() != Some(bytes.as_slice()) {
            batch.put_meta(&key, &bytes)?;
        }
        Ok::<_, StoreError>(())
    })?;
    for key in old_keys {
        batch.delete_meta(&key)?;
    }
    Ok(())
}

pub(crate) fn convert_undo(
    batch: &LmdbWriteTransaction<'_>,
    mut journal: burst_store_lmdb::block_undo::BlockUndo,
) -> Result<burst_store_lmdb::block_undo::BlockUndo, StoreError> {
    use burst_store_lmdb::block_undo::UndoChange;
    use std::collections::BTreeMap;
    fn entries(bytes: Option<&[u8]>) -> Result<BTreeMap<String, Vec<u8>>, StoreError> {
        let mut entries = BTreeMap::new();
        if let Some(bytes) = bytes {
            let graph = MergerGraph::from_bytes(bytes).map_err(invalid)?;
            graph.visit_records(|mut record| {
                // Historical snapshot positions can differ after an unrelated
                // rollback. Edge identity, not that old position, is the change.
                if edge(&record).is_some() {
                    set_position(&mut record, 0);
                }
                let (key, value) = record.storage_entry().map_err(invalid)?;
                if entries.insert(key, value).is_some() {
                    return Err(invalid("duplicate graph migration key"));
                }
                Ok::<_, StoreError>(())
            })?;
        }
        Ok(entries)
    }
    let mut changes = BTreeMap::new();
    for change in journal.changes {
        let expanded = if change.table == UndoTable::Meta && change.key == LEGACY_KEY.as_bytes() {
            let mut before = entries(change.before.as_deref())?;
            let after = entries(change.after.as_deref())?;
            let mut expanded = Vec::new();
            for (key, mut value) in after {
                let previous = before.remove(&key);
                if previous.as_ref() == Some(&value) {
                    continue;
                }
                let record = GraphRecord::from_storage_entry(&key, &value).map_err(invalid)?;
                if edge(&record).is_some() {
                    if previous.is_some() {
                        return Err(invalid("legacy journal changes existing graph edge"));
                    }
                    let installed = batch.get_meta(&key)?.ok_or_else(|| {
                        invalid("journal graph edge is absent from installed state")
                    })?;
                    let mut normalized =
                        GraphRecord::from_storage_entry(&key, &installed).map_err(invalid)?;
                    set_position(&mut normalized, 0);
                    if normalized.storage_entry().map_err(invalid)?.1 != value {
                        return Err(invalid("journal graph edge differs from installed state"));
                    }
                    value = installed;
                }
                expanded.push(UndoChange {
                    table: UndoTable::Meta,
                    key: key.into_bytes(),
                    before: previous,
                    after: Some(value),
                });
            }
            for (key, value) in before {
                let record = GraphRecord::from_storage_entry(&key, &value).map_err(invalid)?;
                if edge(&record).is_some() {
                    return Err(invalid("legacy journal removes merge ancestry"));
                }
                expanded.push(UndoChange {
                    table: UndoTable::Meta,
                    key: key.into_bytes(),
                    before: Some(value),
                    after: None,
                });
            }
            expanded
        } else {
            vec![change]
        };
        for change in expanded {
            if changes
                .insert((change.table, change.key.clone()), change)
                .is_some()
            {
                return Err(invalid("conflicting aggregate and row graph journal"));
            }
        }
    }
    journal.changes = changes.into_values().collect();
    Ok(journal)
}

/// Called after authenticated startup recovery, before admission workers.
pub(crate) fn migrate(store: &burst_store_lmdb::LmdbStore) -> Result<bool, StoreError> {
    let mut batch = store.tx_begin_write()?;
    if row_format(&batch)? {
        return Ok(false);
    }
    let graph = load(&batch)?;
    write_rows(&mut batch, &graph, false)?;
    let mut journals = Vec::new();
    let mut bytes = 0u64;
    for account in batch.account_entries()? {
        let account = account?;
        if account.confirmation_height > account.block_count {
            return Err(invalid("graph migration has invalid confirmation height"));
        }
        for previous_height in account.confirmation_height..account.block_count {
            if journals.len() >= 131_072 {
                return Err(invalid(
                    "graph migration exceeds provisional journal budget",
                ));
            }
            let hash = batch
                .block_at_height(&account.address, previous_height + 1)?
                .ok_or_else(|| invalid("graph migration requires complete provisional indexes"))?;
            let journal = batch.block_undo(&hash)?.ok_or_else(|| {
                invalid("graph migration requires recovered provisional journals")
            })?;
            let converted = convert_undo(&batch, journal)?;
            bytes = bytes
                .checked_add(bincode::serialized_size(&converted).map_err(invalid)?)
                .filter(|bytes| *bytes <= 64 << 20)
                .ok_or_else(|| {
                    invalid("graph migration exceeds provisional journal byte budget")
                })?;
            journals.push(converted);
        }
    }
    batch.delete_meta(LEGACY_KEY)?;
    batch.put_meta(FORMAT_KEY, &[1])?;
    batch.replace_checkpoint_undo(&journals)?;
    batch.commit()?;
    store.force_sync().map_err(invalid)?;
    Ok(true)
}

#[cfg(test)]
pub(crate) fn stage_rows_for_test(
    batch: &mut LmdbWriteTransaction<'_>,
    graph: &MergerGraph,
) -> Result<(), StoreError> {
    write_rows(batch, graph, false)?;
    batch.delete_meta(LEGACY_KEY)?;
    batch.put_meta(FORMAT_KEY, &[1])
}

#[cfg(test)]
mod tests;
