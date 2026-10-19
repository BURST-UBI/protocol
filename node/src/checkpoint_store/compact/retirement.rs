use super::*;
use burst_store_lmdb::LmdbWriteTransaction;

/// Called after semantic replay and installed-parent checks, under the writer
/// that will persist the new certificate and head. This does not authenticate
/// records, commit, create a snapshot or erase original bytes.
pub(super) fn make_room(
    batch: &mut LmdbWriteTransaction<'_>,
    anchor: &CheckpointAnchor,
    tip: u64,
    new_bytes: usize,
) -> Result<(), StoreError> {
    if new_bytes as u64 > MAX_RECORD_BYTES {
        return Err(invalid("new checkpoint exceeds record byte limit"));
    }
    let mut used = new_bytes
        .checked_add(MAX_BEACON_BYTES)
        .ok_or_else(|| invalid("checkpoint chain size overflow"))?;
    let record_prefix = format!("{}/", prefix(anchor));
    let mut retained = Vec::new();
    let mut retained_bytes = 0usize;
    batch.visit_checkpoint_metadata(record_prefix.as_bytes(), |key, record| {
        let key = std::str::from_utf8(key).map_err(invalid)?;
        let suffix = key
            .strip_prefix(&record_prefix)
            .ok_or_else(|| invalid("invalid checkpoint key"))?;
        if suffix == "head" {
            return Ok(());
        }
        let height: u64 = suffix.parse().map_err(invalid)?;
        if height == 0 || height > tip || suffix != height.to_string() {
            return Err(invalid("invalid retained checkpoint height"));
        }
        if retained.len() as u64 >= MAX_CHECKPOINTS {
            return Err(invalid("checkpoint inventory exceeds active record limit"));
        }
        if record.len() as u64 > MAX_RECORD_BYTES {
            return Err(invalid("retained checkpoint exceeds record byte limit"));
        }
        retained_bytes = retained_bytes
            .checked_add(record.len())
            .filter(|size| *size <= MAX_CHAIN_BYTES)
            .ok_or_else(|| invalid("checkpoint inventory exceeds active byte limit"))?;
        retained.push((height, key.to_owned(), record.len()));
        used = used
            .checked_add(record.len())
            .and_then(|size| size.checked_add(MAX_BEACON_BYTES))
            .ok_or_else(|| invalid("checkpoint chain size overflow"))?;
        Ok(())
    })?;
    retained.sort_unstable_by_key(|(height, _, _)| *height);
    let mut count = retained.len() as u64 + 1;
    for (height, key, size) in retained {
        if count <= MAX_CHECKPOINTS && used <= MAX_CHAIN_BYTES {
            break;
        }
        let record = batch
            .get_meta(&key)?
            .ok_or_else(|| invalid("checkpoint disappeared during retirement"))?;
        let archived = archived_record_key(anchor, height);
        match batch.get_meta(&archived)? {
            Some(existing) if existing != record => {
                return Err(invalid(
                    "retired checkpoint archive conflicts with local record",
                ));
            }
            Some(_) => (),
            None => batch.put_meta(&archived, &record)?,
        }
        batch.delete_meta(&key)?;
        used -= size + MAX_BEACON_BYTES;
        count -= 1;
    }
    if used > MAX_CHAIN_BYTES {
        return Err(invalid("new checkpoint exceeds active byte limit"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
