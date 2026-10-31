use super::*;
use crate::{checkpoint_deadline::PreparedDeadline, verification_deadline};

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_deadline_journal_recovers_active_and_terminal_rounds_from_peers() {
    use crate::checkpoint_agreement::{self, Command, CompactReplayInput};
    use crate::checkpoint_replay::SigningClock;
    use crate::confirmed_replay::tests::snapshot_history::{
        authenticate, cemented, compact, installed_recovery::WitnessService,
    };
    use std::sync::Arc;
    let (fixture, _source_dir, source, _, _) = setup();
    let parent = activate(&fixture, &source);
    let manifest =
        PreparedDeadline::prepare_next(&source, NetworkId::Dev, &fixture.params, CUTOFF + 91)
            .unwrap()
            .unwrap()
            .manifest()
            .clone();
    let certificate = certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 6]);
    let source = Arc::new(source);
    for case in 1..=5 {
        let snapshot = authenticate(&source, &fixture.params, parent);
        let selections = snapshot.recover_selections().unwrap();
        let proofs = compact::retained(&snapshot, &fixture, &source, &[]);
        let base = authenticate(&source, &fixture.params, parent).into_continuation();
        let (dir, local) = cemented::sparse(&base, &source);
        let tip = format!(
            "checkpoint_chain/v1/dev/{}/head",
            base.anchor().genesis_hash()
        );
        local
            .meta_store()
            .put_meta(&tip, &base.anchor().height().to_be_bytes())
            .unwrap();
        let tail = base
            .read_tail(&local, manifest.checkpoint.cutoff, &manifest.heads, proofs)
            .unwrap();
        let coverage = base
            .deadline_vote_coverage_for_manifest(
                &tail,
                &manifest,
                &selections.iter().collect::<Vec<_>>(),
            )
            .unwrap();
        let seed = (0..9)
            .find(|seed| address(*seed) == *base.anchor().proposer(0))
            .unwrap();
        let key = burst_crypto::keypair_from_seed(&[seed; 32]);
        let inputs = [CompactReplayInput::Deadline(&coverage)];
        let mut expected = checkpoint_agreement::apply_compact_candidates(
            &local,
            base.anchor(),
            &key.private,
            Command::Propose(manifest.checkpoint.clone()),
            &inputs,
            Some(SigningClock {
                now: CUTOFF + 93,
                error_bound: 0,
            }),
        )
        .unwrap();
        if case == 2 || case == 5 {
            expected = checkpoint_agreement::apply_compact_candidates(
                &local,
                base.anchor(),
                &key.private,
                Command::Decide(certificate.clone()),
                &inputs,
                None,
            )
            .unwrap();
        }
        if case == 2 || case >= 4 {
            crate::checkpoint_store::compact::accept_candidate(
                &local,
                base.anchor(),
                inputs[0],
                &certificate,
            )
            .unwrap();
        }
        if case == 5 {
            local
                .meta_store()
                .put_meta(
                    burst_store::meta::PAYMENT_EPOCH_KEY,
                    manifest.checkpoint.hash().as_bytes(),
                )
                .unwrap();
        }
        drop(local);
        let local = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
        let recovered = snapshot
            .recover_stored_epoch(&local, parent, |_, _, _| unreachable!())
            .unwrap();
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let service =
            WitnessService::start(source.clone(), local.clone(), if case == 3 { 5 } else { 0 })
                .await;
        let recovered = if case == 1 {
            let recovered = Arc::new(recovered);
            let unavailable = Arc::new(tokio::sync::RwLock::new(
                crate::connection_registry::ConnectionRegistry::new(),
            ));
            let result = crate::checkpoint_service::Session::resume_or_discover_compact_shared(
                crate::checkpoint_service::Settings {
                    store: local.clone(),
                    network: NetworkId::Dev,
                    initial_params: fixture.params.clone(),
                    key: Some(Arc::new(
                        burst_crypto::keypair_from_seed(&[seed; 32]).private,
                    )),
                },
                unavailable,
                recovered.clone(),
                None,
                std::time::Duration::from_secs(30),
            )
            .await;
            assert!(result.is_err());
            assert_eq!(Arc::strong_count(&recovered), 1);
            assert_eq!(
                local
                    .tx_begin_write()
                    .unwrap()
                    .checkpoint_metadata(b"")
                    .unwrap(),
                before
            );
            let resumed = crate::checkpoint_service::Session::resume_or_discover_compact_shared(
                crate::checkpoint_service::Settings {
                    store: local.clone(),
                    network: NetworkId::Dev,
                    initial_params: fixture.params.clone(),
                    key: Some(Arc::new(
                        burst_crypto::keypair_from_seed(&[seed; 32]).private,
                    )),
                },
                service.registry.clone(),
                recovered.clone(),
                None,
                std::time::Duration::from_secs(30),
            )
            .await
            .unwrap()
            .expect("saved candidate resumes after proof service returns");
            drop(resumed);
            assert_eq!(Arc::strong_count(&recovered), 1);
            assert_eq!(
                local
                    .tx_begin_write()
                    .unwrap()
                    .checkpoint_metadata(b"")
                    .unwrap(),
                before
            );
            Arc::try_unwrap(recovered).ok().unwrap()
        } else {
            recovered
        };
        let result = checkpoint_agreement::recover_compact_candidates_from_peers(
            local.clone(),
            service.registry.clone(),
            recovered,
            address(seed),
        )
        .await;
        assert!(service.stop().await > 0);
        let after = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        if case == 3 {
            assert!(result
                .err()
                .unwrap()
                .to_string()
                .contains("journal changed during recovery"));
            let without_journal = |rows: Vec<(Vec<u8>, Vec<u8>)>| {
                rows.into_iter()
                    .filter(|(key, _)| !key.starts_with(b"checkpoint_round/v1/"))
                    .collect::<Vec<_>>()
            };
            assert_eq!(without_journal(before), without_journal(after));
            continue;
        }
        assert_eq!(before, after);
        if case == 4 {
            assert!(result.is_err());
            continue;
        }
        let (recovered, candidates) = result.unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].input().manifest(),
            ReplayManifest::Deadline(manifest.clone())
        );
        let inputs = candidates
            .iter()
            .map(|value| value.input())
            .collect::<Vec<_>>();
        let resumed = checkpoint_agreement::apply_compact_candidates(
            &local,
            recovered.continuation.anchor(),
            &key.private,
            Command::Resume,
            &inputs,
            None,
        )
        .unwrap();
        assert_eq!(resumed, expected);
        if case == 2 || case == 5 {
            assert!(checkpoint_agreement::apply_compact_candidates(
                &local,
                recovered.continuation.anchor(),
                &key.private,
                Command::Propose(manifest.checkpoint.clone()),
                &inputs,
                Some(SigningClock {
                    now: CUTOFF + 100,
                    error_bound: 0
                }),
            )
            .is_err());
        }
        let service = WitnessService::start(source.clone(), local.clone(), 0).await;
        let started = crate::checkpoint_service::Session::resume_or_discover_compact(
            crate::checkpoint_service::Settings {
                store: local.clone(),
                network: NetworkId::Dev,
                initial_params: fixture.params.clone(),
                key: Some(Arc::new(
                    burst_crypto::keypair_from_seed(&[seed; 32]).private,
                )),
            },
            service.registry.clone(),
            recovered,
            (case == 1).then_some(SigningClock {
                now: CUTOFF + 100,
                error_bound: 0,
            }),
            std::time::Duration::from_secs(30),
        )
        .await
        .unwrap();
        assert!(matches!(
            started,
            crate::checkpoint_service::CompactStart::Ready(_)
        ));
        drop(started);
        assert!(service.stop().await > 0);
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_deadline_peer_replay_rejects_forged_effects_and_changed_authority() {
    use crate::confirmed_replay::tests::snapshot_history::{
        authenticate, cemented, installed_recovery::WitnessService,
    };
    use std::sync::Arc;
    let (fixture, _dir, source, _, _) = setup();
    let parent = activate(&fixture, &source);
    let expected =
        PreparedDeadline::prepare_next(&source, NetworkId::Dev, &fixture.params, CUTOFF + 91)
            .unwrap()
            .unwrap()
            .manifest()
            .clone();
    let source = Arc::new(source);
    for (change, observer) in [
        (0, false),
        (1, false),
        (4, false),
        (7, false),
        (1, true),
        (4, true),
        (7, true),
    ] {
        let snapshot = authenticate(&source, &fixture.params, parent);
        let base = authenticate(&source, &fixture.params, parent).into_continuation();
        let (_local_dir, local) = cemented::sparse(&base, &source);
        let tip = format!(
            "checkpoint_chain/v1/dev/{}/head",
            base.anchor().genesis_hash()
        );
        local
            .meta_store()
            .put_meta(&tip, &base.anchor().height().to_be_bytes())
            .unwrap();
        let recovered = Arc::new(
            snapshot
                .recover_stored_successors(&local, |_, _, _| unreachable!())
                .unwrap(),
        );
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let local = Arc::new(local);
        let service = WitnessService::start(
            source.clone(),
            local.clone(),
            if change == 7 { 0 } else { change },
        )
        .await;
        let mut manifest = expected.clone();
        if change == 7 {
            manifest.checkpoint.selection_root = BlockHash::new([99; 32]);
        }
        let result = if observer {
            let certificate = certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 6]);
            recovered
                .clone()
                .accept_decision_from_peers(
                    local.clone(),
                    service.registry.clone(),
                    certificate,
                    ReplayManifest::Deadline(manifest),
                )
                .await
                .map(|()| None)
        } else {
            recovered
                .clone()
                .candidate_coverage_shared(
                    local.clone(),
                    service.registry.clone(),
                    ReplayManifest::Deadline(manifest),
                )
                .await
                .map(|candidate| Some(candidate.input().manifest()))
        };
        assert!(service.stop().await > 0);
        if change == 0 {
            assert_eq!(
                result.unwrap(),
                Some(ReplayManifest::Deadline(expected.clone()))
            );
        } else {
            assert!(result.is_err(), "change {change}");
        }
        let after = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let changed_key = match change {
            1 => Some(burst_store::meta::PAYMENT_EPOCH_KEY.as_bytes()),
            4 => Some(tip.as_bytes()),
            _ => None,
        };
        let unchanged = |rows: Vec<(Vec<u8>, Vec<u8>)>| {
            rows.into_iter()
                .filter(|(key, _)| Some(key.as_slice()) != changed_key)
                .collect::<Vec<_>>()
        };
        assert_eq!(unchanged(before), unchanged(after));
        assert_eq!(
            recovered.continuation.state().snapshot_bytes().unwrap(),
            base.state().snapshot_bytes().unwrap()
        );
    }
}

#[test]
fn deadline_batch_returns_expired_pending_and_completes_a_panel_together() {
    let mut fixture = Fixture::selection_with(|params| {
        params.verification_timeout_secs = 60;
        params.trst_expiry_secs = 90;
    });
    let burn = fixture.push(0, BlockType::Burn, CUTOFF - 1, |block| {
        block.brn_spent += 40;
        block.link = destination(21);
    });
    let (fixture, _dir, store, selection, _) = setup_fixture(fixture);
    activate(&fixture, &store);
    complete(&fixture, &store);
    crate::confirmed_replay::tests::snapshot_history::boundaries::assert_chain(&fixture, &store);
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let state = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 92),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    assert_eq!(state.pending.len(), 1);
    let receipt = &state.pending[&(address(0), burn.into_tx_hash())];
    assert!(receipt.return_credit);
    assert_eq!(receipt.amount, 40);
    assert_eq!(state.account(&address(0)).unwrap().expired_trst, 40);
    assert_eq!(
        state.portfolio(&address(0)).unwrap().tokens[0].origin,
        burn.into_tx_hash()
    );
    assert_eq!(
        state
            .verification
            .get_state(&address(20))
            .unwrap()
            .revote_count,
        1
    );
    let context = selection
        .replayed()
        .request()
        .vote_context(&selection.certificate().checkpoint)
        .unwrap();
    assert!(store
        .meta_store()
        .get_meta(&verification_deadline::key(context))
        .is_ok());
    assert!(
        PreparedDeadline::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 92)
            .unwrap()
            .is_none()
    );
}

#[test]
fn deadline_batch_completes_all_due_panels_at_one_boundary() {
    let mut fixture = Fixture::selection_with(|params| params.verification_timeout_secs = 60);
    for seed in [1, 2] {
        fixture.push(seed, BlockType::Endorse, 1_595_431_106, |block| {
            block.brn_spent += 1;
            block.link = destination(21);
        });
    }
    let (fixture, _dir, store, first, _) = setup_fixture(fixture);
    activate(&fixture, &store);
    let heads = ConfirmedHistory::heads_at(&store, CUTOFF + 35).unwrap();
    let replay = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 35,
        &heads,
        &address(21),
    )
    .unwrap();
    let mut second = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certify(replay.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]),
        heads,
        address(21),
    )
    .unwrap();
    let beacon: burst_vrf::DrandBeacon = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/revote-beacon.json"
    ))
    .unwrap();
    second
        .attach_beacon(
            &store,
            &fixture.params,
            burst_vrf::DrandClient::beacon_to_random_output(&beacon).unwrap(),
        )
        .unwrap();
    PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    let activation =
        PreparedActivation::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 61)
            .unwrap()
            .unwrap();
    let manifest = activation.manifest().clone();
    chain::accept_manifest(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 6]),
        &ReplayManifest::Activation(manifest),
    )
    .unwrap();
    PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    complete_at(&fixture, &store, CUTOFF + 121);
    crate::confirmed_replay::tests::snapshot_history::boundaries::assert_chain(&fixture, &store);
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let state = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 122),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    for record in [&first, &second] {
        let target = record.replayed().request().target();
        assert_eq!(
            state.verification.get_state(target).unwrap().revote_count,
            1
        );
        let context = record
            .replayed()
            .request()
            .vote_context(&record.certificate().checkpoint)
            .unwrap();
        let completion = verification_deadline::decode(
            &store
                .meta_store()
                .get_meta(&verification_deadline::key(context))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(completion.completed_at, CUTOFF + 121);
        assert_eq!(completion.absent.len(), 2);
    }
    assert!(
        PreparedDeadline::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 122)
            .unwrap()
            .is_none()
    );
}

fn complete(fixture: &Fixture, store: &LmdbStore) -> BlockHash {
    complete_at(fixture, store, CUTOFF + 91)
}

#[test]
fn compact_checkpoint_rejects_skips_wrong_cuts_and_stale_witnesses() {
    use crate::confirmed_replay::tests::snapshot_history::{
        authenticate, compact::retained, suffix_store,
    };
    let (fixture, _dir, store, _, _) = setup();
    activate(&fixture, &store);
    complete(&fixture, &store);
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let certificates: Vec<_> = events[..1]
        .iter()
        .filter_map(chain::CertifiedEvent::selection)
        .collect();
    let first = events[0].certificate().checkpoint.hash();
    let authority = store.meta_store().payment_epoch().unwrap();
    let (_tail_dir, suffix) = suffix_store(&[]);
    for case in 0..5 {
        let snapshot = authenticate(&store, &fixture.params, first);
        let mut old = retained(&snapshot, &fixture, &store, &[]);
        let stale = old.pop().unwrap();
        let witnesses = retained(&snapshot, &fixture, &store, &[]);
        let base = snapshot.into_continuation();
        let event = &events[if case == 0 { 2 } else { 1 }];
        let cutoff = event.certificate().checkpoint.cutoff + u64::from(case == 1);
        let tail = base
            .read_tail(&suffix, cutoff, event.heads(), witnesses)
            .unwrap();
        if case <= 2 {
            let selected = if case == 2 {
                &[][..]
            } else {
                certificates.as_slice()
            };
            assert!(
                base.replay_checkpoint(tail, event, selected).is_err(),
                "case {case}"
            );
        } else {
            let base = base.replay_checkpoint(tail, event, &certificates).unwrap();
            if case == 3 {
                assert!(base
                    .read_tail(&suffix, cutoff, event.heads(), vec![stale])
                    .is_err());
            } else {
                assert!(
                    base.read_tail(&suffix, cutoff, event.heads(), vec![])
                        .is_err(),
                    "repeated checkpoints still require historical witnesses"
                );
                let current = authenticate(
                    &store,
                    &fixture.params,
                    event.certificate().checkpoint.hash(),
                );
                let witnesses = retained(&current, &fixture, &store, &[]);
                let tail = base
                    .read_tail(&suffix, cutoff, event.heads(), witnesses)
                    .unwrap();
                assert!(base.replay_checkpoint(tail, event, &certificates).is_err());
            }
        }
        assert_eq!(store.meta_store().payment_epoch().unwrap(), authority);
    }
}

#[test]
fn compact_manifest_requires_quorum_exact_domain_and_replayed_effects() {
    use crate::checkpoint_roster::RosterManifest;
    use crate::confirmed_replay::tests::snapshot_history::{
        authenticate, compact::retained, suffix_store,
    };
    let (fixture, _dir, store, _, _) = setup();
    activate(&fixture, &store);
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let selected = events[0].selection().unwrap();
    let event = &events[1];
    let first = events[0].certificate().checkpoint.hash();
    let authority = store.meta_store().payment_epoch().unwrap();
    let (_tail_dir, suffix) = suffix_store(&[]);
    for case in 0..11 {
        let snapshot = authenticate(&store, &fixture.params, first);
        let witnesses = retained(&snapshot, &fixture, &store, &[]);
        let base = snapshot.into_continuation();
        let tail = base
            .read_tail(
                &suffix,
                event.certificate().checkpoint.cutoff,
                event.heads(),
                witnesses,
            )
            .unwrap();
        let ReplayManifest::Activation(mut manifest) = event.manifest() else {
            unreachable!()
        };
        match case {
            2 => {
                manifest.heads.pop();
            }
            3 => manifest.activation.selection = BlockHash::ZERO,
            4 => {
                manifest.activation.proof[0] ^= 1;
                manifest.checkpoint.selection_root = manifest.activation.commitment().unwrap();
            }
            5 => manifest.checkpoint.next_quorum_bps = 10_000,
            6 => manifest.checkpoint.next_roster = BlockHash::new([31; 32]),
            7 => manifest.checkpoint.selection_root = BlockHash::new([32; 32]),
            8 => {
                manifest.checkpoint.selection_root = crate::checkpoint_roster::commitment();
                manifest.checkpoint.next_roster = base.anchor().roster().commitment();
            }
            10 => manifest.activation.proof.resize(4097, 0),
            _ => {}
        }
        let mut certificate = certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 6]);
        if case == 0 {
            certificate.signatures.clear();
        }
        if case == 9 {
            manifest.checkpoint.params_hash = BlockHash::new([33; 32]);
        }
        let manifest = if case == 1 || case == 8 {
            ReplayManifest::Roster(RosterManifest {
                checkpoint: manifest.checkpoint,
                heads: manifest.heads,
            })
        } else {
            ReplayManifest::Activation(manifest)
        };
        assert!(
            base.replay_manifest(tail, &certificate, &manifest, &[selected])
                .is_err(),
            "case {case}"
        );
        assert_eq!(store.meta_store().payment_epoch().unwrap(), authority);
    }
}

fn complete_at(fixture: &Fixture, store: &LmdbStore, cutoff: u64) -> BlockHash {
    let prepared = PreparedDeadline::prepare_next(store, NetworkId::Dev, &fixture.params, cutoff)
        .unwrap()
        .unwrap();
    let manifest = prepared.manifest().clone();
    assert!(prepared
        .certify(certify(&manifest.checkpoint, &[0]))
        .is_err());
    chain::accept_manifest(
        store,
        NetworkId::Dev,
        &fixture.params,
        certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 6]),
        &ReplayManifest::Deadline(manifest.clone()),
    )
    .unwrap();
    assert_ne!(
        store.meta_store().payment_epoch().unwrap(),
        manifest.checkpoint.hash()
    );
    PreparedInstallation::next(store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(store)
        .unwrap();
    manifest.checkpoint.hash()
}

#[test]
fn deadline_revote_keeps_cast_stakes_and_later_signed_success_settles_prior_defaults_once() {
    let (mut fixture, _dir, store, first, _) = setup_config(|params| {
        params.verification_timeout_secs = 2;
        params.max_revotes = 1;
    });
    let previous = panel(&first);
    vote_choice(&mut fixture, &first, previous[0], CUTOFF + 31, 2);
    fixture.append_confirmed_prefix(&store, CUTOFF + 31);
    activate(&fixture, &store);
    complete_at(&fixture, &store, CUTOFF + 33);
    assert_eq!(
        store
            .account_store()
            .get_account(&address(previous[0]))
            .unwrap()
            .total_brn_staked,
        fixture.params.verifier_stake_amount
    );
    let replay =
        CertifiedCheckpoint::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 35)
            .unwrap()
            .unwrap();
    assert_eq!(replay.request().attempt(), 1);
    assert_eq!(replay.request().round(), 103);
    let mut second = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certify(replay.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]),
        replay.manifest().heads,
        address(20),
    )
    .unwrap();
    let beacon: burst_vrf::DrandBeacon = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/revote-beacon.json"
    ))
    .unwrap();
    second
        .attach_beacon(
            &store,
            &fixture.params,
            burst_vrf::DrandClient::beacon_to_random_output(&beacon).unwrap(),
        )
        .unwrap();
    let installed = PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(crate::checkpoint_selection::stage(
        &mut batch,
        &first,
        &fixture.params,
        &installed.verification
    )
    .unwrap()
    .is_none());
    drop(batch);
    let next = panel(&second);
    assert!(next.iter().all(|seed| !previous.contains(seed)));
    for (index, voter) in next.iter().enumerate() {
        vote(&mut fixture, &second, *voter, CUTOFF + 61 + index as u64);
    }
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let state = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 62),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    assert!(state.account(&address(20)).unwrap().state.is_verified());
    assert_eq!(
        state
            .brn
            .get_wallet(&address(previous[0]))
            .unwrap()
            .total_staked,
        0
    );
    assert_eq!(
        state
            .brn
            .get_wallet(&address(previous[0]))
            .unwrap()
            .total_burned,
        fixture.params.verifier_stake_amount + u128::from(previous[0] == 1 || previous[0] == 2)
    );
    assert_eq!(state.pending.len(), 2);
    let outcome = fixture.blocks.last().unwrap().hash;
    for voter in next {
        assert_eq!(
            state.pending[&(address(voter), outcome.into_tx_hash())].amount,
            fixture.params.verifier_stake_amount / 2
        );
    }
    assert!(!state
        .pending
        .keys()
        .any(|(wallet, _)| wallet == &address(previous[1])));
}

#[test]
fn deadline_all_absent_completion_is_certified_restartable_and_transferred_without_a_relay() {
    let (fixture, _dir, store, selection, first) = setup();
    let activated = activate(&fixture, &store);
    let context = selection
        .replayed()
        .request()
        .vote_context(&selection.certificate().checkpoint)
        .unwrap();
    assert!(
        PreparedDeadline::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 90)
            .unwrap()
            .is_none()
    );
    let candidate =
        PreparedDeadline::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 91)
            .unwrap()
            .unwrap();
    assert_eq!(store.meta_store().payment_epoch().unwrap(), activated);
    assert!(store
        .meta_store()
        .get_meta(&verification_deadline::key(context))
        .is_err());
    for mutation in 0..4 {
        let mut forged = candidate.manifest().clone();
        match mutation {
            0 => forged.checkpoint.cutoff -= 1,
            1 => forged.checkpoint.selection_root = BlockHash::ZERO,
            2 => forged.checkpoint.parent = BlockHash::ZERO,
            _ => forged.checkpoint.next_roster = BlockHash::ZERO,
        }
        assert!(PreparedDeadline::replay_manifest(&store, &fixture.params, &forged).is_err());
    }
    let epoch = complete(&fixture, &store);
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let mut state = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 92),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    let process = state.verification.get_state(&address(20)).unwrap();
    assert_eq!(process.revote_count, 1);
    assert!(process.selected_verifiers.is_empty());
    assert!(!state.verification.completed_selection(
        &address(20),
        context,
        &panel(&selection)
            .iter()
            .map(|seed| address(*seed))
            .collect::<Vec<_>>()
    ));
    let request = state.prepare_selection(&address(20)).unwrap();
    assert_eq!(request.attempt(), 1);
    assert!(!fixture
        .blocks
        .iter()
        .any(|block| block.hash == request.trigger()));
    assert!(
        PreparedDeadline::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 92)
            .unwrap()
            .is_none()
    );
    let bytes = store
        .meta_store()
        .get_meta(&verification_deadline::key(context))
        .unwrap();
    let completion = verification_deadline::decode(&bytes).unwrap();
    assert_eq!(completion.absent.len(), 2);
    assert_eq!(completion.completed_at, CUTOFF + 91);
    let dir = tempfile::tempdir().unwrap();
    let fresh = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let mut batch = fresh.tx_begin_write().unwrap();
    crate::payment_epoch::initialize(&mut batch, fixture.blocks[0].hash).unwrap();
    batch.commit().unwrap();
    for height in 1..=3 {
        let transfer = if height == 1 {
            first.clone()
        } else {
            CertifiedCheckpoint::export_transfer(&store, NetworkId::Dev, &fixture.params, height)
                .unwrap()
                .unwrap()
        };
        CertifiedCheckpoint::import_transfer(
            &fresh,
            NetworkId::Dev,
            &fixture.params,
            height,
            &transfer,
            &HistoricalAdmissions::default(),
        )
        .unwrap();
        PreparedInstallation::next(&fresh, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap()
            .commit(&fresh)
            .unwrap();
    }
    drop(fresh);
    let fresh = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    assert_eq!(fresh.meta_store().payment_epoch().unwrap(), epoch);
    assert_eq!(
        fresh
            .meta_store()
            .get_meta(&verification_deadline::key(context))
            .unwrap(),
        bytes
    );
    assert!(
        PreparedInstallation::next(&fresh, NetworkId::Dev, &fixture.params)
            .unwrap()
            .is_none()
    );
}

#[test]
fn deadline_partial_failed_panel_slashes_only_cast_stakes_after_certified_installation() {
    let (mut fixture, _dir, store, selection, _) = setup_config(|params| {
        params.verification_timeout_secs = 60;
        params.max_revotes = 0;
    });
    let activation = activate(&fixture, &store);
    let members = panel(&selection);
    let voter = members[0];
    vote_choice_in_epoch(&mut fixture, &selection, voter, CUTOFF + 32, 1, activation);
    fixture.append_confirmed_prefix(&store, CUTOFF + 32);
    let before = store.account_store().get_account(&address(voter)).unwrap();
    assert_eq!(
        before.total_brn_staked,
        fixture.params.verifier_stake_amount
    );
    let epoch = complete(&fixture, &store);
    let after = store.account_store().get_account(&address(voter)).unwrap();
    assert_eq!(after.total_brn_staked, 0);
    assert_eq!(
        after.total_brn_burned,
        before.total_brn_burned + fixture.params.verifier_stake_amount
    );
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let replay = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 92),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    assert_eq!(replay.payment_epoch(), epoch);
    assert_eq!(
        replay.verification.get_state(&address(20)).unwrap().phase,
        burst_verification::state::VerificationPhase::Failed
    );
    assert_eq!(
        replay
            .account(&address(members[1]))
            .unwrap()
            .total_brn_staked,
        0
    );
    assert_eq!(replay.pending.len(), 0);
}

#[test]
fn deadline_partial_success_verifies_at_the_certified_cut_and_preserves_refund_authority() {
    let (mut fixture, _dir, store, selection, _) = setup_config(|params| {
        params.verification_timeout_secs = 60;
        params.num_verifiers = 3;
        params.verification_threshold_bps = 6600;
    });
    let activation = activate(&fixture, &store);
    let members = panel(&selection);
    for voter in &members[..2] {
        vote_choice_in_epoch(&mut fixture, &selection, *voter, CUTOFF + 32, 1, activation);
    }
    fixture.append_confirmed_prefix(&store, CUTOFF + 32);
    let epoch = complete(&fixture, &store);
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let replay = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 92),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    assert!(replay.account(&address(20)).unwrap().state.is_verified());
    assert_eq!(
        replay.brn.get_wallet(&address(20)).unwrap().verified_at,
        Timestamp::new(CUTOFF + 91)
    );
    for voter in &members[..2] {
        let wallet = replay.brn.get_wallet(&address(*voter)).unwrap();
        assert_eq!(wallet.total_staked, 0);
        assert_eq!(wallet.total_burned, u128::from(*voter == 1 || *voter == 2));
    }
    fixture.push(20, BlockType::Open, CUTOFF + 92, |block| {
        block.payment_epoch = epoch;
    });
    fixture.push(20, BlockType::Burn, CUTOFF + 93, |block| {
        block.payment_epoch = epoch;
        block.brn_spent = 1;
    });
    let replay = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 93),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    assert_eq!(replay.account(&address(20)).unwrap().total_brn_burned, 1);
    fixture.blocks.last_mut().unwrap().payment_epoch = activation;
    sign(20, fixture.blocks.last_mut().unwrap());
    assert!(ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 93),
        NetworkId::Dev,
        &fixture.params,
        &events
    )
    .is_err());
}
