use super::*;
use burst_types::Timestamp;

fn bytes(state: &ConstiEngine) -> Vec<u8> {
    bincode::serialize(state).unwrap()
}
fn rewind(
    before: &ConstiEngine,
    after: &ConstiEngine,
    current: &ConstiEngine,
) -> Result<ConstiEngine, String> {
    bincode::deserialize(&ConstiEngine::rewind_snapshot(
        &bytes(before),
        &bytes(after),
        &bytes(current),
    )?)
    .map_err(|error| error.to_string())
}
fn add(engine: &mut ConstiEngine, id: u8) -> TxHash {
    let hash = TxHash::new([id; 32]);
    engine
        .submit_amendment(
            Amendment {
                hash,
                proposer: WalletAddress::new("brst_author"),
                title: format!("Amendment {id}"),
                text: "Proposed article".into(),
                phase: GovernancePhase::Exploration,
                votes_yea: 0,
                votes_nay: 0,
                votes_abstain: 0,
                created_at: Timestamp::new(1),
                operations: Vec::new(),
            },
            &engine.document.clone(),
        )
        .unwrap();
    hash
}

#[test]
fn independent_amendment_votes_survive_repeated_undo_without_resurrection() {
    let before = ConstiEngine::bootstrap();
    let mut after = before.clone();
    let first = add(&mut after, 1);
    let mut current = after.clone();
    let second = add(&mut current, 2);
    current
        .vote_amendment(&second, &WalletAddress::new("brst_voter"), ConstiVote::Yea)
        .unwrap();
    let restored = rewind(&before, &after, &current).unwrap();
    assert!(!restored.amendments.contains_key(&first));
    assert_eq!(restored.amendments[&second].votes_yea, 1);
    assert_eq!(restored.document, before.document);
    assert_eq!(rewind(&after, &current, &restored).unwrap(), before);
}

#[test]
fn votes_conflicts_and_document_dependencies_cannot_be_overwritten() {
    let before = ConstiEngine::bootstrap();
    let mut after = before.clone();
    let id = add(&mut after, 1);
    let mut current = after.clone();
    current
        .vote_amendment(&id, &WalletAddress::new("brst_voter"), ConstiVote::Nay)
        .unwrap();
    assert!(rewind(&before, &after, &current).is_err());
    let mut current = after.clone();
    current
        .conflicts
        .insert(id, AmendmentConflict::ArticleNotFound(99));
    assert!(rewind(&before, &after, &current).is_err());
    let mut changed_document = before.clone();
    changed_document.document.version += 1;
    assert_eq!(
        rewind(&before, &changed_document, &changed_document).unwrap(),
        before
    );
    let mut current = changed_document.clone();
    add(&mut current, 2);
    assert!(rewind(&before, &changed_document, &current).is_err());
    let mut trailing = bytes(&after);
    trailing.push(0);
    assert!(ConstiEngine::rewind_snapshot(&bytes(&before), &trailing, &bytes(&after)).is_err());
    assert!(ConstiEngine::rewind_snapshot(&[], &bytes(&after), &bytes(&after)).is_err());
}
