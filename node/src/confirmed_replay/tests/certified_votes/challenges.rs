use super::*;
use crate::checkpoint_replay::SelectionLifecycle;
use burst_verification::{bootstrap::BootstrapPhase, ChallengeReason, OrchestratorSnapshot};

mod deadline;
mod outcomes;
mod rejected;

fn challenge(fixture: &mut Fixture, target: u8, reason: ChallengeReason, time: u64) -> BlockHash {
    let stake = fixture.params.challenge_stake_amount;
    fixture.push(8, BlockType::Challenge, time, |block| {
        block.link = destination(target);
        block.origin = TxHash::new(reason.to_origin());
        block.transaction = TxHash::ZERO;
        block.brn_spent += stake;
    })
}

fn pending() -> (Fixture, CertifiedCheckpoint, BlockHash) {
    pending_reason(ChallengeReason::Fraud)
}

fn pending_reason(reason: ChallengeReason) -> (Fixture, CertifiedCheckpoint, BlockHash) {
    pending_with(reason, |_| {})
}

fn pending_with(
    reason: ChallengeReason,
    configure: impl FnOnce(&mut ProtocolParams),
) -> (Fixture, CertifiedCheckpoint, BlockHash) {
    let mut fixture = Fixture::selection_with(configure);
    let first = certificate(&fixture, true);
    for (index, seed) in panel(&first).iter().enumerate() {
        vote(&mut fixture, &first, *seed, CUTOFF + 31 + index as u64);
    }
    fixture.push(20, BlockType::Open, CUTOFF + 33, |_| {});
    let source = challenge(&mut fixture, 20, reason, CUTOFF + 34);
    fixture.push(20, BlockType::Burn, CUTOFF + 35, |block| {
        block.link = destination(8);
        block.brn_spent += 1;
    });
    let funding = fixture.push(0, BlockType::Burn, CUTOFF + 35, |block| {
        block.link = destination(20);
        block.brn_spent += 1;
    });
    fixture.push(20, BlockType::Receive, CUTOFF + 35, |block| {
        block.link = funding;
        block.trst_balance += 1;
    });
    fixture.push(20, BlockType::Send, CUTOFF + 35, |block| {
        block.link = destination(8);
        block.origin = funding.into_tx_hash();
        block.trst_balance -= 1;
    });
    (fixture, first, source)
}

fn certify_challenge(
    fixture: &Fixture,
    first: &CertifiedCheckpoint,
) -> (tempfile::TempDir, LmdbStore, CertifiedCheckpoint) {
    let (dir, store, _) = fixture.store();
    let mut restored = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        first.certificate().clone(),
        first.heads().to_vec(),
        address(20),
    )
    .unwrap();
    restored
        .attach_beacon(&store, &fixture.params, first.beacon().unwrap().clone())
        .unwrap();
    let replay =
        CertifiedCheckpoint::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 35)
            .unwrap()
            .expect("pending challenge requires a new lifecycle, not normal round one");
    assert_eq!(replay.request().target(), &address(20));
    assert_eq!(replay.request().attempt(), 0);
    let mut record = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certify(replay.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]),
        replay.manifest().heads,
        address(20),
    )
    .unwrap();
    let beacon: burst_vrf::DrandBeacon = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/revote-beacon.json"
    ))
    .unwrap();
    record
        .attach_beacon(
            &store,
            &fixture.params,
            burst_vrf::DrandClient::beacon_to_random_output(&beacon).unwrap(),
        )
        .unwrap();
    (dir, store, record)
}

#[test]
fn pending_challenge_replays_its_signed_admission_without_revoking_transaction_rights() {
    let (fixture, first, source) = pending();
    let state = replay(&fixture, std::slice::from_ref(&first)).unwrap();
    assert!(state.account(&address(20)).unwrap().state.is_verified());
    assert_eq!(
        state.account(&address(8)).unwrap().total_brn_staked,
        fixture.params.challenge_stake_amount
    );
    assert_eq!(
        state
            .verification
            .active_challenge_context(&address(20))
            .unwrap(),
        Some(source)
    );
    assert_eq!(
        state.selection_lifecycle(&address(20)).unwrap(),
        SelectionLifecycle::Challenge(source)
    );
    assert!(state.pending.contains_key(&(
        address(8),
        fixture.blocks.last().unwrap().hash.into_tx_hash()
    )));
    let (_dir, store, second) = certify_challenge(&fixture, &first);
    assert_eq!(second.replayed().request().trigger(), source);
    assert_ne!(first.replayed().lifecycle(), second.replayed().lifecycle());
    let chain = CertifiedCheckpoint::load_chain(&store, NetworkId::Dev, &fixture.params).unwrap();
    assert_eq!(chain.len(), 2);
    let again = replay(&fixture, &chain).unwrap();
    for seed in [20, 8] {
        assert_eq!(
            bincode::serialize(&again.account(&address(seed))).unwrap(),
            bincode::serialize(&state.account(&address(seed))).unwrap()
        );
    }
    assert!(CertifiedCheckpoint::prepare_next(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 60
    )
    .unwrap()
    .is_none());
}

#[test]
fn original_genesis_wallet_can_be_challenged_and_needs_no_endorsement_history() {
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        let mut fixture = Fixture::selection();
        let source = challenge(&mut fixture, 0, reason, CUTOFF - 1);
        let mut replay = fixture.replay(CUTOFF).unwrap();
        assert!(replay.account(&address(0)).unwrap().state.is_verified());
        assert_eq!(
            replay
                .verification
                .active_challenge_context(&address(0))
                .unwrap(),
            Some(source)
        );
        let request = replay.prepare_selection(&address(0)).unwrap();
        assert_eq!(request.trigger(), source);
        let (_dir, store, heads) = fixture.store();
        let prepared = CertifiedCheckpoint::prepare(
            &store,
            NetworkId::Dev,
            &fixture.params,
            CUTOFF,
            &heads,
            &address(0),
        )
        .unwrap();
        let accepted = CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certify(prepared.checkpoint(), &[0]),
            heads,
            address(0),
        )
        .unwrap();
        assert_eq!(
            accepted.replayed().lifecycle(),
            SelectionLifecycle::Challenge(source)
        );
        let restored =
            CertifiedCheckpoint::load_chain(&store, NetworkId::Dev, &fixture.params).unwrap();
        assert_eq!(restored[0].replayed().request(), &request);
    }
}

#[test]
fn pending_challenge_rejects_duplicate_admission_and_unauthenticated_payloads() {
    let (fixture, first, _) = pending();
    for invalid_case in ["duplicate", "signature", "stake"] {
        let mut changed = Fixture {
            params: fixture.params.clone(),
            blocks: fixture.blocks.clone(),
        };
        if invalid_case == "duplicate" {
            challenge(&mut changed, 20, ChallengeReason::Inactivity, CUTOFF + 36);
        } else {
            let block = changed
                .blocks
                .iter_mut()
                .find(|block| block.block_type == BlockType::Challenge)
                .unwrap();
            if invalid_case == "signature" {
                block.signature.0[0] ^= 1;
            } else {
                block.brn_spent -= changed.params.challenge_stake_amount - 1;
                sign(8, block);
            }
        }
        assert!(
            replay(&changed, std::slice::from_ref(&first)).is_err(),
            "{invalid_case}"
        );
    }
}

#[test]
fn challenge_admission_requires_completed_target_verification_but_does_not_start_a_voting_window() {
    let mut fixture = Fixture::selection();
    let first = certificate(&fixture, true);
    vote(&mut fixture, &first, panel(&first)[0], CUTOFF + 31);
    challenge(&mut fixture, 20, ChallengeReason::Fraud, CUTOFF + 34);
    let error = replay(&fixture, &[first])
        .err()
        .expect("partial panel cannot authorize a challenge");
    assert!(
        error
            .to_string()
            .contains("challenge target requires its complete verification panel"),
        "{error}"
    );

    let mut fixture = Fixture::selection();
    challenge(&mut fixture, 0, ChallengeReason::Fraud, CUTOFF - 1);
    let deadline = CUTOFF - 1 + fixture.params.challenge_duration_secs;
    assert!(fixture.replay(deadline - 1).is_ok());
    let state = fixture.replay(deadline).unwrap();
    assert!(state.verification.has_active_challenge(&address(0)));
    assert!(state.account(&address(0)).unwrap().state.is_verified());
    assert_eq!(
        state.account(&address(8)).unwrap().total_brn_staked,
        fixture.params.challenge_stake_amount
    );
}

#[test]
fn a_certified_pending_challenge_does_not_enable_unordered_outcome_replay() {
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        for choice in [2, 3] {
            let (mut fixture, first, _) = pending_reason(reason);
            let (_dir, _store, second) = certify_challenge(&fixture, &first);
            for seed in panel(&second) {
                vote_choice(&mut fixture, &second, seed, CUTOFF + 60, choice);
            }
            let state = replay(&fixture, &[first, second]).unwrap();
            assert!(state.account(&address(20)).unwrap().state.is_verified());
            assert!(state.verification.has_active_challenge(&address(20)));
            assert_eq!(
                state.account(&address(8)).unwrap().total_brn_staked,
                fixture.params.challenge_stake_amount
            );
            if choice == 2 {
                assert_eq!(
                    state
                        .verification
                        .pending_challenge_verdict(&address(20), &fixture.params),
                    Some(burst_verification::ChallengeResult::FraudConfirmed)
                );
                assert_eq!(state.verification(&address(20)).unwrap().votes.len(), 2);
            } else {
                assert_eq!(state.verification(&address(20)).unwrap().revote_count, 1);
                assert!(state
                    .verification(&address(20))
                    .unwrap()
                    .selected_verifiers
                    .is_empty());
            }
        }
    }
    const { assert!(!burst_verification::CHECKPOINT_SELECTION_ENABLED) };
}

#[test]
fn collected_upheld_verdict_preserves_rights_stakes_and_survives_its_vote_deadline() {
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        let (mut fixture, first, _) =
            pending_with(reason, |params| params.challenge_duration_secs = 100);
        let (_dir, _store, second) = certify_challenge(&fixture, &first);
        let selected = panel(&second);
        for seed in &selected {
            vote_choice(&mut fixture, &second, *seed, CUTOFF + 60, 2);
        }
        fixture.push(20, BlockType::Burn, CUTOFF + 61, |block| {
            block.link = destination(8);
            block.brn_spent += 1;
        });
        let records = [first, second];
        let state = ConfirmedReplay::replay_with_certificates(
            &fixture.history(CUTOFF + 134),
            NetworkId::Dev,
            &fixture.params,
            &records,
        )
        .unwrap();
        assert!(state.account(&address(20)).unwrap().state.is_verified());
        assert_eq!(state.account(&address(20)).unwrap().total_brn_burned, 2);
        for seed in selected {
            let brn = state.brn.get_wallet(&address(seed)).unwrap();
            assert_eq!(brn.total_staked, fixture.params.verifier_stake_amount);
            // Earlier normal-panel releases may remain; none names a challenge vote.
            assert!(brn
                .pending_releases
                .values()
                .all(|release| release.sources.iter().all(|hash| fixture
                    .blocks
                    .iter()
                    .find(|block| block.hash == *hash)
                    .unwrap()
                    .timestamp
                    .as_secs()
                    < CUTOFF + 60)));
        }
        let mut restored = VerificationOrchestrator::restore(state.verification.snapshot());
        assert_eq!(
            restored.pending_challenge_verdict(&address(20), &fixture.params),
            Some(burst_verification::ChallengeResult::FraudConfirmed)
        );
        assert!(!restored.has_expired_challenges(Timestamp::new(CUTOFF + 134), 100));
        assert!(restored
            .cleanup_expired_challenges(Timestamp::new(CUTOFF + 134), 100)
            .unwrap()
            .is_empty());
        assert!(restored.has_active_challenge(&address(20)));
        let mut late = state.verification.snapshot();
        late.states.get_mut(&address(20)).unwrap().votes[0].timestamp =
            Timestamp::new(CUTOFF + 134);
        let late = VerificationOrchestrator::restore(late);
        assert!(late.has_expired_challenges(Timestamp::new(CUTOFF + 134), 100));
    }
}

#[test]
fn incomplete_challenge_panels_replay_votes_without_settlement_or_loss_of_rights() {
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        for choice in [1, 2, 3] {
            let (mut fixture, first, admission) = pending_reason(reason);
            let (_dir, _store, second) = certify_challenge(&fixture, &first);
            let selected = panel(&second);
            let voter = selected[0];
            let context = second
                .replayed()
                .request()
                .vote_context(&second.certificate().checkpoint)
                .unwrap();
            let records = [first, second];
            let before = replay(&fixture, &records).unwrap();
            let hash = vote_choice(&mut fixture, &records[1], voter, CUTOFF + 60, choice);
            fixture.push(20, BlockType::Burn, CUTOFF + 60, |block| {
                block.link = destination(8);
                block.brn_spent += 1;
            });
            let after = replay(&fixture, &records).unwrap();
            assert!(after.account(&address(20)).unwrap().state.is_verified());
            let process = after.verification(&address(20)).unwrap();
            assert_eq!(process.votes.len(), 1);
            assert_eq!(process.votes[0].verifier, address(voter));
            assert_eq!(
                process.votes[0].stake_amount,
                if choice == 3 {
                    0
                } else {
                    fixture.params.verifier_stake_amount
                }
            );
            assert_eq!(
                after.account(&address(voter)).unwrap().total_brn_staked,
                before.account(&address(voter)).unwrap().total_brn_staked
                    + process.votes[0].stake_amount
            );
            assert_eq!(
                after.account(&address(8)).unwrap().total_brn_staked,
                fixture.params.challenge_stake_amount
                    + if voter == 8 {
                        process.votes[0].stake_amount
                    } else {
                        0
                    }
            );
            assert!(
                !after
                    .pending
                    .keys()
                    .any(|(_, source)| *source == hash.into_tx_hash()),
                "an incomplete panel must not mint a reward"
            );
            let restored = VerificationOrchestrator::restore(
                OrchestratorSnapshot::decode(&after.verification.snapshot().encode().unwrap())
                    .unwrap(),
            );
            assert_eq!(
                restored.active_challenge_context(&address(20)).unwrap(),
                Some(admission)
            );
            assert_eq!(restored.selection_context(&address(20)), Some(context));
            assert_eq!(restored.get_state(&address(20)).unwrap().votes.len(), 1);
        }
    }
}

#[test]
fn incomplete_challenge_replay_still_checks_context_membership_and_duplicate_votes() {
    for invalid_case in ["context", "member", "duplicate"] {
        let (mut fixture, first, _) = pending();
        let (_dir, _store, second) = certify_challenge(&fixture, &first);
        let selected = panel(&second);
        let voter = if invalid_case == "member" {
            (1..=8).find(|seed| !selected.contains(seed)).unwrap()
        } else {
            selected[0]
        };
        let hash = vote(&mut fixture, &second, voter, CUTOFF + 60);
        if invalid_case == "context" {
            let block = fixture
                .blocks
                .iter_mut()
                .find(|block| block.hash == hash)
                .unwrap();
            block.origin = TxHash::new([99; 32]);
            sign(voter, block);
        } else if invalid_case == "duplicate" {
            vote(&mut fixture, &second, voter, CUTOFF + 60);
        }
        assert!(
            replay(&fixture, &[first, second]).is_err(),
            "{invalid_case}"
        );
    }
}

#[test]
fn an_incomplete_challenge_vote_cannot_release_its_stake() {
    let (mut fixture, first, _) = pending();
    let (_dir, _store, second) = certify_challenge(&fixture, &first);
    let voter = panel(&second)[0];
    let source = vote(&mut fixture, &second, voter, CUTOFF + 60);
    let records = [first, second];
    assert!(replay(&fixture, &records).is_ok());
    fixture.push(voter, BlockType::Burn, CUTOFF + 60, |block| {
        block.link = destination(20);
        block.brn_spent += 1;
        block.release_sources = vec![source];
    });
    assert!(
        replay(&fixture, &records).is_err(),
        "an unsettled vote is not a release proof"
    );
}

#[tokio::test]
async fn committee_installation_rechecks_challenge_admission_under_the_publication_lock() {
    use std::sync::Arc;
    let (fixture, first, _) = pending();
    let (_dir, store, _second) = certify_challenge(&fixture, &first);
    let pending = replay(&fixture, std::slice::from_ref(&first))
        .unwrap()
        .verification;
    let prior = Fixture {
        params: fixture.params.clone(),
        blocks: fixture
            .blocks
            .iter()
            .take_while(|block| block.block_type != BlockType::Challenge)
            .cloned()
            .collect(),
    };
    let live = Arc::new(tokio::sync::Mutex::new(
        replay(&prior, std::slice::from_ref(&first))
            .unwrap()
            .verification,
    ));
    let mut guard = live.lock().await;
    let install = tokio::spawn(crate::checkpoint_selection::install(
        Arc::new(store),
        Arc::clone(&live),
        first,
        fixture.params,
    ));
    tokio::task::yield_now().await;
    assert!(!install.is_finished());
    *guard = pending;
    drop(guard);
    assert!(install.await.unwrap().unwrap().is_none());
    assert!(live.lock().await.selection_context(&address(20)).is_none());
}

#[tokio::test]
async fn challenge_beacon_worker_skips_the_completed_lifecycle_and_recovers_after_restart() {
    use std::{sync::Arc, time::Duration};
    let (fixture, first, _) = pending();
    let (_proof_dir, _proof_store, second) = certify_challenge(&fixture, &first);
    let prefix = Fixture {
        params: fixture.params.clone(),
        blocks: fixture
            .blocks
            .iter()
            .filter(|block| {
                first
                    .replayed()
                    .history_hashes()
                    .unwrap()
                    .contains(&block.hash)
            })
            .cloned()
            .collect(),
    };
    let (_dir, store, _) = prefix.store();
    let mut old = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        first.certificate().clone(),
        first.heads().to_vec(),
        address(20),
    )
    .unwrap();
    old.attach_beacon(&store, &fixture.params, first.beacon().unwrap().clone())
        .unwrap();
    crate::confirmed_replay::installation::PreparedInstallation::next(
        &store,
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap()
    .unwrap()
    .commit(&store)
    .unwrap();
    let replayed = replay(&fixture, std::slice::from_ref(&first)).unwrap();
    fixture.append_confirmed_prefix(&store, second.certificate().checkpoint.cutoff);
    let mut batch = store.tx_begin_write().unwrap();
    let live = replayed.verification;
    batch
        .put_brn_meta(BootstrapPhase::KEY, &BootstrapPhase::Complete.encoding())
        .unwrap();
    batch
        .put_meta(
            crate::endorsement_commit::SNAPSHOT_KEY,
            &live.snapshot().encode().unwrap(),
        )
        .unwrap();
    batch.commit().unwrap();
    let store = Arc::new(store);
    let service = super::super::snapshot_history::installed_recovery::WitnessService::start(
        store.clone(),
        store.clone(),
        0,
    )
    .await;
    for restart in [false, true] {
        let bytes = store
            .meta_store()
            .get_meta(crate::endorsement_commit::SNAPSHOT_KEY)
            .unwrap();
        let live = Arc::new(tokio::sync::Mutex::new(VerificationOrchestrator::restore(
            OrchestratorSnapshot::decode(&bytes).unwrap(),
        )));
        let shutdown = Arc::new(crate::shutdown::ShutdownController::new());
        let worker = tokio::spawn(crate::checkpoint_beacon::run(
            Arc::clone(&store),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::new(burst_vrf::DrandClient::new()),
            Arc::clone(&shutdown),
            Duration::from_millis(10),
            Some(crate::checkpoint_beacon::Live {
                recovery: None,
                registry: service.registry.clone(),
                orchestrator: live.clone(),
            }),
        ));
        if !restart {
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(
                !worker.is_finished(),
                "pending admission must not stop its beacon worker"
            );
            assert!(live.lock().await.selection_context(&address(20)).is_none());
            let mut accepted = CertifiedCheckpoint::accept(
                &store,
                NetworkId::Dev,
                &fixture.params,
                second.certificate().clone(),
                second.heads().to_vec(),
                address(20),
            )
            .unwrap();
            let mut state = live.lock().await;
            let installed = crate::confirmed_replay::installation::PreparedInstallation::next(
                &store,
                NetworkId::Dev,
                &fixture.params,
            )
            .unwrap()
            .unwrap()
            .commit(&store)
            .unwrap();
            *state = installed.verification;
            accepted
                .attach_beacon(&store, &fixture.params, second.beacon().unwrap().clone())
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if live.lock().await.selection_context(&address(20)).is_some() {
                    break;
                }
                assert!(
                    !worker.is_finished(),
                    "worker failed before installing the challenge"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            live.lock()
                .await
                .get_state(&address(20))
                .unwrap()
                .selected_verifiers
                .len(),
            2
        );
        if restart {
            tokio::time::sleep(Duration::from_millis(30)).await;
            assert!(!worker.is_finished());
        }
        shutdown.shutdown();
        worker.await.unwrap();
    }
    service.stop().await;
}
