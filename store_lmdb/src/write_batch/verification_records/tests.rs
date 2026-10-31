use super::*;
fn state() -> VerificationOrchestrator {
    let mut engine = VerificationOrchestrator::new();
    for n in 0..128 {
        engine.initialize_genesis(&WalletAddress::new(format!("brst_target_{n}")));
    }
    engine
}
fn seed(batch: &mut LmdbWriteTransaction<'_>, engine: &VerificationOrchestrator) {
    batch
        .put_meta(
            "verification_orchestrator",
            &engine.snapshot().encode().unwrap(),
        )
        .unwrap();
}
fn begin_block(batch: &mut LmdbWriteTransaction<'_>, block: BlockHash) {
    let owner = WalletAddress::new("brst_journal");
    batch.begin_block_undo(block, owner.clone(), 1).unwrap();
    let mut info = AccountInfo::unopened(owner.clone());
    info.head = block;
    info.block_count = 1;
    batch
        .put_block_with_account(&block, b"block", &owner, 1)
        .unwrap();
    batch.put_account_info(&info, false).unwrap();
    batch.put_frontier(&owner, &block).unwrap();
}
fn bytes(engine: &VerificationOrchestrator) -> Vec<u8> {
    engine.snapshot().encode().unwrap()
}
#[test]
fn typed_recovery_matches_all_formats_and_rejects_block_writer_publication() {
    let original = state();
    let mut snapshot = original.snapshot();
    snapshot.states.remove(&WalletAddress::new("brst_target_0"));
    let mut after = VerificationOrchestrator::restore(snapshot);
    after.initialize_genesis(&WalletAddress::new("brst_recovered"));
    let mut params = burst_types::ProtocolParams::default();
    params.neither_penalty_threshold_bps = 1234;
    after.apply_governed_rules(&params);
    assert!(!after.record_scope_matches(&original));
    for format in 0..3 {
        let mut after = after.storage_clone();
        if format == 2 {
            let params = burst_types::ProtocolParams { endorsement_threshold: 2000, ..Default::default() };
            let target = WalletAddress::new("brst_recovered_large");
            for n in 0..1024 {
                after.process_endorsement_at(&target, &WalletAddress::new(format!("brst_recovered_endorser_{n:06}")),
                    params.endorsement_burn_amount, &params, Timestamp::new(1)).unwrap();
            }
            after.drain_events();
            assert!(after.target_record(&target).unwrap().storage_entry().is_err());
        }
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        seed(&mut batch, &original);
        if format == 1 { batch.migrate_verification_records().unwrap(); }
        if format == 2 { batch.normalize_verification_records().unwrap(); }
        batch.commit().unwrap();
        {
            let mut batch = store.tx_begin_write().unwrap();
            batch.retain_checkpoint_lifecycle(|key| key == LEGACY || key.starts_with(b"verification_state/")).unwrap();
            batch.publish_recovered_verification(&original).unwrap();
            assert!(!batch.changed_value, "unchanged verification projection rewrote format {format}");
        }
        let expected = {
            let mut batch = store.tx_begin_write().unwrap();
            seed(&mut batch, &after);
            batch.checkpoint_metadata(b"verification_").unwrap()
        };
        {
            let mut batch = store.tx_begin_write().unwrap();
            batch.publish_recovered_verification(&after).unwrap();
            assert_eq!(batch.checkpoint_metadata(b"verification_").unwrap(), expected);
            batch.publish_recovered_verification(&after).unwrap();
            assert_eq!(batch.checkpoint_metadata(b"verification_").unwrap(), expected);
        }
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.undo_value(UndoTable::Meta, LEGACY).unwrap(), Some(bytes(&original)));
        batch.publish_recovered_verification(&after).unwrap();
        batch.commit().unwrap();
        drop(store);
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.checkpoint_metadata(b"verification_").unwrap(), expected);
        begin_block(&mut batch, BlockHash::new([87; 32]));
        assert!(batch.publish_recovered_verification(&original).is_err());
        assert!(batch.commit().is_err());
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.checkpoint_metadata(b"verification_").unwrap(), expected);
    }
}
#[test]
fn normalized_recovery_initializes_all_formats_atomically_and_preserves_journals() {
    let original = state();
    let mut after = original.storage_clone();
    let params = burst_types::ProtocolParams { endorsement_threshold: 2000, ..Default::default() };
    let target = WalletAddress::new("brst_full_recovery_large");
    for n in 0..1024 {
        after.process_endorsement_at(&target, &WalletAddress::new(format!("brst_full_recovery_{n:06}")),
            params.endorsement_burn_amount, &params, Timestamp::new(1)).unwrap();
    }
    after.drain_events();
    assert!(after.target_record(&target).unwrap().storage_entry().is_err());
    for format in [None, Some(0), Some(1), Some(2)] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        if format.is_some() { seed(&mut batch, &original); }
        if format == Some(1) { batch.migrate_verification_records().unwrap(); }
        if format == Some(2) { batch.normalize_verification_records().unwrap(); }
        batch.commit().unwrap();
        let block = BlockHash::new([89; 32]);
        let mut batch = store.tx_begin_write().unwrap();
        begin_block(&mut batch, block);
        batch.commit().unwrap();
        let (before, journals) = {
            let batch = store.tx_begin_write().unwrap();
            (batch.checkpoint_metadata(b"verification_").unwrap(),
             batch.checkpoint_metadata(crate::block_undo::PREFIX).unwrap())
        };
        {
            let mut batch = store.tx_begin_write().unwrap();
            batch.publish_normalized_recovered_verification(&after).unwrap();
            assert!(batch.uses_normalized_verification_records().unwrap());
            assert_eq!(bytes(&batch.read_verification_records().unwrap()), bytes(&after));
            assert_eq!(batch.checkpoint_metadata(crate::block_undo::PREFIX).unwrap(), journals);
        }
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.checkpoint_metadata(b"verification_").unwrap(), before);
        batch.publish_normalized_recovered_verification(&after).unwrap();
        batch.commit().unwrap();
        drop(store);
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch.uses_normalized_verification_records().unwrap());
        assert_eq!(bytes(&batch.read_verification_records().unwrap()), bytes(&after));
        assert_eq!(batch.checkpoint_metadata(crate::block_undo::PREFIX).unwrap(), journals);
        assert!(batch.env.meta_db.get(&batch.txn, LEGACY).unwrap().is_none());
        batch.publish_normalized_recovered_verification(&after).unwrap();
        assert!(!batch.changed_value);
        begin_block(&mut batch, BlockHash::new([90; 32]));
        assert!(batch.publish_normalized_recovered_verification(&original).is_err());
        assert!(batch.commit().is_err());
    }
}

#[test]
fn migration_scoped_commit_abort_reopen_and_row_undo() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let original = state();
    seed(&mut batch, &original);
    assert!(batch.migrate_verification_records().unwrap());
    assert!(!batch.migrate_verification_records().unwrap());
    assert!(batch
        .undo_value_ref(UndoTable::Meta, LEGACY)
        .unwrap()
        .is_none());
    batch.commit().unwrap();
    let block = BlockHash::new([7; 32]);
    for commit in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        let before = batch.read_verification_records().unwrap();
        let mut after = before.clone();
        after.begin_record_changes();
        after.initialize_genesis(&WalletAddress::new("brst_new"));
        begin_block(&mut batch, block);
        batch
            .apply_verification_record_delta(&before, &after)
            .unwrap();
        assert_eq!(
            bytes(&batch.read_verification_records().unwrap()),
            bytes(&after)
        );
        if commit {
            batch.commit().unwrap();
        }
    }
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let undo = batch.block_undo(&block).unwrap().unwrap();
    assert_eq!(
        undo.changes
            .iter()
            .filter(|r| r
                .key
                .starts_with(VerificationRecord::STORAGE_PREFIX.as_bytes()))
            .count(),
        1
    );
    batch.rewind_block(&block).unwrap();
    assert_eq!(
        bytes(&batch.read_verification_records().unwrap()),
        bytes(&original)
    );
    batch.commit().unwrap();
}
#[test]
fn historical_aggregate_undo_and_overlay_survive_migration() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let original = state();
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch, &original);
    batch.commit().unwrap();
    let block = BlockHash::new([8; 32]);
    let mut after = original.clone();
    after.initialize_genesis(&WalletAddress::new("brst_added"));
    let mut batch = store.tx_begin_write().unwrap();
    begin_block(&mut batch, block);
    seed(&mut batch, &after);
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let historical = batch.block_undo(&block).unwrap().unwrap();
    batch.migrate_verification_records().unwrap();
    assert_eq!(batch.block_undo(&block).unwrap().unwrap(), historical);
    assert_eq!(
        batch.undo_value(UndoTable::Meta, LEGACY).unwrap(),
        Some(bytes(&after))
    );
    let key = after
        .target_record(&WalletAddress::new("brst_added"))
        .unwrap()
        .storage_entry()
        .unwrap()
        .0;
    let overlay = BTreeMap::from([((UndoTable::Meta, key.into_bytes()), None)]);
    assert_eq!(
        batch
            .verification_undo_snapshot_with_overlay(&overlay, 64 << 20)
            .unwrap(),
        Some(bytes(&original))
    );
    assert!(batch
        .verification_undo_snapshot_with_overlay(&overlay, 1)
        .is_err());
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.rewind_block(&block).unwrap();
    assert_eq!(
        bytes(&batch.read_verification_records().unwrap()),
        bytes(&original)
    );
    batch.commit().unwrap();
}
#[test]
fn stale_scope_failed_batch_and_global_rules_keep_conflict_guards() {
    for normalized in [false, true] {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let initial = state();
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch, &initial);
    batch.migrate_verification_records().unwrap();
    if normalized { batch.normalize_verification_records().unwrap(); }
    batch.commit().unwrap();
    let before = store
        .tx_begin_write()
        .unwrap()
        .read_verification_records()
        .unwrap();
    let mut after = before.clone();
    after.begin_record_changes();
    after.initialize_genesis(&WalletAddress::new("brst_new"));
    let unrelated = state();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch
        .apply_verification_record_delta(&unrelated, &after)
        .is_err());
    assert!(batch.commit().is_err());
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .apply_verification_record_delta(&before, &after)
        .unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch
        .apply_verification_record_delta(&before, &after)
        .is_err());
    assert!(batch.commit().is_err());
    let mut global = before.clone();
    global.begin_record_changes();
    let mut params = burst_types::ProtocolParams::default();
    params.neither_penalty_threshold_bps = 1234;
    global.apply_governed_rules(&params);
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch
        .apply_verification_record_delta(&before, &global)
        .is_err());
    assert!(batch.commit().is_err());
    let mut batch = store.tx_begin_write().unwrap();
    let before = batch.read_verification_records().unwrap();
    let mut global = before.clone();
    global.begin_record_changes();
    global.apply_governed_rules(&params);
    let block = BlockHash::new([9; 32]);
    begin_block(&mut batch, block);
    batch
        .apply_verification_record_delta(&before, &global)
        .unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let journal = batch.block_undo(&block).unwrap().unwrap();
    assert_eq!(
        journal.changes.iter().filter(|r| r.key == LEGACY).count(),
        1
    );
    assert!(!journal.changes.iter().any(|r| r
        .key
        .starts_with(VerificationRecord::STORAGE_PREFIX.as_bytes())));
    batch.rewind_block(&block).unwrap();
    assert_eq!(
        bytes(&batch.read_verification_records().unwrap()),
        bytes(&before)
    );
    }
}
#[test]
fn malformed_or_mixed_format_never_commits_migration() {
    for corrupt in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        if corrupt {
            batch
                .put_meta("verification_orchestrator", b"invalid")
                .unwrap();
        } else {
            let (key, value) = state().rules_record().storage_entry().unwrap();
            batch.put_meta(&key, &value).unwrap();
        }
        assert!(batch.migrate_verification_records().is_err());
        assert!(batch.commit().is_err());
        assert!(!store
            .tx_begin_write()
            .unwrap()
            .uses_verification_records()
            .unwrap());
    }
}

#[test]
fn schema_five_upgrade_keeps_legacy_verification_until_explicit_migration() {
    let dir = tempfile::tempdir().unwrap();
    let original = state();
    {
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        seed(&mut batch, &original);
        batch
            .put_meta("schema_version", &5u32.to_le_bytes())
            .unwrap();
        batch.commit().unwrap();
    }
    let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(
        batch.get_meta("schema_version").unwrap(),
        Some(crate::CURRENT_SCHEMA_VERSION.to_le_bytes().to_vec())
    );
    assert!(!batch.uses_verification_records().unwrap());
    assert_eq!(
        batch.undo_value(UndoTable::Meta, LEGACY).unwrap(),
        Some(bytes(&original))
    );
}

#[test]
fn overlay_budget_counts_canonical_snapshot_not_keyed_framing() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let original = state();
    let encoded = bytes(&original);
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch, &original);
    batch.migrate_verification_records().unwrap();
    let raw_bytes: usize = batch
        .checkpoint_metadata_entries(VerificationRecord::STORAGE_PREFIX.as_bytes())
        .unwrap()
        .map(|row| {
            let (k, v) = row.unwrap();
            k.len() + v.len()
        })
        .sum();
    assert!(
        raw_bytes > encoded.len(),
        "fixture must expose keyed framing overhead"
    );
    assert_eq!(
        batch
            .verification_undo_snapshot_with_overlay(&BTreeMap::new(), encoded.len())
            .unwrap(),
        Some(encoded.clone())
    );
    assert!(batch
        .verification_undo_snapshot_with_overlay(&BTreeMap::new(), encoded.len() - 1)
        .is_err());
    let key = original
        .target_record(&WalletAddress::new("brst_target_0"))
        .unwrap()
        .storage_entry()
        .unwrap()
        .0;
    let overlay = BTreeMap::from([((UndoTable::Meta, key.into_bytes()), None)]);
    let reduced = batch
        .verification_undo_snapshot_with_overlay(&overlay, encoded.len())
        .unwrap()
        .unwrap();
    assert_eq!(
        batch
            .verification_undo_snapshot_with_overlay(&overlay, reduced.len())
            .unwrap(),
        Some(reduced.clone())
    );
    assert!(batch
        .verification_undo_snapshot_with_overlay(&overlay, reduced.len() - 1)
        .is_err());
}

#[test]
fn cached_roots_follow_commits_aborts_external_writes_and_projection_clear() {
    for normalized in [false, true] {
    use burst_store::MetaStore;
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let original = state();
    seed(&mut batch, &original);
    batch.migrate_verification_records().unwrap();
    if normalized { batch.normalize_verification_records().unwrap(); }
    let root = batch.read_verification_records().unwrap();
    batch.commit().unwrap();
    for commit in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        let before = batch.read_verification_records().unwrap();
        assert!(before.shares_record_storage_with(&root));
        let mut after = before.clone();
        after.begin_record_changes();
        after.initialize_genesis(&WalletAddress::new("brst_cached_new"));
        batch
            .apply_verification_record_delta(&before, &after)
            .unwrap();
        let warm = batch.read_verification_records().unwrap();
        assert!(warm.shares_record_storage_with(&after));
        assert!(warm.visit_record_changes(|_, _| Ok(())).is_err());
        if commit {
            batch.commit().unwrap();
        }
    }
    let mut batch = store.tx_begin_write().unwrap();
    let committed = batch.read_verification_records().unwrap();
    assert!(committed
        .get_state(&WalletAddress::new("brst_cached_new"))
        .is_some());
    batch.put_meta("unrelated", b"value").unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(committed.shares_record_storage_with(&batch.read_verification_records().unwrap()));
    let key = committed
        .target_record(&WalletAddress::new("brst_target_0"))
        .unwrap()
        .storage_entry()
        .unwrap()
        .0;
    let key = if normalized { normalized::target_header_key(&WalletAddress::new("brst_target_0")).unwrap() } else { key };
    batch.put_meta(&key, b"invalid").unwrap();
    assert!(batch.read_verification_records().is_err());
    drop(batch);
    let batch = store.tx_begin_write().unwrap();
    assert!(committed.shares_record_storage_with(&batch.read_verification_records().unwrap()));
    drop(batch);
    store
        .meta_store()
        .put_meta("external_unrelated", b"value")
        .unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let cold = batch.read_verification_records().unwrap();
    assert!(!committed.shares_record_storage_with(&cold));
    assert_eq!(bytes(&cold), bytes(&committed));
    batch.clear_checkpoint_rows(UndoTable::Meta, None).unwrap();
    assert!(batch.read_verification_records().is_err());
    drop(batch);
    let mut batch = store.tx_begin_write().unwrap();
    batch.clear_checkpoint_lifecycle().unwrap();
    assert!(batch.read_verification_records().is_err());
    }
}

/// Diagnostic timings, not release throughput or a machine-specific latency gate.
#[test]
#[ignore = "explicit capped-host verification scaling probe"]
fn verification_scoped_storage_scaling() {
    use std::time::Instant;
    let mut expected_row_bytes = None;
    for population in [100usize, 1_000, 10_000] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 128 << 20).unwrap();
        let mut original = VerificationOrchestrator::new();
        for n in 0..population {
            original.initialize_genesis(&WalletAddress::new(format!("brst_scale_{n:08}")));
        }
        let snapshot_bytes = bytes(&original).len();
        let mut batch = store.tx_begin_write().unwrap();
        seed(&mut batch, &original);
        batch.migrate_verification_records().unwrap();
        batch.commit().unwrap();
        drop(store);
        let store = crate::LmdbStore::open(dir.path(), 32, 128 << 20).unwrap();
        let batch = store.tx_begin_write().unwrap();
        let start = Instant::now();
        let cold = batch.read_verification_records().unwrap();
        let cold_ns = start.elapsed().as_nanos();
        assert_eq!(bytes(&cold), bytes(&original));
        batch.commit().unwrap();
        for sample in 0..5 {
            let mut batch = store.tx_begin_write().unwrap();
            let start = Instant::now();
            let before = batch.read_verification_records().unwrap();
            let warm_ns = start.elapsed().as_nanos();
            assert!(before.shares_record_storage_with(&cold));
            let start = Instant::now();
            let mut after = before.storage_clone();
            after.begin_record_changes();
            after.initialize_genesis(&WalletAddress::new("brst_scale_added"));
            let mutation_ns = start.elapsed().as_nanos();
            let mut changed = Vec::new();
            after.visit_record_delta(&before, |key, old, new| {
                changed.push((key, old, new));
                Ok(())
            }).unwrap();
            assert_eq!(changed.len(), 1);
            let block = BlockHash::new([77; 32]);
            begin_block(&mut batch, block);
            let start = Instant::now();
            batch.apply_verification_record_delta(&before, &after).unwrap();
            let persist_ns = start.elapsed().as_nanos();
            assert!(batch.read_verification_records().unwrap().shares_record_storage_with(&after));
            assert!(before.get_state(&WalletAddress::new("brst_scale_added")).is_none());
            if sample == 4 {
                let start = Instant::now();
                batch.commit().unwrap();
                let commit_ns = start.elapsed().as_nanos();
                let batch = store.tx_begin_write().unwrap();
                let undo = batch.block_undo(&block).unwrap().unwrap();
                let rows: Vec<_> = undo.changes.iter().filter(|change| {
                    change.table == UndoTable::Meta
                        && change.key.starts_with(VerificationRecord::STORAGE_PREFIX.as_bytes())
                }).collect();
                assert_eq!(rows.len(), 1);
                assert!(rows[0].before.is_none());
                assert!(!undo.changes.iter().any(|change| change.key == LEGACY));
                let row_bytes = rows[0].key.len() + rows[0].after.as_ref().unwrap().len();
                if let Some(expected) = expected_row_bytes {
                    assert_eq!(row_bytes, expected, "unrelated state must not enlarge the undo row");
                } else {
                    expected_row_bytes = Some(row_bytes);
                }
                println!("verification_scale population={population} snapshot_bytes={snapshot_bytes} cold_ns={cold_ns} sample={sample} warm_ns={warm_ns} mutation_ns={mutation_ns} persist_ns={persist_ns} commit_ns={commit_ns} undo_rows=1 undo_row_bytes={row_bytes}");
            } else {
                println!("verification_scale population={population} snapshot_bytes={snapshot_bytes} cold_ns={cold_ns} sample={sample} warm_ns={warm_ns} mutation_ns={mutation_ns} persist_ns={persist_ns} aborted=true");
                drop(batch);
            }
        }
        assert!(expected_row_bytes.is_some());
        drop(store);
        let store = crate::LmdbStore::open(dir.path(), 32, 128 << 20).unwrap();
        let batch = store.tx_begin_write().unwrap();
        let restored = batch.read_verification_records().unwrap();
        assert!(restored.get_state(&WalletAddress::new("brst_scale_added")).is_some());
        assert!(batch.get_meta("verification_orchestrator").unwrap().is_none());
    }
}

#[test]
fn normalized_large_target_commit_abort_reopen_and_inverse() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let target = WalletAddress::new("brst_large_normalized_target");
    let params = burst_types::ProtocolParams {
        endorsement_threshold: 2000, ..Default::default()
    };
    let mut original = VerificationOrchestrator::new();
    for n in 0..1024 {
        original.process_endorsement_at(&target, &WalletAddress::new(format!("brst_endorser_{n:06}")),
            params.endorsement_burn_amount, &params, burst_types::Timestamp::new(1)).unwrap();
    }
    original.drain_events();
    assert!(original.target_record(&target).unwrap().storage_entry().is_err());
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch, &original);
    batch.normalize_verification_records().unwrap();
    assert!(!batch.normalize_verification_records().unwrap());
    assert_eq!(bytes(&batch.read_verification_records().unwrap()), bytes(&original));
    batch.commit().unwrap();
    let block = BlockHash::new([51; 32]);
    for commit in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        let before = batch.read_verification_records().unwrap();
        assert_eq!(bytes(&before), bytes(&original));
        let mut after = before.clone();
        after.begin_record_changes();
        after.process_endorsement_at(&target, &WalletAddress::new("brst_one_more"),
            params.endorsement_burn_amount, &params, burst_types::Timestamp::new(2)).unwrap();
        after.drain_events();
        begin_block(&mut batch, block);
        batch.apply_verification_record_delta(&before, &after).unwrap();
        assert_eq!(bytes(&batch.read_verification_records().unwrap()), bytes(&after));
        if commit { batch.commit().unwrap(); }
    }
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let journal = batch.block_undo(&block).unwrap().unwrap();
    assert_eq!(journal.changes.iter().filter(|c| c.key.starts_with(normalized::PREFIX.as_bytes())).count(), 3);
    batch.rewind_block(&block).unwrap();
    assert_eq!(bytes(&batch.read_verification_records().unwrap()), bytes(&original));
    batch.commit().unwrap();
}

#[test]
fn normalized_migration_preserves_both_journal_formats_and_sorted_overlay() {
    for keyed in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let original = state();
        let mut batch = store.tx_begin_write().unwrap();
        seed(&mut batch, &original);
        if keyed { batch.migrate_verification_records().unwrap(); }
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let before = if keyed { batch.read_verification_records().unwrap() } else { original.clone() };
        let mut after = before.clone();
        after.begin_record_changes();
        after.initialize_genesis(&WalletAddress::new("brst_aaa"));
        after.initialize_genesis(&WalletAddress::new("brst_zzz"));
        let block = BlockHash::new([52; 32]);
        begin_block(&mut batch, block);
        if keyed { batch.apply_verification_record_delta(&before, &after).unwrap(); }
        else { seed(&mut batch, &after); }
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let historical = batch.block_undo(&block).unwrap().unwrap();
        batch.normalize_verification_records().unwrap();
        if !keyed { assert_eq!(batch.block_undo(&block).unwrap().unwrap(), historical); }
        let mut overlay = BTreeMap::new();
        normalized::visit_state_rows(&after, |key, _| {
            if key.starts_with(normalized::PREFIX) { overlay.insert((UndoTable::Meta, key.into_bytes()), None); }
            Ok(())
        }).unwrap();
        normalized::visit_state_rows(&original, |key, bytes| {
            overlay.insert((UndoTable::Meta, key.into_bytes()), Some(bytes)); Ok(())
        }).unwrap();
        assert_eq!(batch.verification_undo_snapshot_with_overlay(&overlay, 64 << 20).unwrap(), Some(bytes(&original)));
        assert!(batch.verification_undo_snapshot_with_overlay(&overlay, 1).is_err());
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.rewind_block(&block).unwrap();
        assert_eq!(bytes(&batch.read_verification_records().unwrap()), bytes(&original));
        batch.commit().unwrap();
    }
}

#[test]
fn normalized_migration_cache_is_private_until_commit_and_invalidated_by_writes() {
    for keyed in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let original = state();
        let mut batch = store.tx_begin_write().unwrap();
        seed(&mut batch, &original);
        if keyed { batch.migrate_verification_records().unwrap(); }
        batch.commit().unwrap();
        {
            let mut batch = store.tx_begin_write().unwrap();
            batch.normalize_verification_records().unwrap();
            let cached = batch.auxiliary_cache.borrow().verification.as_ref().unwrap().storage_clone();
            assert!(cached.shares_record_storage_with(&batch.read_verification_records().unwrap()));
            assert_eq!(bytes(&cached), bytes(&original));
            let cold = batch.read_verification_overlay(&BTreeMap::new(), 64 << 20).unwrap().unwrap();
            assert_eq!(bytes(&cached), bytes(&cold));
            let key = normalized::target_header_key(&WalletAddress::new("brst_target_0")).unwrap();
            batch.put_meta(&key, b"invalid").unwrap();
            assert!(batch.auxiliary_cache.borrow().verification.is_none());
            assert!(batch.read_verification_records().is_err());
            // Drop the entire migration and corrupt row without publication.
        }
        let mut batch = store.tx_begin_write().unwrap();
        assert!(!batch.uses_normalized_verification_records().unwrap());
        batch.normalize_verification_records().unwrap();
        batch.commit().unwrap();
        drop(store);
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(bytes(&batch.read_verification_records().unwrap()), bytes(&original));
    }
}

#[test]
fn normalized_migration_bad_journal_aborts_all_state_and_marker_writes() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let original = state();
    seed(&mut batch, &original);
    batch.migrate_verification_records().unwrap();
    let key = crate::block_undo::key(&BlockHash::new([53; 32]));
    batch.env.meta_db.put(&mut batch.txn, &key, b"bad journal").unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch.normalize_verification_records().is_err());
    assert!(batch.commit().is_err());
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch.publish_normalized_recovered_verification(&original).is_err());
    assert!(batch.commit().is_err());
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.get_meta(MODE).unwrap(), Some(vec![1]));
    assert_eq!(bytes(&batch.read_verification_records().unwrap()), bytes(&original));
    assert!(batch.checkpoint_metadata_entries(normalized::PREFIX.as_bytes()).unwrap().next().is_none());
}

#[test]
fn normalized_large_delta_promotes_composed_undo_and_keeps_abort_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 128 << 20).unwrap();
    let original = VerificationOrchestrator::new();
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch, &original);
    batch.normalize_verification_records().unwrap();
    batch.commit().unwrap();
    let block = BlockHash::new([54; 32]);
    let mut expected = Vec::new();
    for commit in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        begin_block(&mut batch, block);
        batch.put_meta("unrelated_promoted_value", b"keep inverse").unwrap();
        let before = batch.read_verification_records().unwrap();
        assert_eq!(bytes(&before), bytes(&original));
        let mut small = before.clone();
        small.begin_record_changes();
        small.initialize_genesis(&WalletAddress::new("brst_before_large"));
        batch.apply_verification_record_delta(&before, &small).unwrap();
        let mut large = small.clone();
        large.begin_record_changes();
        // One header per target exceeds the 65,536-change journal limit.
        for n in 0..65_537 {
            large.initialize_genesis(&WalletAddress::new(format!("brst_large_{n:06}")));
        }
        batch.apply_verification_record_delta(&small, &large).unwrap();
        let mut last = large.clone();
        last.begin_record_changes();
        last.initialize_genesis(&WalletAddress::new("brst_after_large"));
        batch.apply_verification_record_delta(&large, &last).unwrap();
        expected = bytes(&last);
        assert_eq!(bytes(&batch.read_verification_records().unwrap()), expected);
        if commit { batch.commit().unwrap(); }
    }
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 128 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert_eq!(bytes(&batch.read_verification_records().unwrap()), expected);
    let journal = batch.block_undo(&block).unwrap().unwrap();
    assert_eq!(journal.changes.iter().filter(|c| c.key == LEGACY).count(), 1);
    assert!(!journal.changes.iter().any(|c| c.key.starts_with(b"verification_state/")));
    let change = journal.changes.iter().find(|c| c.key == LEGACY).unwrap();
    assert_eq!(change.before, Some(bytes(&original)));
    assert_eq!(change.after, Some(expected));
    assert!(journal.changes.iter().any(|c| c.key == b"unrelated_promoted_value"));
    batch.rewind_block(&block).unwrap();
    assert_eq!(bytes(&batch.read_verification_records().unwrap()), bytes(&original));
    assert!(batch.get_meta("unrelated_promoted_value").unwrap().is_none());
    batch.commit().unwrap();
}

#[test]
fn physical_then_global_rules_and_later_delta_share_original_undo_image() {
    for normalized in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let original = state();
        let mut batch = store.tx_begin_write().unwrap();
        seed(&mut batch, &original);
        batch.migrate_verification_records().unwrap();
        if normalized { batch.normalize_verification_records().unwrap(); }
        batch.commit().unwrap();
        let block = BlockHash::new([55; 32]);
        let mut batch = store.tx_begin_write().unwrap();
        begin_block(&mut batch, block);
        let mut before = batch.read_verification_records().unwrap();
        for step in 0..3 {
            let mut after = before.clone();
            after.begin_record_changes();
            if step == 1 {
                let params = burst_types::ProtocolParams { neither_penalty_threshold_bps: 1234, ..Default::default() };
                after.apply_governed_rules(&params);
            } else { after.initialize_genesis(&WalletAddress::new(format!("brst_composed_{step}"))); }
            batch.apply_verification_record_delta(&before, &after).unwrap();
            before = after;
        }
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let journal = batch.block_undo(&block).unwrap().unwrap();
        assert!(!journal.changes.iter().any(|c| c.key.starts_with(b"verification_state/")));
        batch.rewind_block(&block).unwrap();
        assert_eq!(bytes(&batch.read_verification_records().unwrap()), bytes(&original));
    }
}

#[test]
fn normalized_limits_exhausted_aggregate_budget_aborts_all_publication() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 256 << 20).unwrap();
    let original = state();
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch, &original);
    batch.normalize_verification_records().unwrap();
    batch.commit().unwrap();
    let block = BlockHash::new([56; 32]);
    let mut batch = store.tx_begin_write().unwrap();
    begin_block(&mut batch, block);
    let unrelated = vec![1; (64 << 20) - 8192];
    batch.put_meta("unrelated_full_journal", &unrelated).unwrap();
    drop(unrelated);
    let before = batch.read_verification_records().unwrap();
    let mut after = before.clone();
    after.begin_record_changes();
    for n in 0..128 {
        after.initialize_genesis(&WalletAddress::new(format!("brst_over_budget_{n}")));
    }
    let error = batch.apply_verification_record_delta(&before, &after).unwrap_err();
    assert!(error.to_string().contains("block undo requires incremental economic storage"), "{error}");
    assert!(batch.commit().is_err());
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(bytes(&batch.read_verification_records().unwrap()), bytes(&original));
    assert!(batch.get_meta("unrelated_full_journal").unwrap().is_none());
    assert!(batch.block_undo(&block).unwrap().is_none());
    assert!(batch.get_block(&block).is_err());
}

#[test]
fn normalized_limits_format_marker_is_not_reversible() {
    for format in [1, 2] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        seed(&mut batch, &state());
        batch.migrate_verification_records().unwrap();
        if format == 2 { batch.normalize_verification_records().unwrap(); }
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let block = BlockHash::new([57; 32]);
        begin_block(&mut batch, block);
        assert!(batch.put_meta(MODE, &[3]).is_err());
        assert!(batch.commit().is_err());
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.get_meta(MODE).unwrap(), Some(vec![format]));
        assert!(batch.block_undo(&block).unwrap().is_none());
    }
}

#[test]
fn normalized_historical_target_point_overlay_preserves_exact_image_and_scope() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let original = state();
    let target = WalletAddress::new("brst_target_0");
    let (key, expected) = original.target_record(&target).unwrap().storage_entry().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch, &original);
    batch.normalize_verification_records().unwrap();
    assert_eq!(batch.verification_target_undo_with_overlay(&key, &BTreeMap::new(), 64 << 20).unwrap(), Some(expected));
    assert!(batch.verification_target_undo_with_overlay(&key, &BTreeMap::new(), 1).is_err());
    let unknown = VerificationRecord::target_storage_key(&WalletAddress::new("brst_missing")).unwrap();
    assert_eq!(batch.verification_target_undo_with_overlay(&unknown, &BTreeMap::new(), 64 << 20).unwrap(), None);
    assert!(batch.verification_target_undo_with_overlay("verification_state/v1/1/BRST", &BTreeMap::new(), 64 << 20).is_err());
    let rows = normalized::target_rows(&original.target_record(&target).unwrap()).unwrap();
    let removed = rows.keys().map(|key| ((UndoTable::Meta, key.as_bytes().to_vec()), None)).collect();
    assert_eq!(batch.verification_target_undo_with_overlay(&key, &removed, 64 << 20).unwrap(), None);
    let mut changed = VerificationOrchestrator::new();
    let params = burst_types::ProtocolParams::default();
    changed.process_endorsement_at(&target, &WalletAddress::new("brst_new_endorser"),
        params.endorsement_burn_amount, &params, Timestamp::new(1)).unwrap();
    let record = changed.target_record(&target).unwrap();
    let mut overlay = removed;
    for (key, bytes) in normalized::target_rows(&record).unwrap() {
        overlay.insert((UndoTable::Meta, key.into_bytes()), Some(bytes));
    }
    // A point read must not restore an unrelated target, even when its bytes are
    // malformed. Full state reconstruction must still reject that corruption.
    batch.put_meta(&normalized::target_header_key(&WalletAddress::new("brst_target_1")).unwrap(), b"invalid").unwrap();
    assert_eq!(batch.verification_target_undo_with_overlay(&key, &overlay, 64 << 20).unwrap(), Some(record.storage_entry().unwrap().1));
    assert!(batch.read_verification_records().is_err());
    let header = normalized::target_header_key(&target).unwrap();
    overlay.insert((UndoTable::Meta, header.into_bytes()), None);
    assert!(batch.verification_target_undo_with_overlay(&key, &overlay, 64 << 20).is_err());
}

#[test]
fn retained_v1_target_journal_rewinds_normalized_members_without_rewriting_history() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let target = WalletAddress::new("brst_retained_target");
    let params = burst_types::ProtocolParams { endorsement_threshold: 10, ..Default::default() };
    let mut original = state();
    original.process_endorsement_at(&target, &WalletAddress::new("brst_first"),
        params.endorsement_burn_amount, &params, Timestamp::new(1)).unwrap();
    original.drain_events();
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch, &original);
    batch.migrate_verification_records().unwrap();
    batch.commit().unwrap();
    let block = BlockHash::new([58; 32]);
    let mut batch = store.tx_begin_write().unwrap();
    let before = batch.read_verification_records().unwrap();
    let mut after = before.clone();
    after.begin_record_changes();
    after.process_endorsement_at(&target, &WalletAddress::new("brst_second"),
        params.endorsement_burn_amount, &params, Timestamp::new(2)).unwrap();
    after.initialize_genesis(&WalletAddress::new("brst_created"));
    after.drain_events();
    begin_block(&mut batch, block);
    batch.apply_verification_record_delta(&before, &after).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let journal_key = crate::block_undo::key(&block);
    let original_journal = batch.undo_value_ref(UndoTable::Meta, &journal_key).unwrap().unwrap().to_vec();
    batch.normalize_verification_records().unwrap();
    assert_eq!(batch.undo_value_ref(UndoTable::Meta, &journal_key).unwrap(), Some(original_journal.as_slice()));
    batch.commit().unwrap();
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.undo_value_ref(UndoTable::Meta, &journal_key).unwrap(), Some(original_journal.as_slice()));
    batch.rewind_block(&block).unwrap();
    assert_eq!(bytes(&batch.read_verification_records().unwrap()), bytes(&original));
    assert!(batch.checkpoint_metadata_entries(b"verification_state/v1/1/").unwrap().next().is_none());
    batch.commit().unwrap();
}

#[test]
fn virtual_target_mutation_cannot_start_a_new_legacy_block_journal() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let original = state();
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch, &original);
    batch.normalize_verification_records().unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    begin_block(&mut batch, BlockHash::new([59; 32]));
    let key = VerificationRecord::target_storage_key(&WalletAddress::new("brst_target_0")).unwrap();
    assert!(batch.delete_meta(&key).is_err());
    assert!(batch.commit().is_err());
    assert_eq!(bytes(&store.tx_begin_write().unwrap().read_verification_records().unwrap()), bytes(&original));
}

#[test]
fn normalization_streams_history_larger_than_retained_buffer_and_preserves_bytes() {
    use crate::block_undo::UndoChange;
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 128 << 20).unwrap();
    let original = state();
    let mut batch = store.tx_begin_write().unwrap();
    seed(&mut batch, &original);
    batch.migrate_verification_records().unwrap();
    let mut identities = Vec::new();
    let mut total = 0;
    let journal_bytes = |n: u8| {
        let hash = BlockHash::new([n; 32]);
        let journal = BlockUndo {
            block: hash, account: WalletAddress::new(format!("brst_history_{n}")), height: 1,
            changes: vec![UndoChange { table: UndoTable::Meta, key: b"unrelated_history".to_vec(),
                before: None, after: Some(vec![n; 1 << 20]) }],
        };
        let bytes = bincode::serialize(&journal).unwrap();
        BlockUndo::decode(&hash, &bytes).unwrap();
        bytes
    };
    for n in 1..=65u8 {
        let bytes = journal_bytes(n);
        total += bytes.len();
        let key = crate::block_undo::key(&BlockHash::new([n; 32]));
        identities.push((key.clone(), n));
        batch.env.meta_db.put(&mut batch.txn, &key, &bytes).unwrap();
    }
    assert!(total > 64 << 20);
    batch.commit().unwrap();
    // Aborting after completed staging still leaves old rows, marker and history.
    for commit in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.get_meta(MODE).unwrap(), Some(vec![1]));
        batch.normalize_verification_records().unwrap();
        for (key, n) in &identities {
            assert_eq!(batch.undo_value_ref(UndoTable::Meta, key).unwrap().unwrap(), journal_bytes(*n));
        }
        if commit { batch.commit().unwrap(); }
    }
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 128 << 20).unwrap();
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(bytes(&batch.read_verification_records().unwrap()), bytes(&original));
    assert_eq!(batch.get_meta(MODE).unwrap(), Some(vec![2]));
    for (key, n) in identities {
        assert_eq!(batch.undo_value_ref(UndoTable::Meta, &key).unwrap().unwrap(), journal_bytes(n));
    }
}

#[test]
fn captured_verification_reader_preserves_both_record_formats_and_old_rules() {
    for normalized in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
        let mut writer = store.tx_begin_write().unwrap();
        seed(&mut writer, &state());
        writer.migrate_verification_records().unwrap();
        if normalized { writer.normalize_verification_records().unwrap(); }
        let before = bytes(&writer.read_verification_records().unwrap());
        writer.commit().unwrap();
        let reader = store.tx_begin_read().unwrap();
        let view = store.read_view(&reader).unwrap();
        assert_eq!(bytes(&view.read_verification_records().unwrap()), before);
        let expected = state();
        let rules = expected.rules_record();
        let mut seen = std::collections::BTreeSet::new();
        view.visit_verification_records_with_poll(before.len() as u64, &|_| Ok(()), |key, fragment| {
            assert!(seen.insert(key.to_owned()));
            let record = expected.record_at_storage_key(key).unwrap().unwrap();
            assert_eq!(fragment.encode().unwrap(),
                record.into_snapshot_fragment(&rules, before.len() as u64).unwrap().encode().unwrap());
            Ok(())
        }).unwrap();
        assert_eq!(seen.len(), 129);
        assert!(view.visit_verification_records_with_poll(before.len() as u64 - 1,
            &|_| Ok(()), |_, _| Ok(())).is_err());
        for stage in ["storage:verification-row", "storage:verification-finish"] {
            let error = view.visit_verification_records_with_poll(64 << 20,
                &|at| if at == stage { Err(StoreError::StaleRead(stage.into())) } else { Ok(()) },
                |_, _| Ok(())).unwrap_err();
            assert!(matches!(error, StoreError::StaleRead(ref message) if message == stage));
        }
        let mut calls = 0;
        let error = view.visit_verification_records_with_poll(64 << 20, &|_| Ok(()), |_, _| {
            calls += 1;
            Err(StoreError::StaleRead("consumer stopped".into()))
        }).unwrap_err();
        assert!(matches!(error, StoreError::StaleRead(ref message) if message == "consumer stopped"));
        assert_eq!(calls, 1);
        let mut writer = store.tx_begin_write().unwrap();
        writer.put_meta(MODE, &[99]).unwrap();
        writer.commit().unwrap();
        assert_eq!(bytes(&view.read_verification_records().unwrap()), before);
        let writer = store.tx_begin_write().unwrap();
        assert!(writer.read_verification_records().is_err());
        assert!(writer.read_view().read_verification_records().is_err());
        view.visit_verification_records_with_poll(64 << 20, &|_| Ok(()), |_, _| Ok(())).unwrap();
        assert!(writer.read_view().visit_verification_records_with_poll(64 << 20,
            &|_| Ok(()), |_, _| Ok(())).is_err());
    }
}
