use super::*;

/// No whole-graph old-key inventory. Only changed rows and newly extended
/// parents are read. The caller must abort on error, before publishing engines.
pub(crate) fn persist_delta(
    batch: &mut LmdbWriteTransaction<'_>,
    before: &MergerGraph,
    after: &MergerGraph,
) -> Result<(), StoreError> {
    if !row_format(batch)? {
        return persist(batch, after);
    }
    let mut highest = HashMap::<String, u64>::new();
    after
        .visit_record_delta(before, |old, mut new| {
            let old = old.map(|record| record.storage_entry()).transpose()?;
            let key = if let Some(record) = &new {
                record.storage_entry()?.0
            } else {
                old.as_ref().ok_or("empty graph delta")?.0.clone()
            };
            if old.as_ref().is_some_and(|(old_key, _)| *old_key != key) {
                return Err("graph delta changes its primary key".into());
            }
            let stored = batch.get_meta(&key).map_err(|error| error.to_string())?;
            if stored.as_deref() != old.as_ref().map(|(_, bytes)| bytes.as_slice()) {
                return Err("graph delta conflicts with persisted state".into());
            }
            if let Some(record) = &mut new {
                if let Some((group, _)) = edge(record) {
                    if old.is_some() {
                        return Err("ordinary graph delta modifies an existing edge".into());
                    }
                    let position = if let Some(highest) = highest.get(&group) {
                        highest
                            .checked_add(1)
                            .ok_or("graph edge ordering exhausted")?
                    } else {
                        let mut maximum: Option<u64> = None;
                        for row in batch
                            .checkpoint_metadata_entries(group.as_bytes())
                            .map_err(|error| error.to_string())?
                        {
                            let (key, bytes) = row.map_err(|error| error.to_string())?;
                            let key =
                                std::str::from_utf8(key).map_err(|error| error.to_string())?;
                            let stored = GraphRecord::from_storage_entry(key, bytes)?;
                            let (stored_group, position) =
                                edge(&stored).ok_or("invalid graph edge namespace")?;
                            if stored_group != group {
                                return Err("invalid graph edge parent".into());
                            }
                            maximum = Some(maximum.map_or(position, |old| old.max(position)));
                        }
                        maximum.map_or(Ok(0), |value| {
                            value.checked_add(1).ok_or("graph edge ordering exhausted")
                        })?
                    };
                    highest.insert(group, position);
                    set_position(record, position);
                }
                let (_, bytes) = record.storage_entry()?;
                batch
                    .put_meta(&key, &bytes)
                    .map_err(|error| error.to_string())
            } else {
                batch.delete_meta(&key).map_err(|error| error.to_string())
            }
        })
        .map_err(invalid)
}
