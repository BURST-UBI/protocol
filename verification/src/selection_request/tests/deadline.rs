use super::*;
use crate::{state::VerificationPhase, CommitteeActivation, VerificationEvent, VerificationResult};

fn panel(
    max_revotes: u32,
    threshold: u32,
) -> (
    VerificationOrchestrator,
    ProtocolParams,
    CommitteeActivation,
) {
    let params = ProtocolParams {
        endorsement_threshold: 1,
        endorsement_burn_amount: 100,
        num_verifiers: 2,
        max_revotes,
        verification_threshold_bps: threshold,
        verification_timeout_secs: 60,
        ..ProtocolParams::default()
    };
    let mut state = VerificationOrchestrator::new();
    state
        .process_endorsement_at(
            &address(99),
            &address(90),
            100,
            &params,
            Timestamp::new(GENESIS - 2),
        )
        .unwrap();
    state.drain_events();
    let request = state
        .prepare_selection(
            &address(99),
            BlockHash::new([5; 32]),
            Timestamp::new(GENESIS - 1),
            &[address(1), address(2)],
            &params,
        )
        .unwrap();
    let (parent, certificate) = certificate(&request);
    let context = state
        .install_selection(&address(99), &parent, &certificate, &output())
        .unwrap();
    state.drain_events();
    let activation = CommitteeActivation {
        target: address(99),
        selection: certificate.checkpoint.hash(),
        context,
        params_hash: params.params_hash(),
        rules_at_activation: params.clone(),
        started_at: GENESIS,
        deadline: GENESIS + 60,
    };
    (state, params, activation)
}

#[test]
fn governed_rules_preserve_the_panel_deadline_and_already_paid_stakes() {
    let (mut state, params, activation) = panel(0, 9000);
    state
        .process_certified_vote_at(
            &activation.target,
            &address(1),
            Vote::Legitimate,
            activation.context,
            &params,
            Timestamp::new(GENESIS + 1),
        )
        .unwrap();
    let mut governed = params.clone();
    governed.brn_rate += 1;
    governed.verifier_stake_amount += 7;
    governed.verification_timeout_secs = 1;
    governed.verification_threshold_bps = 5000;
    let mut voted = state.clone();
    voted
        .process_certified_vote_at(
            &activation.target,
            &address(2),
            Vote::Legitimate,
            activation.context,
            &governed,
            Timestamp::new(GENESIS + 2),
        )
        .unwrap();
    let votes = &voted.get_state(&activation.target).unwrap().votes;
    assert_eq!(votes[0].stake_amount, params.verifier_stake_amount);
    assert_eq!(votes[1].stake_amount, governed.verifier_stake_amount);
    assert_eq!(
        voted.selection_context(&activation.target),
        Some(activation.context)
    );
    assert!(state
        .normal_deadline_candidate(
            &activation,
            &governed,
            Timestamp::new(activation.deadline - 1)
        )
        .is_err());
    let (mut completed, absent) = state
        .normal_deadline_candidate(&activation, &governed, Timestamp::new(activation.deadline))
        .unwrap();
    assert_eq!(absent, vec![address(2)]);
    assert!(completed.drain_events().iter().any(|event| matches!(event,
        VerificationEvent::VerificationComplete { result: VerificationResult::Verified, outcomes, .. }
        if outcomes.verifiers.iter().any(|voter| voter.address == address(1)
            && voter.staked == params.verifier_stake_amount))));
}

#[test]
fn all_absent_voters_default_to_neither_and_request_a_revote_without_signed_bindings() {
    let (state, params, activation) = panel(3, 9000);
    let original = state.snapshot().encode().unwrap();
    let (mut candidate, absent) = state
        .normal_deadline_candidate(&activation, &params, Timestamp::new(activation.deadline))
        .unwrap();
    let mut expected = vec![address(1), address(2)];
    expected.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    assert_eq!(absent, expected);
    assert_eq!(state.snapshot().encode().unwrap(), original);
    let process = candidate.get_state(&activation.target).unwrap();
    assert_eq!(process.phase, VerificationPhase::Voting);
    assert_eq!(process.revote_count, 1);
    assert!(process.selected_verifiers.is_empty());
    assert!(process.votes.is_empty());
    assert_eq!(process.excluded_verifiers.len(), 2);
    assert!(!candidate.completed_selection(&activation.target, activation.context, &absent));
    assert!(candidate.selection_context(&activation.target).is_none());
    let events = candidate.drain_events();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|event| matches!(event,
        VerificationEvent::VerifierPenalized { cooldown_until, .. }
            if *cooldown_until == activation.deadline + params.neither_penalty_cooldown_secs)));
    let bytes = candidate.snapshot().encode().unwrap();
    let restored = VerificationOrchestrator::restore(OrchestratorSnapshot::decode(&bytes).unwrap());
    assert_eq!(
        restored.get_state(&activation.target).unwrap().revote_count,
        1
    );
    assert!(restored
        .normal_deadline_candidate(
            &activation,
            &params,
            Timestamp::new(activation.deadline + 1)
        )
        .is_err());
    assert!(!restored.completed_selection(&activation.target, activation.context, &absent));
}

#[test]
fn partial_signed_votes_keep_their_stakes_and_defaults_never_gain_receipts_or_rewards() {
    for (threshold, result) in [
        (9000, VerificationResult::Failed),
        (5000, VerificationResult::Verified),
    ] {
        let (mut state, params, activation) = panel(0, threshold);
        state
            .process_certified_vote_at(
                &activation.target,
                &address(1),
                Vote::Legitimate,
                activation.context,
                &params,
                Timestamp::new(GENESIS + 1),
            )
            .unwrap();
        state.drain_events();
        let before = state.snapshot().encode().unwrap();
        let (mut candidate, absent) = state
            .normal_deadline_candidate(
                &activation,
                &params,
                Timestamp::new(activation.deadline + 50),
            )
            .unwrap();
        assert_eq!(absent, vec![address(2)]);
        assert_eq!(state.snapshot().encode().unwrap(), before);
        let events = candidate.drain_events();
        let outcome = events
            .iter()
            .find_map(|event| match event {
                VerificationEvent::VerificationComplete {
                    result: observed,
                    outcomes,
                    ..
                } => {
                    assert_eq!(observed, &result);
                    Some(outcomes)
                }
                _ => None,
            })
            .unwrap();
        let cast = outcome
            .verifiers
            .iter()
            .find(|vote| vote.address == address(1))
            .unwrap();
        assert_eq!(cast.selection_context, Some(activation.context));
        assert_eq!(cast.staked, params.verifier_stake_amount);
        assert_eq!(
            cast.penalty,
            if result == VerificationResult::Failed {
                params.verifier_stake_amount
            } else {
                0
            }
        );
        let default = outcome
            .verifiers
            .iter()
            .find(|vote| vote.address == address(2))
            .unwrap();
        assert_eq!(default.selection_context, None);
        assert_eq!(
            (default.staked, default.penalty, default.trst_reward),
            (0, 0, 0)
        );
        assert!(outcome.verifiers.iter().all(|vote| vote.trst_reward == 0));
        assert!(!candidate.completed_selection(
            &activation.target,
            activation.context,
            &[address(1), address(2)]
        ));
        assert!(candidate
            .normal_deadline_candidate(
                &activation,
                &params,
                Timestamp::new(activation.deadline + 1)
            )
            .is_err());
    }
}

#[test]
fn exhausted_all_neither_panel_fails_instead_of_verifying_or_requesting_another_revote() {
    let (state, params, activation) = panel(0, 9000);
    let (mut candidate, absent) = state
        .normal_deadline_candidate(&activation, &params, Timestamp::new(activation.deadline))
        .unwrap();
    assert_eq!(absent.len(), 2);
    assert_eq!(
        candidate.get_state(&activation.target).unwrap().phase,
        VerificationPhase::Failed
    );
    assert!(candidate.drain_events().iter().any(|event| matches!(event,
        VerificationEvent::VerificationComplete { result: VerificationResult::Failed, outcomes, .. }
            if outcomes.verifiers.iter().all(|v| v.selection_context.is_none()
                && v.staked == 0 && v.penalty == 0 && v.trst_reward == 0))));
}

#[test]
fn prior_default_history_needs_explicit_completion_bindings_and_keeps_signed_history_distinct() {
    let (state, params, activation) = panel(3, 9000);
    let (mut candidate, absent) = state
        .normal_deadline_candidate(&activation, &params, Timestamp::new(activation.deadline))
        .unwrap();
    candidate.drain_events();
    assert!(candidate
        .selection_history_commitment(&activation.target)
        .is_err());
    let defaults: std::collections::HashMap<_, _> = absent
        .iter()
        .cloned()
        .map(|voter| (voter, (BlockHash::new([42; 32]), activation.deadline)))
        .collect();
    let commitment = candidate
        .selection_history_with_defaults(&activation.target, &defaults)
        .unwrap();
    for change in 0..4 {
        let mut changed = defaults.clone();
        match change {
            0 => {
                changed.remove(&absent[0]);
            }
            1 => {
                changed.get_mut(&absent[0]).unwrap().1 += 1;
            }
            2 => {
                changed.get_mut(&absent[0]).unwrap().0 = BlockHash::ZERO;
            }
            _ => {
                changed.insert(address(98), (BlockHash::new([42; 32]), activation.deadline));
            }
        }
        assert!(candidate
            .selection_history_with_defaults(&activation.target, &changed)
            .is_err());
    }
    let restored = VerificationOrchestrator::restore(
        OrchestratorSnapshot::decode(&candidate.snapshot().encode().unwrap()).unwrap(),
    );
    assert_eq!(
        restored
            .selection_history_with_defaults(&activation.target, &defaults)
            .unwrap(),
        commitment
    );
    assert!(!restored.completed_selection(&activation.target, activation.context, &absent));
    let (mut signed, params, activation) = panel(3, 9000);
    for voter in [address(1), address(2)] {
        signed
            .process_certified_vote_at(
                &activation.target,
                &voter,
                Vote::Neither,
                activation.context,
                &params,
                Timestamp::new(activation.deadline),
            )
            .unwrap();
    }
    signed.drain_events();
    assert!(signed
        .selection_history_commitment(&activation.target)
        .is_ok());
    assert!(signed
        .selection_history_with_defaults(&activation.target, &defaults)
        .is_err());
}

#[test]
fn deadline_rejects_early_or_changed_activation_inputs_without_mutating_the_original() {
    let (state, params, activation) = panel(3, 9000);
    let before = state.snapshot().encode().unwrap();
    assert!(state
        .normal_deadline_candidate(
            &activation,
            &params,
            Timestamp::new(activation.deadline - 1)
        )
        .is_err());
    for change in 0..7 {
        let mut forged = activation.clone();
        match change {
            0 => forged.target = address(98),
            1 => forged.selection = BlockHash::new([77; 32]),
            2 => forged.context = BlockHash::ZERO,
            3 => forged.params_hash = BlockHash::ZERO,
            4 => forged.started_at = GENESIS - 1,
            5 => forged.deadline += 1,
            _ => forged.started_at = u64::MAX,
        }
        assert!(state
            .normal_deadline_candidate(&forged, &params, Timestamp::new(u64::MAX))
            .is_err());
        assert_eq!(state.snapshot().encode().unwrap(), before);
    }
    let mut forged = activation.clone();
    forged.rules_at_activation.verification_timeout_secs = 0;
    assert!(state
        .normal_deadline_candidate(&forged, &params, Timestamp::new(u64::MAX))
        .is_err());
}

#[test]
fn a_late_vote_or_an_already_complete_panel_cannot_be_reinterpreted_as_a_timeout() {
    let (mut state, params, activation) = panel(3, 9000);
    state
        .process_certified_vote_at(
            &activation.target,
            &address(1),
            Vote::Legitimate,
            activation.context,
            &params,
            Timestamp::new(activation.deadline),
        )
        .unwrap();
    state.drain_events();
    let before = state.snapshot().encode().unwrap();
    let error = state
        .normal_deadline_candidate(&activation, &params, Timestamp::new(activation.deadline))
        .err()
        .unwrap();
    assert!(error.to_string().contains("unbound or late vote"));
    assert_eq!(state.snapshot().encode().unwrap(), before);
    let (mut state, params, activation) = panel(3, 9000);
    for voter in [address(1), address(2)] {
        state
            .process_certified_vote_at(
                &activation.target,
                &voter,
                Vote::Legitimate,
                activation.context,
                &params,
                Timestamp::new(GENESIS + 1),
            )
            .unwrap();
    }
    state.drain_events();
    assert!(state
        .normal_deadline_candidate(&activation, &params, Timestamp::new(activation.deadline))
        .is_err());
}

#[test]
fn legacy_panels_and_active_challenges_do_not_use_normal_deadline_completion() {
    let params = ProtocolParams::default();
    let mut state = VerificationOrchestrator::new();
    state
        .genesis_verify_at(
            &address(99),
            &address(4),
            1,
            50,
            Timestamp::new(GENESIS - 2),
        )
        .unwrap();
    state.drain_events();
    state
        .initiate_signed_challenge_at(
            &address(99),
            &address(3),
            true,
            params.challenge_stake_amount,
            (
                crate::ChallengeReason::Fraud,
                Timestamp::new(GENESIS),
                BlockHash::new([71; 32]),
            ),
            &params,
        )
        .unwrap();
    state.drain_events();
    let (_, _, activation) = panel(3, 9000);
    let error = state
        .normal_deadline_candidate(&activation, &params, Timestamp::new(activation.deadline))
        .err()
        .unwrap();
    assert!(error.to_string().contains("cannot resolve a challenge"));
    assert!(VerificationOrchestrator::new()
        .normal_deadline_candidate(&activation, &params, Timestamp::new(activation.deadline))
        .is_err());
}
