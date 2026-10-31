use super::*;
use burst_types::{ProtocolParams, Timestamp};
use burst_verification::VerificationOrchestrator;

fn endorse(engine: &mut VerificationOrchestrator, target: &WalletAddress, voter: &str) {
    let params = ProtocolParams {
        endorsement_threshold: 10_000,
        ..ProtocolParams::default()
    };
    engine
        .process_endorsement_at(
            target,
            &WalletAddress::new(voter),
            params.endorsement_burn_amount,
            &params,
            Timestamp::new(1),
        )
        .unwrap();
    engine.drain_events();
}
fn physical(engine: &VerificationOrchestrator) -> BTreeMap<String, Vec<u8>> {
    let mut rows = BTreeMap::new();
    normalized::visit_state_rows(engine, |key, bytes| {
        rows.insert(key, bytes);
        Ok(())
    })
    .unwrap();
    rows
}
fn journal(changes: Vec<UndoChange>) -> BlockUndo {
    let mut changes = changes;
    changes.sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
    BlockUndo {
        block: BlockHash::new([77; 32]),
        account: WalletAddress::new("brst_journal"),
        height: 3,
        changes,
    }
}
fn fixture() -> (
    VerificationOrchestrator,
    VerificationOrchestrator,
    BlockUndo,
) {
    let a = WalletAddress::new("brst_a");
    let b = WalletAddress::new("brst_b");
    let c = WalletAddress::new("brst_c");
    let mut before = VerificationOrchestrator::new();
    endorse(&mut before, &a, "brst_v1");
    endorse(&mut before, &b, "brst_v2");
    let mut snapshot = before.snapshot();
    snapshot.states.remove(&b);
    let mut after = VerificationOrchestrator::restore(snapshot);
    endorse(&mut after, &a, "brst_v3");
    endorse(&mut after, &c, "brst_v4");
    let mut changes = Vec::new();
    for target in [a, b, c] {
        changes.push(UndoChange {
            table: UndoTable::Meta,
            key: VerificationRecord::target_storage_key(&target)
                .unwrap()
                .into_bytes(),
            before: before
                .target_record(&target)
                .map(|r| r.storage_entry().unwrap().1),
            after: after
                .target_record(&target)
                .map(|r| r.storage_entry().unwrap().1),
        });
    }
    changes.push(UndoChange {
        table: UndoTable::Pending,
        key: b"unrelated".to_vec(),
        before: Some(vec![1]),
        after: Some(vec![2]),
    });
    (before, after, journal(changes))
}
#[test]
fn conversion_preserves_exact_forward_inverse_and_unrelated_images() {
    let (before, after, old) = fixture();
    let converted = convert(old.clone()).unwrap();
    assert_eq!(
        (converted.block, &converted.account, converted.height),
        (old.block, &old.account, old.height)
    );
    assert_eq!(
        converted
            .changes
            .iter()
            .find(|c| c.table == UndoTable::Pending),
        old.changes.iter().find(|c| c.table == UndoTable::Pending)
    );
    assert_eq!(convert(converted.clone()).unwrap(), converted);
    let mut rows = physical(&before);
    for change in converted
        .changes
        .iter()
        .filter(|c| c.table == UndoTable::Meta)
    {
        let key = std::str::from_utf8(&change.key).unwrap();
        assert_eq!(rows.get(key), change.before.as_ref());
        match &change.after {
            Some(bytes) => {
                rows.insert(key.to_owned(), bytes.clone());
            }
            None => {
                rows.remove(key);
            }
        }
    }
    assert_eq!(rows, physical(&after));
    for change in converted
        .changes
        .iter()
        .rev()
        .filter(|c| c.table == UndoTable::Meta)
    {
        let key = std::str::from_utf8(&change.key).unwrap();
        assert_eq!(rows.get(key), change.after.as_ref());
        match &change.before {
            Some(bytes) => {
                rows.insert(key.to_owned(), bytes.clone());
            }
            None => {
                rows.remove(key);
            }
        }
    }
    assert_eq!(rows, physical(&before));
}
#[test]
fn conversion_rejects_malformed_and_mixed_images_without_changing_input() {
    let (_, _, original) = fixture();
    for case in 0..4 {
        let mut bad = original.clone();
        let index = bad
            .changes
            .iter()
            .position(|c| c.table == UndoTable::Meta && c.after.is_some())
            .unwrap();
        match case {
            0 => bad.changes[index].after.as_mut().unwrap().push(0),
            1 => bad.changes[index].key.push(b'0'),
            2 => bad.changes.push(bad.changes[index].clone()),
            _ => bad.changes.push(UndoChange {
                table: UndoTable::Meta,
                key: b"verification_orchestrator".to_vec(),
                before: None,
                after: Some(vec![1]),
            }),
        }
        bad.changes
            .sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
        let saved = bad.clone();
        assert!(convert(bad.clone()).is_err());
        if let Ok(canonical) = BlockUndo::decode(&bad.block, &bincode::serialize(&bad).unwrap()) {
            assert!(validate_retained(&canonical).is_err());
        }
        assert_eq!(bad, saved);
    }
    let aggregate = journal(vec![UndoChange {
        table: UndoTable::Meta,
        key: b"verification_orchestrator".to_vec(),
        before: Some(vec![1]),
        after: Some(vec![2]),
    }]);
    assert_eq!(convert(aggregate.clone()).unwrap(), aggregate);
}
#[test]
fn conversion_preserves_existing_change_count_limit() {
    let (before, _, _) = fixture();
    let mut snapshot = before.snapshot();
    let mut template = snapshot
        .states
        .remove(&WalletAddress::new("brst_a"))
        .unwrap();
    template.endorsements = (0..400)
        .map(|n| burst_verification::state::Endorsement {
            endorser: WalletAddress::new(format!("brst_v{n}")),
            burn_amount: 1,
            timestamp: Timestamp::new(1),
        })
        .collect();
    template.endorser_set = template
        .endorsements
        .iter()
        .map(|e| e.endorser.clone())
        .collect();
    snapshot.states.clear();
    for n in 0..100 {
        let mut state = template.clone();
        state.target = WalletAddress::new(format!("brst_target{n}"));
        snapshot.states.insert(state.target.clone(), state);
    }
    let engine = VerificationOrchestrator::restore(snapshot);
    let changes = engine
        .states()
        .map(|s| {
            let (key, bytes) = engine
                .target_record(&s.target)
                .unwrap()
                .storage_entry()
                .unwrap();
            UndoChange {
                table: UndoTable::Meta,
                key: key.into_bytes(),
                before: None,
                after: Some(bytes),
            }
        })
        .collect();
    let original = journal(changes);
    validate_retained(&original).unwrap();
    let error = convert(original).unwrap_err().to_string();
    assert!(error.contains("existing limits"), "{error}");
}

#[test]
fn projected_transition_preserves_physical_forward_and_inverse() {
    let (before, after, old) = fixture();
    let base = journal(
        old.changes
            .into_iter()
            .filter(|row| row.table == UndoTable::Pending)
            .collect(),
    );
    let changes = project_transition(&base, &before, &after).unwrap();
    assert!(!changes.is_empty());
    assert!(changes
        .iter()
        .all(|row| row.key.starts_with(b"verification_state/")));
    let mut rows = physical(&before);
    for row in &changes {
        let key = std::str::from_utf8(&row.key).unwrap();
        assert_eq!(rows.get(key), row.before.as_ref());
        match &row.after {
            Some(value) => {
                rows.insert(key.to_owned(), value.clone());
            }
            None => {
                rows.remove(key);
            }
        }
    }
    assert_eq!(rows, physical(&after));
    for row in changes.iter().rev() {
        let key = std::str::from_utf8(&row.key).unwrap();
        assert_eq!(rows.get(key), row.after.as_ref());
        match &row.before {
            Some(value) => {
                rows.insert(key.to_owned(), value.clone());
            }
            None => {
                rows.remove(key);
            }
        }
    }
    assert_eq!(rows, physical(&before));
    assert!(project_transition(&base, &before, &before)
        .unwrap()
        .is_empty());
    assert!(project_transition(&journal(changes), &before, &after).is_err());
}

#[test]
fn projected_transition_respects_whole_journal_budget_and_rules_ownership() {
    let (before, mut after, _) = fixture();
    let mut base = journal(
        (0..MAX_CHANGES - 1)
            .map(|n| UndoChange {
                table: UndoTable::Pending,
                key: n.to_be_bytes().to_vec(),
                before: None,
                after: Some(vec![1]),
            })
            .collect(),
    );
    let changes = project_transition(&base, &before, &after).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].key, b"verification_orchestrator");
    assert_eq!(
        changes[0].before.as_ref().unwrap(),
        &before.snapshot().encode().unwrap()
    );
    assert_eq!(
        changes[0].after.as_ref().unwrap(),
        &after.snapshot().encode().unwrap()
    );
    base.changes.push(UndoChange {
        table: UndoTable::Pending,
        key: MAX_CHANGES.to_be_bytes().to_vec(),
        before: None,
        after: Some(vec![1]),
    });
    assert!(project_transition(&base, &before, &after).is_err());
    after.apply_governed_rules(&ProtocolParams {
        neither_penalty_threshold_bps: 1234,
        ..ProtocolParams::default()
    });
    let changes = project_transition(&journal(vec![]), &before, &after).unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].key, b"verification_orchestrator");
}
