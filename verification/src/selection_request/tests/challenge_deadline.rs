use super::*;
use crate::{
    state::VerificationPhase, ChallengeReason, ChallengeResult, CommitteeActivation,
    VerificationEvent,
};

fn challenge() -> BlockHash {
    BlockHash::new([71; 32])
}

fn panel(
    max_revotes: u32,
    threshold: u32,
    reason: ChallengeReason,
) -> (
    VerificationOrchestrator,
    ProtocolParams,
    CommitteeActivation,
) {
    panel_kind(max_revotes, threshold, reason, false)
}

fn panel_kind(
    max_revotes: u32,
    threshold: u32,
    reason: ChallengeReason,
    appeal: bool,
) -> (
    VerificationOrchestrator,
    ProtocolParams,
    CommitteeActivation,
) {
    let params = ProtocolParams {
        num_verifiers: 2,
        max_revotes,
        verification_threshold_bps: threshold,
        verification_timeout_secs: 30,
        challenge_duration_secs: 60,
        ..ProtocolParams::default()
    };
    let mut state = VerificationOrchestrator::new();
    state
        .process_endorsement_at(
            &address(99),
            &address(90),
            params.endorsement_burn_amount,
            &params,
            Timestamp::new(GENESIS - 3),
        )
        .unwrap();
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
    if appeal {
        state
            .initiate_signed_appeal_at(
                &address(99),
                &address(90),
                true,
                params.challenge_stake_amount,
                (
                    reason,
                    Timestamp::new(GENESIS - 2),
                    challenge(),
                    crate::AppealContext {
                        reviewed_outcome: BlockHash::new([72; 32]),
                        previously_legitimate: true,
                    },
                ),
                &params,
            )
            .unwrap();
    } else {
        state
            .initiate_signed_challenge_at(
                &address(99),
                &address(90),
                true,
                params.challenge_stake_amount,
                (reason, Timestamp::new(GENESIS - 2), challenge()),
                &params,
            )
            .unwrap();
    }
    state.drain_events();
    let request = state
        .prepare_selection(
            &address(99),
            challenge(),
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
fn appeal_deadline_candidate_preserves_review_context_and_signed_default_distinction() {
    for (vote, threshold, expected) in [
        (Vote::Legitimate, 5000, ChallengeResult::ChallengeRejected),
        (Vote::Illegitimate, 9000, ChallengeResult::FraudConfirmed),
    ] {
        let (mut state, params, activation) =
            panel_kind(0, threshold, ChallengeReason::Fraud, true);
        state
            .process_certified_vote_at(
                &activation.target,
                &address(1),
                vote,
                activation.context,
                &params,
                Timestamp::new(GENESIS + 1),
            )
            .unwrap();
        state = restarted(&state);
        let history = state
            .selection_history_commitment(&activation.target)
            .unwrap();
        let (mut candidate, absent) = state
            .challenge_deadline_candidate(&activation, &params, Timestamp::new(activation.deadline))
            .unwrap();
        assert_eq!(absent, vec![address(2)]);
        assert_eq!(
            state
                .selection_history_commitment(&activation.target)
                .unwrap(),
            history
        );
        assert!(state.has_active_challenge(&activation.target));
        let events = candidate.drain_events();
        let outcome = events
            .iter()
            .find_map(|event| match event {
                VerificationEvent::ChallengeResolved { outcome, .. } => Some(outcome),
                _ => None,
            })
            .unwrap();
        assert_eq!(outcome.outcome, expected);
        assert_eq!(
            outcome.appeal,
            Some(crate::AppealContext {
                reviewed_outcome: BlockHash::new([72; 32]),
                previously_legitimate: true,
            })
        );
        let missing = outcome
            .verifier_outcomes
            .iter()
            .find(|voter| voter.address == address(2))
            .unwrap();
        assert_eq!(missing.selection_context, None);
        assert_eq!(missing.staked, 0);
        assert_eq!(missing.trst_reward, 0);
        assert!(!events.iter().any(|event| matches!(
            event,
            VerificationEvent::WalletUnverified { .. }
                | VerificationEvent::WalletDeactivated { .. }
        )));
        assert!(restarted(&candidate)
            .active_appeal(&activation.target)
            .is_none());
    }
}

fn restarted(state: &VerificationOrchestrator) -> VerificationOrchestrator {
    VerificationOrchestrator::restore(
        OrchestratorSnapshot::decode(&state.snapshot().encode().unwrap()).unwrap(),
    )
}

#[test]
fn a_completed_challenge_can_restart_only_with_fresh_endorsements() {
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        let (state, params, activation) = panel(0, 9000, reason);
        let (mut completed, _) = state
            .challenge_deadline_candidate(&activation, &params, Timestamp::new(activation.deadline))
            .unwrap();
        completed.drain_events();
        let penalized = completed.penalized_count();
        let mut completed = restarted(&completed);
        let before = completed.snapshot().encode().unwrap();
        assert!(completed
            .process_endorsement_at(
                &activation.target,
                &address(90),
                params.endorsement_burn_amount - 1,
                &params,
                Timestamp::new(activation.deadline + 1),
            )
            .is_err());
        assert_eq!(completed.snapshot().encode().unwrap(), before);
        assert!(completed
            .process_endorsement_at(
                &activation.target,
                &address(90),
                params.endorsement_burn_amount,
                &params,
                Timestamp::new(activation.deadline - 1),
            )
            .is_err());
        assert_eq!(completed.snapshot().encode().unwrap(), before);
        completed
            .process_endorsement_at(
                &activation.target,
                &address(90),
                params.endorsement_burn_amount,
                &params,
                Timestamp::new(activation.deadline + 1),
            )
            .unwrap();
        let state = completed.get_state(&activation.target).unwrap();
        assert_eq!(state.phase, VerificationPhase::Endorsing);
        assert_eq!(state.started_at, Timestamp::new(activation.deadline + 1));
        assert_eq!(state.endorsements.len(), 1);
        assert_eq!(state.endorsements[0].endorser, address(90));
        assert!(state.selected_verifiers.is_empty());
        assert!(state.votes.is_empty());
        assert!(state.excluded_verifiers.is_empty());
        assert_eq!(state.revote_count, 0);
        assert!(completed.selection_context(&activation.target).is_none());
        assert!(completed.selection_request(&activation.target).is_none());
        assert!(completed
            .active_challenge_context(&activation.target)
            .unwrap()
            .is_none());
        assert!(completed.prior_panel_votes(&activation.target).is_empty());
        assert_eq!(completed.penalized_count(), penalized);
        assert!(completed
            .process_certified_vote_at(
                &activation.target,
                &address(1),
                Vote::Legitimate,
                activation.context,
                &params,
                Timestamp::new(activation.deadline + 2),
            )
            .is_err());
        let before = completed.snapshot().encode().unwrap();
        assert!(completed
            .process_endorsement_at(
                &activation.target,
                &address(90),
                params.endorsement_burn_amount,
                &params,
                Timestamp::new(activation.deadline + 2),
            )
            .is_err());
        assert_eq!(completed.snapshot().encode().unwrap(), before);
        let restored = restarted(&completed);
        assert_eq!(
            restored
                .selection_history_commitment(&activation.target)
                .unwrap(),
            completed
                .selection_history_commitment(&activation.target)
                .unwrap(),
        );
        assert_eq!(
            bincode::serialize(restored.get_state(&activation.target).unwrap()).unwrap(),
            bincode::serialize(completed.get_state(&activation.target).unwrap()).unwrap(),
        );
        assert_eq!(
            restored.snapshot().penalized_verifiers,
            completed.snapshot().penalized_verifiers
        );
        for voter in [address(1), address(2)] {
            assert_eq!(
                restored.neither_tracker.total_assignments(&voter),
                completed.neither_tracker.total_assignments(&voter)
            );
            assert_eq!(
                restored.neither_tracker.neither_count(&voter),
                completed.neither_tracker.neither_count(&voter)
            );
        }
    }
}

#[test]
fn an_active_challenge_cannot_be_restarted_by_an_endorsement() {
    let (mut state, params, activation) = panel(3, 9000, ChallengeReason::Fraud);
    let before = state.snapshot().encode().unwrap();
    assert!(state
        .process_endorsement_at(
            &activation.target,
            &address(91),
            params.endorsement_burn_amount,
            &params,
            Timestamp::new(activation.deadline + 1),
        )
        .is_err());
    assert_eq!(state.snapshot().encode().unwrap(), before);
}

#[test]
fn silent_challenge_panel_requests_a_revote_without_a_verdict_or_signed_defaults() {
    let (state, params, activation) = panel(3, 9000, ChallengeReason::Fraud);
    for original in [state.clone(), restarted(&state)] {
        let before = original.snapshot().encode().unwrap();
        let (mut candidate, absent) = original
            .challenge_deadline_candidate(&activation, &params, Timestamp::new(activation.deadline))
            .unwrap();
        assert_eq!(original.snapshot().encode().unwrap(), before);
        assert_eq!(absent.len(), 2);
        let process = candidate.get_state(&activation.target).unwrap();
        assert_eq!(process.phase, VerificationPhase::Voting);
        assert_eq!(process.revote_count, 1);
        assert!(process.selected_verifiers.is_empty());
        assert_eq!(process.excluded_verifiers.len(), 2);
        assert_eq!(
            candidate
                .active_challenge_context(&activation.target)
                .unwrap(),
            Some(challenge())
        );
        assert!(candidate.selection_context(&activation.target).is_none());
        assert!(candidate
            .prior_panel_votes(&activation.target)
            .iter()
            .all(|vote| vote.vote == Vote::Neither
                && vote.stake_amount == 0
                && vote.timestamp.as_secs() == activation.deadline));
        assert!(!candidate.completed_selection(&activation.target, activation.context, &absent));
        let events = candidate.drain_events();
        assert_eq!(events.len(), 2);
        assert!(events
            .iter()
            .all(|event| matches!(event, VerificationEvent::VerifierPenalized { .. })));
        assert_eq!(
            restarted(&candidate)
                .active_challenge_context(&activation.target)
                .unwrap(),
            Some(challenge())
        );
    }
}

#[test]
fn partial_challenge_deadlines_use_verdict_rules_not_the_legacy_half_stake_expiry() {
    for (vote, threshold, expected) in [
        (Vote::Legitimate, 5000, ChallengeResult::ChallengeRejected),
        (Vote::Illegitimate, 9000, ChallengeResult::FraudConfirmed),
        (Vote::Legitimate, 9000, ChallengeResult::FraudConfirmed),
    ] {
        let (mut state, params, activation) = panel(0, threshold, ChallengeReason::Fraud);
        state
            .process_certified_vote_at(
                &activation.target,
                &address(1),
                vote,
                activation.context,
                &params,
                Timestamp::new(GENESIS + 1),
            )
            .unwrap();
        state.drain_events();
        let before = state.snapshot().encode().unwrap();
        let (mut candidate, absent) = state
            .challenge_deadline_candidate(
                &activation,
                &params,
                Timestamp::new(activation.deadline + 500),
            )
            .unwrap();
        assert_eq!(state.snapshot().encode().unwrap(), before);
        assert_eq!(absent, vec![address(2)]);
        assert!(!candidate.has_active_challenge(&activation.target));
        let events = candidate.drain_events();
        let outcome = events
            .iter()
            .find_map(|event| match event {
                VerificationEvent::ChallengeResolved { outcome, .. } => Some(outcome),
                _ => None,
            })
            .unwrap();
        assert_eq!(outcome.outcome, expected);
        assert_eq!(outcome.challenge_context, Some(challenge()));
        assert_eq!(outcome.challenger_stake, params.challenge_stake_amount);
        let default = outcome
            .verifier_outcomes
            .iter()
            .find(|v| v.address == address(2))
            .unwrap();
        assert_eq!(
            (default.staked, default.penalty, default.trst_reward),
            (0, 0, 0)
        );
        assert_eq!(default.selection_context, None);
        let signed = outcome
            .verifier_outcomes
            .iter()
            .find(|v| v.address == address(1))
            .unwrap();
        assert_eq!(signed.selection_context, Some(activation.context));
        assert_eq!(signed.staked, params.verifier_stake_amount);
        let loses = expected == ChallengeResult::FraudConfirmed && vote == Vote::Legitimate;
        assert_eq!(
            signed.penalty,
            if loses {
                params.verifier_stake_amount
            } else {
                0
            }
        );
        assert_eq!(
            candidate.get_state(&activation.target).unwrap().phase,
            if expected == ChallengeResult::FraudConfirmed {
                VerificationPhase::Unverified
            } else {
                VerificationPhase::Verified
            }
        );
    }
}

#[test]
fn exhausted_silent_panels_preserve_fraud_vs_inactivity_consequences() {
    for (reason, phase) in [
        (ChallengeReason::Fraud, VerificationPhase::Unverified),
        (ChallengeReason::Inactivity, VerificationPhase::Deactivated),
    ] {
        let (state, params, activation) = panel(0, 9000, reason);
        let (mut candidate, absent) = state
            .challenge_deadline_candidate(&activation, &params, Timestamp::new(activation.deadline))
            .unwrap();
        assert_eq!(absent.len(), 2);
        assert_eq!(
            candidate.get_state(&activation.target).unwrap().phase,
            phase
        );
        let events = candidate.drain_events();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, VerificationEvent::WalletUnverified { .. }))
                .count(),
            usize::from(phase == VerificationPhase::Unverified)
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, VerificationEvent::WalletDeactivated { .. }))
                .count(),
            usize::from(phase == VerificationPhase::Deactivated)
        );
        let outcome = events
            .iter()
            .find_map(|event| match event {
                VerificationEvent::ChallengeResolved { outcome, .. } => Some(outcome),
                _ => None,
            })
            .unwrap();
        assert_eq!(outcome.outcome, ChallengeResult::FraudConfirmed);
        assert!(outcome.verifier_outcomes.iter().all(|v| v.staked == 0
            && v.penalty == 0
            && v.trst_reward == 0
            && v.selection_context.is_none()));
    }
}

#[test]
fn forged_or_early_challenge_deadlines_fail_without_mutation() {
    let (state, params, activation) = panel(3, 9000, ChallengeReason::Fraud);
    let before = state.snapshot().encode().unwrap();
    assert!(state
        .challenge_deadline_candidate(
            &activation,
            &params,
            Timestamp::new(activation.deadline - 1)
        )
        .is_err());
    assert!(state
        .normal_deadline_candidate(&activation, &params, Timestamp::new(activation.deadline))
        .is_err());
    for change in 0..7 {
        let mut forged = activation.clone();
        match change {
            0 => forged.target = address(98),
            1 => forged.selection = BlockHash::new([77; 32]),
            2 => forged.context = BlockHash::ZERO,
            3 => forged.params_hash = BlockHash::ZERO,
            4 => forged.started_at = GENESIS - 3,
            5 => forged.deadline += 1,
            _ => forged.started_at = u64::MAX,
        }
        assert!(state
            .challenge_deadline_candidate(&forged, &params, Timestamp::new(u64::MAX))
            .is_err());
        assert_eq!(state.snapshot().encode().unwrap(), before);
    }
}

#[test]
fn late_votes_and_complete_challenge_panels_cannot_be_rewritten_as_defaults() {
    for complete in [false, true] {
        let (mut state, params, activation) = panel(3, 9000, ChallengeReason::Fraud);
        state
            .process_certified_vote_at(
                &activation.target,
                &address(1),
                Vote::Legitimate,
                activation.context,
                &params,
                Timestamp::new(if complete {
                    GENESIS + 1
                } else {
                    activation.deadline
                }),
            )
            .unwrap();
        if complete {
            state
                .process_certified_vote_at(
                    &activation.target,
                    &address(2),
                    Vote::Legitimate,
                    activation.context,
                    &params,
                    Timestamp::new(GENESIS + 2),
                )
                .unwrap();
        }
        state.drain_events();
        let before = state.snapshot().encode().unwrap();
        for original in [state.clone(), restarted(&state)] {
            assert!(original
                .challenge_deadline_candidate(
                    &activation,
                    &params,
                    Timestamp::new(activation.deadline)
                )
                .is_err());
        }
        assert_eq!(state.snapshot().encode().unwrap(), before);
    }
}

#[test]
fn activation_not_admission_sets_the_candidate_window_and_uncommitted_events_block_it() {
    let (mut state, params, mut activation) = panel(3, 9000, ChallengeReason::Fraud);
    activation.started_at = GENESIS + params.challenge_duration_secs + 100;
    activation.deadline = activation.started_at + params.challenge_duration_secs;
    assert!(state
        .challenge_deadline_candidate(
            &activation,
            &params,
            Timestamp::new(activation.deadline - 1)
        )
        .is_err());
    let (_, absent) = state
        .challenge_deadline_candidate(&activation, &params, Timestamp::new(activation.deadline))
        .unwrap();
    assert_eq!(absent.len(), 2);

    state
        .genesis_verify_at(&address(98), &address(4), 1, 50, Timestamp::new(GENESIS))
        .unwrap();
    let before = state.snapshot().encode().unwrap();
    let error = state
        .challenge_deadline_candidate(&activation, &params, Timestamp::new(activation.deadline))
        .err()
        .unwrap();
    assert!(error.to_string().contains("uncommitted lifecycle events"));
    assert_eq!(state.snapshot().encode().unwrap(), before);
    assert_eq!(state.drain_events().len(), 1);
}
