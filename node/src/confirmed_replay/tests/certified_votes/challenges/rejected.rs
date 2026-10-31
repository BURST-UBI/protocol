use super::*;

fn at(
    fixture: &Fixture,
    records: &[CertifiedCheckpoint],
    cutoff: u64,
) -> Result<ConfirmedReplay, ReplayError> {
    ConfirmedReplay::replay_with_certificates(
        &fixture.history(cutoff),
        NetworkId::Dev,
        &fixture.params,
        records,
    )
}

#[test]
fn rejection_preserves_identity_and_forfeits_only_the_challengers_locked_stake() {
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        let (mut fixture, first, admission) = pending_reason(reason);
        let (_dir, _store, second) = certify_challenge(&fixture, &first);
        let selected = panel(&second);
        let records = [first, second];
        let before = replay(&fixture, &records).unwrap();
        let mut sources = vec![admission];
        for seed in &selected {
            sources.push(vote(&mut fixture, &records[1], *seed, CUTOFF + 60));
        }
        let outcome = *sources[1..].iter().max().unwrap();
        sources.sort_unstable();
        let resolved = replay(&fixture, &records).unwrap();
        assert!(!resolved.verification.has_active_challenge(&address(20)));
        assert_eq!(
            bincode::serialize(&resolved.account(&address(20))).unwrap(),
            bincode::serialize(&before.account(&address(20))).unwrap()
        );
        let challenger = resolved.account(&address(8)).unwrap();
        assert_eq!(challenger.total_brn_staked, 0);
        assert_eq!(
            challenger.total_brn_burned,
            before.account(&address(8)).unwrap().total_brn_burned
                + fixture.params.challenge_stake_amount
        );
        assert_eq!(
            resolved.pending.len(),
            before.pending.len(),
            "unanimous votes have no dissent-funded reward"
        );
        for voter in &selected {
            assert_eq!(
                resolved.account(&address(*voter)).unwrap().total_brn_staked,
                0
            );
            let release = &resolved
                .brn
                .get_wallet(&address(*voter))
                .unwrap()
                .pending_releases[&outcome];
            assert_eq!(release.sources, sources);
            assert_eq!(release.amount, fixture.params.verifier_stake_amount);
        }
        let restored = VerificationOrchestrator::restore(
            OrchestratorSnapshot::decode(&resolved.verification.snapshot().encode().unwrap())
                .unwrap(),
        );
        assert!(!restored.has_active_challenge(&address(20)));
        assert_eq!(
            restored.get_state(&address(20)).unwrap().votes.len(),
            selected.len()
        );

        let voter = selected[0];
        let mut incomplete = Fixture {
            params: fixture.params.clone(),
            blocks: fixture.blocks.clone(),
        };
        incomplete.push(voter, BlockType::Burn, CUTOFF + 60, |block| {
            block.link = destination(20);
            block.brn_spent += 1;
            block.release_sources = sources
                .iter()
                .copied()
                .filter(|hash| *hash != admission)
                .collect();
        });
        assert!(
            replay(&incomplete, &records).is_err(),
            "a release must include the signed challenge admission"
        );
        fixture.push(voter, BlockType::Burn, CUTOFF + 60, |block| {
            block.link = destination(20);
            block.brn_spent += 1;
            block.release_sources = sources;
        });
        let spent = replay(&fixture, &records).unwrap();
        assert!(!spent
            .brn
            .get_wallet(&address(voter))
            .unwrap()
            .pending_releases
            .contains_key(&outcome));
        assert!(spent.account(&address(20)).unwrap().state.is_verified());
    }
}

#[test]
fn rejection_distributes_only_the_dissenting_verifiers_burned_stake() {
    let (mut fixture, first, _) = pending_with(ChallengeReason::Fraud, |params| {
        params.num_verifiers = 3;
        params.verification_threshold_bps = 6000;
    });
    let (_dir, _store, second) = certify_challenge(&fixture, &first);
    let selected = panel(&second);
    let records = [first, second];
    let before = replay(&fixture, &records).unwrap();
    let mut sources = Vec::new();
    for (index, seed) in selected.iter().enumerate() {
        sources.push(vote_choice(
            &mut fixture,
            &records[1],
            *seed,
            CUTOFF + 60,
            if index == 2 { 2 } else { 1 },
        ));
    }
    let outcome = *sources.iter().max().unwrap();
    let resolved = replay(&fixture, &records).unwrap();
    let share = fixture.params.verifier_stake_amount / 2;
    for voter in &selected[..2] {
        let reward = &resolved.pending[&(address(*voter), outcome.into_tx_hash())];
        assert_eq!(reward.amount, share);
        fixture.push(*voter, BlockType::Receive, CUTOFF + 60, |block| {
            block.link = outcome;
            block.trst_balance += share;
        });
    }
    let dissent = selected[2];
    assert_eq!(
        resolved
            .account(&address(dissent))
            .unwrap()
            .total_brn_burned,
        before.account(&address(dissent)).unwrap().total_brn_burned
            + fixture.params.verifier_stake_amount
            + if dissent == 8 {
                fixture.params.challenge_stake_amount
            } else {
                0
            }
    );
    let claimed = replay(&fixture, &records).unwrap();
    assert!(!claimed
        .pending
        .keys()
        .any(|(_, hash)| *hash == outcome.into_tx_hash()));
    assert!(claimed.account(&address(20)).unwrap().state.is_verified());
}

#[test]
fn unactivated_challenge_has_no_receipt_or_admission_clock_deadline() {
    for late in [false, true] {
        let (mut fixture, first, _) = pending_with(ChallengeReason::Fraud, |params| {
            params.challenge_duration_secs = 100
        });
        let (_dir, _store, second) = certify_challenge(&fixture, &first);
        let deadline = CUTOFF + 34 + fixture.params.challenge_duration_secs;
        let selected = panel(&second);
        vote(&mut fixture, &second, selected[0], deadline - 1);
        vote(
            &mut fixture,
            &second,
            selected[1],
            deadline - u64::from(!late),
        );
        let result = at(&fixture, &[first, second], deadline);
        assert!(result
            .unwrap()
            .account(&address(20))
            .unwrap()
            .state
            .is_verified());
    }
}

#[test]
fn a_later_challenge_cannot_infer_its_order_from_packet_or_timestamp_order() {
    let (mut fixture, first, _) = pending();
    let (_dir, _store, second) = certify_challenge(&fixture, &first);
    for seed in panel(&second) {
        vote(&mut fixture, &second, seed, CUTOFF + 60);
    }
    challenge(&mut fixture, 20, ChallengeReason::Inactivity, CUTOFF + 60);
    assert!(replay(&fixture, &[first, second])
        .err()
        .unwrap()
        .to_string()
        .contains("repeated challenge admissions require agreed lifecycle ordering"));
}
