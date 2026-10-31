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

#[test]
fn selected_projection_matches_phase_rules_and_only_requires_due_electorates() {
    let (engine, delegation, params, id, verified) = fixture();
    let before = bincode::serialize(&engine).unwrap();
    let original = engine.get_proposal(&id).unwrap();
    for (seconds, needs_electorate) in [
        (99, false),
        (109, false),
        (110, false),
        (111, false),
        (112, true),
        (u64::MAX, true),
    ] {
        let now = Timestamp::new(seconds);
        assert_eq!(
            engine.projection_requires_electorate(original, now, &params),
            needs_electorate
        );
        let selected = engine.project_proposal_at(original, now, &params, &delegation, &verified);
        // Compare with the same phase-transition primitive, not an independent tally.
        let mut reference = original.clone();
        for _ in 0..3 {
            if !matches!(
                reference.phase,
                GovernancePhase::Proposal
                    | GovernancePhase::Exploration
                    | GovernancePhase::Cooldown
            ) {
                break;
            }
            if engine
                .try_advance(&mut reference, now, &params, Some((&delegation, &verified)))
                .is_err()
            {
                break;
            }
        }
        assert_eq!(selected, reference);
        if !needs_electorate {
            assert_eq!(
                selected,
                engine.project_proposal_at(
                    original,
                    now,
                    &params,
                    &DelegationEngine::default(),
                    &[]
                )
            );
        }
    }
    for phase in [
        GovernancePhase::Cooldown,
        GovernancePhase::Activation,
        GovernancePhase::Activated,
        GovernancePhase::Rejected,
    ] {
        let mut proposal = original.clone();
        proposal.phase = phase;
        assert!(!engine.projection_requires_electorate(
            &proposal,
            Timestamp::new(u64::MAX),
            &params
        ));
    }
    let mut support = original.clone();
    support.phase = GovernancePhase::Proposal;
    support.endorsement_count = params.governance_proposal_endorsements;
    let end = engine
        .timing(&support, &params)
        .unwrap()
        .proposal_end(support.created_at)
        .unwrap();
    let counting = engine
        .timing(&support, &params)
        .unwrap()
        .counting_at(end)
        .unwrap();
    assert!(!engine.projection_requires_electorate(&support, end, &params));
    assert!(engine.projection_requires_electorate(&support, counting, &params));
    support.endorsement_count = 0;
    assert!(!engine.projection_requires_electorate(&support, counting, &params));

    let mut invalid = original.clone();
    invalid.exploration_started_at = Some(Timestamp::new(u64::MAX));
    assert!(!engine.projection_requires_electorate(&invalid, Timestamp::new(u64::MAX), &params));
    assert_eq!(bincode::serialize(&engine).unwrap(), before);
}

#[test]
fn projection_reuse_windows_match_fresh_projection_at_phase_boundaries() {
    let (engine, delegation, mut params, id, verified) = fixture();
    params.governance_cooldown_duration_secs = 5;
    let original = engine.get_proposal(&id).unwrap();
    let mut variants = vec![original.clone()];
    let mut support = original.clone();
    support.phase = GovernancePhase::Proposal;
    support.endorsement_count = params.governance_proposal_endorsements;
    variants.push(support.clone());
    support.endorsement_count = 0;
    variants.push(support);
    let mut cooldown = original.clone();
    cooldown.phase = GovernancePhase::Cooldown;
    cooldown.cooldown_started_at = Some(Timestamp::new(112));
    variants.push(cooldown.clone());
    cooldown.activation_at = Some(Timestamp::new(130));
    variants.push(cooldown);
    for phase in [GovernancePhase::Rejected, GovernancePhase::Withdrawn,
        GovernancePhase::Activation, GovernancePhase::Activated, GovernancePhase::Promotion,
        GovernancePhase::Conflicted] {
        let mut terminal = original.clone();
        terminal.phase = phase;
        variants.push(terminal);
    }
    let proposal_end = engine.timing(original, &params).unwrap()
        .proposal_end(original.created_at).unwrap().as_secs();
    let times = [0, 99, 100, 109, 110, 111, 112, 113, 116, 117, 118, 129, 130,
        proposal_end.saturating_sub(1), proposal_end, proposal_end.saturating_add(12), u64::MAX];
    for input in variants {
        for electorate in [&verified[..], &[][..]] {
            for seconds in times {
                let now = Timestamp::new(seconds);
                let projected = engine.project_proposal_at(&input, now, &params, &delegation, electorate);
                let until = engine.projection_valid_until(&projected, now, &params);
                for later in times.into_iter().filter(|later| *later >= seconds) {
                    if until.is_none_or(|end| later < end.as_secs()) {
                        assert_eq!(projected, engine.project_proposal_at(&input,
                            Timestamp::new(later), &params, &delegation, electorate),
                            "invalid reuse window at {seconds} -> {later} for {:?}", input.phase);
                    }
                }
            }
        }
    }
    assert_eq!(engine.projection_valid_until(original, Timestamp::new(110), &params), Some(Timestamp::new(112)));
    let mut invalid = original.clone();
    invalid.exploration_started_at = None;
    assert_eq!(engine.projection_valid_until(&invalid, Timestamp::new(110), &params), Some(Timestamp::new(110)));
    invalid.exploration_started_at = Some(Timestamp::new(u64::MAX));
    assert_eq!(engine.projection_valid_until(&invalid, Timestamp::new(110), &params), Some(Timestamp::new(110)));
    let mut overflow = original.clone();
    overflow.phase = GovernancePhase::Cooldown;
    overflow.cooldown_started_at = Some(Timestamp::new(u64::MAX - 1));
    assert_eq!(engine.projection_valid_until(&overflow, Timestamp::new(110), &params), Some(Timestamp::new(u64::MAX)));
}

#[test]
fn shared_electorate_preserves_canonical_receipts_and_lazy_validation() {
    let (engine, delegation, params, id, verified) = fixture();
    let electorate = BallotElectorate::new(verified.clone());
    assert!(electorate.commitment.get().is_none());
    assert!(engine.prepare_ballot_with_electorate(&id, BlockHash::ZERO,
        Timestamp::new(112), &params, &delegation, &electorate).is_err());
    assert!(electorate.commitment.get().is_none(), "invalid history must precede electorate work");
    let expected = prepare(&engine, &delegation, &params, id, &verified);
    // Construct the expected encoding directly from the count and sorted keys.
    let mut keys: Vec<_> = verified.iter().map(|a| burst_crypto::decode_address(a.as_str()).unwrap()).collect();
    keys.sort();
    let mut bytes = b"BURST/governance-electorate/v1\0".to_vec();
    bytes.extend_from_slice(&(verified.len() as u32).to_be_bytes());
    for key in keys { bytes.extend_from_slice(&key); }
    for _ in 0..4 {
        let ballot = engine.prepare_ballot_with_electorate(&id, BlockHash::new([9; 32]),
            Timestamp::new(112), &params, &delegation, &electorate).unwrap();
        assert_eq!(ballot.receipt.electorate, BlockHash::new(burst_crypto::blake2b_256(&bytes)));
        assert_eq!(ballot.commitment().unwrap(), expected.commitment().unwrap());
        assert_eq!(ballot.next_proposal(), expected.next_proposal());
        assert!(electorate.commitment.get().is_some());
    }
    let mut reordered = verified.clone();
    reordered.reverse();
    assert_eq!(BallotElectorate::new(reordered).commitment().unwrap(), electorate.commitment().unwrap());
    for invalid in [vec![wallet(1), wallet(1)], vec![WalletAddress::new("brst_invalid")]] {
        let invalid = BallotElectorate::new(invalid);
        assert!(invalid.commitment.get().is_none());
        assert!(invalid.commitment().is_err());
        assert!(invalid.commitment().is_err());
    }
    let smaller = BallotElectorate::new(verified[..verified.len() - 1].to_vec());
    assert_ne!(smaller.commitment().unwrap(), electorate.commitment().unwrap());
}

#[test]
#[ignore = "explicit capped-host governance scaling diagnostic"]
fn electorate_commitment_scaling() {
    use std::time::Instant;
    let n: usize = std::env::var("BURST_GOV_PROBE_POPULATION").unwrap().parse().unwrap();
    assert!((10..=100_000).contains(&n));
    let start = Instant::now();
    let voters: Vec<_> = (0..n).map(|i| {
        let mut key = [0u8; 32];
        key[..8].copy_from_slice(&(i as u64).to_be_bytes());
        burst_crypto::derive_address(&burst_types::PublicKey(key))
    }).collect();
    let setup_ms = start.elapsed().as_secs_f64() * 1000.0;
    for sample in 0..5 {
        let electorate = BallotElectorate::new(voters.as_slice());
        let start = Instant::now();
        let expected = electorate.commitment().unwrap();
        let cold_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        for _ in 0..1000 {
            assert_eq!(std::hint::black_box(electorate.commitment().unwrap()), expected);
        }
        let warm_per_call_ns = start.elapsed().as_nanos() as f64 / 1000.0;
        assert_eq!(expected.0 as usize, n);
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let memory: Vec<_> = status.lines().filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:")).collect();
        println!("gov_scale {}", serde_json::json!({"kind":"electorate", "population":n,
            "sample":sample,"setup_ms":setup_ms,"cold_ms":cold_ms,
            "warm_per_call_ns":warm_per_call_ns,"memory":memory}));
    }
}

impl GovernanceEngine {
    /// Project elapsed phases for display and candidate preparation. A local
    /// clock and a possibly incomplete electorate cannot close the stored ballot.
    /// This copy neither applies activation effects nor authenticates a result.
    fn project_at(
        &self,
        now: Timestamp,
        params: &ProtocolParams,
        delegation: &DelegationEngine,
        verified: &[WalletAddress],
    ) -> Self {
        let mut projected = self.clone();
        for original in self.all_proposals() {
            let next = self.project_proposal_at(original, now, params, delegation, verified);
            *projected
                .get_proposal_mut(&original.hash)
                .expect("cloned proposal") = next;
        }
        projected
    }

    fn due_activation_hashes(&self, now: Timestamp) -> Vec<TxHash> {
        let mut due: Vec<_> = self
            .all_proposals()
            .filter(|proposal| {
                proposal.phase == GovernancePhase::Activation
                    && proposal.activation_at.is_some_and(|at| at <= now)
            })
            .collect();
        due.sort_by_key(|proposal| {
            (
                proposal.activation_at.map(|at| at.as_secs()),
                *proposal.hash.as_bytes(),
            )
        });
        due.into_iter().map(|proposal| proposal.hash).collect()
    }
}
