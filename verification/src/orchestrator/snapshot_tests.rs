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
    current.votes = votes.clone().into();
    current.endorsements = votes
        .iter()
        .map(|vote| crate::state::Endorsement {
            endorser: vote.verifier.clone(),
            burn_amount: 1,
            timestamp: Timestamp::new(90),
        })
        .collect();
    current.selected_verifiers = panel.clone().into();
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
        *restored.get_state(&target).unwrap().selected_verifiers,
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
    for selection in snapshot.selections.values() {
        let expected = selection
            .vote_context()
            .expect("installed certified selection");
        assert_eq!(
            bincode::serialize(selection).unwrap(),
            bincode::serialize(&(&selection.request, &selection.certified)).unwrap(),
            "context cache must not change the legacy round wire shape"
        );
        let mut changed = selection.clone();
        let pool = changed.replace_storage_pool(Vec::new());
        assert_eq!(
            changed.vote_context(),
            None,
            "pool removal must invalidate warm identity"
        );
        let encoded = bincode::serialize(&changed).unwrap();
        let restored: super::SelectionRound = bincode::deserialize(&encoded).unwrap();
        assert_eq!(
            restored.vote_context(),
            None,
            "decoded malformed pool must not inherit authority"
        );
        changed.replace_storage_pool(pool);
        assert_eq!(changed.vote_context(), Some(expected));
        assert_eq!(
            selection.vote_context(),
            Some(expected),
            "baseline remains isolated"
        );
    }
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

#[test]
fn candidate_roots_are_shared_and_mutations_preserve_snapshot_and_attempt_isolation() {
    let address =
        |seed| burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[seed; 32]).public);
    let mut original = VerificationOrchestrator::new();
    for seed in 1..=32 {
        let target = address(seed);
        original.initialize_genesis(&target);
        original
            .attempts
            .bind_vote(&target, &target, BlockHash::new([seed; 32]))
            .unwrap();
        original
            .attempts
            .bind_challenge(&target, BlockHash::new([seed; 32]));
        original.neither_tracker.record_vote(&target, Vote::Neither);
        original.penalized_verifiers.insert(target, 100);
    }
    let bytes = original.snapshot().encode().unwrap();
    let mut candidate = original.clone();
    assert!(original.states.ptr_eq(&candidate.states));
    assert!(original
        .active_challenges
        .ptr_eq(&candidate.active_challenges));
    assert!(original.active_appeals.ptr_eq(&candidate.active_appeals));
    assert!(original.prior_votes.ptr_eq(&candidate.prior_votes));
    assert!(original.selections.ptr_eq(&candidate.selections));
    assert!(original
        .penalized_verifiers
        .ptr_eq(&candidate.penalized_verifiers));
    assert!(original.attempts.votes.ptr_eq(&candidate.attempts.votes));
    assert!(original
        .attempts
        .challenges
        .ptr_eq(&candidate.attempts.challenges));
    assert!(original
        .neither_tracker
        .history
        .ptr_eq(&candidate.neither_tracker.history));
    let target = address(1);
    candidate.states.get_mut(&target).unwrap().started_at = Timestamp::new(99);
    candidate.attempts.clear(&target);
    candidate
        .neither_tracker
        .record_vote(&target, Vote::Legitimate);
    candidate.penalized_verifiers.remove(&target);
    assert_eq!(original.snapshot().encode().unwrap(), bytes);
    assert_ne!(candidate.snapshot().encode().unwrap(), bytes);
    assert_eq!(
        original.attempts.vote_context(&target, &target),
        Some(BlockHash::new([1; 32]))
    );
    assert_eq!(candidate.attempts.vote_context(&target, &target), None);
    assert_eq!(original.neither_tracker.neither_ratio_bps(&target), 10_000);
    assert_eq!(candidate.neither_tracker.neither_ratio_bps(&target), 5_000);
    super::records::assert_record_roundtrip(&original);
    super::records::assert_record_roundtrip(&candidate);
    let restored = VerificationOrchestrator::restore(OrchestratorSnapshot::decode(&bytes).unwrap());
    assert_eq!(restored.snapshot().encode().unwrap(), bytes);
    // Persistent containers retain the existing serialized map/struct shapes.
    let (history, threshold, window): (
        std::collections::BTreeMap<String, crate::voting::VoteWindow>,
        u32,
        u32,
    ) = bincode::deserialize(&bincode::serialize(&original.neither_tracker).unwrap()).unwrap();
    assert_eq!(history.len(), 32);
    assert_eq!(threshold, original.neither_tracker.penalty_threshold_bps);
    assert_eq!(window, original.neither_tracker.window_assignments);
    let (votes, challenges): (
        HashMap<WalletAddress, HashMap<WalletAddress, BlockHash>>,
        HashMap<WalletAddress, BlockHash>,
    ) = bincode::deserialize(&bincode::serialize(&original.attempts).unwrap()).unwrap();
    assert_eq!(votes.len(), 32);
    assert_eq!(challenges.len(), 32);
}
