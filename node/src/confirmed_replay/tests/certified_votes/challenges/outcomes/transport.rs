use super::*;
use crate::{
    checkpoint_agreement::{self, ReplayManifest, ReplayedCandidate},
    checkpoint_driver::{CheckpointDriver, Message},
    checkpoint_replay::SigningClock,
    checkpoint_service::{self, Clock, Session},
    checkpoint_transport::Incoming,
    connection_registry::ConnectionRegistry,
    shutdown::ShutdownController,
    wire_message::WireMessage,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
    sync::{mpsc, RwLock},
};

fn driver(
    store: Arc<LmdbStore>,
    params: &ProtocolParams,
    seed: u8,
    heads: &[HistoryHead],
) -> CheckpointDriver {
    let candidate = PreparedChallengeOutcome::prepare(
        &store,
        NetworkId::Dev,
        params,
        CUTOFF + 70,
        heads,
        &address(20),
    )
    .unwrap();
    CheckpointDriver::new_candidate(
        store,
        params.clone(),
        burst_crypto::keypair_from_seed(&[seed; 32]).private,
        ReplayedCandidate::ChallengeOutcome(Box::new(candidate)),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outcome_workers_agree_over_tcp_after_seven_humans_and_recover_without_installing() {
    outcome_workers(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outcome_runtime_discovers_panels_persists_decisions_and_pauses_epoch_over_tcp() {
    outcome_workers(true).await;
}

async fn outcome_workers(runtime: bool) {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let (mut fixture, chain) = completed(ChallengeReason::Fraud);
    merged_payments(&mut fixture);
    let expected = PreparedChallengeOutcome::build(
        &fixture.history(CUTOFF + 70),
        &fixture.params,
        &chain,
        &address(20),
    )
    .unwrap();
    let anchor = expected.parent().clone();
    let proposer = (0..=8)
        .chain([20])
        .find(|seed| address(*seed) == *anchor.proposer(0))
        .unwrap();
    let seeds: Vec<_> = std::iter::once(proposer)
        .chain((0..=8).chain([20]).filter(|seed| *seed != proposer))
        .take(8)
        .collect();
    let mut dirs = Vec::new();
    let mut stores = Vec::new();
    let mut drivers = Vec::new();
    let mut heads = Vec::new();
    let mut snapshots = Vec::new();
    for seed in &seeds {
        let (dir, store, cut) = if runtime {
            super::installation::installed_chain(&fixture, &chain)
        } else {
            restore_chain(&fixture, &chain)
        };
        let key = burst_crypto::keypair_from_seed(&[*seed; 32]);
        let mut voter = burst_consensus::VoteGenerator::new(address(*seed), key.private.0);
        crate::durable_votes::generate_vote(
            &store,
            &mut voter,
            fixture.blocks.last().unwrap().hash,
            true,
        )
        .unwrap();
        snapshots.push(
            bincode::serialize(&store.account_store().get_account(&address(20)).unwrap()).unwrap(),
        );
        let store = Arc::new(store);
        drivers.push((!runtime).then(|| driver(store.clone(), &fixture.params, *seed, &cut)));
        stores.push(Some(store));
        dirs.push(dir);
        heads.push(cut);
    }
    let registries: Vec<_> = seeds
        .iter()
        .map(|_| Arc::new(RwLock::new(ConnectionRegistry::new())))
        .collect();
    let stops: Vec<_> = seeds
        .iter()
        .map(|_| Arc::new(ShutdownController::new()))
        .collect();
    if runtime {
        for index in 0..seeds.len() {
            crate::checkpoint_sync::witness::start(
                stores[index].as_ref().unwrap().clone(),
                registries[index].clone(),
                stops[index].clone(),
            )
            .await;
        }
    }
    let (senders, mut receivers): (Vec<_>, Vec<_>) = seeds
        .iter()
        .map(|_| {
            let (sender, receiver) = mpsc::channel(checkpoint_service::INBOX_CAPACITY);
            (sender, Some(receiver))
        })
        .unzip();
    let mut readers = tokio::task::JoinSet::new();
    for left in 0..seeds.len() {
        registries[left]
            .write()
            .await
            .set_agreement_inbox(senders[left].clone());
        for right in left + 1..seeds.len() {
            // Preconnected sockets isolate agreement delivery from peer discovery.
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let (outgoing, incoming) = tokio::time::timeout(Duration::from_secs(10), async {
                tokio::try_join!(
                    TcpStream::connect(listener.local_addr().unwrap()),
                    listener.accept()
                )
            })
            .await
            .unwrap()
            .unwrap();
            for (owner, peer, stream) in [(left, right, outgoing), (right, left, incoming.0)] {
                let (mut reader, writer) = stream.into_split();
                registries[owner]
                    .write()
                    .await
                    .insert(peer.to_string(), writer);
                let sender = senders[owner].clone();
                let registry = registries[owner].clone();
                readers.spawn(async move {
                    while let Ok(size) = reader.read_u32().await {
                        assert!(size as usize <= burst_types::MAX_P2P_MESSAGE_BYTES);
                        let mut body = vec![0; size as usize];
                        if reader.read_exact(&mut body).await.is_err() {
                            break;
                        }
                        match crate::wire_message::WireMessage::decode(&body).unwrap() {
                            WireMessage::CheckpointAgreement(packet) => {
                                let _ = sender.try_send(Incoming {
                                    peer: peer.to_string(),
                                    packet,
                                });
                            }
                            WireMessage::CheckpointSync(message) if runtime => {
                                registry
                                    .read()
                                    .await
                                    .dispatch_checkpoint(&peer.to_string(), message);
                            }
                            _ => panic!("unexpected outcome agreement frame"),
                        }
                    }
                });
            }
        }
    }
    let mut workers = Vec::new();
    let clock_started = Instant::now();
    for stage in 0..3 {
        let slots = match stage {
            0 => 0..6,
            1 => 6..7,
            _ => 7..8,
        };
        for index in slots {
            // Historical fixture clock; the late signer has no clock authority.
            let clock: Clock = Arc::new(move || {
                (stage != 2).then_some(SigningClock {
                    now: CUTOFF
                        + 75
                        + if runtime {
                            clock_started.elapsed().as_secs()
                        } else {
                            0
                        },
                    error_bound: 0,
                })
            });
            if runtime {
                workers.push(tokio::spawn(checkpoint_service::run_node(
                    checkpoint_service::Settings {
                        store: stores[index].as_ref().unwrap().clone(),
                        network: NetworkId::Dev,
                        initial_params: fixture.params.clone(),
                        key: (stage != 2).then(|| {
                            Arc::new(burst_crypto::keypair_from_seed(&[seeds[index]; 32]).private)
                        }),
                    },
                    registries[index].clone(),
                    receivers[index].take().unwrap(),
                    clock,
                    stops[index].clone(),
                )));
            } else {
                let session = Session::new(
                    drivers[index].take().unwrap(),
                    Instant::now(),
                    Duration::from_secs(60),
                )
                .unwrap();
                workers.push(tokio::spawn(checkpoint_service::run(
                    session,
                    registries[index].clone(),
                    receivers[index].take().unwrap(),
                    clock,
                    stops[index].clone(),
                )));
            }
        }
        let count = match stage {
            0 => 6,
            1 => 7,
            _ => 8,
        };
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if let Some(index) = workers.iter().position(|worker| worker.is_finished()) {
                let result = workers.swap_remove(index).await;
                panic!("outcome agreement worker exited in stage {stage}: {result:?}");
            }
            let states: Vec<_> = (0..count)
                .map(|index| {
                    checkpoint_agreement::read_state(
                        stores[index].as_ref().unwrap(),
                        &anchor,
                        &address(seeds[index]),
                    )
                    .unwrap()
                })
                .collect();
            if stage == 0 {
                assert!(states.iter().all(|state| state
                    .as_ref()
                    .is_none_or(|state| state.decision().is_none())));
                if states.iter().all(|state| {
                    state
                        .as_ref()
                        .is_some_and(|state| state.prevote().is_some())
                }) {
                    break;
                }
            } else if if runtime {
                (0..count).all(|index| {
                    CertifiedCheckpoint::stored_tip(
                        stores[index].as_ref().unwrap(),
                        NetworkId::Dev,
                        &fixture.params,
                    )
                    .unwrap()
                        == 3
                })
            } else {
                states.iter().all(|state| {
                    state
                        .as_ref()
                        .is_some_and(|state| state.decision().is_some())
                })
            } {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "outcome TCP agreement stalled in stage {stage}"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        if stage == 0 {
            tokio::time::sleep(Duration::from_secs(2)).await;
            for index in 0..6 {
                assert!(
                    checkpoint_agreement::read_state(
                        stores[index].as_ref().unwrap(),
                        &anchor,
                        &address(seeds[index])
                    )
                    .unwrap()
                    .unwrap()
                    .decision()
                    .is_none(),
                    "six of ten humans cannot decide an outcome"
                );
            }
        }
    }
    for stop in &stops {
        stop.shutdown();
    }
    for worker in workers {
        tokio::time::timeout(Duration::from_secs(15), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    readers.abort_all();
    while let Some(result) = readers.join_next().await {
        if let Err(error) = result {
            assert!(error.is_cancelled(), "socket reader failed: {error}");
        }
    }
    for registry in &registries {
        let tasks = {
            let mut registry = registry.write().await;
            registry.close();
            registry.take_tasks()
        };
        for task in tasks {
            if let Err(error) = task.await {
                assert!(
                    error.is_cancelled(),
                    "outcome proof service failed: {error}"
                );
            }
        }
    }
    let mut canonical_decision = None;
    for index in 0..seeds.len() {
        let store = stores[index].take().unwrap();
        let decision = if runtime {
            crate::checkpoint_store::chain::load(&store, NetworkId::Dev, &fixture.params)
                .unwrap()
                .last()
                .unwrap()
                .certificate()
                .clone()
        } else {
            checkpoint_agreement::read_state(&store, &anchor, &address(seeds[index]))
                .unwrap()
                .unwrap()
                .decision()
                .unwrap()
                .clone()
        };
        if runtime {
            assert!(decision.checkpoint.cutoff >= CUTOFF + 74);
        } else {
            assert_eq!(decision.checkpoint.cutoff, CUTOFF + 74);
        }
        let replayed = PreparedChallengeOutcome::build(
            &fixture.history(decision.checkpoint.cutoff),
            &fixture.params,
            &chain,
            &address(20),
        )
        .unwrap();
        assert_eq!(decision.checkpoint, replayed.manifest().checkpoint);
        if let Some(hash) = canonical_decision {
            assert_eq!(decision.checkpoint.hash(), hash);
        }
        canonical_decision = Some(decision.checkpoint.hash());
        anchor.verify_certificate(&decision).unwrap();
        assert_eq!(
            CertifiedCheckpoint::stored_tip(&store, NetworkId::Dev, &fixture.params).unwrap(),
            if runtime { 3 } else { 2 }
        );
        assert_eq!(
            bincode::serialize(&store.account_store().get_account(&address(20)).unwrap()).unwrap(),
            snapshots[index]
        );
        let key = burst_crypto::keypair_from_seed(&[seeds[index]; 32]);
        let mut voter = burst_consensus::VoteGenerator::new(address(seeds[index]), key.private.0);
        if !runtime || index < 7 {
            assert!(crate::durable_votes::generate_vote(
                &store,
                &mut voter,
                fixture.blocks.last().unwrap().hash,
                true
            )
            .is_err());
        }
        assert_eq!(Arc::strong_count(&store), 1);
        drop(store);
        let reopened = Arc::new(LmdbStore::open(dirs[index].path(), 32, 32 << 20).unwrap());
        if !runtime || index < 7 {
            assert!(crate::durable_votes::generate_vote(
                &reopened,
                &mut voter,
                fixture.blocks.last().unwrap().hash,
                false
            )
            .is_err());
        }
        if runtime {
            let mut recovered = checkpoint_service::Runtime::new(checkpoint_service::Settings {
                store: reopened.clone(),
                network: NetworkId::Dev,
                initial_params: fixture.params.clone(),
                key: Some(Arc::new(
                    burst_crypto::keypair_from_seed(&[seeds[index]; 32]).private,
                )),
            })
            .unwrap();
            let clock: Clock = Arc::new(|| {
                Some(SigningClock {
                    now: CUTOFF + 100,
                    error_bound: 0,
                })
            });
            recovered.poll(&[], Instant::now(), &clock).unwrap();
            let chain =
                crate::checkpoint_store::chain::load(&reopened, NetworkId::Dev, &fixture.params)
                    .unwrap();
            let next = crate::checkpoint_store::chain::next_anchor(
                &chain,
                NetworkId::Dev,
                &fixture.params,
            )
            .unwrap();
            assert_eq!(next.roster().weight(&address(20)), 0);
            assert!(
                checkpoint_agreement::read_state(&reopened, &next, &address(seeds[index]))
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                CertifiedCheckpoint::stored_tip(&reopened, NetworkId::Dev, &fixture.params)
                    .unwrap(),
                3
            );
            continue;
        }
        let mut recovered = driver(reopened, &fixture.params, seeds[index], &heads[index]);
        let resumed = recovered.resume().unwrap();
        assert_eq!(resumed.len(), 1);
        let Message::Decision {
            certificate,
            manifest,
        } = &resumed[0]
        else {
            panic!("missing recovered outcome decision")
        };
        assert_eq!(certificate, &decision);
        assert_eq!(
            manifest,
            &ReplayManifest::ChallengeOutcome(replayed.manifest().clone())
        );
    }
}

#[test]
fn outcome_driver_rejects_manifest_tampering_and_selection_reinterpretation_before_signing() {
    let (fixture, chain) = completed(ChallengeReason::Fraud);
    let (_dir, source, heads) = restore_chain(&fixture, &chain);
    let expected = PreparedChallengeOutcome::prepare(
        &source,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 70,
        &heads,
        &address(20),
    )
    .unwrap();
    let proposer = (0..=8)
        .chain([20])
        .find(|seed| address(*seed) == *expected.parent().proposer(0))
        .unwrap();
    let mut leader = driver(Arc::new(source), &fixture.params, proposer, &heads);
    let messages = leader
        .propose(Some(SigningClock {
            now: CUTOFF + 75,
            error_bound: 0,
        }))
        .unwrap();
    let message = messages
        .into_iter()
        .find(|message| matches!(message, Message::Proposal { .. }))
        .unwrap();
    let (_other_dir, other, other_heads) = restore_chain(&fixture, &chain);
    let mut follower = driver(
        Arc::new(other),
        &fixture.params,
        if proposer == 0 { 1 } else { 0 },
        &other_heads,
    );
    let mut tampered = Vec::new();
    for changed in 0..3 {
        let mut bad = message.clone();
        let Message::Proposal { manifest, .. } = &mut bad else {
            unreachable!()
        };
        let ReplayManifest::ChallengeOutcome(outcome) = manifest else {
            unreachable!()
        };
        match changed {
            0 => outcome.target = address(8),
            1 => {
                outcome.heads.pop();
            }
            _ => {
                *manifest =
                    ReplayManifest::Selection(crate::checkpoint_replay::CheckpointManifest {
                        checkpoint: outcome.checkpoint.clone(),
                        heads: outcome.heads.clone(),
                        target: outcome.target.clone(),
                    });
            }
        }
        // Signature authentication alone does not establish replay semantics.
        follower.authenticate(&bad).unwrap();
        tampered.push(bad.clone());
        assert!(follower
            .receive(
                bad,
                Some(SigningClock {
                    now: CUTOFF + 75,
                    error_bound: 0
                })
            )
            .is_err());
        assert!(follower.state().unwrap().is_none());
    }
    assert!(follower
        .receive(
            message.clone(),
            Some(SigningClock {
                now: CUTOFF + 75,
                error_bound: 0
            })
        )
        .unwrap()
        .iter()
        .any(|message| matches!(message, Message::Vote(_))));
    let signed = follower.state().unwrap();
    for bad in tampered {
        // The remembered-proposal fast path must not authorize relaying a
        // different manifest merely because its signed checkpoint is familiar.
        assert!(follower.receive(bad, None).is_err());
        assert_eq!(follower.state().unwrap(), signed);
    }
    assert!(follower.receive(message, None).unwrap().is_empty());
    assert_eq!(follower.state().unwrap(), signed);
}
