use super::*;
use crate::checkpoint_outcome::PreparedChallengeOutcome;

mod activation_retry;
mod agreement;
mod appeals;
mod chain;
mod fence;
mod installation;
mod reverification;
mod runtime;
mod transport;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_challenge_archive_preserves_active_panel_and_stakes() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        let (fixture, directory, store, _, _) = super::deadline::setup(reason, 0, 9000);
        crate::confirmed_replay::tests::snapshot_history::installed_recovery::installation::live::bootstrap::assert_lifecycle_archive(fixture, directory, store).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_challenge_archive_preserves_fraud_and_inactivity_outcomes() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        let (mut fixture, selections) = completed(reason);
        merged_payments(&mut fixture);
        let (directory, store, heads) = restore_chain(&fixture, &selections);
        let outcome = PreparedChallengeOutcome::prepare(
            &store,
            NetworkId::Dev,
            &fixture.params,
            CUTOFF + 70,
            &heads,
            &address(20),
        )
        .unwrap();
        let certificate = certify(&outcome.manifest().checkpoint, &[0, 1, 2, 3, 4, 5, 6]);
        let outcome = outcome
            .certify(certificate)
            .unwrap()
            .persist(&store, NetworkId::Dev, &fixture.params)
            .unwrap();
        assert_eq!(
            outcome.state().account(&address(20)).unwrap().state,
            if reason == ChallengeReason::Fraud {
                WalletState::Revoked
            } else {
                WalletState::Deactivated
            }
        );
        crate::confirmed_replay::tests::snapshot_history::installed_recovery::installation::live::bootstrap::assert_lifecycle_archive(fixture, directory, store).await;
    }
}

fn completed(reason: ChallengeReason) -> (Fixture, Vec<CertifiedCheckpoint>) {
    let (mut fixture, first, _) = pending_reason(reason);
    let (_dir, _store, second) = certify_challenge(&fixture, &first);
    for seed in panel(&second) {
        vote_choice(&mut fixture, &second, seed, CUTOFF + 60, 2);
    }
    (fixture, vec![first, second])
}

fn restore_chain(
    fixture: &Fixture,
    chain: &[CertifiedCheckpoint],
) -> (tempfile::TempDir, LmdbStore, Vec<HistoryHead>) {
    let (dir, store, heads) = fixture.store();
    for entry in chain {
        let mut imported = CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            entry.certificate().clone(),
            entry.heads().to_vec(),
            entry.replayed().request().target().clone(),
        )
        .unwrap();
        imported
            .attach_beacon(&store, &fixture.params, entry.beacon().unwrap().clone())
            .unwrap();
    }
    (dir, store, heads)
}

fn merged_payments(fixture: &mut Fixture) {
    let tainted = fixture.push(20, BlockType::Burn, CUTOFF + 61, |block| {
        block.link = destination(8);
        block.brn_balance += 10_000;
    });
    let clean = fixture.push(0, BlockType::Burn, CUTOFF + 61, |block| {
        block.link = destination(8);
        block.brn_balance += 10_000;
    });
    let first = fixture.push(8, BlockType::Receive, CUTOFF + 62, |block| {
        block.link = tainted;
        block.trst_balance += 10_000;
    });
    let second = fixture.push(8, BlockType::Receive, CUTOFF + 63, |block| {
        block.link = clean;
        block.trst_balance += 10_000;
    });
    let merged = fixture.push(8, BlockType::Merge, CUTOFF + 64, |block| {
        block.merge_sources = vec![first.into_tx_hash(), second.into_tx_hash()];
    });
    let payment = fixture.push(8, BlockType::Send, CUTOFF + 65, |block| {
        block.origin = merged.into_tx_hash();
        block.link = destination(1);
        block.trst_balance -= 10_000;
    });
    fixture.push(1, BlockType::Receive, CUTOFF + 66, |block| {
        block.link = payment;
        block.trst_balance += 10_000;
    });
}

#[test]
fn outcome_cut_preserves_payments_then_splits_fraud_but_not_inactivity() {
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        let (mut fixture, chain) = completed(reason);
        merged_payments(&mut fixture);
        let (_dir, store, heads) = restore_chain(&fixture, &chain);
        let candidate = PreparedChallengeOutcome::prepare(
            &store,
            NetworkId::Dev,
            &fixture.params,
            CUTOFF + 70,
            &heads,
            &address(20),
        )
        .unwrap();
        let manifest = candidate.manifest().clone();
        let state = candidate.state();
        assert_eq!(
            state.account(&address(20)).unwrap().state,
            if reason == ChallengeReason::Fraud {
                WalletState::Revoked
            } else {
                WalletState::Deactivated
            }
        );
        assert_eq!(
            state.account(&address(20)).unwrap().total_brn_burned,
            10_001
        );
        assert_eq!(state.account(&address(8)).unwrap().total_brn_staked, 0);
        for holder in [1, 8] {
            let revoked: u128 = state
                .portfolio(&address(holder))
                .unwrap()
                .tokens
                .iter()
                .filter(|token| token.state == burst_types::TrstState::Revoked)
                .map(|token| token.amount)
                .sum();
            assert_eq!(
                revoked,
                if reason == ChallengeReason::Fraud {
                    5_000
                } else {
                    0
                }
            );
        }
        let source = fixture
            .blocks
            .iter()
            .filter(|block| {
                block.block_type == BlockType::VerificationVote
                    && block.timestamp == Timestamp::new(CUTOFF + 60)
            })
            .max_by_key(|block| *block.hash.as_bytes())
            .unwrap()
            .hash;
        let expected_reward = (10_001 * fixture.params.challenge_reward_bps as u128 / 10_000)
            .min(fixture.params.challenge_reward_cap);
        if reason == ChallengeReason::Fraud && expected_reward > 0 {
            let reward = state.pending(&address(8), source.into_tx_hash()).unwrap();
            assert_eq!(reward.amount, expected_reward);
            assert_eq!(reward.timestamp, Timestamp::new(CUTOFF + 70));
        } else {
            assert!(state.pending(&address(8), source.into_tx_hash()).is_none());
        }
        let release = &state.brn.get_wallet(&address(8)).unwrap().pending_releases[&source];
        assert_eq!(release.completed_at, Timestamp::new(CUTOFF + 70));
        assert!(release.sources.contains(&source));
        assert_eq!(
            state
                .brn
                .get_wallet(&address(20))
                .unwrap()
                .accrual_stopped_at,
            Some(Timestamp::new(CUTOFF + 70))
        );

        // The challenged human is still part of the *previous* signing roster.
        let certificate = certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 20]);
        let certified = candidate.certify(certificate).unwrap();
        assert_eq!(certified.next_roster().weight(&address(20)), 0);
        assert_eq!(certified.next_roster().total_humans(), 9);
        assert_eq!(certified.certificate().checkpoint, manifest.checkpoint);

        let replayed =
            PreparedChallengeOutcome::replay_manifest(&store, &fixture.params, &manifest).unwrap();
        for seed in [1, 8, 20] {
            assert_eq!(
                bincode::serialize(&replayed.state().account(&address(seed))).unwrap(),
                bincode::serialize(&certified.state().account(&address(seed))).unwrap()
            );
        }
        let still_pending = ConfirmedReplay::replay_with_certificates(
            &fixture.history(CUTOFF + 70),
            NetworkId::Dev,
            &fixture.params,
            &chain,
        )
        .unwrap();
        assert!(
            still_pending
                .account(&address(20))
                .unwrap()
                .state
                .is_verified(),
            "preparing/certifying a candidate must not install it"
        );
    }
}

#[test]
fn outcome_certificates_require_old_quorum_exact_cut_and_outcome_domain() {
    let (fixture, chain) = completed(ChallengeReason::Fraud);
    let (_dir, store, heads) = restore_chain(&fixture, &chain);
    let prepare = || {
        PreparedChallengeOutcome::prepare(
            &store,
            NetworkId::Dev,
            &fixture.params,
            CUTOFF + 60,
            &heads,
            &address(20),
        )
        .unwrap()
    };
    let candidate = prepare();
    let manifest = candidate.manifest().clone();
    assert!(prepare()
        .certify(certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5]))
        .is_err());
    assert!(prepare().certify(chain[1].certificate().clone()).is_err());
    assert!(PreparedChallengeOutcome::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 60,
        &heads,
        &address(8)
    )
    .is_err());
    for mutation in 0..5 {
        let mut wrong = manifest.clone();
        match mutation {
            0 => wrong.checkpoint.cutoff += 1,
            1 => wrong.checkpoint.selection_root = chain[1].certificate().checkpoint.selection_root,
            2 => wrong.checkpoint.next_roster = candidate.parent().roster().commitment(),
            3 => wrong.target = address(8),
            _ => wrong.checkpoint.parent = BlockHash::ZERO,
        }
        assert!(
            PreparedChallengeOutcome::replay_manifest(&store, &fixture.params, &wrong).is_err()
        );
        if wrong.checkpoint != manifest.checkpoint {
            assert!(prepare()
                .certify(certify(&wrong.checkpoint, &[0, 1, 2, 3, 4, 5, 6]))
                .is_err());
        }
    }
    // Selection import must not reinterpret an outcome root as a drand request.
    assert!(CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 6]),
        heads,
        address(20)
    )
    .is_err());
}

#[test]
fn incomplete_and_rejected_panels_cannot_prepare_upheld_outcomes() {
    for choice in [1, 2, 3] {
        let (mut fixture, first, _) = pending();
        let (_dir, _store, second) = certify_challenge(&fixture, &first);
        let selected = panel(&second);
        vote_choice(&mut fixture, &second, selected[0], CUTOFF + 60, choice);
        let chain = vec![first, second];
        let build = |fixture: &Fixture| {
            PreparedChallengeOutcome::build(
                &fixture.history(CUTOFF + 60),
                &fixture.params,
                &chain,
                &address(20),
            )
        };
        assert!(build(&fixture).is_err());
        if choice != 2 {
            vote_choice(&mut fixture, &chain[1], selected[1], CUTOFF + 60, choice);
            assert!(build(&fixture).is_err());
        }
    }
}

#[test]
fn reordered_history_has_one_outcome_but_a_payment_tail_requires_a_new_certificate() {
    let (mut fixture, chain) = completed(ChallengeReason::Fraud);
    let before = PreparedChallengeOutcome::build(
        &fixture.history(CUTOFF + 60),
        &fixture.params,
        &chain,
        &address(20),
    )
    .unwrap();
    let earlier_certificate = certify(&before.manifest().checkpoint, &[0, 1, 2, 3, 4, 5, 6]);
    merged_payments(&mut fixture);
    let history = fixture.history(CUTOFF + 70);
    let expected =
        PreparedChallengeOutcome::build(&history, &fixture.params, &chain, &address(20)).unwrap();
    assert_ne!(
        before.manifest().checkpoint.ledger_root,
        expected.manifest().checkpoint.ledger_root
    );
    assert_ne!(
        before.manifest().checkpoint.hash(),
        expected.manifest().checkpoint.hash()
    );
    for reverse in [false, true] {
        let mut blocks = fixture.blocks.clone();
        if reverse {
            blocks.reverse();
        } else {
            blocks.rotate_left(17);
        }
        let reordered = ConfirmedHistory::from_blocks(
            NetworkId::Dev,
            fixture.blocks[0].hash,
            CUTOFF + 70,
            history.heads(),
            chain.last().unwrap().heads(),
            blocks,
        )
        .unwrap();
        let candidate =
            PreparedChallengeOutcome::build(&reordered, &fixture.params, &chain, &address(20))
                .unwrap();
        assert_eq!(candidate.manifest(), expected.manifest());
        for seed in [1, 8, 20] {
            assert_eq!(
                bincode::serialize(&candidate.state().account(&address(seed))).unwrap(),
                bincode::serialize(&expected.state().account(&address(seed))).unwrap()
            );
            assert_eq!(
                bincode::serialize(&candidate.state().portfolio(&address(seed))).unwrap(),
                bincode::serialize(&expected.state().portfolio(&address(seed))).unwrap()
            );
        }
        assert!(candidate.certify(earlier_certificate.clone()).is_err());
    }
}
