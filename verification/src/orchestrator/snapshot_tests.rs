use super::*;

#[test]
fn snapshot_evidence_is_order_independent_but_panel_ranking_is_preserved() {
    let address =
        |seed| burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[seed; 32]).public);
    let target = address(9);
    let mut state = VerificationOrchestrator::new();
    state.initialize_genesis(&target);
    let votes: Vec<_> = (0..3)
        .map(|index| crate::state::VerifierVote {
            verifier: address(index),
            vote: if index == 0 {
                Vote::Legitimate
            } else {
                Vote::Illegitimate
            },
            stake_amount: 10,
            timestamp: Timestamp::new(100),
        })
        .collect();
    let panel = votes
        .iter()
        .rev()
        .map(|vote| vote.verifier.clone())
        .collect::<Vec<_>>();
    let current = state.states.get_mut(&target).unwrap();
    current.votes = votes.clone();
    current.endorsements = votes
        .iter()
        .map(|vote| crate::state::Endorsement {
            endorser: vote.verifier.clone(),
            burn_amount: 1,
            timestamp: Timestamp::new(90),
        })
        .collect();
    current.selected_verifiers = panel.clone();
    state.prior_votes.insert(target.clone(), votes);
    let original = state.snapshot().encode().unwrap();
    super::records::assert_record_roundtrip(&state);
    let current = state.states.get_mut(&target).unwrap();
    current.votes.reverse();
    current.endorsements.reverse();
    state.prior_votes.get_mut(&target).unwrap().reverse();
    assert_eq!(state.snapshot().encode().unwrap(), original);
    let restored =
        VerificationOrchestrator::restore(OrchestratorSnapshot::decode(&original).unwrap());
    assert_eq!(
        restored.get_state(&target).unwrap().selected_verifiers,
        panel
    );
    state.states.get_mut(&target).unwrap().votes[0].stake_amount += 1;
    assert_ne!(state.snapshot().encode().unwrap(), original);
    state.states.get_mut(&target).unwrap().votes[0].stake_amount -= 1;
    state
        .states
        .get_mut(&target)
        .unwrap()
        .selected_verifiers
        .reverse();
    assert_ne!(state.snapshot().encode().unwrap(), original);
}

pub(crate) fn assert_legacy_selection_migration(
    state: &VerificationOrchestrator,
    certificate: &CheckpointCertificate,
) {
    super::records::assert_record_roundtrip(state);
    let snapshot = state.snapshot();
    let selections: HashMap<_, _> = snapshot
        .selections
        .iter()
        .map(|(wallet, selection)| {
            let certified = selection.certified.as_ref().map(|(checkpoint, beacon)| {
                assert_eq!(checkpoint, &certificate.checkpoint);
                (certificate.clone(), beacon.clone())
            });
            (wallet.clone(), (selection.request.clone(), certified))
        })
        .collect();
    let legacy_neither = crate::voting::LegacyNeitherVoteTracker::empty();
    for version in 4..=7 {
        let mut bytes = b"BURST-VERIFICATION".to_vec();
        bytes.push(version);
        let prefix = (
            &snapshot.states,
            &snapshot.active_challenges,
            &snapshot.penalized_verifiers,
        );
        let payload = match version {
            4 => bincode::serialize(&(prefix, &legacy_neither, &snapshot.prior_votes, &selections)),
            5 => bincode::serialize(&(
                prefix,
                &legacy_neither,
                &snapshot.prior_votes,
                &selections,
                &snapshot.attempts,
            )),
            6 => bincode::serialize(&(
                prefix,
                &legacy_neither,
                &snapshot.prior_votes,
                &selections,
                &snapshot.attempts,
                &snapshot.active_appeals,
            )),
            _ => bincode::serialize(&(
                prefix,
                &snapshot.neither_tracker,
                &snapshot.prior_votes,
                &selections,
                &snapshot.attempts,
                &snapshot.active_appeals,
            )),
        }
        .unwrap();
        bytes.extend(payload);
        let restored =
            VerificationOrchestrator::restore(OrchestratorSnapshot::decode(&bytes).unwrap());
        for wallet in selections.keys() {
            assert_eq!(
                restored.selection_request(wallet),
                state.selection_request(wallet)
            );
            assert_eq!(
                restored.selection_context(wallet),
                state.selection_context(wallet)
            );
            assert_eq!(
                restored.get_state(wallet).unwrap().selected_verifiers,
                state.get_state(wallet).unwrap().selected_verifiers
            );
        }
        if version == 7 {
            assert_eq!(
                restored.snapshot().encode().unwrap(),
                snapshot.encode().unwrap()
            );
        }
        bytes.push(0);
        assert!(OrchestratorSnapshot::decode(&bytes).is_err());
    }
}
