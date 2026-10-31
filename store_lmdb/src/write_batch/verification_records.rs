//! Verification record persistence, cached reads and historical rollback adapters.
use super::*;
use burst_verification::orchestrator::records::{normalized, VerificationRecord, VerificationRecordReader, VerificationRecordVisitor};
use burst_verification::{OrchestratorSnapshot, VerificationOrchestrator};
use std::collections::BTreeMap;

const MODE: &str = "verification_records_format";
const LEGACY: &[u8] = b"verification_orchestrator";
fn invalid(message: impl ToString) -> StoreError {
    StoreError::Corruption(message.to_string())
}
fn decode(bytes: &[u8]) -> Result<VerificationOrchestrator, StoreError> {
    if bytes.len() > 64 << 20 {
        return Err(invalid("verification snapshot exceeds byte limit"));
    }
    OrchestratorSnapshot::decode(bytes)
        .map(VerificationOrchestrator::restore)
        .map_err(invalid)
}
fn encode_snapshot(engine: &VerificationOrchestrator) -> Result<Vec<u8>, StoreError> {
    let snapshot = engine.snapshot();
    if snapshot.encoded_size().map_err(invalid)? > 64 << 20 {
        return Err(invalid("verification snapshot exceeds byte limit"));
    }
    snapshot.encode().map_err(invalid)
}
fn rows(engine: &VerificationOrchestrator) -> Result<BTreeMap<String, Vec<u8>>, StoreError> {
    let mut rows = BTreeMap::new();
    engine
        .visit_records(|record| {
            let (key, value) = record.storage_entry()?;
            rows.insert(key, value);
            Ok(())
        })
        .map_err(invalid)?;
    Ok(rows)
}
impl LmdbWriteTransaction<'_> {
    pub fn uses_verification_records(&self) -> Result<bool, StoreError> {
        self.read_view().uses_verification_records()
    }

    /// Read verification state including this writer's changes, reusing its cache.
    /// A cache miss reconstructs records under the 64 MiB snapshot budget.
    /// Missing rules are an error.
    pub fn read_verification_records(&self) -> Result<VerificationOrchestrator, StoreError> {
        if !self.uses_verification_records()? {
            return Err(invalid("verification record mode is not initialized"));
        }
        if let Some(engine) = self.auxiliary_cache.borrow().verification.as_ref() {
            return Ok(engine.storage_clone());
        }
        let engine = self.read_verification_overlay(&BTreeMap::new(), 64 << 20)?
            .ok_or_else(|| invalid("missing verification records"))?.storage_clone();
        self.auxiliary_cache.borrow_mut().verification = Some(engine.storage_clone());
        Ok(engine)
    }

    /// Preserve complete historical/global undo images through a bounded virtual
    /// aggregate. Overlay rows take precedence during non-tail rollback preflight.
    pub fn verification_undo_snapshot_with_overlay(
        &self,
        overlay: &BTreeMap<(UndoTable, Vec<u8>), Option<Vec<u8>>>,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        if !self.uses_verification_records()? {
            return Ok(None);
        }
        let limit = max_bytes.min(64 << 20);
        let Some(engine) = self.read_verification_overlay(overlay, limit)? else { return Ok(None); };
        let snapshot = engine.snapshot();
        // Check before allocating the serialized aggregate, including format prefix.
        if snapshot.encoded_size().map_err(invalid)? > limit as u64 {
            return Err(invalid("verification undo snapshot exceeds byte limit"));
        }
        snapshot.encode().map(Some).map_err(invalid)
    }

    pub fn uses_normalized_verification_records(&self) -> Result<bool, StoreError> {
        self.read_view().uses_normalized_verification_records()
    }

    fn verification_rows(&self, engine: &VerificationOrchestrator)
        -> Result<BTreeMap<String, Vec<u8>>, StoreError>
    {
        if !self.uses_normalized_verification_records()? { return rows(engine); }
        let mut rows = BTreeMap::new();
        normalized::visit_state_rows(engine, |key, bytes| { rows.insert(key, bytes); Ok(()) })
            .map_err(invalid)?;
        Ok(rows)
    }

    fn read_verification_overlay(
        &self, overlay: &BTreeMap<(UndoTable, Vec<u8>), Option<Vec<u8>>>, limit: usize,
    ) -> Result<Option<VerificationOrchestrator>, StoreError> {
        decode_verification_rows(self.uses_normalized_verification_records()?,
            self.checkpoint_metadata_with_overlay(overlay, b"verification_state/")?, limit)
    }

    /// Compatibility read for a retained v1 target journal. Reads only this target,
    /// including staged normalized overrides; no unrelated state is reconstructed.
    /// This alone does not authorize migration or activate historical target writes.
    pub fn verification_target_undo_with_overlay(
        &self, key: &str,
        overlay: &BTreeMap<(UndoTable, Vec<u8>), Option<Vec<u8>>>, max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        if !self.uses_normalized_verification_records()? {
            return Err(invalid("target undo adapter requires normalized storage"));
        }
        let address = normalized::legacy_target_address(key).map_err(invalid)?;
        let header = normalized::target_header_key(&address).map_err(invalid)?;
        let prefix = header.strip_suffix('0').unwrap();
        let entries = self.checkpoint_metadata_with_overlay(overlay, prefix.as_bytes())?
            .map(|row| {
                let (key, bytes) = row.map_err(|e| e.to_string())?;
                Ok((std::str::from_utf8(key).map_err(|e| e.to_string())?, bytes))
            });
        normalized::target_from_entries(&address, entries, max_bytes.min(64 << 20) as u64)
            .map_err(invalid)?.map(|record| record.storage_entry().map(|(_, bytes)| bytes).map_err(invalid))
            .transpose()
    }

    pub fn uses_virtual_verification_target(&self, key: &[u8]) -> Result<bool, StoreError> {
        if !key.starts_with(b"verification_state/v1/1/") { return Ok(false); }
        // A migration stages rows before publishing its marker. Dispatch only
        // explicit format2; do not validate a half-staged legacy migration here.
        match self.get_meta(MODE)?.as_deref() {
            Some([2]) => self.uses_normalized_verification_records(),
            Some([1]) | None => Ok(false),
            Some(_) => Err(invalid("unsupported verification record format")),
        }
    }

    pub(super) fn replace_verification_target_undo(
        &mut self, key: &[u8], value: Option<&[u8]>,
    ) -> Result<(), StoreError> {
        self.invalidate_body_changes();
        if self.undo.is_some() {
            return Err(invalid("historical target writes require a non-block recovery writer"));
        }
        let key = std::str::from_utf8(key).map_err(invalid)?;
        let old = self.verification_target_undo_with_overlay(key, &BTreeMap::new(), 64 << 20)?;
        if old.as_deref() == value { return Ok(()); }
        let rows = |value: Option<&[u8]>| -> Result<BTreeMap<String, Vec<u8>>, StoreError> {
            value.map(|bytes| {
                let record = VerificationRecord::from_storage_entry(key, bytes).map_err(invalid)?;
                normalized::target_rows(&record).map_err(invalid)
            }).transpose().map(Option::unwrap_or_default)
        };
        let old = rows(old.as_deref())?;
        let new = rows(value)?;
        for key in old.keys().chain(new.keys()).collect::<std::collections::BTreeSet<_>>() {
            if old.get(key) == new.get(key) { continue; }
            if let Some(bytes) = new.get(key) {
                self.env.meta_db.put(&mut self.txn, key.as_bytes(), bytes).map_err(LmdbError::from)?;
            } else {
                self.env.meta_db.delete(&mut self.txn, key.as_bytes()).map_err(LmdbError::from)?;
            }
        }
        self.auxiliary_cache.get_mut().verification = None;
        self.changed_value = true;
        self.dirty = true;
        Ok(())
    }

    pub(super) fn replace_verification_undo_snapshot(
        &mut self,
        bytes: Option<&[u8]>,
    ) -> Result<(), StoreError> {
        self.invalidate_body_changes();
        let before = self.verification_undo_snapshot_with_overlay(&BTreeMap::new(), 64 << 20)?;
        if before.as_deref() == bytes {
            return Ok(());
        }
        let old = before
            .as_deref()
            .map(decode)
            .transpose()?
            .as_ref()
            .map(|engine| self.verification_rows(engine))
            .transpose()?
            .unwrap_or_default();
        let new = bytes
            .map(decode)
            .transpose()?
            .as_ref()
            .map(|engine| self.verification_rows(engine))
            .transpose()?
            .unwrap_or_default();
        if old == new {
            return Ok(());
        }
        self.auxiliary_cache.get_mut().verification = None;
        let original = if let Some(undo) = self.undo.as_ref() {
            let overlay = undo.verification_before_images();
            if overlay.is_empty() { before } else {
                self.verification_undo_snapshot_with_overlay(&overlay, 64 << 20)?
            }
        } else { before };
        if let Some(undo) = self.undo.as_mut() {
            undo.promote_verification(original, bytes)?;
        }
        // The aggregate undo entry owns these writes. Never record them twice.
        for key in old
            .keys()
            .chain(new.keys())
            .collect::<std::collections::BTreeSet<_>>()
        {
            if old.get(key) == new.get(key) {
                continue;
            }
            if let Some(value) = new.get(key) {
                self.env
                    .meta_db
                    .put(&mut self.txn, key.as_bytes(), value)
                    .map_err(LmdbError::from)?;
            } else {
                self.env
                    .meta_db
                    .delete(&mut self.txn, key.as_bytes())
                    .map_err(LmdbError::from)?;
            }
        }
        self.changed_value = true;
        self.dirty = true;
        Ok(())
    }

    /// Publish replay-derived state without allocating whole-state encoded row
    /// maps. Journals are replaced separately in the same recovery transaction.
    pub fn publish_recovered_verification(&mut self, after: &VerificationOrchestrator) -> Result<(), StoreError> {
        let result = (|| {
            if self.failed || self.undo.is_some() {
                return Err(invalid("recovered verification requires a healthy non-block writer"));
            }
            after.validate_storage_state().map_err(invalid)?;
            if after.snapshot().encoded_size().map_err(invalid)? > 64 << 20 {
                return Err(invalid("verification recovery snapshot exceeds byte limit"));
            }
            if !self.uses_verification_records()? {
                return self.put_meta(std::str::from_utf8(LEGACY).unwrap(), &encode_snapshot(after)?);
            }
            let before = self.read_verification_overlay(&BTreeMap::new(), 64 << 20)?;
            let normalized = self.uses_normalized_verification_records()?;
            if let Some(before) = before {
                let next = after.storage_replacement_scope(&before);
                let mut write = |key: String, old: Option<Vec<u8>>, new: Option<Vec<u8>>| {
                    if old == new { return Ok(()); }
                    match new {
                        Some(bytes) => self.put_meta(&key, &bytes),
                        None => self.delete_meta(&key),
                    }.map_err(|e| e.to_string())
                };
                if normalized {
                    normalized::visit_state_delta(&before, &next, &mut write).map_err(invalid)?;
                } else {
                    next.visit_record_delta(&before, |key, old, new| {
                        write(key, old.map(|r| r.storage_entry().map(|(_, v)| v)).transpose()?,
                            new.map(|r| r.storage_entry().map(|(_, v)| v)).transpose()?)
                    }).map_err(invalid)?;
                }
            } else if normalized {
                normalized::visit_state_rows(after, |key, bytes| self.put_meta(&key, &bytes)
                    .map_err(|e| e.to_string())).map_err(invalid)?;
            } else {
                after.visit_records(|record| {
                    let (key, bytes) = record.storage_entry()?;
                    self.put_meta(&key, &bytes).map_err(|e| e.to_string())
                }).map_err(invalid)?;
            }
            Ok(())
        })();
        if result.is_err() { self.failed = true; }
        result
    }

    /// Explicit atomic conversion. Existing aggregate journals retain their bytes
    /// and use the virtual adapter; no semantic undo history is discarded.
    pub fn migrate_verification_records(&mut self) -> Result<bool, StoreError> {
        self.invalidate_body_changes();
        let result = (|| {
            if self.failed || self.undo.is_some() {
                return Err(invalid(
                    "verification migration requires a healthy non-block writer",
                ));
            }
            if self.uses_verification_records()? {
                return Ok(false);
            }
            let schema = self.get_meta("schema_version")?;
            if crate::schema::check_version(schema.as_deref()).map_err(invalid)? {
                return Err(invalid("verification migration requires versioned storage"));
            }
            let engine = decode(
                self.undo_value_ref(UndoTable::Meta, LEGACY)?
                    .ok_or_else(|| invalid("missing legacy verification state"))?,
            )?;
            for (key, value) in rows(&engine)? {
                self.put_meta(&key, &value)?;
            }
            self.env
                .meta_db
                .delete(&mut self.txn, LEGACY)
                .map_err(LmdbError::from)?;
            self.put_meta(
                "schema_version",
                &crate::CURRENT_SCHEMA_VERSION.to_le_bytes(),
            )?;
            self.put_meta(MODE, &[1])?;
            self.changed_value = true;
            self.dirty = true;
            Ok(true)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Convert current state to normalized rows, preserving existing journal bytes.
    /// Input rows, output rows and the canonical snapshot each have a 64 MiB budget.
    /// Collect old state keys, reconstruct state, and validate every retained journal
    /// one at a time. Cost depends on both current state and journal history.
    /// Errors prevent commit.
    pub fn normalize_verification_records(&mut self) -> Result<bool, StoreError> {
        self.normalize_verification_records_from(None)
    }

    /// Full authenticated replacement may initialize normalized storage directly.
    /// Retained journals still pass the ordinary migration preflight, unchanged.
    pub fn publish_normalized_recovered_verification(
        &mut self,
        after: &VerificationOrchestrator,
    ) -> Result<(), StoreError> {
        let result = (|| {
            if self.uses_normalized_verification_records()? {
                return self.publish_recovered_verification(after);
            }
            self.normalize_verification_records_from(Some(after)).map(|_| ())
        })();
        if result.is_err() { self.failed = true; }
        result
    }

    fn normalize_verification_records_from(
        &mut self,
        replacement: Option<&VerificationOrchestrator>,
    ) -> Result<bool, StoreError> {
        self.invalidate_body_changes();
        let result = (|| {
            if self.failed || self.undo.is_some() {
                return Err(invalid("verification migration requires a healthy non-block writer"));
            }
            if self.uses_normalized_verification_records()? { return Ok(false); }
            let schema = self.get_meta("schema_version")?;
            if crate::schema::check_version(schema.as_deref()).map_err(invalid)? {
                return Err(invalid("verification migration requires versioned storage"));
            }
            let mut input_bytes = 0usize;
            let mut output_bytes = 0usize;
            let mut charge = |key: &[u8], bytes: &[u8], output: bool| -> Result<(), StoreError> {
                let used = if output { &mut output_bytes } else { &mut input_bytes };
                *used = used.checked_add(key.len()).and_then(|n| n.checked_add(bytes.len()))
                    .filter(|n| *n <= 64 << 20).ok_or_else(|| invalid("verification migration exceeds total byte budget"))?;
                Ok(())
            };
            let mut old_keys = Vec::new();
            for row in self.checkpoint_metadata_entries(b"verification_state/")? {
                let (key, bytes) = row?;
                charge(key, bytes, false)?;
                old_keys.push(key.to_vec());
            }
            let legacy = if self.uses_verification_records()? {
                None
            } else {
                self.undo_value_ref(UndoTable::Meta, LEGACY)?
            };
            if let Some(bytes) = legacy { charge(LEGACY, bytes, false)?; }
            let restored;
            let engine = if let Some(after) = replacement {
                after.validate_storage_state().map_err(invalid)?;
                after
            } else {
                restored = if self.uses_verification_records()? {
                    self.read_verification_records()?
                } else {
                    decode(legacy.ok_or_else(|| invalid("missing legacy verification state"))?)?
                };
                &restored
            };
            // Preserve the cold reader's canonical snapshot budget even when a
            // legacy encoding expands during conversion.
            if engine.snapshot().encoded_size().map_err(invalid)? > 64 << 20 {
                return Err(invalid("verification recovery snapshot exceeds byte limit"));
            }
            for row in self.checkpoint_metadata_entries(crate::block_undo::PREFIX)? {
                let (key, bytes) = row?;
                let hash = BlockHash::new(key[crate::block_undo::PREFIX.len()..].try_into()
                    .map_err(|_| invalid("invalid migration journal key"))?);
                let journal = BlockUndo::decode(&hash, bytes)?;
                crate::block_undo::verification::validate_retained(&journal)?;
            }
            // Preflight every output before mutating this writer; a later LMDB
            // failure still poisons the transaction and cannot partially commit.
            normalized::visit_state_rows(&engine, |key, bytes| {
                charge(key.as_bytes(), &bytes, true).map_err(|e| e.to_string())
            }).map_err(invalid)?;
            for key in old_keys {
                self.env.meta_db.delete(&mut self.txn, &key).map_err(LmdbError::from)?;
            }
            normalized::visit_state_rows(&engine, |key, bytes| {
                self.env.meta_db.put(&mut self.txn, key.as_bytes(), &bytes)
                    .map_err(|e| e.to_string())
            }).map_err(invalid)?;
            self.env.meta_db.delete(&mut self.txn, LEGACY).map_err(LmdbError::from)?;
            self.put_meta("schema_version", &crate::CURRENT_SCHEMA_VERSION.to_le_bytes())?;
            self.put_meta(MODE, &[2])?;
            // These rows were exported from this validated engine in this writer.
            // Keep its durable roots instead of decoding the same rows again.
            self.auxiliary_cache.get_mut().verification = Some(engine.storage_clone());
            self.changed_value = true;
            self.dirty = true;
            Ok(true)
        })();
        if result.is_err() { self.failed = true; }
        result
    }

    /// Stage changed records after checking their stored before-images.
    /// Rules changes, an existing aggregate undo entry, or row-budget overflow use
    /// whole-state comparison and aggregate undo. The caller authorizes the protocol
    /// transition; any storage or before-image error prevents commit.
    pub fn apply_verification_record_delta(
        &mut self,
        before: &VerificationOrchestrator,
        after: &VerificationOrchestrator,
    ) -> Result<(), StoreError> {
        let derived_from_cache = self
            .auxiliary_cache
            .borrow()
            .verification
            .as_ref()
            .is_some_and(|cached| cached.shares_record_storage_with(before));
        let result = (|| {
            if self.failed || !self.uses_verification_records()? {
                return Err(invalid("verification delta requires healthy keyed storage"));
            }
            if !after.record_scope_matches(before) {
                return Err(invalid("verification change scope differs from baseline"));
            }
            if before.rules_record().storage_entry().map_err(invalid)?
                != after.rules_record().storage_entry().map_err(invalid)?
                || self.undo.as_ref().is_some_and(UndoRecorder::has_verification_aggregate)
            {
                if self
                    .verification_undo_snapshot_with_overlay(&BTreeMap::new(), 64 << 20)?
                    .as_deref()
                    != Some(encode_snapshot(before)?.as_slice())
                {
                    return Err(invalid("global verification before-image mismatch"));
                }
                return self.put_meta(
                    std::str::from_utf8(LEGACY).unwrap(),
                    &encode_snapshot(after)?,
                );
            }
            if self.uses_normalized_verification_records()? {
                let mut pending = Vec::new();
                let mut budget = self.undo.as_ref().map(UndoRecorder::budget);
                let mut retained_bytes = 0usize;
                let mut aggregate = false;
                normalized::visit_state_delta(before, after, |key, old, new| {
                    if self.undo_value_ref(UndoTable::Meta, key.as_bytes()).map_err(|e| e.to_string())?
                        != old.as_deref() {
                        return Err(format!("verification before-image mismatch at {key}"));
                    }
                    if let Some(budget) = budget.as_mut() {
                        if !aggregate {
                            retained_bytes = retained_bytes.saturating_add(key.len())
                                .saturating_add(new.as_ref().map_or(0, Vec::len));
                            aggregate = retained_bytes > 64 << 20 || !self.undo.as_ref().unwrap()
                                .preview_meta_change(budget, &key, old.as_deref(), new.as_deref());
                            if aggregate { pending.clear(); }
                            else { pending.push((key, new)); }
                        }
                        Ok(())
                    } else {
                        match new {
                            Some(value) => self.put_meta(&key, &value),
                            None => self.delete_meta(&key),
                        }.map_err(|e| e.to_string())
                    }
                }).map_err(invalid)?;
                if aggregate {
                    let before_bytes = encode_snapshot(before)?;
                    if self.verification_undo_snapshot_with_overlay(&BTreeMap::new(), 64 << 20)?
                        .as_deref() != Some(before_bytes.as_slice()) {
                        return Err(invalid("global verification before-image mismatch"));
                    }
                    return self.put_meta(std::str::from_utf8(LEGACY).unwrap(),
                        &encode_snapshot(after)?);
                }
                for (key, value) in pending {
                    match value {
                        Some(value) => self.put_meta(&key, &value)?,
                        None => self.delete_meta(&key)?,
                    }
                }
                return Ok(());
            }
            after
                .visit_record_delta(before, |key, old, new| {
                    let old = old.map(|r| r.storage_entry()).transpose()?;
                    let new = new.map(|r| r.storage_entry()).transpose()?;
                    if self
                        .undo_value_ref(UndoTable::Meta, key.as_bytes())
                        .map_err(|e| e.to_string())?
                        != old.as_ref().map(|(_, v)| v.as_slice())
                    {
                        return Err(format!("verification before-image mismatch at {key}"));
                    }
                    match new {
                        Some((_, value)) => self.put_meta(&key, &value),
                        None => self.delete_meta(&key),
                    }
                    .map_err(|e| e.to_string())
                })
                .map_err(invalid)
        })();
        if result.is_err() {
            self.failed = true;
        } else if derived_from_cache {
            self.auxiliary_cache.get_mut().verification = Some(after.storage_clone());
        }
        result
    }
}

#[cfg(test)]
mod tests;

impl crate::read_view::LedgerReadView<'_> {
    pub fn uses_verification_records(&self) -> Result<bool, StoreError> {
        match self.get_meta(MODE)?.as_deref() {
            Some([1] | [2]) => {
                if self.undo_value_ref(UndoTable::Meta, LEGACY)?.is_some() {
                    return Err(invalid("mixed aggregate and keyed verification state"));
                }
                Ok(true)
            }
            Some(_) => Err(invalid("unsupported verification record format")),
            None => {
                if self
                    .checkpoint_metadata_entries(b"verification_state/")?
                    .next()
                    .transpose()?
                    .is_some()
                {
                    return Err(invalid("verification rows exist without format marker"));
                }
                Ok(false)
            }
        }
    }

    pub fn uses_normalized_verification_records(&self) -> Result<bool, StoreError> {
        self.uses_verification_records()?;
        Ok(self.get_meta(MODE)?.as_deref() == Some(&[2]))
    }
}

fn decode_verification_rows<'a>(normalized: bool,
    rows: impl IntoIterator<Item = Result<(&'a [u8], &'a [u8]), StoreError>>, limit: usize,
) -> Result<Option<VerificationOrchestrator>, StoreError> {
        let mut legacy = VerificationRecordReader::default();
        let mut reader = normalized::StateReader::new(limit as u64);
        let mut present = false;
        for row in rows {
            let (key, bytes) = row?;
            present = true;
            let key = std::str::from_utf8(key).map_err(invalid)?;
            if normalized { reader.push(key, bytes).map_err(invalid)?; }
            else {
                legacy.push_bounded(VerificationRecord::from_storage_entry(key, bytes).map_err(invalid)?,
                    limit as u64).map_err(invalid)?;
            }
        }
        if !present { return Ok(None); }
        if normalized { reader.finish().map(Some).map_err(invalid) }
        else { legacy.finish().map(Some).map_err(invalid) }
    }

impl crate::read_view::LedgerReadView<'_> {
    /// Visit logical records from this view, validating storage structure as read.
    /// Limits count canonical snapshot bytes; physical rows retain decoder bounds.
    /// A later row or final validation can fail after earlier callbacks. Polls occur
    /// between physical rows and before completion, not during decoding or callbacks.
    pub fn visit_verification_records_with_poll(&self, limit: u64,
        poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
        mut visit: impl FnMut(&str, &OrchestratorSnapshot) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        if !self.uses_verification_records()? { return Err(invalid("verification record mode is not initialized")); }
        let normalized = self.uses_normalized_verification_records()?;
        let mut failure = None;
        let mut callback = |key: &str, snapshot: &OrchestratorSnapshot| {
            visit(key, snapshot).map_err(|error| {
                let message = error.to_string(); failure = Some(error); message
            })
        };
        let result = (|| {
            if normalized {
                let mut reader = normalized::StateVisitor::new(limit, &mut callback);
                for row in self.checkpoint_metadata_entries(b"verification_state/")? {
                    poll("storage:verification-row")?;
                    let (key, bytes) = row?;
                    reader.push(std::str::from_utf8(key).map_err(invalid)?, bytes).map_err(invalid)?;
                }
                poll("storage:verification-finish")?;
                reader.finish().map_err(invalid)
            } else {
                let mut reader = VerificationRecordVisitor::new(&mut callback);
                for row in self.checkpoint_metadata_entries(b"verification_state/")? {
                    poll("storage:verification-row")?;
                    let (key, bytes) = row?;
                    let record = VerificationRecord::from_storage_entry(std::str::from_utf8(key).map_err(invalid)?, bytes).map_err(invalid)?;
                    reader.push_bounded(record, limit).map_err(invalid)?;
                }
                poll("storage:verification-finish")?;
                reader.finish().map_err(invalid)
            }
        })();
        if let Some(error) = failure { return Err(error); }
        result
    }

    pub fn read_verification_records(&self) -> Result<VerificationOrchestrator, StoreError> {
        if !self.uses_verification_records()? { return Err(invalid("verification record mode is not initialized")); }
        decode_verification_rows(self.uses_normalized_verification_records()?,
            self.checkpoint_metadata_entries(b"verification_state/")?, 64 << 20)?
            .ok_or_else(|| invalid("missing verification records"))
    }
}
