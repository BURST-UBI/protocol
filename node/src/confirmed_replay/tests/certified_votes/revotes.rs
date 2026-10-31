use super::*;

fn prepare_revote(
    fixture: &Fixture,
    first: &CertifiedCheckpoint,
) -> (tempfile::TempDir, LmdbStore, CertifiedCheckpoint) {
    let (dir, store, _) = fixture.store();
    let mut accepted = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        first.certificate().clone(),
        first.heads().to_vec(),
        address(20),
    )
    .unwrap();
    accepted
        .attach_beacon(&store, &fixture.params, first.beacon().unwrap().clone())
        .unwrap();
    let replay =
        CertifiedCheckpoint::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 35)
            .unwrap()
            .expect("a completed inconclusive panel must queue its revote");
    assert_eq!(replay.request().target(), &address(20));
    assert_eq!(replay.request().attempt(), 1);
    assert_eq!(replay.request().round(), 103);
    let previous_panel = panel(first);
    let expected = SelectionRequest::new(
        address(20),
        fixture.blocks.last().unwrap().hash,
        1,
        CUTOFF + 35,
        fixture.params.params_hash(),
        (3..=8)
            .filter(|seed| !previous_panel.contains(seed))
            .map(address)
            .collect(),
        2,
    )
    .unwrap();
    assert_eq!(replay.request(), &expected);
    let mut next = CertifiedCheckpoint::accept(
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
    assert!(
        next.attach_beacon(&store, &fixture.params, first.beacon().unwrap().clone())
            .is_err(),
        "a revote cannot reuse the earlier round's randomness"
    );
    next.attach_beacon(
        &store,
        &fixture.params,
        burst_vrf::DrandClient::beacon_to_random_output(&beacon).unwrap(),
    )
    .unwrap();
    (dir, store, next)
}

fn replay_rounds(
    fixture: &Fixture,
    certificates: &[CertifiedCheckpoint],
) -> Result<ConfirmedReplay, ReplayError> {
    ConfirmedReplay::replay_with_certificates(
        &fixture.history(CUTOFF + 90),
        NetworkId::Dev,
        &fixture.params,
        certificates,
    )
}

#[test]
fn certified_revote_preserves_old_stakes_excludes_old_panel_and_settles_once() {
    let mut fixture = Fixture::selection_with(|params| params.max_revotes = 1);
    let first = certificate(&fixture, true);
    let previous = panel(&first);
    vote_choice(&mut fixture, &first, previous[0], CUTOFF + 31, 1);
    vote_choice(&mut fixture, &first, previous[1], CUTOFF + 32, 2);
    let waiting = replay_rounds(&fixture, std::slice::from_ref(&first)).unwrap();
    assert_eq!(waiting.verification(&address(20)).unwrap().revote_count, 1);
    assert!(waiting
        .verification(&address(20))
        .unwrap()
        .selected_verifiers
        .is_empty());
    for seed in &previous {
        assert_eq!(
            waiting.account(&address(*seed)).unwrap().total_brn_staked,
            fixture.params.verifier_stake_amount
        );
    }
    let (_dir, _store, second) = prepare_revote(&fixture, &first);
    let next = panel(&second);
    assert!(next.iter().all(|seed| !previous.contains(seed)));
    vote(&mut fixture, &second, next[0], CUTOFF + 61);
    let certificates = [first, second];
    let partial = replay_rounds(&fixture, &certificates).unwrap();
    assert!(!partial
        .account(&address(20))
        .is_some_and(|account| account.state.is_verified()));
    assert!(partial.pending.is_empty());
    for seed in previous.iter().chain(std::iter::once(&next[0])) {
        assert_eq!(
            partial.account(&address(*seed)).unwrap().total_brn_staked,
            fixture.params.verifier_stake_amount
        );
    }
    let outcome = vote(&mut fixture, &certificates[1], next[1], CUTOFF + 62);
    let resolved = replay_rounds(&fixture, &certificates).unwrap();
    assert!(resolved.account(&address(20)).unwrap().state.is_verified());
    assert_eq!(resolved.human_roster().unwrap().total_humans(), 10);
    for seed in previous.iter().chain(&next) {
        assert_eq!(
            resolved.account(&address(*seed)).unwrap().total_brn_staked,
            0
        );
        assert_eq!(
            resolved
                .brn
                .get_wallet(&address(*seed))
                .unwrap()
                .total_staked,
            0
        );
    }
    assert_eq!(
        resolved
            .brn
            .get_wallet(&address(previous[1]))
            .unwrap()
            .total_burned,
        fixture.params.verifier_stake_amount
    );
    let share = fixture.params.verifier_stake_amount / 3;
    assert_eq!(resolved.pending.len(), 3);
    for seed in [previous[0], next[0], next[1]] {
        let reward = &resolved.pending[&(address(seed), outcome.into_tx_hash())];
        assert_eq!(reward.amount, share);
        assert_eq!(reward.timestamp, Timestamp::new(CUTOFF + 62));
    }
    assert!(
        replay_rounds(&fixture, &certificates[1..]).is_err(),
        "revote cannot omit its earlier certified attempt"
    );
    fixture.push(previous[0], BlockType::Receive, CUTOFF + 63, |block| {
        block.link = outcome;
        block.trst_balance += share;
    });
    let claimed = replay_rounds(&fixture, &certificates).unwrap();
    assert_eq!(claimed.pending.len(), 2);
    assert_eq!(
        claimed
            .portfolio(&address(previous[0]))
            .unwrap()
            .cached_transferable,
        share
    );
}

#[test]
fn certified_revote_limits_neither_rounds_and_recovers_the_same_chain() {
    let mut fixture = Fixture::selection_with(|params| params.max_revotes = 1);
    let first = certificate(&fixture, true);
    let previous = panel(&first);
    for (index, seed) in previous.iter().enumerate() {
        vote_choice(&mut fixture, &first, *seed, CUTOFF + 31 + index as u64, 3);
    }
    let (dir, store, second) = prepare_revote(&fixture, &first);
    let next = panel(&second);
    let encoded: Vec<_> = (1..=2)
        .map(|height| {
            CertifiedCheckpoint::export_record(&store, NetworkId::Dev, &fixture.params, height)
                .unwrap()
                .unwrap()
        })
        .collect();
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let recovered =
        CertifiedCheckpoint::load_chain(&store, NetworkId::Dev, &fixture.params).unwrap();
    assert_eq!(recovered.len(), 2);
    assert_eq!(recovered[1].certificate(), second.certificate());
    assert_eq!(panel(&recovered[1]), next);
    for (index, seed) in next.iter().enumerate() {
        vote_choice(&mut fixture, &second, *seed, CUTOFF + 61 + index as u64, 3);
    }
    let (_observer_dir, observer, _) = fixture.store();
    for (index, bytes) in encoded.iter().enumerate() {
        CertifiedCheckpoint::import_record(
            &observer,
            NetworkId::Dev,
            &fixture.params,
            index as u64 + 1,
            bytes,
        )
        .unwrap();
    }
    let imported =
        CertifiedCheckpoint::load_chain(&observer, NetworkId::Dev, &fixture.params).unwrap();
    let replay = replay_rounds(&fixture, &imported).unwrap();
    assert_eq!(
        replay.verification(&address(20)).unwrap().phase,
        burst_verification::state::VerificationPhase::Failed
    );
    assert_eq!(replay.verification(&address(20)).unwrap().revote_count, 1);
    assert_eq!(replay.human_roster().unwrap().total_humans(), 9);
    assert!(replay.pending.is_empty());
    for seed in previous.iter().chain(&next) {
        assert_eq!(replay.account(&address(*seed)).unwrap().total_brn_staked, 0);
        assert_eq!(
            replay.brn.get_wallet(&address(*seed)).unwrap().total_burned,
            0
        );
    }
    assert!(
        CertifiedCheckpoint::prepare_next(&observer, NetworkId::Dev, &fixture.params, CUTOFF + 90)
            .unwrap()
            .is_none(),
        "exhausting revotes cannot queue another selection"
    );
}

#[test]
fn certified_revote_rejects_missing_prior_votes_old_panel_and_stale_context() {
    let mut fixture = Fixture::selection();
    let first = certificate(&fixture, true);
    let previous = panel(&first);
    vote_choice(&mut fixture, &first, previous[0], CUTOFF + 31, 1);
    vote_choice(&mut fixture, &first, previous[1], CUTOFF + 32, 2);
    let (_dir, _store, second) = prepare_revote(&fixture, &first);
    let next = panel(&second);
    let certificates = [first, second];
    for case in [
        "missing prior vote",
        "old panel",
        "old context",
        "before beacon",
    ] {
        let mut changed = Fixture {
            params: fixture.params.clone(),
            blocks: fixture.blocks.clone(),
        };
        match case {
            "missing prior vote" => {
                changed.blocks.pop();
                vote(&mut changed, &certificates[1], next[0], CUTOFF + 61);
            }
            "old panel" => {
                vote(&mut changed, &certificates[1], previous[0], CUTOFF + 61);
            }
            "old context" => {
                vote(&mut changed, &certificates[0], previous[0], CUTOFF + 61);
            }
            _ => {
                vote(&mut changed, &certificates[1], next[0], CUTOFF + 36);
            }
        }
        assert!(
            replay_rounds(&changed, &certificates).is_err(),
            "{case} must fail closed"
        );
    }
}

#[test]
fn certified_incomplete_panel_does_not_authorize_a_revote() {
    let mut fixture = Fixture::selection();
    let first = certificate(&fixture, true);
    vote_choice(&mut fixture, &first, panel(&first)[0], CUTOFF + 31, 2);
    let (_dir, store, _) = fixture.store();
    let mut accepted = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        first.certificate().clone(),
        first.heads().to_vec(),
        address(20),
    )
    .unwrap();
    accepted
        .attach_beacon(&store, &fixture.params, first.beacon().unwrap().clone())
        .unwrap();
    assert!(
        CertifiedCheckpoint::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 35)
            .unwrap()
            .is_none(),
        "a missing vote is not an agreed timeout"
    );
}

#[tokio::test]
async fn certified_revote_worker_preserves_prior_votes_and_resumes_the_next_panel() {
    use crate::checkpoint_selection;
    use burst_verification::{bootstrap::BootstrapPhase, OrchestratorSnapshot};
    use std::sync::Arc;
    use std::time::Duration;

    let mut fixture = Fixture::selection();
    let first = certificate(&fixture, true);
    let previous = panel(&first);
    let prefix = Fixture {
        params: fixture.params.clone(),
        blocks: fixture.blocks.clone(),
    };
    vote_choice(&mut fixture, &first, previous[0], CUTOFF + 31, 1);
    vote_choice(&mut fixture, &first, previous[1], CUTOFF + 32, 2);
    let (_proof_dir, _proof_store, second) = prepare_revote(&fixture, &first);
    let (_dir, store, _) = prefix.store();
    let mut accepted = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        first.certificate().clone(),
        first.heads().to_vec(),
        address(20),
    )
    .unwrap();
    accepted
        .attach_beacon(&store, &fixture.params, first.beacon().unwrap().clone())
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
    fixture.append_confirmed_prefix(&store, CUTOFF + 35);
    let mut accepted = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        second.certificate().clone(),
        second.heads().to_vec(),
        address(20),
    )
    .unwrap();
    let waiting = crate::confirmed_replay::installation::PreparedInstallation::next(
        &store,
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap()
    .unwrap()
    .commit(&store)
    .unwrap()
    .verification;
    accepted
        .attach_beacon(&store, &fixture.params, second.beacon().unwrap().clone())
        .unwrap();
    let expected = second
        .replayed()
        .request()
        .vote_context(&second.certificate().checkpoint)
        .unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .put_brn_meta(BootstrapPhase::KEY, &BootstrapPhase::Complete.encoding())
        .unwrap();
    let persisted = crate::verification_storage::load(&batch).unwrap();
    assert_eq!(
        persisted.snapshot().encode().unwrap(),
        waiting.snapshot().encode().unwrap()
    );
    batch.commit().unwrap();

    let mut batch = store.tx_begin_write().unwrap();
    assert!(
        checkpoint_selection::stage(&mut batch, &first, &fixture.params, &waiting)
            .unwrap()
            .is_none(),
        "an already completed panel must not be reinstalled"
    );
    let candidate = checkpoint_selection::stage(&mut batch, &second, &fixture.params, &waiting)
        .unwrap()
        .unwrap();
    assert_eq!(candidate.selection_context(&address(20)), Some(expected));
    drop(batch);
    let before = crate::verification_storage::load(&store.tx_begin_write().unwrap())
        .unwrap()
        .snapshot()
        .encode()
        .unwrap();
    let restored =
        VerificationOrchestrator::restore(OrchestratorSnapshot::decode(&before).unwrap());
    assert!(restored.selection_context(&address(20)).is_none());

    for mutation in 0..4 {
        let mut snapshot = waiting.snapshot();
        match mutation {
            0 => {
                snapshot.prior_votes.get_mut(&address(20)).unwrap()[0].vote =
                    burst_verification::Vote::Neither
            }
            1 => snapshot.prior_votes.get_mut(&address(20)).unwrap()[0].stake_amount += 1,
            2 => {
                snapshot.prior_votes.get_mut(&address(20)).unwrap()[0].timestamp =
                    Timestamp::new(CUTOFF + 33)
            }
            _ => snapshot
                .states
                .get_mut(&address(20))
                .unwrap()
                .excluded_verifiers
                .clear(),
        }
        let changed = VerificationOrchestrator::restore(snapshot);
        let mut batch = store.tx_begin_write().unwrap();
        let error = checkpoint_selection::stage(&mut batch, &second, &fixture.params, &changed)
            .err()
            .unwrap();
        assert!(
            error.to_string().contains("selection history differs"),
            "{error}"
        );
    }
    assert_eq!(
        crate::verification_storage::load(&store.tx_begin_write().unwrap())
            .unwrap()
            .snapshot()
            .encode()
            .unwrap(),
        before
    );
    let store = Arc::new(store);
    let live = Arc::new(tokio::sync::Mutex::new(restored));
    let active = format!("checkpoint_chain/v1/dev/{}/1", fixture.blocks[0].hash);
    let archived = format!(
        "checkpoint_record_archive/v1/dev/{}/1",
        fixture.blocks[0].hash
    );
    {
        let mut batch = store.tx_begin_write().unwrap();
        let bytes = batch.get_meta(&active).unwrap().unwrap();
        batch.put_meta(&archived, &bytes).unwrap();
        batch.delete_meta(&active).unwrap();
        batch.commit().unwrap();
    }
    let service = super::super::snapshot_history::installed_recovery::WitnessService::start(
        store.clone(),
        store.clone(),
        0,
    )
    .await;
    let shutdown = Arc::new(crate::shutdown::ShutdownController::new());
    let worker = tokio::spawn(crate::checkpoint_beacon::run(
        store.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        Arc::new(burst_vrf::DrandClient::mainnet().unwrap()),
        shutdown.clone(),
        Duration::from_millis(20),
        Some(crate::checkpoint_beacon::Live {
            recovery: None,
            registry: service.registry.clone(),
            orchestrator: live.clone(),
        }),
    ));
    let installed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if live.lock().await.selection_context(&address(20)) == Some(expected) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    shutdown.shutdown();
    worker.await.unwrap();
    service.stop().await;
    assert!(shutdown.failure().is_none(), "{:?}", shutdown.failure());
    installed.unwrap();
    let bytes = crate::verification_storage::load(&store.tx_begin_write().unwrap())
        .unwrap()
        .snapshot()
        .encode()
        .unwrap();
    let snapshot = OrchestratorSnapshot::decode(&bytes).unwrap();
    let votes = &snapshot.prior_votes[&address(20)];
    assert_eq!(votes.len(), 2);
    assert_eq!(
        votes.iter().map(|vote| vote.stake_amount).sum::<u128>(),
        2 * fixture.params.verifier_stake_amount
    );
    let restarted = VerificationOrchestrator::restore(snapshot);
    assert_eq!(restarted.selection_context(&address(20)), Some(expected));
    let mut batch = store.tx_begin_write().unwrap();
    assert!(
        checkpoint_selection::stage(&mut batch, &first, &fixture.params, &restarted)
            .unwrap()
            .is_none()
    );
    assert!(
        checkpoint_selection::stage(&mut batch, &second, &fixture.params, &restarted)
            .unwrap()
            .is_none()
    );
}
