use super::*;

fn wallet(id: &str) -> WalletAddress {
    WalletAddress::new(format!("brst_{id}"))
}

fn rewind(
    before: &DelegationEngine,
    after: &DelegationEngine,
    current: &DelegationEngine,
) -> Result<DelegationEngine, String> {
    let bytes = DelegationSnapshot::rewind(
        &before.save_state(),
        &after.save_state(),
        &current.save_state(),
    )?;
    DelegationEngine::load_state(&bytes).map_err(|error| error.to_string())
}

#[test]
fn independent_global_and_scoped_changes_survive_undo_and_reverse_index_rebuild() {
    let before = DelegationEngine::default();
    let mut after = before.clone();
    after.delegate(&wallet("a"), &wallet("b")).unwrap();
    after
        .delegate_scoped(
            &wallet("a"),
            &wallet("c"),
            DelegationScope::Category("economic".into()),
        )
        .unwrap();
    let mut current = after.clone();
    current.delegate(&wallet("d"), &wallet("b")).unwrap();
    current
        .delegate_scoped(
            &wallet("d"),
            &wallet("c"),
            DelegationScope::Category("economic".into()),
        )
        .unwrap();
    let restored = rewind(&before, &after, &current).unwrap();
    assert_eq!(restored.resolve(&wallet("a")), Some(wallet("a")));
    assert_eq!(restored.resolve(&wallet("d")), Some(wallet("b")));
    assert_eq!(restored.voting_power(&wallet("b")), 2);
    assert_eq!(
        restored.resolve_for_context(&wallet("d"), None, Some("economic")),
        Some(wallet("c"))
    );
    let empty = rewind(&after, &current, &restored).unwrap();
    assert_eq!(empty.resolve(&wallet("a")), Some(wallet("a")));
    assert_eq!(empty.resolve(&wallet("d")), Some(wallet("d")));
    assert!(empty.reverse_delegations.is_empty());
}

#[test]
fn later_owner_changes_conflict_across_scopes_but_cycles_keep_ordinary_semantics() {
    let mut before = DelegationEngine::default();
    before.delegate(&wallet("a"), &wallet("b")).unwrap();
    let mut after = before.clone();
    after.undelegate(&wallet("a"));
    let mut current = after.clone();
    current
        .delegate_scoped(
            &wallet("a"),
            &wallet("c"),
            DelegationScope::Category("economic".into()),
        )
        .unwrap();
    assert!(rewind(&before, &after, &current).is_err());
    let mut current = after.clone();
    current.delegate(&wallet("b"), &wallet("a")).unwrap();
    let restored = rewind(&before, &after, &current).unwrap();
    assert_eq!(restored.resolve(&wallet("a")), None);
    assert_eq!(restored.resolve(&wallet("b")), None);
}

#[test]
fn changed_depth_requires_complete_state_and_malformed_snapshots_fail() {
    let before = DelegationEngine::new(10);
    let after = DelegationEngine::new(2);
    assert_eq!(rewind(&before, &after, &after).unwrap().max_depth(), 10);
    let mut current = after.clone();
    current.delegate(&wallet("a"), &wallet("b")).unwrap();
    assert!(rewind(&before, &after, &current).is_err());
    let mut trailing = after.save_state();
    trailing.push(0);
    assert!(
        DelegationSnapshot::rewind(&before.save_state(), &trailing, &after.save_state()).is_err()
    );
    let invalid = DelegationSnapshot {
        delegations: HashMap::from([(wallet("a"), wallet("a"))]),
        scoped_delegations: HashMap::new(),
        max_depth: 10,
    };
    assert!(DelegationSnapshot::rewind(
        &bincode::serialize(&invalid).unwrap(),
        &after.save_state(),
        &after.save_state()
    )
    .is_err());
}
