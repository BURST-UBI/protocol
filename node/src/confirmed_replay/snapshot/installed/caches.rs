use super::economics::EconomicRead;
use super::*;
use bincode::Options;
use burst_store_lmdb::block_undo::UndoTable;
use serde::{de::DeserializeOwned, Serialize};

fn compare<T: DeserializeOwned + Serialize>(bytes: &[u8], expected: &T) -> Result<(), StoreError> {
    let actual: T = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(invalid)?;
    equivalent(&actual, expected)
}

fn equivalent<T: Serialize>(actual: &T, expected: &T) -> Result<(), StoreError> {
    if super::super::canonical::encode(actual).map_err(invalid)?
        != super::super::canonical::encode(expected).map_err(invalid)?
    {
        return Err(invalid("persisted engine differs from applied replay"));
    }
    Ok(())
}

pub(super) fn verify_read_with_poll(
    input: EconomicRead<'_, '_>,
    state: &crate::confirmed_replay::ConfirmedReplay,
    poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let view = input.view();
    compare(
        &input.read(UndoTable::BrnMeta, b"protocol_params")?,
        &state.params,
    )?;
    if input.read(
        UndoTable::BrnMeta,
        burst_verification::bootstrap::BootstrapPhase::KEY,
    )? != state.bootstrap_phase.encoding()
    {
        return Err(invalid(
            "persisted bootstrap phase differs from applied replay",
        ));
    }
    poll("metadata:verification-snapshot")?;
    if view.uses_verification_records()? {
        verify_verification_records_view(&view, &state.verification, poll)?;
    } else {
        let actual = input.verification_snapshot()?;
        if !super::super::canonical::equivalent_verification(
            actual.snapshot_view(),
            state.verification.snapshot_view(),
            view.storage_directory(),
        )
        .map_err(|error| invalid(format!("verification snapshot: {error}")))?
        {
            return Err(invalid(
                "verification snapshot: persisted engine differs from applied replay",
            ));
        }
    }
    verify_lifecycle_view_with_poll(&view, &state.lifecycle_metadata, poll)?;
    poll("metadata:governance-snapshot")?;
    let governance = state
        .governance
        .as_ref()
        .ok_or_else(|| invalid("applied replay lacks governance state"))?;
    if view.uses_governance_records()? {
        // Check the replayed engine's canonical encoding and total byte limit,
        // then compare persisted records individually.
        let canonical_failure = std::cell::RefCell::new(None);
        let checked = super::super::canonical::encode_governance_with_poll(
            &governance.governance,
            &mut std::io::sink(),
            Some(view.storage_directory()),
            &|| {
                poll("metadata:governance-canonical").map_err(|error| {
                    let message = error.to_string();
                    *canonical_failure.borrow_mut() = Some(error);
                    super::super::invalid(message)
                })
            },
        );
        if let Some(error) = canonical_failure.into_inner() {
            return Err(error);
        }
        checked.map_err(invalid)?;
        crate::governance_storage::verify_records_view(&view, &governance.governance, poll)?;
    } else {
        if !super::super::canonical::equivalent_governance(
            &input
                .governance()?
                .ok_or_else(|| invalid("missing applied governance state"))?,
            &governance.governance,
            view.storage_directory(),
        )
        .map_err(invalid)?
        {
            return Err(invalid("persisted engine differs from applied replay"));
        }
    }
    let bytes = input.read(UndoTable::Meta, b"consti_engine")?;
    let actual = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(&bytes)
        .map_err(invalid)?;
    if !super::super::canonical::equivalent_constitution(
        &actual,
        &governance.constitution,
        view.storage_directory(),
    )
    .map_err(invalid)?
    {
        return Err(invalid("persisted engine differs from applied replay"));
    }
    poll("metadata:delegation-snapshot")?;
    // The persisted delegation format contains the forward graph. Reverse
    // indexes are derived and key-ballot receipts are checked separately.
    compare_delegation(
        &input.read(
            UndoTable::Meta,
            burst_governance::DelegationEngine::meta_key().as_bytes(),
        )?,
        &governance.delegation,
    )
}

pub(super) fn verify_verification_records_view(
    view: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    expected: &burst_verification::VerificationOrchestrator,
    poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    // Check canonical encoding and total size before comparing records.
    let canonical_failure = std::cell::RefCell::new(None);
    let checked = super::super::canonical::validate_verification_with_poll(expected, &|| {
        poll("metadata:verification-canonical").map_err(|error| {
            let message = error.to_string();
            *canonical_failure.borrow_mut() = Some(error);
            super::super::invalid(message)
        })
    });
    if let Some(error) = canonical_failure.into_inner() {
        return Err(error);
    }
    checked.map_err(invalid)?;
    let mut expected_count = 0usize;
    let mut failure = None;
    let counted = expected.visit_record_keys(|_| {
        poll("metadata:verification-expected").map_err(|error| {
            let message = error.to_string();
            failure = Some(error);
            message
        })?;
        expected_count = expected_count
            .checked_add(1)
            .ok_or("verification record count overflow")?;
        Ok(())
    });
    if let Some(error) = failure {
        return Err(error);
    }
    counted.map_err(invalid)?;
    let rules = expected.rules_record();
    let mut actual_count = 0usize;
    view.visit_verification_records_with_poll(64 << 20, poll, |key, actual| {
        poll("metadata:verification-record")?;
        let record = expected
            .record_at_storage_key(key)
            .map_err(invalid)?
            .ok_or_else(|| invalid("persisted verification record has no replayed origin"))?;
        let expected = record
            .into_snapshot_fragment(&rules, 64 << 20)
            .map_err(invalid)?;
        equivalent(actual, &expected)
            .map_err(|error| invalid(format!("verification snapshot: {error}")))?;
        actual_count = actual_count
            .checked_add(1)
            .ok_or_else(|| invalid("verification record count overflow"))?;
        Ok(())
    })?;
    if actual_count != expected_count {
        return Err(invalid(
            "persisted verification inventory differs from applied replay",
        ));
    }
    Ok(())
}

fn compare_delegation(
    bytes: &[u8],
    expected: &burst_governance::DelegationEngine,
) -> Result<(), StoreError> {
    let actual = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(invalid)?;
    if !expected.matches_snapshot(&actual) {
        return Err(invalid("persisted engine differs from applied replay"));
    }
    Ok(())
}

#[cfg(test)]
fn verify_lifecycle(
    batch: &LmdbWriteTransaction<'_>,
    expected: &std::collections::HashMap<String, Vec<u8>>,
) -> Result<(), StoreError> {
    verify_lifecycle_view(&batch.read_view(), expected)
}

#[cfg(test)]
fn verify_lifecycle_view(
    batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    expected: &std::collections::HashMap<String, Vec<u8>>,
) -> Result<(), StoreError> {
    verify_lifecycle_view_with_poll(batch, expected, &|_| Ok(()))
}

fn verify_lifecycle_view_with_poll(
    batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    expected: &std::collections::HashMap<String, Vec<u8>>,
    poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    let mut count = 0usize;
    batch.visit_checkpoint_lifecycle(|key, bytes| {
        poll("metadata:lifecycle-row")?;
        let key = std::str::from_utf8(key).map_err(invalid)?;
        if crate::verification_storage::is_state_key(key) {
            return Ok(());
        }
        if expected.get(key).map(Vec::as_slice) != Some(bytes) {
            return Err(invalid(format!(
                "persisted lifecycle entry differs from applied replay: {key}"
            )));
        }
        count += 1;
        Ok(())
    })?;
    if count
        != expected
            .keys()
            .filter(|key| !crate::verification_storage::is_state_key(key))
            .count()
    {
        return Err(invalid(
            "persisted lifecycle inventory differs from applied replay",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegation_comparison_checks_forward_graph_scope_depth_and_strict_encoding() {
        use burst_governance::{
            vote_delegation::{DelegationScope, DelegationSnapshot},
            DelegationEngine,
        };
        use burst_types::WalletAddress;
        let owner = WalletAddress::new("brst_delegation_owner");
        let target = WalletAddress::new("brst_delegation_target");
        let mut engine = DelegationEngine::new(4);
        engine.delegate(&owner, &target).unwrap();
        engine
            .delegate_scoped(
                &owner,
                &target,
                DelegationScope::Category("economic".into()),
            )
            .unwrap();
        let bytes = engine.save_state();
        compare_delegation(&bytes, &engine).unwrap();
        let snapshot: DelegationSnapshot = bincode::deserialize(&bytes).unwrap();
        for case in 0..5 {
            let mut changed = snapshot.clone();
            match case {
                0 => changed.max_depth += 1,
                1 => {
                    changed.delegations.clear();
                }
                2 => {
                    changed.delegations.insert(owner.clone(), owner.clone());
                }
                3 => {
                    changed.scoped_delegations.clear();
                }
                _ => {
                    changed
                        .scoped_delegations
                        .insert((target.clone(), DelegationScope::Global), owner.clone());
                }
            }
            assert!(compare_delegation(&bincode::serialize(&changed).unwrap(), &engine).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(compare_delegation(&trailing, &engine).is_err());
        assert!(compare_delegation(&bytes[..bytes.len() - 1], &engine).is_err());
    }

    #[test]
    fn lifecycle_projection_streams_large_inventory_and_rejects_mismatches() {
        let directory = tempfile::tempdir().unwrap();
        let store = burst_store_lmdb::LmdbStore::open(directory.path(), 32, 64 << 20).unwrap();
        let mut expected = std::collections::HashMap::new();
        let mut batch = store.tx_begin_write().unwrap();
        for index in 0..131_073 {
            let key = format!("verification_receipt/{index:08}");
            batch.put_meta(&key, b"receipt").unwrap();
            expected.insert(key, b"receipt".to_vec());
        }
        // Intermediate orchestrator state is deliberately checked elsewhere.
        expected.insert(crate::endorsement_commit::SNAPSHOT_KEY.into(), vec![1]);
        batch
            .put_meta(crate::endorsement_commit::SNAPSHOT_KEY, &[2])
            .unwrap();
        batch
            .put_meta("checkpoint_round/v1/local", b"journal")
            .unwrap();
        let polls = std::cell::Cell::new(0);
        assert!(
            verify_lifecycle_view_with_poll(&batch.read_view(), &expected, &|stage| {
                assert_eq!(stage, "metadata:lifecycle-row");
                polls.set(polls.get() + 1);
                if polls.get() == 3 {
                    return Err(StoreError::Backend("lifecycle checkpoint sentinel".into()));
                }
                Ok(())
            })
            .unwrap_err()
            .to_string()
            .contains("lifecycle checkpoint sentinel")
        );
        assert_eq!(polls.get(), 3);
        verify_lifecycle(&batch, &expected).unwrap();
        let key = "verification_receipt/00000000";
        expected.insert(key.into(), b"changed".to_vec());
        assert!(verify_lifecycle(&batch, &expected).is_err());
        expected.remove(key);
        assert!(verify_lifecycle(&batch, &expected).is_err());
        expected.insert(key.into(), b"receipt".to_vec());
        expected.insert("verification_receipt/missing".into(), b"receipt".to_vec());
        assert!(verify_lifecycle(&batch, &expected).is_err());
        expected.remove("verification_receipt/missing");
        batch.commit().unwrap();
        store.force_sync().unwrap();
        drop(store);
        let store = burst_store_lmdb::LmdbStore::open(directory.path(), 32, 64 << 20).unwrap();
        verify_lifecycle(&store.tx_begin_write().unwrap(), &expected).unwrap();
    }
}
