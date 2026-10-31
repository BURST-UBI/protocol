use super::*;

#[cfg(test)]
pub(super) fn owns(table: UndoTable, key: &[u8]) -> bool {
    (table == UndoTable::BrnMeta
        && (key == b"governance_engine"
            || key == b"rate_history"
            || key == burst_brn::rate_storage::HEADER
            || key.starts_with(burst_brn::rate_storage::ROW_PREFIX)))
        || (table == UndoTable::Meta
            && (key == crate::endorsement_commit::SNAPSHOT_KEY.as_bytes()
                || key == burst_governance::DelegationEngine::meta_key().as_bytes()
                || key == b"consti_engine"
                || key == b"verified_count"))
}

pub(super) struct Aggregates {
    verification: burst_verification::VerificationOrchestrator,
    governance: burst_governance::GovernanceEngine,
    delegation: burst_governance::DelegationEngine,
    constitution: burst_consti::ConstiEngine,
    rates: burst_brn::RateHistory,
}

impl Aggregates {
    pub(super) fn capture(state: &ConfirmedReplay) -> Result<Self, ReplayError> {
        let governance = state
            .governance
            .as_ref()
            .ok_or_else(|| crate::confirmed_replay::invalid("replay lacks governance state"))?;
        Ok(Self {
            verification: state.verification.storage_clone(),
            governance: governance.governance.clone(),
            delegation: governance.delegation.clone(),
            constitution: governance.constitution.clone(),
            rates: state.brn.rate_history.clone(),
        })
    }

    fn write_verification(&self, rows: &mut Rows) -> Result<(), ReplayError> {
        if rows.verification_projection.is_some() {
            self.verification
                .validate_storage_state()
                .map_err(replay_error)?;
            let size = self
                .verification
                .snapshot()
                .encoded_size()
                .map_err(replay_error)?;
            let size = usize::try_from(size).map_err(replay_error)?;
            rows.charge_bytes(crate::endorsement_commit::SNAPSHOT_KEY.len(), size)
        } else {
            rows.put(
                UndoTable::Meta,
                crate::endorsement_commit::SNAPSHOT_KEY.as_bytes(),
                self.verification
                    .snapshot()
                    .encode()
                    .map_err(replay_error)?,
            )
        }
    }

    fn write_governance(&self, rows: &mut Rows) -> Result<(), ReplayError> {
        if rows.governance_projection.is_some() {
            let size = bincode::serialized_size(&self.governance).map_err(replay_error)?;
            let size = usize::try_from(size).map_err(replay_error)?;
            rows.charge_bytes(b"governance_engine".len(), size)
        } else {
            rows.encoded(UndoTable::BrnMeta, b"governance_engine", &self.governance)
        }
    }

    fn write_delegation(&self, rows: &mut Rows) -> Result<(), ReplayError> {
        rows.put(
            UndoTable::Meta,
            burst_governance::DelegationEngine::meta_key().as_bytes(),
            self.delegation.save_state(),
        )
    }

    fn write_constitution(&self, rows: &mut Rows) -> Result<(), ReplayError> {
        rows.encoded(UndoTable::Meta, b"consti_engine", &self.constitution)
    }

    fn write_rates(&self, rows: &mut Rows) -> Result<(), ReplayError> {
        if rows.rate_records {
            for (key, value) in
                burst_brn::rate_storage::encode(&self.rates).map_err(replay_error)?
            {
                rows.put(UndoTable::BrnMeta, &key, value.to_vec())?;
            }
            rows.put(
                UndoTable::BrnMeta,
                burst_brn::rate_storage::HEADER,
                (self.rates.segments().len() as u64).to_be_bytes().to_vec(),
            )
        } else {
            rows.encoded(UndoTable::BrnMeta, b"rate_history", &self.rates)
        }
    }

    pub(super) fn write_all(&self, rows: &mut Rows) -> Result<(), ReplayError> {
        self.write_rates(rows)?;
        self.write_verification(rows)?;
        self.write_governance(rows)?;
        self.write_delegation(rows)?;
        self.write_constitution(rows)
    }
}

#[cfg(test)]
pub(super) fn materialize_changed(before: &mut Rows, after: &mut Rows) -> Result<(), ReplayError> {
    materialize_transition(before, after, false, false).map(|_| ())
}

pub(super) struct ChangedAggregates {
    pub verification: Option<(
        burst_verification::VerificationOrchestrator,
        burst_verification::VerificationOrchestrator,
    )>,
    pub governance: Option<(
        burst_governance::GovernanceEngine,
        burst_governance::GovernanceEngine,
    )>,
}

/// Skip aggregates with shared storage. Encode detached aggregates, or return
/// verification and governance pairs for record-level comparison when enabled.
pub(super) fn materialize_transition(
    before: &mut Rows,
    after: &mut Rows,
    normalized_verification: bool,
    keyed_governance: bool,
) -> Result<ChangedAggregates, ReplayError> {
    let old = before
        .aggregates
        .take()
        .ok_or_else(|| crate::confirmed_replay::invalid("missing prior recovery aggregates"))?;
    let new = after
        .aggregates
        .take()
        .ok_or_else(|| crate::confirmed_replay::invalid("missing next recovery aggregates"))?;
    if old.rates != new.rates {
        old.write_rates(before)?;
        new.write_rates(after)?;
    }
    let verification_changed = !old
        .verification
        .shares_record_storage_with(&new.verification);
    if verification_changed && !normalized_verification {
        old.write_verification(before)?;
        new.write_verification(after)?;
    }
    let governance_changed = !old.governance.shares_record_storage_with(&new.governance);
    if governance_changed && !keyed_governance {
        old.write_governance(before)?;
        new.write_governance(after)?;
    }
    if !old.delegation.shares_storage_with(&new.delegation) {
        old.write_delegation(before)?;
        new.write_delegation(after)?;
    }
    if !old.constitution.shares_storage_with(&new.constitution) {
        old.write_constitution(before)?;
        new.write_constitution(after)?;
    }
    Ok(ChangedAggregates {
        verification: (normalized_verification && verification_changed)
            .then_some((old.verification, new.verification)),
        governance: (keyed_governance && governance_changed)
            .then_some((old.governance, new.governance)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_recovery_aggregates_skip_encoding_and_detached_changes_match_full_images() {
        let state = crate::confirmed_replay::snapshot::SnapshotContinuation::from_genesis(
            burst_types::NetworkId::Dev,
            &burst_types::ProtocolParams::default(),
        )
        .unwrap()
        .into_state();
        let directory = tempfile::tempdir().unwrap();
        let aggregate = Aggregates::capture(&state).unwrap();
        let mut encoded = Rows::new(directory.path()).unwrap();
        aggregate.write_governance(&mut encoded).unwrap();
        let mut typed = Rows::new(directory.path()).unwrap();
        typed.governance_projection = Some(aggregate.governance.clone());
        aggregate.write_governance(&mut typed).unwrap();
        assert_eq!(typed.bytes, encoded.bytes);
        assert!(typed
            .values
            .get(UndoTable::BrnMeta, b"governance_engine")
            .unwrap()
            .is_none());
        typed.bytes = MAX_BYTES;
        assert!(aggregate.write_governance(&mut typed).is_err());
        typed.publication_lifecycle = Some(Default::default());
        aggregate.write_governance(&mut typed).unwrap();
        assert!(typed.bytes > MAX_BYTES);
        typed.bytes = MAX_WORK_BYTES;
        assert!(aggregate.write_governance(&mut typed).is_err());
        let mut encoded = Rows::new(directory.path()).unwrap();
        aggregate.write_verification(&mut encoded).unwrap();
        let mut typed = Rows::new(directory.path()).unwrap();
        typed.verification_projection = Some(aggregate.verification.storage_clone());
        aggregate.write_verification(&mut typed).unwrap();
        assert_eq!(typed.bytes, encoded.bytes);
        assert!(typed
            .values
            .get(
                UndoTable::Meta,
                crate::endorsement_commit::SNAPSHOT_KEY.as_bytes()
            )
            .unwrap()
            .is_none());
        typed.bytes = MAX_BYTES;
        assert!(aggregate.write_verification(&mut typed).is_err());
        typed.publication_lifecycle = Some(Default::default());
        aggregate.write_verification(&mut typed).unwrap();
        assert!(typed.bytes > MAX_BYTES);
        typed.bytes = MAX_WORK_BYTES;
        assert!(aggregate.write_verification(&mut typed).is_err());
        for normalized in [false, true] {
            for changed in [false, true] {
                let old = Aggregates::capture(&state).unwrap();
                let mut new = Aggregates::capture(&state).unwrap();
                if changed {
                    new.verification
                        .initialize_genesis(&burst_types::WalletAddress::new(
                            "brst_projection_human",
                        ));
                    new.delegation
                        .delegate(
                            &burst_types::WalletAddress::new("brst_projection_a"),
                            &burst_types::WalletAddress::new("brst_projection_b"),
                        )
                        .unwrap();
                    new.governance = burst_governance::GovernanceEngine::from_records([Ok(
                        burst_governance::engine::GovernanceRecord::PendingEffects {
                            changes: vec![(burst_governance::GovernableParam::BrnRate, 17)],
                            amendments: vec![],
                            evictions: vec![],
                        },
                    )])
                    .unwrap();
                    new.constitution = burst_consti::ConstiEngine::new();
                    new.rates
                        .apply_rate_change(17, Timestamp::new(1001))
                        .unwrap();
                }
                let mut full_old = Rows::new(directory.path()).unwrap();
                let mut full_new = Rows::new(directory.path()).unwrap();
                full_old.rate_records = normalized;
                full_new.rate_records = normalized;
                old.write_all(&mut full_old).unwrap();
                new.write_all(&mut full_new).unwrap();
                let mut before = Rows::new(directory.path()).unwrap();
                let mut after = Rows::new(directory.path()).unwrap();
                before.rate_records = normalized;
                after.rate_records = normalized;
                let mut normalized_before = Rows::new(directory.path()).unwrap();
                let mut normalized_after = Rows::new(directory.path()).unwrap();
                normalized_before.aggregates = Some(Aggregates::capture(&state).unwrap());
                normalized_after.aggregates = Some(Aggregates::capture(&state).unwrap());
                if changed {
                    normalized_after
                        .aggregates
                        .as_mut()
                        .unwrap()
                        .verification
                        .initialize_genesis(&burst_types::WalletAddress::new(
                            "brst_projection_human",
                        ));
                }
                assert_eq!(
                    materialize_transition(
                        &mut normalized_before,
                        &mut normalized_after,
                        true,
                        false
                    )
                    .unwrap()
                    .verification
                    .is_some(),
                    changed
                );
                assert_eq!(normalized_before.bytes + normalized_after.bytes, 0);
                before.aggregates = Some(old);
                after.aggregates = Some(new);
                materialize_changed(&mut before, &mut after).unwrap();
                let mut count = 0;
                full_old
                    .values
                    .visit(|table, key, old| {
                        let new = full_new.values.get(table, &key)?.unwrap();
                        let projected_old = before.values.get(table, &key)?;
                        let projected_new = after.values.get(table, &key)?;
                        if old != new {
                            count += 1;
                            assert_eq!(projected_old.as_ref(), Some(&old));
                            assert_eq!(projected_new.as_ref(), Some(&new));
                        } else if !changed {
                            assert!(projected_old.is_none() && projected_new.is_none());
                        }
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(count, if changed { 5 } else { 0 });
                if normalized && changed {
                    let key = burst_brn::rate_storage::row_key(1);
                    assert_eq!(
                        after.values.get(UndoTable::BrnMeta, &key).unwrap(),
                        full_new.values.get(UndoTable::BrnMeta, &key).unwrap()
                    );
                    assert!(before
                        .values
                        .get(UndoTable::BrnMeta, &key)
                        .unwrap()
                        .is_none());
                }
                if !changed {
                    assert_eq!(before.bytes + after.bytes, 0);
                }
                assert!(materialize_changed(&mut before, &mut after).is_err());
            }
        }
    }
}
