use super::*;

fn address(seed: u8) -> WalletAddress {
    WalletAddress::new(format!("brst_{}", seed.to_string().repeat(60)))
}

fn fixture(
    reason: crate::ChallengeReason,
    legitimate: bool,
) -> (VerificationOrchestrator, ProtocolParams) {
    let mut orchestrator = VerificationOrchestrator::new();
    orchestrator.initialize_genesis(&address(1));
    orchestrator.states.get_mut(&address(1)).unwrap().phase = if legitimate {
        VerificationPhase::Verified
    } else if reason == crate::ChallengeReason::Fraud {
        VerificationPhase::Unverified
    } else {
        VerificationPhase::Deactivated
    };
    let params = ProtocolParams {
        num_verifiers: 3,
        verification_threshold_bps: 9000,
        challenge_stake_amount: 500,
        verifier_stake_amount: 100,
        ..ProtocolParams::burst_defaults()
    };
    (orchestrator, params)
}

fn context(legitimate: bool) -> AppealContext {
    AppealContext {
        reviewed_outcome: BlockHash::new([7; 32]),
        previously_legitimate: legitimate,
    }
}

fn admit(
    orchestrator: &mut VerificationOrchestrator,
    params: &ProtocolParams,
    reason: crate::ChallengeReason,
    legitimate: bool,
) -> Result<(), VerificationError> {
    orchestrator.initiate_signed_appeal_at(
        &address(1),
        &address(2),
        true,
        500,
        (
            reason,
            Timestamp::new(100),
            BlockHash::new([8; 32]),
            context(legitimate),
        ),
        params,
    )
}

#[test]
fn appeal_selection_commitment_binds_the_reviewed_judgment() {
    let (mut orchestrator, params) = fixture(crate::ChallengeReason::Fraud, true);
    admit(
        &mut orchestrator,
        &params,
        crate::ChallengeReason::Fraud,
        true,
    )
    .unwrap();
    let hash = orchestrator
        .selection_history_commitment(&address(1))
        .unwrap();
    super::super::records::assert_record_roundtrip(&orchestrator);
    let restored = VerificationOrchestrator::restore(
        OrchestratorSnapshot::decode(&orchestrator.snapshot().encode().unwrap()).unwrap(),
    );
    assert_eq!(
        restored.selection_history_commitment(&address(1)).unwrap(),
        hash
    );
    for change in 0..3 {
        let mut other = orchestrator.clone();
        match change {
            0 => {
                other.active_appeals.remove(&address(1));
            }
            1 => {
                other
                    .active_appeals
                    .get_mut(&address(1))
                    .unwrap()
                    .reviewed_outcome = BlockHash::new([9; 32]);
            }
            _ => {
                other
                    .active_appeals
                    .get_mut(&address(1))
                    .unwrap()
                    .previously_legitimate = false;
            }
        }
        assert_ne!(
            other.selection_history_commitment(&address(1)).unwrap(),
            hash
        );
    }
}

#[test]
fn appeal_panels_survive_restart_and_all_verdicts_wait_for_certified_settlement() {
    for reason in [
        crate::ChallengeReason::Fraud,
        crate::ChallengeReason::Inactivity,
    ] {
        for previous in [false, true] {
            for legitimate in [false, true] {
                let (mut orchestrator, params) = fixture(reason, previous);
                admit(&mut orchestrator, &params, reason, previous).unwrap();
                let pool: Vec<_> = (3..=5).map(address).collect();
                let selected = orchestrator
                    .select_verifiers(&address(1), &pool, &[9; 32], &params)
                    .unwrap();
                orchestrator.drain_events();
                for voter in &selected {
                    assert!(orchestrator
                        .process_vote_at(
                            &address(1),
                            voter,
                            if legitimate {
                                Vote::Legitimate
                            } else {
                                Vote::Illegitimate
                            },
                            &params,
                            Timestamp::new(101)
                        )
                        .unwrap()
                        .is_none());
                    let bytes = orchestrator.snapshot().encode().unwrap();
                    orchestrator = VerificationOrchestrator::restore(
                        OrchestratorSnapshot::decode(&bytes).unwrap(),
                    );
                    assert_eq!(
                        orchestrator.active_appeal(&address(1)),
                        Some(&context(previous))
                    );
                }
                assert!(orchestrator
                    .collect_challenge_outcome(&address(1), &params)
                    .unwrap()
                    .is_none());
                assert!(orchestrator.has_active_challenge(&address(1)));
                assert!(orchestrator.drain_events().is_empty());
                // Candidate resolution is not permission to install it in a ledger.
                let VerificationEvent::ChallengeResolved { outcome, .. } = orchestrator
                    .resolve_challenge(&address(1), &params)
                    .unwrap()
                else {
                    panic!("missing verdict")
                };
                assert_eq!(outcome.appeal, Some(context(previous)));
                assert_eq!(outcome.challenge_context, Some(BlockHash::new([8; 32])));
                assert_eq!(
                    outcome.outcome,
                    if legitimate {
                        ChallengeResult::ChallengeRejected
                    } else {
                        ChallengeResult::FraudConfirmed
                    }
                );
                assert!(outcome
                    .verifier_outcomes
                    .iter()
                    .all(|voter| voter.voted_correctly && voter.penalty == 0));
                assert!(orchestrator.active_appeal(&address(1)).is_none());
                // Ordinary challenge side effects must not bypass appeal settlement.
                assert!(orchestrator
                    .drain_events()
                    .iter()
                    .all(|event| matches!(event, VerificationEvent::ChallengeResolved { .. })));
            }
        }
    }
}

#[test]
fn local_expiry_cannot_restore_a_revoked_appeal_target_or_release_stakes() {
    for reason in [
        crate::ChallengeReason::Fraud,
        crate::ChallengeReason::Inactivity,
    ] {
        let (mut orchestrator, params) = fixture(reason, false);
        admit(&mut orchestrator, &params, reason, false).unwrap();
        assert!(!orchestrator.has_expired_challenges(Timestamp::new(1000), 60));
        assert!(orchestrator
            .cleanup_expired_challenges(Timestamp::new(1000), 60)
            .unwrap()
            .is_empty());
        assert_eq!(
            orchestrator.get_state(&address(1)).unwrap().phase,
            VerificationPhase::Challenged
        );
        assert_eq!(
            orchestrator.active_challenges[&address(1)].stake_amount,
            500
        );
        assert_eq!(
            orchestrator.active_appeal(&address(1)),
            Some(&context(false))
        );
    }
}

#[test]
fn appeal_admission_rejects_invalid_context_and_concurrent_attempts_without_mutation() {
    let reason = crate::ChallengeReason::Fraud;
    for (verified, stake, reference, admission, prior) in [
        (
            false,
            500,
            BlockHash::new([7; 32]),
            BlockHash::new([8; 32]),
            false,
        ),
        (
            true,
            499,
            BlockHash::new([7; 32]),
            BlockHash::new([8; 32]),
            false,
        ),
        (true, 500, BlockHash::ZERO, BlockHash::new([8; 32]), false),
        (true, 500, BlockHash::new([7; 32]), BlockHash::ZERO, false),
        (
            true,
            500,
            BlockHash::new([7; 32]),
            BlockHash::new([7; 32]),
            false,
        ),
        (
            true,
            500,
            BlockHash::new([7; 32]),
            BlockHash::new([8; 32]),
            true,
        ),
    ] {
        let (mut orchestrator, params) = fixture(reason, false);
        let before = orchestrator.snapshot().encode().unwrap();
        assert!(orchestrator
            .initiate_signed_appeal_at(
                &address(1),
                &address(2),
                verified,
                stake,
                (
                    reason,
                    Timestamp::new(100),
                    admission,
                    AppealContext {
                        reviewed_outcome: reference,
                        previously_legitimate: prior
                    }
                ),
                &params
            )
            .is_err());
        assert_eq!(orchestrator.snapshot().encode().unwrap(), before);
    }
    let (mut orchestrator, params) = fixture(reason, false);
    admit(&mut orchestrator, &params, reason, false).unwrap();
    let before = orchestrator.snapshot().encode().unwrap();
    assert!(admit(&mut orchestrator, &params, reason, false).is_err());
    assert!(orchestrator
        .initiate_signed_challenge_at(
            &address(1),
            &address(3),
            true,
            500,
            (reason, Timestamp::new(101), BlockHash::new([9; 32])),
            &params
        )
        .is_err());
    assert_eq!(orchestrator.snapshot().encode().unwrap(), before);
}

#[test]
fn appeal_snapshot_rejects_missing_challenge_or_zero_reference_and_reads_v5() {
    let reason = crate::ChallengeReason::Fraud;
    let (mut orchestrator, params) = fixture(reason, true);
    orchestrator
        .initiate_signed_challenge_at(
            &address(1),
            &address(2),
            true,
            500,
            (reason, Timestamp::new(100), BlockHash::new([8; 32])),
            &params,
        )
        .unwrap();
    let original = orchestrator.snapshot();
    let mut bytes = b"BURST-VERIFICATION\x05".to_vec();
    bytes.extend(
        bincode::serialize(&(
            &original.states,
            &original.active_challenges,
            &original.penalized_verifiers,
            &crate::voting::LegacyNeitherVoteTracker::empty(),
            &original.prior_votes,
            &original.selections,
            &original.attempts,
        ))
        .unwrap(),
    );
    let decoded = OrchestratorSnapshot::decode(&bytes).unwrap();
    assert!(decoded.active_appeals.is_empty());
    assert_eq!(
        decoded.attempts.challenge_context(&address(1)),
        Some(BlockHash::new([8; 32]))
    );
    assert_eq!(decoded.active_challenges[&address(1)].stake_amount, 500);
    for missing in [false, true] {
        let mut snapshot = original.clone();
        snapshot.active_appeals.insert(address(1), context(true));
        if missing {
            snapshot.active_challenges.clear();
        } else {
            snapshot
                .active_appeals
                .get_mut(&address(1))
                .unwrap()
                .reviewed_outcome = BlockHash::ZERO;
        }
        assert!(OrchestratorSnapshot::decode(&snapshot.encode().unwrap()).is_err());
    }
}
