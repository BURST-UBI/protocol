//! Governance reads from a pinned, caller-owned database view.
use crate::{LmdbEnvironment, LmdbError, LmdbReadTransaction};
use bincode::Options;
use burst_governance::engine::{GovernanceEngine, GovernanceRecord};
use burst_store::StoreError;

fn invalid(reason: impl ToString) -> StoreError {
    StoreError::Corruption(reason.to_string())
}

impl LmdbEnvironment {
    /// Read persisted governance without acquiring the database writer or live
    /// engine locks. The caller can read parameters and electorate in this view.
    pub fn read_governance_snapshot(
        &self,
        view: &LmdbReadTransaction<'_>,
    ) -> Result<Option<GovernanceEngine>, StoreError> {
        let mode = self
            .meta_db
            .get(&view.txn, b"governance_records_format")
            .map_err(LmdbError::from)?;
        let aggregate = self
            .brn_meta_db
            .get(&view.txn, b"governance_engine")
            .map_err(LmdbError::from)?;
        match mode {
            Some([1]) => {
                if aggregate.is_some() {
                    return Err(invalid("mixed aggregate and keyed governance state"));
                }
                let mut rows = self
                    .meta_db
                    .prefix_iter(&view.txn, GovernanceRecord::STORAGE_NAMESPACE.as_bytes())
                    .map_err(LmdbError::from)?
                    .peekable();
                // Keyed mode may intentionally have no engine after rollback.
                match rows.peek() {
                    None => return Ok(None),
                    Some(Err(error)) => return Err(invalid(error)),
                    Some(Ok(_)) => {}
                }
                if let Some(generation) = view.generation {
                    let cache = self.governance_cache.lock().unwrap();
                    if let Some((cached_generation, engine)) = cache.as_ref() {
                        if *cached_generation == generation {
                            return Ok(Some(engine.clone()));
                        }
                    }
                }
                let engine = GovernanceEngine::from_records(rows.map(|row| {
                    let (key, bytes) = row.map_err(|error| error.to_string())?;
                    let key = std::str::from_utf8(key).map_err(|error| error.to_string())?;
                    GovernanceRecord::from_storage_entry(key, bytes)
                }))
                .map_err(invalid)?;
                if let Some(generation) = view.generation {
                    let mut cache = self.governance_cache.lock().unwrap();
                    // A slow old reader must not evict a newer writer/read view.
                    // Retain one generation, sharing the existing writer cache.
                    if cache.as_ref().is_none_or(|(id, _)| *id < generation) {
                        *cache = Some((generation, engine.clone()));
                    }
                }
                Ok(Some(engine))
            }
            Some(_) => Err(invalid("unsupported governance record format")),
            None => {
                for prefix in [
                    GovernanceRecord::STORAGE_NAMESPACE,
                    "governance_revision/v1/",
                ] {
                    if let Some(row) = self
                        .meta_db
                        .prefix_iter(&view.txn, prefix.as_bytes())
                        .map_err(LmdbError::from)?
                        .next()
                    {
                        row.map_err(LmdbError::from)?;
                        return Err(invalid("governance rows exist without a format marker"));
                    }
                }
                let Some(bytes) = aggregate else {
                    return Ok(None);
                };
                if bytes.len() > 64 << 20 {
                    return Err(invalid("governance snapshot exceeds byte limit"));
                }
                bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .with_limit(bytes.len() as u64)
                    .reject_trailing_bytes()
                    .deserialize(bytes)
                    .map(Some)
                    .map_err(invalid)
            }
        }
    }
}
