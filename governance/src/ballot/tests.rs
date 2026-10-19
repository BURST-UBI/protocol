use super::*;
use burst_transactions::governance::GovernanceVote;

fn wallet(seed: u8) -> WalletAddress {
    burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[seed; 32]).public)
}

fn fixture() -> (
    GovernanceEngine,
    DelegationEngine,
    ProtocolParams,
    TxHash,
    Vec<WalletAddress>,
) {
    let params = ProtocolParams {
        governance_exploration_duration_secs: 10,
        governance_propagation_buffer_secs: 2,
        governance_quorum_bps: 5000,
        governance_supermajority_bps: 8000,
        ..ProtocolParams::default()
    };
    let id = TxHash::new([8; 32]);
    let proposal = Proposal {
        hash: id,
        proposer: wallet(1),
        phase: GovernancePhase::Proposal,
        content: ProposalContent::ParameterChange {
            param: crate::GovernableParam::BrnRate,
            new_value: 5,
        },
        endorsement_count: 1,
        total_eligible_voters: 99,
        exploration_started_at: Some(Timestamp::new(100)),
        exploration_votes_yea: 0,
        exploration_votes_nay: 0,
        exploration_votes_abstain: 0,
        cooldown_started_at: None,
        promotion_started_at: None,
        promotion_votes_yea: 0,
        promotion_votes_nay: 0,
        promotion_votes_abstain: 0,
        round: 0,
        created_at: Timestamp::EPOCH,
        activation_at: None,
    };
    let mut engine = GovernanceEngine::new();
    engine
        .submit_proposal(proposal, u128::MAX, true, &params)
        .unwrap();
    engine.get_proposal_mut(&id).unwrap().phase = GovernancePhase::Exploration;
    engine
        .cast_exploration_vote(
            &id,
            &wallet(1),
            GovernanceVote::Yea,
            Timestamp::new(101),
            &params,
        )
        .unwrap();
    engine
        .cast_exploration_vote(
            &id,
            &wallet(3),
            GovernanceVote::Abstain,
            Timestamp::new(101),
            &params,
        )
        .unwrap();
    // A previously cast vote supplies no eligibility in the final cut.
    engine
        .cast_exploration_vote(
            &id,
            &wallet(5),
            GovernanceVote::Nay,
            Timestamp::new(101),
            &params,
        )
        .unwrap();
    let mut delegation = DelegationEngine::default();
    delegation.delegate(&wallet(2), &wallet(1)).unwrap();
    (
        engine,
        delegation,
        params,
        id,
        (1..=4).map(wallet).collect(),
    )
}

fn prepare(
    engine: &GovernanceEngine,
    delegation: &DelegationEngine,
    params: &ProtocolParams,
    id: TxHash,
    verified: &[WalletAddress],
) -> PreparedBallot {
    engine
        .prepare_ballot_cut(
            &id,
            BlockHash::new([9; 32]),
            Timestamp::new(112),
            params,
            delegation,
            verified,
        )
        .unwrap()
}

#[test]
fn closed_tally_counts_each_eligible_human_once_and_does_not_mutate_the_source() {
    let (mut engine, mut delegation, params, id, verified) = fixture();
    let result = prepare(&engine, &delegation, &params, id, &verified);
    let next = result.next_proposal();
    assert_eq!(next.phase, GovernancePhase::Cooldown);
    assert_eq!(
        (
            next.exploration_votes_yea,
            next.exploration_votes_nay,
            next.exploration_votes_abstain,
            next.total_eligible_voters
        ),
        (2, 0, 1, 4)
    );
    let original = engine.get_proposal(&id).unwrap();
    assert_eq!(original.phase, GovernancePhase::Exploration);
    assert_eq!(original.total_eligible_voters, 99);
    let commitment = result.commitment().unwrap();
    delegation.undelegate(&wallet(2));
    let changed = prepare(&engine, &delegation, &params, id, &verified);
    assert_ne!(changed.commitment().unwrap(), commitment);
    assert_eq!(result.commitment().unwrap(), commitment);
    let mut closed = next.clone();
    engine
        .try_advance(
            &mut closed,
            Timestamp::new(
                112 + params.governance_cooldown_duration_secs
                    + params.governance_propagation_buffer_secs,
            ),
            &params,
            Some((&delegation, &[])),
        )
        .unwrap();
    assert_eq!(closed.exploration_votes_yea, 2);
    assert_eq!(closed.total_eligible_voters, 4);
    *engine.get_proposal_mut(&id).unwrap() = closed;
    assert!(engine
        .prepare_ballot_cut(
            &id,
            BlockHash::new([9; 32]),
            Timestamp::new(112),
            &params,
            &delegation,
            &verified
        )
        .is_err());
}

#[test]
fn approved_schedule_is_bound_to_the_ballot_and_not_later_parameters() {
    let (engine, delegation, params, id, verified) = fixture();
    let prepared = prepare(&engine, &delegation, &params, id, &verified);
    let mut proposal = prepared.next_proposal().clone();
    let at = Timestamp::new(
        112 + params.governance_cooldown_duration_secs + params.governance_propagation_buffer_secs,
    );
    assert_eq!(proposal.activation_at, Some(at));
    let mut changed = params.clone();
    changed.governance_cooldown_duration_secs = 0;
    changed.governance_propagation_buffer_secs = 0;
    assert!(engine
        .try_advance(
            &mut proposal,
            Timestamp::new(at.as_secs() - 1),
            &changed,
            None
        )
        .is_err());
    assert_eq!(
        engine
            .try_advance(&mut proposal, at, &changed, None)
            .unwrap(),
        GovernancePhase::Activation
    );
    assert_eq!(proposal.activation_at, Some(at));
    changed.governance_propagation_buffer_secs = params.governance_propagation_buffer_secs;
    let shorter = prepare(&engine, &delegation, &changed, id, &verified);
    assert_ne!(
        prepared.commitment().unwrap(),
        shorter.commitment().unwrap()
    );
    let mut overflow = params;
    overflow.governance_cooldown_duration_secs = u64::MAX;
    assert!(engine
        .prepare_ballot_cut(
            &id,
            BlockHash::new([9; 32]),
            Timestamp::new(112),
            &overflow,
            &delegation,
            &verified
        )
        .is_err());
}

#[test]
fn direct_votes_override_delegation_and_rejected_tallies_are_retained() {
    let (mut engine, delegation, params, id, verified) = fixture();
    engine
        .cast_exploration_vote(
            &id,
            &wallet(2),
            GovernanceVote::Nay,
            Timestamp::new(102),
            &params,
        )
        .unwrap();
    let result = prepare(&engine, &delegation, &params, id, &verified);
    let next = result.next_proposal();
    assert_eq!(next.phase, GovernancePhase::Rejected);
    assert_eq!(
        (
            next.exploration_votes_yea,
            next.exploration_votes_nay,
            next.exploration_votes_abstain,
            next.total_eligible_voters
        ),
        (1, 1, 1, 4)
    );
    let empty = prepare(&engine, &delegation, &params, id, &[]);
    assert_eq!(empty.next_proposal().phase, GovernancePhase::Rejected);
    assert_eq!(empty.next_proposal().total_eligible_voters, 0);
}

#[test]
fn commitment_binds_the_history_rules_and_canonical_electorate() {
    let (engine, delegation, params, id, mut verified) = fixture();
    let root = prepare(&engine, &delegation, &params, id, &verified)
        .commitment()
        .unwrap();
    verified.reverse();
    assert_eq!(
        prepare(&engine, &delegation, &params, id, &verified)
            .commitment()
            .unwrap(),
        root
    );
    let changed_history = engine
        .prepare_ballot_cut(
            &id,
            BlockHash::new([10; 32]),
            Timestamp::new(112),
            &params,
            &delegation,
            &verified,
        )
        .unwrap();
    assert_ne!(changed_history.commitment().unwrap(), root);
    let mut changed_params = params.clone();
    changed_params.brn_rate += 1;
    assert_ne!(
        prepare(&engine, &delegation, &changed_params, id, &verified)
            .commitment()
            .unwrap(),
        root
    );
    verified[0] = wallet(6);
    let changed_electorate = prepare(&engine, &delegation, &params, id, &verified);
    assert_eq!(changed_electorate.next_proposal().exploration_votes_yea, 2);
    assert_ne!(changed_electorate.commitment().unwrap(), root);
}

#[test]
fn ambiguous_or_noncanonical_cuts_are_rejected_before_preparing_a_result() {
    let (mut engine, delegation, params, id, verified) = fixture();
    for cutoff in [111, 113] {
        assert!(engine
            .prepare_ballot_cut(
                &id,
                BlockHash::new([9; 32]),
                Timestamp::new(cutoff),
                &params,
                &delegation,
                &verified
            )
            .is_err());
    }
    assert!(engine
        .prepare_ballot_cut(
            &id,
            BlockHash::ZERO,
            Timestamp::new(112),
            &params,
            &delegation,
            &verified
        )
        .is_err());
    for invalid in [
        vec![wallet(1), wallet(1)],
        vec![WalletAddress::new("brst_invalid")],
    ] {
        assert!(engine
            .prepare_ballot_cut(
                &id,
                BlockHash::new([9; 32]),
                Timestamp::new(112),
                &params,
                &delegation,
                &invalid
            )
            .is_err());
    }
    engine.get_proposal_mut(&id).unwrap().exploration_started_at = Some(Timestamp::new(u64::MAX));
    assert!(counting_time(engine.get_proposal(&id).unwrap(), &params).is_err());
}

#[test]
fn local_projection_cannot_poison_a_ballot_while_history_is_arriving() {
    let (mut engine, delegation, params, id, verified) = fixture();
    let before = bincode::serialize(&engine).unwrap();
    let incomplete = engine.project_at(Timestamp::new(112), &params, &delegation, &[]);
    assert_eq!(
        incomplete.get_proposal(&id).unwrap().phase,
        GovernancePhase::Rejected
    );
    assert_eq!(bincode::serialize(&engine).unwrap(), before);
    engine
        .cast_exploration_vote(
            &id,
            &wallet(4),
            GovernanceVote::Yea,
            Timestamp::new(103),
            &params,
        )
        .unwrap();
    let before = bincode::serialize(&engine).unwrap();
    let due_at = Timestamp::new(
        112 + params.governance_cooldown_duration_secs + params.governance_propagation_buffer_secs,
    );
    let mut complete = engine.project_at(due_at, &params, &delegation, &verified);
    assert_eq!(complete.due_activation_hashes(due_at), vec![id]);
    assert_eq!(complete.get_proposal(&id).unwrap().exploration_votes_yea, 3);
    assert!(complete.drain_pending_changes().is_empty());
    assert!(complete.drain_activated_amendments().is_empty());
    assert!(complete.drain_pending_evictions().is_empty());
    assert_eq!(bincode::serialize(&engine).unwrap(), before);
}

#[test]
fn projections_do_not_recount_an_activated_result() {
    let (mut engine, delegation, params, id, verified) = fixture();
    let mut closed = prepare(&engine, &delegation, &params, id, &verified)
        .next_proposal()
        .clone();
    closed.phase = GovernancePhase::Activated;
    *engine.get_proposal_mut(&id).unwrap() = closed;
    let projected = engine.project_at(
        Timestamp::new(u64::MAX),
        &params,
        &DelegationEngine::default(),
        &[],
    );
    let result = projected.get_proposal(&id).unwrap();
    assert_eq!(result.phase, GovernancePhase::Activated);
    assert_eq!(result.exploration_votes_yea, 2);
    assert_eq!(result.total_eligible_voters, 4);
    assert!(projected
        .due_activation_hashes(Timestamp::new(u64::MAX))
        .is_empty());
}
