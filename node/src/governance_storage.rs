//! Governance reads shared by startup, candidates and checkpoint verification.
use bincode::Options;
use burst_governance::engine::{GovernanceEngine, GovernanceRecord};
use burst_store::StoreError;
use burst_store_lmdb::{block_undo::UndoTable, LmdbWriteTransaction};

fn invalid(reason: impl ToString) -> StoreError {
    StoreError::Corruption(reason.to_string())
}

fn row_format(batch: &LmdbWriteTransaction<'_>) -> Result<bool, StoreError> {
    row_format_view(&batch.read_view())
}

fn row_format_view(
    batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
) -> Result<bool, StoreError> {
    let keyed = batch.uses_governance_records()?;
    if keyed
        && batch
            .undo_value_ref(UndoTable::BrnMeta, b"governance_engine")?
            .is_some()
    {
        return Err(invalid("mixed aggregate and keyed governance state"));
    }
    if !keyed {
        for prefix in [
            GovernanceRecord::STORAGE_NAMESPACE,
            "governance_revision/v1/",
        ] {
            if let Some(row) = batch.checkpoint_metadata_entries(prefix.as_bytes())?.next() {
                row?;
                return Err(invalid("governance rows exist without a format marker"));
            }
        }
    }
    Ok(keyed)
}

/// Even an absent engine in keyed mode means this is not an untouched database.
pub(crate) fn present(batch: &LmdbWriteTransaction<'_>) -> Result<bool, StoreError> {
    Ok(row_format(batch)?
        || batch
            .undo_value_ref(UndoTable::BrnMeta, b"governance_engine")?
            .is_some())
}

pub(crate) fn load(
    batch: &LmdbWriteTransaction<'_>,
) -> Result<Option<GovernanceEngine>, StoreError> {
    if row_format(batch)? {
        let mut rows =
            batch.checkpoint_metadata_entries(GovernanceRecord::STORAGE_NAMESPACE.as_bytes())?;
        if rows.next().transpose()?.is_none() {
            return Ok(None);
        }
        return batch.read_governance_records().map(Some);
    }
    let Some(bytes) = batch.undo_value_ref(UndoTable::BrnMeta, b"governance_engine")? else {
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

pub(crate) fn load_view(
    batch: &burst_store_lmdb::read_view::LedgerReadView<'_>,
) -> Result<Option<GovernanceEngine>, StoreError> {
    if row_format_view(batch)? {
        let mut rows =
            batch.checkpoint_metadata_entries(GovernanceRecord::STORAGE_NAMESPACE.as_bytes())?;
        if rows.next().transpose()?.is_none() {
            return Ok(None);
        }
        return batch.read_governance_records().map(Some);
    }
    let Some(bytes) = batch.undo_value_ref(UndoTable::BrnMeta, b"governance_engine")? else {
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

/// Compare a captured keyed image with authenticated replay without rebuilding
/// a second engine. Exact inventory size rejects rows absent from replay.
pub(crate) fn verify_records_view(
    view: &burst_store_lmdb::read_view::LedgerReadView<'_>,
    expected: &GovernanceEngine,
    poll: &dyn Fn(&'static str) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    if !row_format_view(view)? {
        return Err(invalid("governance record mode is not initialized"));
    }
    let mut expected_count = 0usize;
    let mut failure = None;
    let result = expected.visit_records(|record| {
        let result = (|| {
            poll("metadata:governance-record")?;
            let key = record.storage_key().map_err(invalid)?;
            let bytes = view
                .undo_value_ref(UndoTable::Meta, key.as_bytes())?
                .ok_or_else(|| invalid("missing applied governance record"))?;
            let actual = GovernanceRecord::from_storage_entry(&key, bytes).map_err(invalid)?;
            if actual != record {
                return Err(invalid(
                    "persisted governance record differs from applied replay",
                ));
            }
            expected_count = expected_count
                .checked_add(1)
                .ok_or_else(|| invalid("governance record count overflow"))?;
            Ok(())
        })();
        result.map_err(|error: StoreError| {
            failure = Some(error);
            "governance record comparison failed".to_owned()
        })
    });
    if let Some(error) = failure {
        return Err(error);
    }
    result.map_err(invalid)?;
    let mut actual_count = 0usize;
    for row in view.checkpoint_metadata_entries(GovernanceRecord::STORAGE_NAMESPACE.as_bytes())? {
        poll("metadata:governance-inventory")?;
        row?;
        actual_count = actual_count
            .checked_add(1)
            .ok_or_else(|| invalid("governance record count overflow"))?;
        if actual_count > expected_count {
            return Err(invalid(
                "persisted governance inventory differs from applied replay",
            ));
        }
    }
    if actual_count != expected_count {
        return Err(invalid(
            "persisted governance inventory differs from applied replay",
        ));
    }
    Ok(())
}

/// Called only after authenticated startup recovery, before admission workers.
pub(crate) fn migrate(store: &burst_store_lmdb::LmdbStore) -> Result<bool, StoreError> {
    let mut batch = store.tx_begin_write()?;
    if !batch.migrate_governance_records()? {
        return Ok(false);
    }
    batch.commit()?;
    store.force_sync().map_err(invalid)?;
    Ok(true)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn readers_cover_legacy_keyed_absent_and_corrupt_states() {
        let dir = tempfile::tempdir().unwrap();
        let store = burst_store_lmdb::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let engine = GovernanceEngine::new();
        let mut batch = store.tx_begin_write().unwrap();
        assert!(!present(&batch).unwrap());
        assert_eq!(load(&batch).unwrap(), None);
        assert_eq!(load_view(&batch.read_view()).unwrap(), None);
        batch
            .put_brn_meta(b"governance_engine", &bincode::serialize(&engine).unwrap())
            .unwrap();
        assert!(present(&batch).unwrap());
        assert_eq!(load(&batch).unwrap(), Some(engine.clone()));
        assert_eq!(load_view(&batch.read_view()).unwrap(), Some(engine.clone()));
        batch.commit().unwrap();
        assert!(migrate(&store).unwrap());
        assert!(!migrate(&store).unwrap());
        drop(store);
        let store = burst_store_lmdb::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert!(present(&batch).unwrap());
        assert_eq!(load_view(&batch.read_view()).unwrap(), Some(engine.clone()));
        assert_eq!(load(&batch).unwrap(), Some(engine));
        let header = format!("{}0", GovernanceRecord::STORAGE_PREFIX);
        batch.put_meta(&header, b"bad").unwrap();
        assert!(load(&batch).is_err());
        assert!(load_view(&batch.read_view()).is_err());
        batch.delete_meta(&header).unwrap();
        assert_eq!(load(&batch).unwrap(), None);
        assert_eq!(load_view(&batch.read_view()).unwrap(), None);
        assert!(
            present(&batch).unwrap(),
            "mode persists even for an absent engine"
        );
        drop(batch);
        let mut batch = store.tx_begin_write().unwrap();
        batch.delete_meta("governance_records_format").unwrap();
        assert!(
            load(&batch).is_err(),
            "orphaned state must not look like an empty database"
        );
        assert!(present(&batch).is_err());
    }
    #[test]
    fn unknown_schema_invalidates_cached_governance_and_is_never_absent() {
        for layout in 0..3 {
            let dir = tempfile::tempdir().unwrap();
            let store = burst_store_lmdb::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
            let engine = GovernanceEngine::new();
            let mut batch = store.tx_begin_write().unwrap();
            if layout != 0 {
                batch.initialize_governance_records(&engine).unwrap();
                if layout == 1 {
                    batch.delete_meta("governance_state/v1/0").unwrap();
                }
            }
            batch.commit().unwrap();
            let expected = (layout == 2).then_some(engine.clone());
            let view = store.tx_begin_read().unwrap();
            assert_eq!(store.read_governance_snapshot(&view).unwrap(), expected);
            let identity = store.read_governance_projection_identity(&view).unwrap();
            drop(view);
            let mut batch = store.tx_begin_write().unwrap();
            assert_eq!(load(&batch).unwrap(), expected);
            assert_eq!(load_view(&batch.read_view()).unwrap(), expected);
            batch
                .put_meta("governance_state/v2/unknown", b"unsupported")
                .unwrap();
            assert!(
                load(&batch).is_err(),
                "layout {layout} must not reuse cached state or report absence"
            );
            assert!(load_view(&batch.read_view()).is_err());
            if layout != 0 {
                assert!(batch
                    .governance_undo_snapshot_with_overlay(&Default::default(), 64 << 20)
                    .is_err());
            }
            batch.commit().unwrap();
            let view = store.tx_begin_read().unwrap();
            assert!(store.read_governance_snapshot(&view).is_err());
            assert_ne!(
                store.read_governance_projection_identity(&view).unwrap(),
                identity
            );
            drop(view);
            assert!(load(&store.tx_begin_write().unwrap()).is_err());
        }
    }

    pub(crate) fn comparison_fixture() -> Vec<GovernanceRecord> {
        use burst_governance::{
            GovernableParam, GovernancePhase, GovernanceTiming, Proposal, ProposalContent,
        };
        use burst_transactions::governance::GovernanceVote;
        use burst_types::{BlockHash, Timestamp, TxHash, WalletAddress};
        let id = TxHash::new([17; 32]);
        let owner = WalletAddress::new("brst_governance_comparison");
        let proposal = Proposal {
            hash: id,
            proposer: owner.clone(),
            phase: GovernancePhase::Exploration,
            content: ProposalContent::ParameterChange {
                param: GovernableParam::BrnRate,
                new_value: 7,
            },
            endorsement_count: 1,
            total_eligible_voters: 3,
            exploration_started_at: Some(Timestamp::new(10)),
            exploration_votes_yea: 1,
            exploration_votes_nay: 0,
            exploration_votes_abstain: 0,
            cooldown_started_at: None,
            promotion_started_at: None,
            promotion_votes_yea: 0,
            promotion_votes_nay: 0,
            promotion_votes_abstain: 0,
            round: 0,
            created_at: Timestamp::new(1),
            activation_at: None,
        };
        vec![
            GovernanceRecord::PendingEffects {
                changes: vec![(GovernableParam::BrnRate, 9)],
                amendments: vec![],
                evictions: vec![(owner.clone(), true)],
            },
            GovernanceRecord::Proposal(Box::new(proposal)),
            GovernanceRecord::VoteBucket {
                proposal: id,
                promotion: false,
            },
            GovernanceRecord::VoteBucket {
                proposal: id,
                promotion: true,
            },
            GovernanceRecord::Vote {
                proposal: id,
                promotion: false,
                voter: owner.clone(),
                vote: GovernanceVote::Yea,
            },
            GovernanceRecord::Endorsement {
                proposal: id,
                count: 1,
                burned: u128::MAX,
            },
            GovernanceRecord::Timing {
                proposal: id,
                timing: GovernanceTiming {
                    proposal_secs: 10,
                    voting_secs: 20,
                    buffer_secs: 30,
                },
            },
            GovernanceRecord::KeyBallot {
                proposal: id,
                owner: owner.clone(),
                grant: BlockHash::new([18; 32]),
                vote: GovernanceVote::Abstain,
            },
        ]
    }

    #[test]
    fn captured_record_comparison_matches_loader_and_rejects_inventory_changes() {
        use burst_transactions::governance::GovernanceVote;
        use burst_types::{TxHash, WalletAddress};
        let id = TxHash::new([17; 32]);
        let records = comparison_fixture();
        let engine = GovernanceEngine::from_records(records.iter().cloned().map(Ok)).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = burst_store_lmdb::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut writer = store.tx_begin_write().unwrap();
        writer.initialize_governance_records(&engine).unwrap();
        writer.commit().unwrap();
        let reader = store.tx_begin_read().unwrap();
        let view = store.read_view(&reader).unwrap();
        assert_eq!(load_view(&view).unwrap(), Some(engine.clone()));
        verify_records_view(&view, &engine, &|_| Ok(())).unwrap();
        for stage in [
            "metadata:governance-record",
            "metadata:governance-inventory",
        ] {
            let polls = std::cell::Cell::new(0);
            let error = verify_records_view(&view, &engine, &|at| {
                if at == stage {
                    polls.set(polls.get() + 1);
                    if polls.get() == 2 {
                        return Err(StoreError::Backend("governance comparison sentinel".into()));
                    }
                }
                Ok(())
            })
            .unwrap_err();
            assert!(
                matches!(error, StoreError::Backend(ref text) if text == "governance comparison sentinel")
            );
            assert_eq!(polls.get(), 2);
        }
        // Each record kind remains necessary, and each decoder still rejects
        // trailing bytes. Abort each mutation so every case starts identically.
        for record in &records {
            let (key, bytes) = record.storage_entry().unwrap();
            for malformed in [false, true] {
                let mut writer = store.tx_begin_write().unwrap();
                if malformed {
                    let mut bad = bytes.clone();
                    bad.push(0);
                    writer.put_meta(&key, &bad).unwrap();
                } else {
                    writer.delete_meta(&key).unwrap();
                }
                let changed = writer.read_view();
                assert!(verify_records_view(&changed, &engine, &|_| Ok(())).is_err());
                assert!(load_view(&changed)
                    .map(|actual| actual != Some(engine.clone()))
                    .unwrap_or(true));
            }
        }
        for case in 0..5 {
            let mut writer = store.tx_begin_write().unwrap();
            match case {
                0 => {
                    let record = GovernanceRecord::Vote {
                        proposal: id,
                        promotion: false,
                        voter: WalletAddress::new("brst_extra_voter"),
                        vote: GovernanceVote::Nay,
                    };
                    let (key, bytes) = record.storage_entry().unwrap();
                    writer.put_meta(&key, &bytes).unwrap();
                }
                1 => {
                    writer
                        .put_meta("governance_state/v2/unknown", b"bad")
                        .unwrap();
                }
                2 => {
                    let bytes = bincode::serialize(&engine).unwrap();
                    // Normal aggregate writes in keyed mode update the rows;
                    // they do not leave a physical legacy image behind.
                    writer.put_brn_meta(b"governance_engine", &bytes).unwrap();
                    verify_records_view(&writer.read_view(), &engine, &|_| Ok(())).unwrap();
                    let marker = writer
                        .get_meta("governance_records_format")
                        .unwrap()
                        .unwrap();
                    writer.delete_meta("governance_records_format").unwrap();
                    writer.put_brn_meta(b"governance_engine", &bytes).unwrap();
                    writer
                        .put_meta("governance_records_format", &marker)
                        .unwrap();
                    assert!(writer
                        .read_view()
                        .undo_value_ref(UndoTable::BrnMeta, b"governance_engine")
                        .unwrap()
                        .is_some());
                }
                3 => {
                    writer.delete_meta("governance_records_format").unwrap();
                }
                _ => {
                    let record = GovernanceRecord::Endorsement {
                        proposal: id,
                        count: 2,
                        burned: u128::MAX,
                    };
                    let (key, bytes) = record.storage_entry().unwrap();
                    writer.put_meta(&key, &bytes).unwrap();
                }
            }
            assert!(
                verify_records_view(&writer.read_view(), &engine, &|_| Ok(())).is_err(),
                "case {case}"
            );
            assert!(
                load_view(&writer.read_view())
                    .map(|actual| actual != Some(engine.clone()))
                    .unwrap_or(true),
                "case {case}"
            );
        }
        let mut writer = store.tx_begin_write().unwrap();
        writer
            .delete_meta(&records[0].storage_key().unwrap())
            .unwrap();
        writer.commit().unwrap();
        verify_records_view(&view, &engine, &|_| Ok(())).unwrap();
        assert_eq!(load_view(&view).unwrap(), Some(engine.clone()));
        let writer = store.tx_begin_write().unwrap();
        assert!(verify_records_view(&writer.read_view(), &engine, &|_| Ok(())).is_err());
    }
}
