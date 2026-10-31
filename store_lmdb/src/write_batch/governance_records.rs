//! Governance record persistence, cached reads and aggregate rollback adapters.
use super::*;
use bincode::Options;
use burst_governance::engine::{GovernanceEngine, GovernanceRecord};

/// Local undo conflict guards, separate from authenticated governance state.
pub const PROPOSAL_REVISION_PREFIX: &str = "governance_revision/v1/";
mod recovery;
pub use recovery::{prepare_governance_undo, GovernanceUndoProjection};

const MODE_KEY: &str = "governance_records_format";

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::Corruption(message.into())
}

impl LmdbWriteTransaction<'_> {
    /// Explicit local storage mode survives rollback to an absent engine.
    /// This marker is not the old-binary rejection gate required for migration.
    pub fn uses_governance_records(&self) -> Result<bool, StoreError> {
        self.read_view().uses_governance_records()
    }

    /// Reconstruct an aggregate undo image after earlier planned keyed changes.
    /// None means the projected engine is absent (or this writer is in legacy mode).
    /// The caller's overlay remains read-only; source bytes are bounded before decode.
    pub fn governance_undo_snapshot_with_overlay(
        &self,
        overlay: &std::collections::BTreeMap<(UndoTable, Vec<u8>), Option<Vec<u8>>>,
        max_bytes: usize,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        self.governance_undo_engine_with_overlay(overlay, max_bytes)?
            .map(|engine| bincode::serialize(&engine).map_err(|e| invalid(e.to_string())))
            .transpose()
    }

    fn governance_undo_engine_with_overlay(
        &self,
        overlay: &std::collections::BTreeMap<(UndoTable, Vec<u8>), Option<Vec<u8>>>,
        max_bytes: usize,
    ) -> Result<Option<GovernanceEngine>, StoreError> {
        if !self.uses_governance_records()? {
            return Ok(None);
        }
        let prefix = GovernanceRecord::STORAGE_NAMESPACE.as_bytes();
        let mut rows = self
            .checkpoint_metadata_with_overlay(overlay, prefix)?
            .peekable();
        if rows.peek().is_none() {
            return Ok(None);
        }
        let limit = max_bytes.min(64 << 20);
        let mut retained = 0usize;
        let engine = GovernanceEngine::from_records(rows.map(|row| {
            let (key, bytes) = row.map_err(|e| e.to_string())?;
            retained = retained
                .checked_add(key.len())
                .and_then(|n| n.checked_add(bytes.len()))
                .ok_or("governance undo overlay size overflow")?;
            if retained > limit {
                return Err("governance undo overlay exceeds byte limit".into());
            }
            let key = std::str::from_utf8(key).map_err(|e| e.to_string())?;
            GovernanceRecord::from_storage_entry(key, bytes)
        }))
        .map_err(invalid)?;
        if bincode::serialized_size(&engine).map_err(|e| invalid(e.to_string()))? > limit as u64 {
            return Err(invalid("governance undo snapshot exceeds byte limit"));
        }
        Ok(Some(engine))
    }

    pub(super) fn governance_undo_snapshot(&self) -> Result<Option<Vec<u8>>, StoreError> {
        self.governance_undo_snapshot_with_overlay(&Default::default(), 64 << 20)
    }

    /// Stage caller-authenticated recovery state, preserving existing journals.
    /// Keyed storage advances local revisions for changed proposals instead of
    /// clearing them. Replace matching journals before commit. Errors prevent commit.
    pub fn publish_recovered_governance(
        &mut self,
        after: &GovernanceEngine,
    ) -> Result<(), StoreError> {
        let result = (|| {
            if self.failed || self.undo.is_some() {
                return Err(invalid(
                    "recovered governance requires a healthy non-block writer",
                ));
            }
            if bincode::serialized_size(after).map_err(|e| invalid(e.to_string()))? > 64 << 20 {
                return Err(invalid("oversized governance recovery projection"));
            }
            if !self.uses_governance_records()? {
                return self.put_brn_meta(
                    b"governance_engine",
                    &bincode::serialize(after).map_err(|e| invalid(e.to_string()))?,
                );
            }
            let before = self.governance_undo_engine_with_overlay(&Default::default(), 64 << 20)?;
            if before.as_ref() == Some(after) {
                return Ok(());
            }
            self.governance_cache.get_mut().take();
            self.auxiliary_cache.get_mut().invalidate_projection();
            let mut scopes = std::collections::BTreeSet::new();
            let mut write = |old: Option<GovernanceRecord>, new: Option<GovernanceRecord>| {
                if let Some(scope) = old
                    .as_ref()
                    .or(new.as_ref())
                    .and_then(GovernanceRecord::proposal_scope)
                {
                    scopes.insert(scope);
                }
                if let Some(old) = old {
                    self.delete_meta(&old.storage_key()?)
                        .map_err(|e| e.to_string())?;
                }
                if let Some(new) = new {
                    let (key, bytes) = new.storage_entry()?;
                    self.put_meta(&key, &bytes).map_err(|e| e.to_string())?;
                }
                Ok::<_, String>(())
            };
            match before.as_ref() {
                Some(before) => after.visit_record_delta(before, &mut write),
                None => after.visit_records(|row| write(None, Some(row))),
            }
            .map_err(invalid)?;
            self.advance_governance_revisions(scopes)?;
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Compatibility for complete historical/global undo images. This is cold
    /// whole-state work; normal proposal deltas do not take this path.
    pub(super) fn replace_governance_undo_snapshot(
        &mut self,
        bytes: Option<&[u8]>,
        advance_revisions: bool,
    ) -> Result<(), StoreError> {
        self.invalidate_body_changes();
        let decode = |bytes: &[u8]| -> Result<GovernanceEngine, StoreError> {
            if bytes.len() > 64 << 20 {
                return Err(invalid("oversized governance undo snapshot"));
            }
            bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .with_limit(64 << 20)
                .reject_trailing_bytes()
                .deserialize(bytes)
                .map_err(|e| invalid(e.to_string()))
        };
        let before_bytes = self.governance_undo_snapshot()?;
        let before = before_bytes.as_deref().map(decode).transpose()?;
        let after = bytes.map(decode).transpose()?;
        if before == after {
            return Ok(());
        }
        self.changed_value = true;
        self.governance_cache.get_mut().take();
        self.auxiliary_cache.get_mut().invalidate_projection();
        if let Some(undo) = self.undo.as_mut() {
            undo.record(
                UndoTable::BrnMeta,
                b"governance_engine",
                before_bytes.as_deref(),
                bytes,
            )?;
        }
        // The aggregate journal entry owns these state changes. Recording both
        // aggregate and per-row images would apply the same undo twice.
        let mut scopes = std::collections::BTreeSet::new();
        GovernanceEngine::visit_optional_snapshot_record_delta(
            before_bytes.as_deref(),
            bytes,
            |old, new| {
                if let Some(scope) = old
                    .as_ref()
                    .or(new.as_ref())
                    .and_then(GovernanceRecord::proposal_scope)
                {
                    scopes.insert(scope);
                }
                if let Some(record) = old {
                    self.env
                        .meta_db
                        .delete(&mut self.txn, record.storage_key()?.as_bytes())
                        .map_err(|e| e.to_string())?;
                }
                if let Some(record) = new {
                    let (key, value) = record.storage_entry()?;
                    self.env
                        .meta_db
                        .put(&mut self.txn, key.as_bytes(), &value)
                        .map_err(|e| e.to_string())?;
                }
                Ok(())
            },
        )
        .map_err(invalid)?;
        if advance_revisions {
            self.advance_governance_revisions(scopes)?;
        }
        self.dirty = true;
        Ok(())
    }

    fn advance_governance_revisions(
        &mut self,
        scopes: std::collections::BTreeSet<TxHash>,
    ) -> Result<(), StoreError> {
        for proposal in scopes {
            let key = format!("{PROPOSAL_REVISION_PREFIX}{proposal}");
            let revision = self
                .get_meta(&key)?
                .map(|bytes| {
                    let bytes: [u8; 16] = bytes
                        .try_into()
                        .map_err(|_| invalid("invalid governance proposal revision"))?;
                    Ok::<_, StoreError>(u128::from_be_bytes(bytes))
                })
                .transpose()?
                .unwrap_or(0);
            let next = revision
                .checked_add(1)
                .ok_or_else(|| invalid("governance proposal revision exhausted"))?;
            self.put_meta(&key, &next.to_be_bytes())?;
        }
        Ok(())
    }

    /// Read governance state including this writer's changes, reusing its cache.
    /// A cache miss reconstructs all records. Each stored row is bounded, but this
    /// read has no aggregate byte limit.
    pub fn read_governance_records(&self) -> Result<GovernanceEngine, StoreError> {
        if !self.uses_governance_records()? {
            return Err(invalid("governance record mode is not initialized"));
        }
        if let Some(engine) = self.governance_cache.borrow().as_ref() {
            return Ok(engine.clone());
        }
        let engine = self.read_view().read_governance_records()?;
        *self.governance_cache.borrow_mut() = Some(engine.clone());
        Ok(engine)
    }

    /// Replace replay-authenticated checkpoint state without decoding the old
    /// projection. The ledger installer must retire old journals first and
    /// install reconstructed journals afterward. This is never block admission.
    pub fn replace_checkpoint_governance_records(
        &mut self,
        engine: &GovernanceEngine,
    ) -> Result<(), StoreError> {
        self.invalidate_body_changes();
        let result = (|| {
            if self.failed || self.undo.is_some() || !self.uses_governance_records()? {
                return Err(invalid(
                    "governance checkpoint replacement requires a healthy keyed non-block writer",
                ));
            }
            if self
                .checkpoint_metadata_entries(b"block_undo/")?
                .next()
                .transpose()?
                .is_some()
            {
                return Err(invalid(
                    "governance checkpoint replacement requires retired undo journals",
                ));
            }
            for prefix in [GovernanceRecord::STORAGE_PREFIX, PROPOSAL_REVISION_PREFIX] {
                self.clear_checkpoint_rows(UndoTable::Meta, Some(prefix.as_bytes()))?;
            }
            engine
                .visit_records(|record| {
                    let (key, value) = record.storage_entry()?;
                    self.put_meta(&key, &value).map_err(|e| e.to_string())
                })
                .map_err(invalid)?;
            self.env
                .brn_meta_db
                .delete(&mut self.txn, b"governance_engine")
                .map_err(LmdbError::from)?;
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Convert a recovered legacy engine atomically, retaining historical undo
    /// images through the virtual aggregate adapter. No block may be active.
    pub fn migrate_governance_records(&mut self) -> Result<bool, StoreError> {
        self.invalidate_body_changes();
        let result = (|| {
            if self.failed || self.undo.is_some() {
                return Err(invalid(
                    "governance migration requires a healthy non-block writer",
                ));
            }
            if self.uses_governance_records()? {
                if self
                    .undo_value_ref(UndoTable::BrnMeta, b"governance_engine")?
                    .is_some()
                {
                    return Err(invalid("mixed aggregate and keyed governance state"));
                }
                return Ok(false);
            }
            let bytes = self
                .undo_value_ref(UndoTable::BrnMeta, b"governance_engine")?
                .ok_or_else(|| invalid("missing legacy governance engine"))?;
            if bytes.len() > 64 << 20 {
                return Err(invalid("governance migration exceeds snapshot byte limit"));
            }
            let engine: GovernanceEngine = bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .with_limit(bytes.len() as u64)
                .reject_trailing_bytes()
                .deserialize(bytes)
                .map_err(|error| invalid(error.to_string()))?;
            self.initialize_governance_records(&engine)?;
            // Bypass the virtual aggregate adapter: this removes only the old
            // physical image, not the newly installed records.
            self.env
                .brn_meta_db
                .delete(&mut self.txn, b"governance_engine")
                .map_err(LmdbError::from)?;
            self.dirty = true;
            Ok(true)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Initialize an empty record namespace at a migration/checkpoint boundary.
    /// Atomically raises the schema gate; leaves the legacy aggregate snapshot intact.
    pub fn initialize_governance_records(
        &mut self,
        engine: &GovernanceEngine,
    ) -> Result<(), StoreError> {
        let result = (|| {
            if self.failed || self.undo.is_some() {
                return Err(invalid(
                    "governance initialization requires a healthy non-block writer",
                ));
            }
            if self
                .checkpoint_metadata_entries(GovernanceRecord::STORAGE_NAMESPACE.as_bytes())?
                .next()
                .transpose()?
                .is_some()
            {
                return Err(invalid("governance record namespace is not empty"));
            }
            if self
                .checkpoint_metadata_entries(b"governance_revision/")?
                .next()
                .transpose()?
                .is_some()
            {
                return Err(invalid("orphaned governance revision namespace"));
            }
            if self.get_meta(MODE_KEY)?.is_some() {
                return Err(invalid("governance record mode is already initialized"));
            }
            let schema = self.get_meta("schema_version")?;
            if crate::schema::check_version(schema.as_deref())
                .map_err(|error| invalid(error.to_string()))?
            {
                return Err(invalid(
                    "governance initialization requires a versioned store",
                ));
            }
            self.put_meta(
                "schema_version",
                &crate::CURRENT_SCHEMA_VERSION.to_le_bytes(),
            )?;
            self.put_meta(MODE_KEY, &[1])?;
            engine
                .visit_records(|record| {
                    let (key, value) = record.storage_entry()?;
                    self.put_meta(&key, &value).map_err(|e| e.to_string())
                })
                .map_err(invalid)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Stage governance changes in the caller's transaction and undo boundary.
    /// Proposal changes check only affected row images. Changed global pending
    /// effects require whole-state comparison and an aggregate undo image.
    /// This checks storage consistency, not proposal authorization. Errors prevent commit.
    pub fn apply_governance_record_delta(
        &mut self,
        before: &GovernanceEngine,
        after: &GovernanceEngine,
    ) -> Result<(), StoreError> {
        let derived_from_cache = self
            .governance_cache
            .borrow()
            .as_ref()
            .is_some_and(|cached| cached.shares_record_storage_with(before));
        let result = (|| {
            if self.failed {
                return Err(invalid("cannot update failed governance writer"));
            }
            if !self.uses_governance_records()? {
                return Err(invalid("governance record mode is not initialized"));
            }
            let marker = format!("{}0", GovernanceRecord::STORAGE_PREFIX);
            let header = self
                .get_meta(&marker)?
                .ok_or_else(|| invalid("governance records are not initialized"))?;
            GovernanceRecord::from_storage_entry(&marker, &header).map_err(invalid)?;
            if after.pending_effects_differ(before) {
                if self.read_governance_records()? != *before {
                    return Err(invalid("global governance before-image mismatch"));
                }
                let bytes = bincode::serialize(after).map_err(|e| invalid(e.to_string()))?;
                return self.put_brn_meta(b"governance_engine", &bytes);
            }
            let mut scopes = std::collections::BTreeSet::new();
            after
                .visit_record_delta(before, |old, new| {
                    if let Some(proposal) = old
                        .as_ref()
                        .or(new.as_ref())
                        .and_then(GovernanceRecord::proposal_scope)
                    {
                        scopes.insert(proposal);
                    }
                    let old = old.map(|r| r.storage_entry()).transpose()?;
                    let new = new.map(|r| r.storage_entry()).transpose()?;
                    if old
                        .as_ref()
                        .zip(new.as_ref())
                        .is_some_and(|(a, b)| a.0 != b.0)
                    {
                        return Err("governance delta changes a record key".into());
                    }
                    let key = old
                        .as_ref()
                        .or(new.as_ref())
                        .ok_or("empty governance delta")?
                        .0
                        .as_str();
                    let expected = old.as_ref().map(|(_, bytes)| bytes.as_slice());
                    if self
                        .undo_value_ref(UndoTable::Meta, key.as_bytes())
                        .map_err(|e| e.to_string())?
                        != expected
                    {
                        return Err(format!("governance record before-image mismatch at {key}"));
                    }
                    match new.as_ref() {
                        Some((_, bytes)) => self.put_meta(key, bytes),
                        None => self.delete_meta(key),
                    }
                    .map_err(|e| e.to_string())
                })
                .map_err(invalid)?;
            self.advance_governance_revisions(scopes)?;
            if derived_from_cache {
                *self.governance_cache.get_mut() = Some(after.clone());
            }
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}

#[cfg(test)]
mod tests;

impl crate::read_view::LedgerReadView<'_> {
    pub fn uses_governance_records(&self) -> Result<bool, StoreError> {
        match self.get_meta(MODE_KEY)?.as_deref() {
            None => Ok(false),
            Some([1]) => Ok(true),
            Some(_) => Err(invalid("unsupported governance record format")),
        }
    }
}

impl crate::read_view::LedgerReadView<'_> {
    pub fn read_governance_records(&self) -> Result<GovernanceEngine, StoreError> {
        if !self.uses_governance_records()? {
            return Err(invalid("governance record mode is not initialized"));
        }
        let rows =
            self.checkpoint_metadata_entries(GovernanceRecord::STORAGE_NAMESPACE.as_bytes())?;
        let engine = GovernanceEngine::from_records(rows.map(|row| {
            let (key, bytes) = row.map_err(|e| e.to_string())?;
            let key = std::str::from_utf8(key).map_err(|e| e.to_string())?;
            GovernanceRecord::from_storage_entry(key, bytes)
        }))
        .map_err(invalid)?;
        Ok(engine)
    }
}
