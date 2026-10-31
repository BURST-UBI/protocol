//! Typed replay changes are not installable until local revisions are allocated.
use super::*;
use crate::block_undo::{BlockUndo, UndoChange, UndoRecorder};

#[derive(serde::Serialize, serde::Deserialize)]
pub struct GovernanceUndoProjection {
    changes: Vec<UndoChange>,
    scopes: Option<std::collections::BTreeSet<TxHash>>,
}
impl GovernanceUndoProjection {
    pub fn changes(&self) -> &[UndoChange] {
        &self.changes
    }
    /// Return projected row count and key/before/after bytes, including reserved
    /// revision images. This excludes collection overhead and journal serialization.
    pub fn retained_budget(&self) -> (usize, usize) {
        let mut count = self.changes.len();
        let mut bytes = self
            .changes
            .iter()
            .map(|row| {
                row.key.len()
                    + row.before.as_ref().map_or(0, Vec::len)
                    + row.after.as_ref().map_or(0, Vec::len)
            })
            .sum();
        if let Some(scopes) = &self.scopes {
            count += scopes.len();
            bytes += scopes
                .iter()
                .map(|proposal| format!("{PROPOSAL_REVISION_PREFIX}{proposal}").len() + 32)
                .sum::<usize>();
        }
        (count, bytes)
    }
}

fn aggregate(
    before: &GovernanceEngine,
    after: &GovernanceEngine,
) -> Result<GovernanceUndoProjection, StoreError> {
    let encode = |engine: &GovernanceEngine| -> Result<Vec<u8>, StoreError> {
        if bincode::serialized_size(engine).map_err(|e| invalid(e.to_string()))? > 64 << 20 {
            return Err(invalid("oversized recovered governance image"));
        }
        bincode::serialize(engine).map_err(|e| invalid(e.to_string()))
    };
    Ok(GovernanceUndoProjection {
        scopes: None,
        changes: vec![UndoChange {
            table: UndoTable::BrnMeta,
            key: b"governance_engine".to_vec(),
            before: Some(encode(before)?),
            after: Some(encode(after)?),
        }],
    })
}

/// Prepare changed governance rows and reserve both images of local revisions.
/// Global effects or insufficient row-journal budget use aggregate engine images,
/// each capped at 64 MiB. The combined journal still needs validation when staged.
/// The caller must authenticate both engines through replay.
pub fn prepare_governance_undo(
    base: &BlockUndo,
    before: &GovernanceEngine,
    after: &GovernanceEngine,
) -> Result<GovernanceUndoProjection, StoreError> {
    if base.changes.iter().any(|row| {
        (row.table == UndoTable::BrnMeta && row.key == b"governance_engine")
            || (row.table == UndoTable::Meta
                && (row
                    .key
                    .starts_with(GovernanceRecord::STORAGE_PREFIX.as_bytes())
                    || row.key.starts_with(PROPOSAL_REVISION_PREFIX.as_bytes())))
    }) {
        return Err(invalid("mixed recovered governance journal ownership"));
    }
    if after.pending_effects_differ(before) {
        return aggregate(before, after);
    }
    let mut recorder = UndoRecorder::new(base.block, base.account.clone(), base.height);
    let mut changes = Vec::new();
    for row in &base.changes {
        if row.table == UndoTable::BrnMeta && row.key == b"governance_engine" {
            continue;
        }
        // Aggregate engine images may fit when physical row reserves do not.
        // Final staging checks the resulting journal together with its base.
        if recorder
            .record(
                row.table,
                &row.key,
                row.before.as_deref(),
                row.after.as_deref(),
            )
            .is_err()
        {
            return aggregate(before, after);
        }
    }
    let mut budget = recorder.budget();
    let mut scopes = std::collections::BTreeSet::new();
    let mut fits = true;
    after
        .visit_record_delta(before, |old, new| {
            if !fits {
                return Ok(());
            }
            if let Some(proposal) = old
                .as_ref()
                .or(new.as_ref())
                .and_then(GovernanceRecord::proposal_scope)
            {
                scopes.insert(proposal);
            }
            let old = old.map(|row| row.storage_entry()).transpose()?;
            let new = new.map(|row| row.storage_entry()).transpose()?;
            let key = old
                .as_ref()
                .or(new.as_ref())
                .ok_or("empty governance row delta")?
                .0
                .clone();
            if old
                .as_ref()
                .zip(new.as_ref())
                .is_some_and(|(old, new)| old.0 != new.0)
            {
                return Err("governance row delta changed key".into());
            }
            let old = old.map(|row| row.1);
            let new = new.map(|row| row.1);
            fits = recorder.preview_meta_change(&mut budget, &key, old.as_deref(), new.as_deref());
            if fits {
                changes.push(UndoChange {
                    table: UndoTable::Meta,
                    key: key.into_bytes(),
                    before: old,
                    after: new,
                });
            }
            Ok(())
        })
        .map_err(invalid)?;
    if !fits {
        return aggregate(before, after);
    }
    for proposal in &scopes {
        let key = format!("{PROPOSAL_REVISION_PREFIX}{proposal}");
        if !recorder.preview_meta_change(&mut budget, &key, Some(&[0; 16]), Some(&[1; 16])) {
            return aggregate(before, after);
        }
    }
    Ok(GovernanceUndoProjection {
        changes,
        scopes: Some(scopes),
    })
}

impl LmdbWriteTransaction<'_> {
    /// Convert an aggregate replay journal to rows when both engine images exist.
    /// Return it unchanged in legacy mode or when either image is absent. This may
    /// stage revision counters; the caller must install the returned journal.
    pub fn project_recovered_governance_undo(
        &mut self,
        journal: BlockUndo,
    ) -> Result<BlockUndo, StoreError> {
        let result = (|| {
            if self.failed || self.undo.is_some() {
                return Err(invalid(
                    "governance journal projection requires a healthy non-block writer",
                ));
            }
            if !self.uses_governance_records()? {
                return Ok(journal);
            }
            let Some(row) = journal
                .changes
                .iter()
                .find(|row| row.table == UndoTable::BrnMeta && row.key == b"governance_engine")
            else {
                return Ok(journal);
            };
            let (Some(old), Some(new)) = (&row.before, &row.after) else {
                return Ok(journal);
            };
            if bincode::serialized_size(&journal).map_err(|e| invalid(e.to_string()))? > 64 << 20 {
                return Err(invalid("oversized recovered governance journal"));
            }
            let bytes = bincode::serialize(&journal).map_err(|e| invalid(e.to_string()))?;
            BlockUndo::decode(&journal.block, &bytes)?;
            let decode = |bytes: &[u8]| -> Result<GovernanceEngine, StoreError> {
                bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .with_limit(64 << 20)
                    .reject_trailing_bytes()
                    .deserialize(bytes)
                    .map_err(|e| invalid(e.to_string()))
            };
            let before = decode(old)?;
            let after = decode(new)?;
            let mut base = journal;
            base.changes.retain(|row| {
                !(row.table == UndoTable::BrnMeta && row.key == b"governance_engine")
            });
            let prepared = prepare_governance_undo(&base, &before, &after)?;
            self.stage_recovered_governance_undo(base, prepared)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }

    /// Stage local revision counters and return the journal containing their images.
    /// Call in replay order after recovered governance publication. This does not
    /// store the returned journal; install the complete inventory before commit.
    /// Any error prevents commit.
    pub fn stage_recovered_governance_undo(
        &mut self,
        mut journal: BlockUndo,
        projection: GovernanceUndoProjection,
    ) -> Result<BlockUndo, StoreError> {
        let result = (|| {
            if self.failed || self.undo.is_some() || !self.uses_governance_records()? {
                return Err(invalid(
                    "typed governance journal requires a healthy keyed projection writer",
                ));
            }
            let physical = projection.scopes.is_some();
            journal.changes.extend(projection.changes);
            let mut revisions = Vec::new();
            if let Some(scopes) = projection.scopes {
                let mut actual = std::collections::BTreeSet::new();
                for row in &journal.changes {
                    if row.table == UndoTable::BrnMeta && row.key == b"governance_engine" {
                        return Err(invalid("mixed typed governance ownership"));
                    }
                    if row.table == UndoTable::Meta
                        && row.key.starts_with(PROPOSAL_REVISION_PREFIX.as_bytes())
                    {
                        return Err(invalid("typed governance already owns local revisions"));
                    }
                    if row.table == UndoTable::Meta
                        && row
                            .key
                            .starts_with(GovernanceRecord::STORAGE_PREFIX.as_bytes())
                    {
                        let key =
                            std::str::from_utf8(&row.key).map_err(|e| invalid(e.to_string()))?;
                        for bytes in row.before.iter().chain(row.after.iter()) {
                            let record = GovernanceRecord::from_storage_entry(key, bytes)
                                .map_err(invalid)?;
                            actual.insert(record.proposal_scope().ok_or_else(|| {
                                invalid("physical governance contains global effects")
                            })?);
                        }
                    }
                }
                if actual != scopes {
                    return Err(invalid("typed governance proposal scopes mismatch"));
                }
                for proposal in scopes {
                    let key = format!("{PROPOSAL_REVISION_PREFIX}{proposal}");
                    let old = self.get_meta(&key)?;
                    let value = old
                        .as_deref()
                        .map(|bytes| {
                            <[u8; 16]>::try_from(bytes)
                                .map(u128::from_be_bytes)
                                .map_err(|_| invalid("invalid recovered governance revision"))
                        })
                        .transpose()?
                        .unwrap_or(0);
                    let next = value
                        .checked_add(1)
                        .ok_or_else(|| invalid("recovered governance revision exhausted"))?
                        .to_be_bytes()
                        .to_vec();
                    journal.changes.push(UndoChange {
                        table: UndoTable::Meta,
                        key: key.as_bytes().to_vec(),
                        before: old,
                        after: Some(next.clone()),
                    });
                    revisions.push((key, next));
                }
            }
            journal
                .changes
                .sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
            if physical {
                let mut recorder =
                    UndoRecorder::new(journal.block, journal.account.clone(), journal.height);
                for row in &journal.changes {
                    recorder.record(
                        row.table,
                        &row.key,
                        row.before.as_deref(),
                        row.after.as_deref(),
                    )?;
                }
            }
            let bytes = bincode::serialize(&journal).map_err(|e| invalid(e.to_string()))?;
            let journal = BlockUndo::decode(&journal.block, &bytes)?;
            for (key, value) in revisions {
                self.put_meta(&key, &value)?;
            }
            Ok(journal)
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}
