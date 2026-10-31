use super::*;
use burst_governance::{GovernableParam, GovernancePhase, Proposal, ProposalContent};
use burst_types::ProtocolParams;

fn state() -> GovernanceEngine {
    let mut engine = GovernanceEngine::new();
    engine
        .submit_proposal(
            Proposal {
                hash: TxHash::new([1; 32]),
                proposer: WalletAddress::new("brst_governance"),
                phase: GovernancePhase::Proposal,
                content: ProposalContent::ParameterChange {
                    param: GovernableParam::BrnRate,
                    new_value: 1,
                },
                endorsement_count: 0,
                total_eligible_voters: 0,
                exploration_started_at: None,
                exploration_votes_yea: 0,
                exploration_votes_nay: 0,
                exploration_votes_abstain: 0,
                cooldown_started_at: None,
                promotion_started_at: None,
                promotion_votes_yea: 0,
                promotion_votes_nay: 0,
                promotion_votes_abstain: 0,
                round: 0,
                created_at: Timestamp::new(100),
                activation_at: None,
            },
            0,
            true,
            &ProtocolParams::default(),
        )
        .unwrap();
    engine
}

fn changed(before: &GovernanceEngine, amount: u128) -> GovernanceEngine {
    let mut after = before.clone();
    after
        .endorse_proposal(&TxHash::new([1; 32]), amount)
        .unwrap();
    after
}

#[test]
fn typed_recovery_matches_aggregate_publication_and_preserves_revision_failure_atomicity() {
    let before = state();
    let after = changed(&before, 7);
    let revision = format!("{PROPOSAL_REVISION_PREFIX}{}", TxHash::new([1; 32]));
    for keyed in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        if keyed { batch.initialize_governance_records(&before).unwrap(); }
        else { batch.put_brn_meta(b"governance_engine", &bincode::serialize(&before).unwrap()).unwrap(); }
        batch.put_meta(&revision, &13u128.to_be_bytes()).unwrap();
        batch.commit().unwrap();
        let expected = {
            let mut batch = store.tx_begin_write().unwrap();
            batch.put_brn_meta(b"governance_engine", &bincode::serialize(&after).unwrap()).unwrap();
            (batch.checkpoint_metadata(b"governance_").unwrap(),
             batch.undo_value(UndoTable::BrnMeta, b"governance_engine").unwrap())
        };
        {
            let mut batch = store.tx_begin_write().unwrap();
            batch.publish_recovered_governance(&after).unwrap();
            assert_eq!(batch.checkpoint_metadata(b"governance_").unwrap(), expected.0);
            assert_eq!(batch.undo_value(UndoTable::BrnMeta, b"governance_engine").unwrap(), expected.1);
            let revision_before = batch.get_meta(&revision).unwrap();
            batch.publish_recovered_governance(&after).unwrap();
            assert_eq!(batch.get_meta(&revision).unwrap(), revision_before);
            // Dropping the writer must leave the original projection intact.
        }
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.get_meta(&revision).unwrap(), Some(13u128.to_be_bytes().to_vec()));
        batch.publish_recovered_governance(&after).unwrap();
        batch.commit().unwrap();
        drop(store);
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.checkpoint_metadata(b"governance_").unwrap(), expected.0);
        if keyed {
            batch.put_meta(&revision, &u128::MAX.to_be_bytes()).unwrap();
            batch.commit().unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            assert!(batch.publish_recovered_governance(&before).is_err());
            assert!(batch.commit().is_err());
            let batch = store.tx_begin_write().unwrap();
            assert_eq!(batch.read_governance_records().unwrap(), after);
            assert_eq!(batch.get_meta(&revision).unwrap(), Some(u128::MAX.to_be_bytes().to_vec()));
        }
    }
}

#[test]
fn rows_commit_abort_reopen_and_join_block_undo() {
    let dir = tempfile::tempdir().unwrap();
    let before = state();
    let after = changed(&before, 7);
    let account = WalletAddress::new("brst_governance");
    let block = BlockHash::new([9; 32]);
    {
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.initialize_governance_records(&before).unwrap();
        assert_eq!(batch.read_governance_records().unwrap(), before);
        batch.commit().unwrap();
        {
            let mut batch = store.tx_begin_write().unwrap();
            batch
                .apply_governance_record_delta(&before, &after)
                .unwrap();
            assert_eq!(batch.read_governance_records().unwrap(), after);
        }
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.read_governance_records().unwrap(), before);
        batch.begin_block_undo(block, account.clone(), 1).unwrap();
        let mut info = AccountInfo::unopened(account.clone());
        info.head = block;
        info.block_count = 1;
        batch
            .put_block_with_account(&block, b"block", &account, 1)
            .unwrap();
        batch.put_account_info(&info, false).unwrap();
        batch.put_frontier(&account, &block).unwrap();
        batch
            .apply_governance_record_delta(&before, &after)
            .unwrap();
        batch.commit().unwrap();
    }
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.read_governance_records().unwrap(), after);
    let undo = batch.block_undo(&block).unwrap().unwrap();
    assert_eq!(
        undo.changes
            .iter()
            .filter(|r| r
                .key
                .starts_with(GovernanceRecord::STORAGE_PREFIX.as_bytes()))
            .count(),
        2
    );
    batch.rewind_block(&block).unwrap();
    assert_eq!(batch.read_governance_records().unwrap(), before);
    batch.commit().unwrap();
    assert_eq!(
        store
            .tx_begin_write()
            .unwrap()
            .read_governance_records()
            .unwrap(),
        before
    );
}

#[test]
fn stale_or_invalid_updates_poison_prior_writes() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let before = state();
    let after = changed(&before, 7);
    let mut batch = store.tx_begin_write().unwrap();
    batch.initialize_governance_records(&before).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta("unrelated", b"must abort").unwrap();
    // Proposal updates sort before endorsements, so this fails after the first
    // changed row has already been staged.
    let endorsement = GovernanceRecord::Endorsement {
        proposal: TxHash::new([1; 32]),
        count: 1,
        burned: 99,
    };
    let (key, bytes) = endorsement.storage_entry().unwrap();
    batch.put_meta(&key, &bytes).unwrap();
    assert!(batch
        .apply_governance_record_delta(&before, &after)
        .is_err());
    assert!(batch.commit().is_err());
    let mut batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.get_meta("unrelated").unwrap(), None);
    assert_eq!(batch.read_governance_records().unwrap(), before);
    assert!(batch.initialize_governance_records(&before).is_err());
    assert!(batch.commit().is_err());
}

fn with_key_ballot(engine: &GovernanceEngine, proposal: u8, owner: &str) -> GovernanceEngine {
    let mut records = Vec::new();
    engine
        .visit_records(|record| {
            records.push(Ok(record));
            Ok(())
        })
        .unwrap();
    records.push(Ok(GovernanceRecord::KeyBallot {
        proposal: TxHash::new([proposal; 32]),
        owner: WalletAddress::new(owner),
        grant: BlockHash::new([7; 32]),
        vote: burst_transactions::governance::GovernanceVote::Yea,
    }));
    GovernanceEngine::from_records(records).unwrap()
}

#[test]
fn rollback_conflicts_with_later_ballots_only_in_the_same_proposal() {
    for recovered in [false, true] {
    for same_proposal in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut before = state();
        let mut second = before.get_proposal(&TxHash::new([1; 32])).unwrap().clone();
        second.hash = TxHash::new([2; 32]);
        before
            .submit_proposal(second, 0, true, &ProtocolParams::default())
            .unwrap();
        let after = with_key_ballot(&before, 1, "brst_first_ballot");
        let later_proposal = if same_proposal { 1 } else { 2 };
        let later = with_key_ballot(&after, later_proposal, "brst_later_ballot");
        let block = BlockHash::new([9; 32]);
        let owner = WalletAddress::new("brst_governance");
        let mut batch = store.tx_begin_write().unwrap();
        batch.initialize_governance_records(&before).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.begin_block_undo(block, owner.clone(), 1).unwrap();
        let mut account = AccountInfo::unopened(owner.clone());
        account.head = block;
        account.block_count = 1;
        batch
            .put_block_with_account(&block, b"block", &owner, 1)
            .unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch.put_frontier(&owner, &block).unwrap();
        batch
            .apply_governance_record_delta(&before, &after)
            .unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        if recovered {
            let original = batch.block_undo(&block).unwrap().unwrap();
            let replayed = aggregate_journal(original, &before, &after);
            let projected = batch.project_recovered_governance_undo(replayed).unwrap();
            assert!(projected.changes.iter().any(|row| row.key.starts_with(PROPOSAL_REVISION_PREFIX.as_bytes())));
            assert!(!projected.changes.iter().any(|row| row.table == UndoTable::BrnMeta && row.key == b"governance_engine"));
            batch.replace_checkpoint_undo(&[projected]).unwrap();
        }
        batch.apply_governance_record_delta(&after, &later).unwrap();
        let key = format!("{PROPOSAL_REVISION_PREFIX}{}", TxHash::new([1; 32]));
        let revision = batch.get_meta(&key).unwrap();
        batch.apply_governance_record_delta(&later, &later).unwrap();
        assert_eq!(
            batch.get_meta(&key).unwrap(),
            revision,
            "no-op must not change conflict guard"
        );
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        if same_proposal {
            assert!(batch.rewind_block(&block).is_err());
            assert!(batch.commit().is_err());
            assert_eq!(
                store
                    .tx_begin_write()
                    .unwrap()
                    .read_governance_records()
                    .unwrap(),
                later
            );
        } else {
            batch.rewind_block(&block).unwrap();
            assert_eq!(
                batch.read_governance_records().unwrap(),
                with_key_ballot(&before, 2, "brst_later_ballot")
            );
            batch.commit().unwrap();
        }
    }
    }
}

fn with_pending_effect(engine: &GovernanceEngine) -> GovernanceEngine {
    let mut rows = Vec::new();
    engine
        .visit_records(|record| {
            rows.push(Ok(match record {
                GovernanceRecord::PendingEffects { .. } => GovernanceRecord::PendingEffects {
                    changes: vec![(GovernableParam::TrstExpirySecs, 42)],
                    amendments: Vec::new(),
                    evictions: Vec::new(),
                },
                other => other,
            }));
            Ok(())
        })
        .unwrap();
    GovernanceEngine::from_records(rows).unwrap()
}

#[test]
fn global_effect_undo_uses_complete_state_and_rejects_later_progress() {
    for later_progress in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let before = state();
        let after = with_pending_effect(&before);
        let block = BlockHash::new([8; 32]);
        let owner = WalletAddress::new("brst_global");
        let mut batch = store.tx_begin_write().unwrap();
        batch.initialize_governance_records(&before).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.begin_block_undo(block, owner.clone(), 1).unwrap();
        let mut account = AccountInfo::unopened(owner.clone());
        account.head = block;
        account.block_count = 1;
        batch
            .put_block_with_account(&block, b"block", &owner, 1)
            .unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch.put_frontier(&owner, &block).unwrap();
        batch
            .apply_governance_record_delta(&before, &after)
            .unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let undo = batch.block_undo(&block).unwrap().unwrap();
        assert!(undo
            .changes
            .iter()
            .any(|c| c.table == UndoTable::BrnMeta && c.key == b"governance_engine"));
        assert!(!undo.changes.iter().any(|c| c
            .key
            .starts_with(GovernanceRecord::STORAGE_PREFIX.as_bytes())));
        let live = if later_progress {
            with_key_ballot(&after, 1, "brst_later")
        } else {
            after.clone()
        };
        batch.apply_governance_record_delta(&after, &live).unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let result = batch.rewind_block_with_snapshots(&block, |key, before, after, current| {
            assert_eq!(key, b"governance_engine");
            GovernanceEngine::rewind_snapshot(before, after, current).map_err(invalid)
        });
        if later_progress {
            assert!(result.is_err());
            assert!(batch.commit().is_err());
            assert_eq!(
                store
                    .tx_begin_write()
                    .unwrap()
                    .read_governance_records()
                    .unwrap(),
                live
            );
        } else {
            result.unwrap();
            assert_eq!(batch.read_governance_records().unwrap(), before);
            batch.commit().unwrap();
        }
    }
}

#[test]
fn aggregate_overlay_tracks_keyed_then_global_undo_without_writing() {
    use std::collections::BTreeMap;
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let before = state();
    let global = with_pending_effect(&before);
    let live = with_key_ballot(&global, 1, "brst_overlay");
    let mut batch = store.tx_begin_write().unwrap();
    let mut overlay = BTreeMap::new();
    assert!(batch
        .governance_undo_snapshot_with_overlay(&overlay, 64 << 20)
        .unwrap()
        .is_none());
    batch.initialize_governance_records(&before).unwrap();
    batch
        .apply_governance_record_delta(&before, &global)
        .unwrap();
    batch.apply_governance_record_delta(&global, &live).unwrap();
    batch.commit().unwrap();
    let batch = store.tx_begin_write().unwrap();
    global
        .visit_record_delta(&live, |old, new| {
            let key = old.as_ref().or(new.as_ref()).unwrap().storage_key()?;
            let value = new
                .map(|record| record.storage_entry().map(|(_, bytes)| bytes))
                .transpose()?;
            overlay.insert((UndoTable::Meta, key.into_bytes()), value);
            Ok(())
        })
        .unwrap();
    let current = batch
        .governance_undo_snapshot_with_overlay(&overlay, 64 << 20)
        .unwrap()
        .unwrap();
    assert_eq!(
        bincode::deserialize::<GovernanceEngine>(&current).unwrap(),
        global
    );
    let restored = GovernanceEngine::rewind_snapshot(
        &bincode::serialize(&before).unwrap(),
        &bincode::serialize(&global).unwrap(),
        &current,
    )
    .unwrap();
    GovernanceEngine::visit_snapshot_record_delta(&current, &restored, |old, new| {
        let key = old.as_ref().or(new.as_ref()).unwrap().storage_key()?;
        let value = new
            .map(|record| record.storage_entry().map(|(_, bytes)| bytes))
            .transpose()?;
        overlay.insert((UndoTable::Meta, key.into_bytes()), value);
        Ok(())
    })
    .unwrap();
    let projected = batch
        .governance_undo_snapshot_with_overlay(&overlay, 64 << 20)
        .unwrap()
        .unwrap();
    assert_eq!(
        bincode::deserialize::<GovernanceEngine>(&projected).unwrap(),
        before
    );
    assert_eq!(batch.read_governance_records().unwrap(), live);
    // Unrelated journal families and tables must not enter reconstruction or
    // consume its encoded byte budget, even with identical key prefixes.
    for index in 0..4096 {
        overlay.insert((UndoTable::Meta, format!("unrelated/{index:05}").into_bytes()), Some(vec![0; 16]));
        overlay.insert((UndoTable::BrnMeta, format!("governance_state/v1/{index:05}").into_bytes()), Some(vec![0; 16]));
    }
    let padded = batch.governance_undo_snapshot_with_overlay(&overlay, 64 << 20).unwrap().unwrap();
    assert_eq!(bincode::deserialize::<GovernanceEngine>(&padded).unwrap(), before);
    assert_eq!(batch.read_governance_records().unwrap(), live);
    assert!(batch
        .governance_undo_snapshot_with_overlay(&overlay, 1)
        .is_err());
    let header = (
        UndoTable::Meta,
        format!("{}0", GovernanceRecord::STORAGE_PREFIX).into_bytes(),
    );
    overlay.insert(header.clone(), Some(b"bad".to_vec()));
    assert!(batch
        .governance_undo_snapshot_with_overlay(&overlay, 64 << 20)
        .is_err());
    overlay.insert(header, None);
    assert!(batch
        .governance_undo_snapshot_with_overlay(&overlay, 64 << 20)
        .is_err());
}

#[test]
fn aggregate_creation_undo_restores_absence_without_losing_storage_mode() {
    use std::collections::BTreeMap;
    let dir = tempfile::tempdir().unwrap();
    let initial = state();
    let block = BlockHash::new([6; 32]);
    {
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.initialize_governance_records(&initial).unwrap();
        batch
            .delete_raw(UndoTable::BrnMeta, b"governance_engine")
            .unwrap();
        assert!(batch.uses_governance_records().unwrap());
        assert_eq!(
            batch
                .undo_value(UndoTable::BrnMeta, b"governance_engine")
                .unwrap(),
            None
        );
        batch.commit().unwrap();
        let owner = WalletAddress::new("brst_absent_governance");
        let mut batch = store.tx_begin_write().unwrap();
        batch.begin_block_undo(block, owner.clone(), 1).unwrap();
        let mut account = AccountInfo::unopened(owner.clone());
        account.head = block;
        account.block_count = 1;
        batch
            .put_block_with_account(&block, b"block", &owner, 1)
            .unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch.put_frontier(&owner, &block).unwrap();
        batch
            .put_brn_meta(b"governance_engine", &bincode::serialize(&initial).unwrap())
            .unwrap();
        batch.commit().unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        let undo = batch.block_undo(&block).unwrap().unwrap();
        let image = undo
            .changes
            .iter()
            .find(|c| c.table == UndoTable::BrnMeta && c.key == b"governance_engine")
            .unwrap();
        assert!(image.before.is_none());
        let mut overlay = BTreeMap::new();
        GovernanceEngine::visit_optional_snapshot_record_delta(
            image.after.as_deref(),
            None,
            |old, new| {
                assert!(new.is_none());
                overlay.insert(
                    (UndoTable::Meta, old.unwrap().storage_key()?.into_bytes()),
                    None,
                );
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            batch
                .governance_undo_snapshot_with_overlay(&overlay, 64 << 20)
                .unwrap(),
            None
        );
        assert!(batch
            .undo_value(UndoTable::BrnMeta, b"governance_engine")
            .unwrap()
            .is_some());
        batch.rewind_block(&block).unwrap();
        assert!(batch.uses_governance_records().unwrap());
        assert_eq!(
            batch
                .undo_value(UndoTable::BrnMeta, b"governance_engine")
                .unwrap(),
            None
        );
        batch.commit().unwrap();
    }
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch.uses_governance_records().unwrap());
    assert_eq!(
        batch
            .undo_value(UndoTable::BrnMeta, b"governance_engine")
            .unwrap(),
        None
    );
    let empty = GovernanceEngine::new();
    batch
        .put_brn_meta(b"governance_engine", &bincode::serialize(&empty).unwrap())
        .unwrap();
    assert_eq!(batch.read_governance_records().unwrap(), empty);
    assert!(batch
        .undo_value(UndoTable::BrnMeta, b"governance_engine")
        .unwrap()
        .is_some());
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .begin_block_undo(BlockHash::new([4; 32]), WalletAddress::new("brst_mode"), 1)
        .unwrap();
    assert!(batch.put_meta(MODE_KEY, &[2]).is_err());
    assert!(batch.commit().is_err());
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(MODE_KEY, &[2]).unwrap();
    assert!(batch.uses_governance_records().is_err());
    assert!(batch.read_governance_records().is_err());
    assert!(batch.apply_governance_record_delta(&empty, &empty).is_err());
    assert!(batch.commit().is_err());
}

#[test]
fn schema_gate_is_atomic_and_legacy_open_does_not_convert() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_meta("schema_version", &3u32.to_le_bytes())
            .unwrap();
        batch.commit().unwrap();
    }
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let before = state();
    {
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(
            batch.get_meta("schema_version").unwrap(),
            Some(crate::CURRENT_SCHEMA_VERSION.to_le_bytes().to_vec())
        );
        assert!(!batch.uses_governance_records().unwrap());
        batch.initialize_governance_records(&before).unwrap();
        assert_eq!(
            batch.get_meta("schema_version").unwrap(),
            Some(crate::CURRENT_SCHEMA_VERSION.to_le_bytes().to_vec())
        );
        // Open upgraded the position index; dropping this writer still aborts governance conversion.
    }
    {
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(
            batch.get_meta("schema_version").unwrap(),
            Some(crate::CURRENT_SCHEMA_VERSION.to_le_bytes().to_vec())
        );
        assert!(!batch.uses_governance_records().unwrap());
        assert!(batch
            .checkpoint_metadata(GovernanceRecord::STORAGE_PREFIX.as_bytes())
            .unwrap()
            .is_empty());
        batch.initialize_governance_records(&before).unwrap();
        batch.commit().unwrap();
    }
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(
        batch.get_meta("schema_version").unwrap(),
        Some(crate::CURRENT_SCHEMA_VERSION.to_le_bytes().to_vec())
    );
    assert_eq!(batch.read_governance_records().unwrap(), before);
}

#[test]
fn initialization_rejects_unknown_schema_and_poisons_prior_writes() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    for schema in [None, Some(2u32), Some(crate::CURRENT_SCHEMA_VERSION + 1)] {
        let mut batch = store.tx_begin_write().unwrap();
        match schema {
            Some(version) => batch
                .put_meta("schema_version", &version.to_le_bytes())
                .unwrap(),
            None => batch.delete_meta("schema_version").unwrap(),
        }
        batch.put_meta("schema_probe", &[1]).unwrap();
        assert!(batch.initialize_governance_records(&state()).is_err());
        assert!(batch.commit().is_err());
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(
            batch.get_meta("schema_version").unwrap(),
            Some(crate::CURRENT_SCHEMA_VERSION.to_le_bytes().to_vec())
        );
        assert_eq!(batch.get_meta("schema_probe").unwrap(), None);
        assert!(!batch.uses_governance_records().unwrap());
    }
}

#[test]
fn legacy_conversion_preserves_journal_and_rewinds_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let before = state();
    let after = changed(&before, 7);
    let account = WalletAddress::new("brst_governance");
    let block = BlockHash::new([91; 32]);
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .put_meta("schema_version", &3u32.to_le_bytes())
        .unwrap();
    batch
        .put_brn_meta(b"governance_engine", &bincode::serialize(&before).unwrap())
        .unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.begin_block_undo(block, account.clone(), 1).unwrap();
    let mut info = AccountInfo::unopened(account.clone());
    info.head = block;
    info.block_count = 1;
    batch
        .put_block_with_account(&block, b"block", &account, 1)
        .unwrap();
    batch.put_account_info(&info, false).unwrap();
    batch.put_frontier(&account, &block).unwrap();
    batch
        .put_brn_meta(b"governance_engine", &bincode::serialize(&after).unwrap())
        .unwrap();
    batch.commit().unwrap();
    let journal_key = crate::block_undo::key(&block);
    let original = store
        .tx_begin_write()
        .unwrap()
        .undo_value(UndoTable::Meta, &journal_key)
        .unwrap();
    {
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch.migrate_governance_records().unwrap());
        assert_eq!(batch.read_governance_records().unwrap(), after);
        assert!(batch
            .undo_value_ref(UndoTable::BrnMeta, b"governance_engine")
            .unwrap()
            .is_none());
        // Abort conversion and verify that both old schema and aggregate survive.
    }
    let mut batch = store.tx_begin_write().unwrap();
    assert_eq!(
        batch.get_meta("schema_version").unwrap(),
        Some(3u32.to_le_bytes().to_vec())
    );
    assert!(!batch.uses_governance_records().unwrap());
    assert_eq!(
        batch
            .undo_value_ref(UndoTable::BrnMeta, b"governance_engine")
            .unwrap(),
        Some(bincode::serialize(&after).unwrap().as_slice())
    );
    assert!(batch.migrate_governance_records().unwrap());
    assert_eq!(
        batch.undo_value(UndoTable::Meta, &journal_key).unwrap(),
        original
    );
    batch.commit().unwrap();
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(!batch.migrate_governance_records().unwrap());
    assert!(!batch.dirty);
    assert_eq!(
        batch.undo_value(UndoTable::Meta, &journal_key).unwrap(),
        original
    );
    batch.rewind_block(&block).unwrap();
    assert_eq!(batch.read_governance_records().unwrap(), before);
    batch.commit().unwrap();
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    assert_eq!(
        store
            .tx_begin_write()
            .unwrap()
            .read_governance_records()
            .unwrap(),
        before
    );
}

#[test]
fn malformed_legacy_conversion_aborts_all_prior_writes() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    for bytes in [
        None,
        Some(vec![1]),
        Some({
            let mut bytes = bincode::serialize(&state()).unwrap();
            bytes.push(0);
            bytes
        }),
    ] {
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_meta("schema_version", &3u32.to_le_bytes())
            .unwrap();
        batch.put_meta("migration_probe", &[1]).unwrap();
        if let Some(bytes) = bytes {
            batch.put_brn_meta(b"governance_engine", &bytes).unwrap();
        }
        assert!(batch.migrate_governance_records().is_err());
        assert!(batch.commit().is_err());
        let batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.get_meta("migration_probe").unwrap(), None);
        assert_eq!(
            batch.get_meta("schema_version").unwrap(),
            Some(crate::CURRENT_SCHEMA_VERSION.to_le_bytes().to_vec())
        );
        assert!(!batch.uses_governance_records().unwrap());
    }
}

#[test]
fn checkpoint_replacement_repairs_corrupt_rows_and_resets_local_guards_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let expected = state();
    let header = format!("{}0", GovernanceRecord::STORAGE_PREFIX);
    let guard = format!("{PROPOSAL_REVISION_PREFIX}old");
    let mut batch = store.tx_begin_write().unwrap();
    batch.initialize_governance_records(&expected).unwrap();
    batch.put_meta(&header, b"corrupt").unwrap();
    batch.put_meta(&guard, b"corrupt local guard").unwrap();
    batch.put_meta("unrelated", b"retain").unwrap();
    batch.commit().unwrap();
    for commit in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        assert!(batch.read_governance_records().is_err());
        batch
            .replace_checkpoint_governance_records(&expected)
            .unwrap();
        assert_eq!(batch.read_governance_records().unwrap(), expected);
        assert_eq!(batch.get_meta(&guard).unwrap(), None);
        assert_eq!(
            batch.get_meta("unrelated").unwrap(),
            Some(b"retain".to_vec())
        );
        assert_eq!(
            batch.get_meta("schema_version").unwrap(),
            Some(crate::CURRENT_SCHEMA_VERSION.to_le_bytes().to_vec())
        );
        if commit {
            batch.commit().unwrap();
        }
    }
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.read_governance_records().unwrap(), expected);
    // Replacement cannot strand old journal evidence or join ordinary admission.
    let key = String::from_utf8(crate::block_undo::key(&BlockHash::new([99; 32]))).unwrap();
    batch.put_meta(&key, b"retained journal evidence").unwrap();
    assert!(batch
        .replace_checkpoint_governance_records(&expected)
        .is_err());
    assert!(batch.commit().is_err());
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .begin_block_undo(
            BlockHash::new([99; 32]),
            WalletAddress::new("brst_governance"),
            1,
        )
        .unwrap();
    assert!(batch
        .replace_checkpoint_governance_records(&expected)
        .is_err());
    assert!(batch.commit().is_err());
}

#[test]
fn committed_cache_tracks_writer_generations_and_never_publishes_aborts() {
    use burst_store::MetaStore;
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &[1; 32]).unwrap();
    batch.initialize_governance_records(&state()).unwrap();
    let original = batch.read_governance_records().unwrap();
    batch.commit().unwrap();
    crate::write_batch::auxiliary_cache::reserve_test_votes(&store);
    let mut batch = store.tx_begin_write().unwrap();
    let shared = batch.read_governance_records().unwrap();
    assert!(original.shares_record_storage_with(&shared));
    batch.put_meta("unrelated", b"payment-like write").unwrap();
    batch.commit().unwrap();
    {
        let mut batch = store.tx_begin_write().unwrap();
        let before = batch.read_governance_records().unwrap();
        assert!(original.shares_record_storage_with(&before));
        let after = changed(&before, 7);
        batch
            .apply_governance_record_delta(&before, &after)
            .unwrap();
        assert!(after.shares_record_storage_with(&batch.read_governance_records().unwrap()));
        // Dropping an uncommitted candidate cannot publish its roots.
    }
    let mut batch = store.tx_begin_write().unwrap();
    let before = batch.read_governance_records().unwrap();
    assert!(original.shares_record_storage_with(&before));
    let after = changed(&before, 11);
    batch
        .apply_governance_record_delta(&before, &after)
        .unwrap();
    batch.commit().unwrap();
    let batch = store.tx_begin_write().unwrap();
    assert!(after.shares_record_storage_with(&batch.read_governance_records().unwrap()));
    drop(batch);
    // A separately committing store handle is outside the managed writer cache.
    store.meta_store().put_meta("external", b"write").unwrap();
    crate::write_batch::auxiliary_cache::reserve_test_votes(&store);
    let batch = store.tx_begin_write().unwrap();
    let reloaded = batch.read_governance_records().unwrap();
    assert_eq!(reloaded, after);
    assert!(!after.shares_record_storage_with(&reloaded));
    batch.commit().unwrap();
    store
        .meta_store()
        .put_meta("governance_state/v1/0", b"corrupt")
        .unwrap();
    crate::write_batch::auxiliary_cache::reserve_test_votes(&store);
    assert!(store
        .tx_begin_write()
        .unwrap()
        .read_governance_records()
        .is_err());
}

#[test]
fn stale_unrelated_candidate_cannot_replace_cached_inventory() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.initialize_governance_records(&state()).unwrap();
    let original = batch.read_governance_records().unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let mut independent = original
        .get_proposal(&TxHash::new([1; 32]))
        .unwrap()
        .clone();
    independent.hash = TxHash::new([2; 32]);
    let (key, value) = GovernanceRecord::Proposal(Box::new(independent))
        .storage_entry()
        .unwrap();
    batch.put_meta(&key, &value).unwrap();
    let complete = batch.read_governance_records().unwrap();
    assert!(complete.get_proposal(&TxHash::new([2; 32])).is_some());
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(complete.shares_record_storage_with(&batch.read_governance_records().unwrap()));
    batch
        .apply_governance_record_delta(&original, &changed(&original, 9))
        .unwrap();
    let actual = batch.read_governance_records().unwrap();
    assert!(actual.get_proposal(&TxHash::new([2; 32])).is_some());
    assert_eq!(
        actual.get_proposal(&TxHash::new([1; 32])),
        changed(&original, 9).get_proposal(&TxHash::new([1; 32]))
    );
    batch.commit().unwrap();
    assert!(actual.shares_record_storage_with(
        &store
            .tx_begin_write()
            .unwrap()
            .read_governance_records()
            .unwrap()
    ));
}

#[test]
fn pinned_governance_electorate_and_parameters_remain_one_view() {
    use burst_store::account::AccountInfo;
    use burst_types::WalletState;
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let before = state();
    let after = changed(&before, 17);
    let mut account = AccountInfo::unopened(WalletAddress::new("brst_snapshot"));
    account.state = WalletState::Verified;
    let mut batch = store.tx_begin_write().unwrap();
    batch.initialize_governance_records(&before).unwrap();
    batch.put_account_info(&account, false).unwrap();
    batch.put_brn_meta(b"protocol_params", b"before").unwrap();
    batch.put_meta("delegation_engine_state", b"before").unwrap();
    let cached = batch.read_governance_records().unwrap();
    batch.commit().unwrap();
    let old = store.tx_begin_read().unwrap();
    assert!(cached.shares_record_storage_with(&store.read_governance_snapshot(&old).unwrap().unwrap()));
    let mut batch = store.tx_begin_write().unwrap();
    batch.apply_governance_record_delta(&before, &after).unwrap();
    account.state = WalletState::Unverified;
    batch.put_account_info(&account, false).unwrap();
    batch.put_brn_meta(b"protocol_params", b"after").unwrap();
    batch.put_meta("delegation_engine_state", b"after").unwrap();
    batch.commit().unwrap();
    assert_eq!(store.read_governance_snapshot(&old).unwrap(), Some(before));
    assert_eq!(store.account_store().all_verified_addresses_txn(&old).unwrap(), vec![account.address]);
    assert_eq!(store.brn_store().get_meta_txn(&old, b"protocol_params").unwrap(), Some(b"before".to_vec()));
    assert_eq!(store.meta_store().get_meta_txn(&old, "delegation_engine_state").unwrap(), Some(b"before".to_vec()));
    drop(old); // LMDB permits one active read slot per thread.
    let mut new = store.tx_begin_read().unwrap();
    assert_eq!(store.read_governance_snapshot(&new).unwrap(), Some(after.clone()));
    assert!(store.account_store().all_verified_addresses_txn(&new).unwrap().is_empty());
    assert_eq!(store.brn_store().get_meta_txn(&new, b"protocol_params").unwrap(), Some(b"after".to_vec()));
    assert_eq!(store.meta_store().get_meta_txn(&new, "delegation_engine_state").unwrap(), Some(b"after".to_vec()));
    // Unknown acquisition identity must decode this view, not reuse a newer cache.
    new.generation = None;
    assert_eq!(store.read_governance_snapshot(&new).unwrap(), Some(after));
}

#[test]
fn pinned_governance_preserves_format_guards_and_corruption_visibility() {
    use burst_store::{MetaStore, BrnStore};
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let read = || store.read_governance_snapshot(&store.tx_begin_read().unwrap());
    assert_eq!(read().unwrap(), None);
    let engine = state();
    store.brn_store().put_meta(b"governance_engine", &bincode::serialize(&engine).unwrap()).unwrap();
    assert_eq!(read().unwrap(), Some(engine.clone()));
    store.meta_store().put_meta("governance_records_format", &[1]).unwrap();
    assert!(read().is_err());
    let mut raw = store.env().write_txn().unwrap();
    store.brn_meta_db.delete(&mut raw, b"governance_engine").unwrap();
    raw.commit().unwrap();
    assert_eq!(read().unwrap(), None);
    let mut batch = store.tx_begin_write().unwrap();
    batch.replace_checkpoint_governance_records(&engine).unwrap();
    batch.commit().unwrap();
    let old = store.tx_begin_read().unwrap();
    store.meta_store().put_meta("governance_state/v1/0", b"corrupt").unwrap();
    assert_eq!(store.read_governance_snapshot(&old).unwrap(), Some(engine));
    drop(old);
    assert!(read().is_err());
    store.meta_store().delete_meta("governance_records_format").unwrap();
    assert!(read().is_err());
    store.meta_store().put_meta("governance_records_format", &[9]).unwrap();
    assert!(read().is_err());
}

#[test]
fn cold_pinned_governance_reads_publish_only_known_nonolder_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let before = state();
    let after = changed(&before, 17);
    let mut batch = store.tx_begin_write().unwrap();
    batch.initialize_governance_records(&before).unwrap();
    batch.commit().unwrap();
    *store.governance_cache.lock().unwrap() = None;

    let mut old = store.tx_begin_read().unwrap();
    let generation = old.generation.take().unwrap();
    assert_eq!(store.read_governance_snapshot(&old).unwrap(), Some(before.clone()));
    assert!(store.governance_cache.lock().unwrap().is_none(), "unknown identity must not publish");
    old.generation = Some(generation);
    let cold = store.read_governance_snapshot(&old).unwrap().unwrap();
    assert_eq!(cold, before);
    let warm = store.read_governance_snapshot(&old).unwrap().unwrap();
    assert!(cold.shares_record_storage_with(&warm), "second read must reuse decoded records");

    let mut batch = store.tx_begin_write().unwrap();
    batch.apply_governance_record_delta(&before, &after).unwrap();
    let writer = batch.read_governance_records().unwrap();
    batch.commit().unwrap();
    let newer_generation = store.governance_cache.lock().unwrap().as_ref().unwrap().0;
    assert!(newer_generation > generation);
    assert_eq!(store.read_governance_snapshot(&old).unwrap(), Some(before));
    {
        let cache = store.governance_cache.lock().unwrap();
        let (id, cached) = cache.as_ref().unwrap();
        assert_eq!(*id, newer_generation, "old decode must not evict newer state");
        assert!(cached.shares_record_storage_with(&writer));
    }
    drop(old);
    let current = store.tx_begin_read().unwrap();
    let read = store.read_governance_snapshot(&current).unwrap().unwrap();
    assert_eq!(read, after);
    assert!(read.shares_record_storage_with(&writer));
}


fn aggregate_journal(mut journal: crate::block_undo::BlockUndo, before: &GovernanceEngine, after: &GovernanceEngine) -> crate::block_undo::BlockUndo {
    journal.changes.retain(|row| !(row.table == UndoTable::Meta &&
        (row.key.starts_with(GovernanceRecord::STORAGE_PREFIX.as_bytes()) || row.key.starts_with(PROPOSAL_REVISION_PREFIX.as_bytes()))));
    journal.changes.push(crate::block_undo::UndoChange { table: UndoTable::BrnMeta, key: b"governance_engine".to_vec(),
        before: Some(bincode::serialize(before).unwrap()), after: Some(bincode::serialize(after).unwrap()) });
    journal.changes.sort_by(|a,b| (a.table,&a.key).cmp(&(b.table,&b.key)));
    journal
}

#[test]
fn recovered_journal_allocates_revisions_atomically_and_preserves_fallbacks() {
    use crate::block_undo::{BlockUndo, UndoChange};
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 128 << 20).unwrap();
    let before = state();
    let after = changed(&before, 7);
    let empty = || BlockUndo { block: BlockHash::new([42;32]), account: WalletAddress::new("brst_recovered"), height: 1, changes: vec![] };
    let journal = aggregate_journal(empty(), &before, &after);
    let key = format!("{PROPOSAL_REVISION_PREFIX}{}", TxHash::new([1;32]));
    let mut batch = store.tx_begin_write().unwrap();
    batch.initialize_governance_records(&after).unwrap();
    batch.put_meta(&key, &13u128.to_be_bytes()).unwrap();
    batch.commit().unwrap();
    {
        let mut batch = store.tx_begin_write().unwrap();
        let prepared = prepare_governance_undo(&empty(), &before, &after).unwrap();
        assert!(prepared.changes().iter().all(|row| row.table == UndoTable::Meta
            && row.key.starts_with(GovernanceRecord::STORAGE_PREFIX.as_bytes())));
        assert_eq!(batch.get_meta(&key).unwrap(), Some(13u128.to_be_bytes().to_vec()));
        let first = batch.stage_recovered_governance_undo(empty(), prepared).unwrap();
        let row = first.changes.iter().find(|row| row.key == key.as_bytes()).unwrap();
        assert_eq!(row.before.as_deref(), Some(13u128.to_be_bytes().as_slice()));
        assert_eq!(row.after.as_deref(), Some(14u128.to_be_bytes().as_slice()));
        let second = batch.project_recovered_governance_undo(journal.clone()).unwrap();
        let row = second.changes.iter().find(|row| row.key == key.as_bytes()).unwrap();
        assert_eq!(row.before.as_deref(), Some(14u128.to_be_bytes().as_slice()));
        assert_eq!(row.after.as_deref(), Some(15u128.to_be_bytes().as_slice()));
        // No journal installation and no commit: all allocated guards abort.
    }
    let mut batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.get_meta(&key).unwrap(), Some(13u128.to_be_bytes().to_vec()));
    let global = aggregate_journal(empty(), &before, &with_pending_effect(&after));
    assert_eq!(batch.project_recovered_governance_undo(global.clone()).unwrap(), global);
    let mut full = empty();
    full.changes = (0u32..65_535).map(|n| UndoChange { table: UndoTable::Pending, key: n.to_be_bytes().to_vec(), before: None, after: Some(vec![1]) }).collect();
    let full = aggregate_journal(full, &before, &after);
    assert_eq!(batch.project_recovered_governance_undo(full.clone()).unwrap(), full);
    assert_eq!(batch.get_meta(&key).unwrap(), Some(13u128.to_be_bytes().to_vec()));
    batch.put_meta(&key, &u128::MAX.to_be_bytes()).unwrap();
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch.project_recovered_governance_undo(journal).is_err());
    assert!(batch.commit().is_err());
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 128 << 20).unwrap();
    assert_eq!(store.tx_begin_write().unwrap().get_meta(&key).unwrap(), Some(u128::MAX.to_be_bytes().to_vec()));
}

#[test]
fn lifecycle_gates_reject_unknown_undo_and_revision_namespaces_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let expected = state();
    for version in ["v1", "v2"] {
        let key = format!("governance_revision/{version}/{}", TxHash::new([1; 32]));
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(&key, &9u128.to_be_bytes()).unwrap();
        batch.commit().unwrap();
        let before = store.tx_begin_write().unwrap().checkpoint_metadata(b"").unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta("must_abort", b"staged").unwrap();
        assert!(batch.initialize_governance_records(&expected).is_err());
        assert!(batch.commit().is_err());
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.checkpoint_metadata(b"").unwrap(), before);
        assert!(!batch.uses_governance_records().unwrap());
        batch.delete_meta(&key).unwrap();
        batch.commit().unwrap();
    }
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta("governance_revision-other", b"unrelated").unwrap();
    batch.initialize_governance_records(&expected).unwrap();
    batch.commit().unwrap();
    for version in ["v1", "v2"] {
        let key = format!("block_undo/{version}/retained");
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(&key, b"unrecognized journal evidence").unwrap();
        batch.commit().unwrap();
        let before = store.tx_begin_write().unwrap().checkpoint_metadata(b"").unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta("must_abort", b"staged").unwrap();
        assert!(batch.replace_checkpoint_governance_records(&changed(&expected, 7)).is_err());
        assert!(batch.commit().is_err());
        let mut batch = store.tx_begin_write().unwrap();
        assert_eq!(batch.checkpoint_metadata(b"").unwrap(), before);
        assert_eq!(batch.read_governance_records().unwrap(), expected);
        batch.delete_meta(&key).unwrap();
        batch.commit().unwrap();
    }
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta("block_undo-other", b"unrelated").unwrap();
    let replacement = changed(&expected, 7);
    batch.replace_checkpoint_governance_records(&replacement).unwrap();
    batch.commit().unwrap();
    drop(store);
    let store = crate::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert_eq!(batch.read_governance_records().unwrap(), replacement);
    for key in ["block_undo-other", "governance_revision-other"] {
        assert_eq!(batch.get_meta(key).unwrap(), Some(b"unrelated".to_vec()));
    }
}

#[test]
fn captured_governance_reader_preserves_rows_across_live_format_corruption() {
    let directory = tempfile::tempdir().unwrap();
    let store = crate::LmdbStore::open(directory.path(), 32, 16 << 20).unwrap();
    let initial = state();
    let mut writer = store.tx_begin_write().unwrap();
    writer.initialize_governance_records(&initial).unwrap();
    let before = bincode::serialize(&writer.read_governance_records().unwrap()).unwrap();
    writer.commit().unwrap();
    let reader = store.tx_begin_read().unwrap();
    let view = store.read_view(&reader).unwrap();
    assert_eq!(bincode::serialize(&view.read_governance_records().unwrap()).unwrap(), before);
    let mut writer = store.tx_begin_write().unwrap();
    writer.put_meta(MODE_KEY, &[99]).unwrap();
    writer.commit().unwrap();
    assert_eq!(bincode::serialize(&view.read_governance_records().unwrap()).unwrap(), before);
    let writer = store.tx_begin_write().unwrap();
    assert!(writer.read_governance_records().is_err());
    assert!(writer.read_view().read_governance_records().is_err());
}
