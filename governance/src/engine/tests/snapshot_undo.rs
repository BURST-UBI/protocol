use super::*;

fn bytes(engine: &GovernanceEngine) -> Vec<u8> {
    bincode::serialize(engine).unwrap()
}

fn rewind(
    before: &GovernanceEngine,
    after: &GovernanceEngine,
    current: &GovernanceEngine,
) -> Result<GovernanceEngine, String> {
    let bytes = GovernanceEngine::rewind_snapshot(&bytes(before), &bytes(after), &bytes(current))?;
    bincode::deserialize(&bytes).map_err(|error| error.to_string())
}

fn add(engine: &mut GovernanceEngine, id: u8) -> TxHash {
    let mut proposal = make_proposal(1, 0);
    proposal.hash = TxHash::new([id; 32]);
    let hash = proposal.hash;
    engine
        .submit_proposal(proposal, 0, true, &default_params())
        .unwrap();
    hash
}

#[test]
fn independent_proposal_progress_survives_repeated_undo_without_resurrection() {
    let before = GovernanceEngine::new();
    let mut after = before.clone();
    let first = add(&mut after, 1);
    let mut current = after.clone();
    let second = add(&mut current, 2);
    current.endorse_proposal(&second, 10).unwrap();
    let retained = current.clone();
    let restored = rewind(&before, &after, &current).unwrap();
    assert!(!restored.proposals.contains_key(&first));
    assert_eq!(restored.proposals[&second].endorsement_count, 1);
    let empty = rewind(&after, &retained, &restored).unwrap();
    assert_eq!(empty, before);
    // Serialization order does not create a conflict for equivalent maps.
    let decoded: GovernanceEngine = bincode::deserialize(&bytes(&retained)).unwrap();
    assert_eq!(rewind(&before, &after, &decoded).unwrap(), restored);
}

#[test]
fn proposal_votes_timing_and_secondary_ballots_share_one_conflict_scope() {
    let before = GovernanceEngine::new();
    let mut after = before.clone();
    let id = add(&mut after, 1);
    for field in 0..6 {
        let mut current = after.clone();
        match field {
            0 => {
                current.proposals.get_mut(&id).unwrap().phase = GovernancePhase::Withdrawn;
            }
            1 => {
                current
                    .exploration_votes
                    .entry(id)
                    .or_default()
                    .insert(dummy_wallet(), GovernanceVote::Yea);
            }
            2 => {
                current
                    .promotion_votes
                    .entry(id)
                    .or_default()
                    .insert(dummy_wallet(), GovernanceVote::Nay);
            }
            3 => {
                current.endorsement_counts.insert(id, (1, 10));
            }
            4 => {
                current.timings.get_mut(&id).unwrap().proposal_secs += 1;
            }
            _ => {
                current.key_ballots.insert(
                    (id, dummy_wallet(), burst_types::BlockHash::new([3; 32])),
                    GovernanceVote::Yea,
                );
            }
        }
        assert!(rewind(&before, &after, &current).is_err(), "field {field}");
    }
}

#[test]
fn global_rule_effects_require_complete_after_state_and_bad_encodings_are_rejected() {
    let before = GovernanceEngine::new();
    let mut after = before.clone();
    Arc::make_mut(&mut after.pending_changes).push((crate::GovernableParam::BrnRate, 2));
    assert_eq!(rewind(&before, &after, &after).unwrap(), before);
    let mut current = after.clone();
    add(&mut current, 1);
    assert!(rewind(&before, &after, &current).is_err());
    let mut trailing = bytes(&after);
    trailing.push(0);
    assert!(GovernanceEngine::rewind_snapshot(&bytes(&before), &trailing, &bytes(&after)).is_err());
    assert!(GovernanceEngine::rewind_snapshot(&[], &bytes(&after), &bytes(&after)).is_err());
}
