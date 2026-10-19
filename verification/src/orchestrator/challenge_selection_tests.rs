use super::*;

const CUTOFF: u64 = 1_595_434_050;

fn address(seed: u8) -> WalletAddress {
    burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[seed; 32]).public)
}

fn endorsed() -> (VerificationOrchestrator, ProtocolParams) {
    let params = ProtocolParams {
        endorsement_threshold: 3,
        num_verifiers: 3,
        ..ProtocolParams::burst_defaults()
    };
    let mut state = VerificationOrchestrator::new();
    for seed in 1..=3 {
        state
            .process_endorsement_at(
                &address(40),
                &address(seed),
                params.endorsement_burn_amount,
                &params,
                Timestamp::new(CUTOFF - 100),
            )
            .unwrap();
    }
    (state, params)
}

fn challenged(context: BlockHash) -> (VerificationOrchestrator, ProtocolParams) {
    let (mut state, params) = endorsed();
    state
        .genesis_verify_at(
            &address(40),
            &address(0),
            1,
            50,
            Timestamp::new(CUTOFF - 90),
        )
        .unwrap();
    state.drain_events();
    state
        .initiate_signed_challenge_at(
            &address(40),
            &address(9),
            true,
            params.challenge_stake_amount,
            (
                crate::ChallengeReason::Fraud,
                Timestamp::new(CUTOFF - 10),
                context,
            ),
            &params,
        )
        .unwrap();
    (state, params)
}

fn request(params: &ProtocolParams, trigger: BlockHash) -> SelectionRequest {
    SelectionRequest::new(
        address(40),
        trigger,
        0,
        CUTOFF,
        params.params_hash(),
        (50..56).map(address).collect(),
        params.num_verifiers,
    )
    .unwrap()
}

fn restarted(state: &VerificationOrchestrator) -> VerificationOrchestrator {
    VerificationOrchestrator::restore(
        OrchestratorSnapshot::decode(&state.snapshot().encode().unwrap()).unwrap(),
    )
}

#[test]
fn challenge_selection_rejects_another_lifecycles_round_zero_without_mutation() {
    let context = BlockHash::new([71; 32]);
    let (state, params) = challenged(context);
    for mut state in [state.clone(), restarted(&state)] {
        let history = state.selection_history_commitment(&address(40)).unwrap();
        let wrong = request(&params, BlockHash::new([72; 32]));
        assert!(state.freeze_replayed_selection(&wrong, &params).is_err());
        assert!(state.selection_request(&address(40)).is_none());
        assert_eq!(
            state.selection_history_commitment(&address(40)).unwrap(),
            history
        );
        assert!(state.drain_events().is_empty());
        let right = request(&params, context);
        assert_eq!(right.trigger(), context);
        state.freeze_replayed_selection(&right, &params).unwrap();
        let mut state = restarted(&state);
        state.freeze_replayed_selection(&right, &params).unwrap();
        assert_eq!(state.selection_request(&address(40)), Some(&right));
        assert!(state.selection_context(&address(40)).is_none());
        assert!(state
            .get_state(&address(40))
            .unwrap()
            .selected_verifiers
            .is_empty());
    }
}

#[test]
fn selection_history_binds_challenge_identity_and_every_economic_input() {
    let context = BlockHash::new([71; 32]);
    let (state, _) = challenged(context);
    let digest = state.selection_history_commitment(&address(40)).unwrap();
    assert_eq!(
        restarted(&state)
            .selection_history_commitment(&address(40))
            .unwrap(),
        digest
    );
    let (normal, _) = endorsed();
    assert_ne!(
        normal.selection_history_commitment(&address(40)).unwrap(),
        digest
    );
    let (other, _) = challenged(BlockHash::new([72; 32]));
    assert_ne!(
        other.selection_history_commitment(&address(40)).unwrap(),
        digest
    );

    // A mismatched live snapshot must not pass comparison even with a copied hash.
    for change in 0..4 {
        let mut other = state.clone();
        let challenge = other.active_challenges.get_mut(&address(40)).unwrap();
        match change {
            0 => challenge.challenger = address(10),
            1 => challenge.stake_amount += 1,
            2 => challenge.initiated_at = Timestamp::new(CUTOFF - 9),
            _ => challenge.reason = crate::ChallengeReason::Inactivity,
        }
        assert_ne!(
            other.selection_history_commitment(&address(40)).unwrap(),
            digest
        );
    }
}

#[test]
fn unsigned_legacy_challenge_cannot_freeze_a_certified_selection() {
    let context = BlockHash::new([71; 32]);
    let (mut state, params) = challenged(context);
    state.attempts.clear(&address(40));
    for mut state in [state.clone(), restarted(&state)] {
        assert!(state.selection_history_commitment(&address(40)).is_err());
        assert!(state
            .freeze_replayed_selection(&request(&params, context), &params)
            .is_err());
        assert!(state.selection_request(&address(40)).is_none());
    }
}

#[test]
fn a_later_challenge_cannot_reuse_the_previous_challenges_first_panel() {
    let first = BlockHash::new([71; 32]);
    let second = BlockHash::new([72; 32]);
    let (mut state, params) = challenged(first);
    let previous = request(&params, first);
    state.freeze_replayed_selection(&previous, &params).unwrap();
    let prior_history = state.selection_history_commitment(&address(40)).unwrap();
    // This component test advances a supplied deadline, not a local node clock.
    state
        .cleanup_expired_challenges(Timestamp::new(CUTOFF - 1), 9)
        .unwrap();
    state
        .initiate_signed_challenge_at(
            &address(40),
            &address(9),
            true,
            params.challenge_stake_amount,
            (
                crate::ChallengeReason::Fraud,
                Timestamp::new(CUTOFF),
                second,
            ),
            &params,
        )
        .unwrap();
    let mut state = restarted(&state);
    assert_eq!(state.get_state(&address(40)).unwrap().revote_count, 0);
    assert_ne!(
        state.selection_history_commitment(&address(40)).unwrap(),
        prior_history
    );
    assert!(state.freeze_replayed_selection(&previous, &params).is_err());
    state
        .freeze_replayed_selection(&request(&params, second), &params)
        .unwrap();
}
