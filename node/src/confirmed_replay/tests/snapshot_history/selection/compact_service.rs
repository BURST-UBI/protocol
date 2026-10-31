use super::*;
use crate::{
    checkpoint_service::{self, Clock, Session, Settings},
    checkpoint_transport::Incoming,
    shutdown::ShutdownController,
    wire_message::WireMessage,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
    sync::mpsc,
};

mod observer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_ahead_session_still_requests_peer_evidence() {
    use crate::{
        checkpoint_driver::CheckpointDriver,
        checkpoint_transport::{Body, Packet},
    };
    use std::time::Instant;
    let (fixture, _source_dir, source, manifest, certificate) = setup();
    let snapshot = authenticate(&source, &fixture.params, manifest.checkpoint.parent);
    let base = snapshot.into_continuation();
    let coverage = || {
        let snapshot = authenticate(&source, &fixture.params, manifest.checkpoint.parent);
        let retained = compact::retained(&snapshot, &fixture, &source, &[]);
        let (_dir, suffix) = suffix_store(&[]);
        let tail = base
            .read_tail(&suffix, CUTOFF, &manifest.heads, retained)
            .unwrap();
        base.selection_vote_coverage(tail, &manifest, &[]).unwrap()
    };
    let (_directory, local) = cemented::sparse(&base, &source);
    let local = Arc::new(local);
    let anchor = base.anchor();
    local
        .meta_store()
        .put_meta(
            &format!("checkpoint_chain/v1/dev/{}/head", anchor.genesis_hash()),
            &anchor.height().to_be_bytes(),
        )
        .unwrap();
    let seed = (0..9)
        .find(|seed| address(*seed) != *anchor.proposer(0))
        .unwrap();
    let driver = CheckpointDriver::new_compact_selection(
        local.clone(),
        fixture.params.clone(),
        Arc::new(burst_crypto::keypair_from_seed(&[seed; 32]).private),
        anchor.clone(),
        coverage(),
        Vec::new(),
    )
    .unwrap();
    // Make the five-second resume due without a wall-clock sleep. Phase timeout
    // remains sixty seconds, so only the stale resume can reject this poll.
    let session = Session::new(
        driver,
        Instant::now() - Duration::from_secs(6),
        Duration::from_secs(60),
    )
    .unwrap();
    crate::checkpoint_store::compact::accept_selection(&local, anchor, &coverage(), &certificate)
        .unwrap();
    let metadata = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    assert!(
        checkpoint_agreement::read_state(&local, anchor, &address(seed))
            .unwrap()
            .unwrap()
            .decision()
            .is_none()
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (mut peer, socket) = tokio::try_join!(
        TcpStream::connect(listener.local_addr().unwrap()),
        listener.accept(),
    )
    .unwrap();
    let (_reader, writer) = socket.0.into_split();
    let registry = Arc::new(tokio::sync::RwLock::new(
        crate::connection_registry::ConnectionRegistry::new(),
    ));
    registry
        .write()
        .await
        .insert("evidence-peer".into(), writer);
    let (sender, receiver) = mpsc::channel(checkpoint_service::INBOX_CAPACITY);
    let context = crate::checkpoint_transport::context_from_anchor(anchor).unwrap();
    let digest = BlockHash::new([91; 32]);
    sender
        .send(Incoming {
            peer: "evidence-peer".into(),
            packet: Packet {
                context,
                body: Body::Offer { digest, total: 1 },
            },
        })
        .await
        .unwrap();
    let shutdown = Arc::new(ShutdownController::new());
    let clock: Clock = Arc::new(|| None);
    let worker = tokio::spawn(checkpoint_service::run(
        session,
        registry.clone(),
        receiver,
        clock,
        shutdown.clone(),
    ));
    let response = tokio::time::timeout(Duration::from_secs(2), async {
        let size = peer.read_u32().await.unwrap();
        assert!(size as usize <= burst_types::MAX_P2P_MESSAGE_BYTES);
        let mut bytes = vec![0; size as usize];
        peer.read_exact(&mut bytes).await.unwrap();
        WireMessage::decode(&bytes).unwrap()
    })
    .await;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    registry.write().await.close();
    assert_eq!(local.meta_store().payment_epoch().unwrap(), anchor.hash());
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        metadata
    );
    let WireMessage::CheckpointAgreement(packet) =
        response.expect("stale signing resume blocked checkpoint evidence retrieval")
    else {
        panic!("unexpected wire message");
    };
    assert_eq!(packet.context, context);
    assert!(
        matches!(packet.body, Body::Request { digest: requested, offset: 0 } if requested == digest)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_service_agrees_and_persists_over_tcp_without_installing_economics() {
    let (fixture, _source_dir, source, manifest, _) = setup();
    let base =
        authenticate(&source, &fixture.params, manifest.checkpoint.parent).into_continuation();
    let (_alternative_dir, alternative_store, heads) = fixture.store();
    let prefix = format!("checkpoint_chain/v1/dev/{}", base.anchor().genesis_hash());
    alternative_store
        .meta_store()
        .put_meta(
            &format!("{prefix}/1"),
            &source
                .meta_store()
                .get_meta(&format!("{prefix}/1"))
                .unwrap(),
        )
        .unwrap();
    alternative_store
        .meta_store()
        .put_meta(&format!("{prefix}/head"), &1u64.to_be_bytes())
        .unwrap();
    let alternative = CertifiedCheckpoint::prepare(
        &alternative_store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 1,
        &heads,
        &address(20),
    )
    .unwrap()
    .manifest();
    assert_ne!(alternative.checkpoint, manifest.checkpoint);
    assert_service(
        &fixture,
        Arc::new(source),
        ReplayManifest::Selection(manifest),
        ReplayManifest::Selection(alternative),
        CUTOFF,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_deadline_service_agrees_and_recovers_over_tcp() {
    let mut fixture = Fixture::selection_with(|params| params.trst_expiry_secs = 90);
    fixture.push(0, BlockType::Burn, CUTOFF - 1, |block| {
        block.brn_spent += 40;
        block.link = destination(21);
    });
    let (_source_dir, source, _) = snapshot(&fixture);
    let prepare = |cutoff| {
        crate::checkpoint_deadline::PreparedDeadline::prepare_next(
            &source,
            NetworkId::Dev,
            &fixture.params,
            cutoff,
        )
        .unwrap()
        .unwrap()
        .manifest()
        .clone()
    };
    let manifest = prepare(CUTOFF + 91);
    let alternative = prepare(CUTOFF + 92);
    assert_service(
        &fixture,
        Arc::new(source),
        ReplayManifest::Deadline(manifest),
        ReplayManifest::Deadline(alternative),
        CUTOFF + 94,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_roster_service_agrees_and_recovers_over_tcp() {
    let mut fixture = Fixture::selection();
    let (_dir, source, snapshot) = snapshot(&fixture);
    let epoch = snapshot.into_continuation().anchor().hash();
    fixture.push(1, BlockType::ChangeRepresentative, CUTOFF + 2, |block| {
        block.payment_epoch = epoch;
        block.representative = address(0);
    });
    fixture.append_confirmed_prefix(&source, CUTOFF + 3);
    let prepare = |cutoff| {
        PreparedRoster::prepare_next(&source, NetworkId::Dev, &fixture.params, cutoff)
            .unwrap()
            .unwrap()
            .manifest()
            .clone()
    };
    let manifest = prepare(CUTOFF + 4);
    let alternative = prepare(CUTOFF + 5);
    assert_service(
        &fixture,
        Arc::new(source),
        ReplayManifest::Roster(manifest),
        ReplayManifest::Roster(alternative),
        CUTOFF + 8,
    )
    .await;
}

pub(in crate::confirmed_replay::tests) async fn assert_service(
    fixture: &Fixture,
    source: Arc<LmdbStore>,
    manifest: ReplayManifest,
    alternative: ReplayManifest,
    clock_at: u64,
) {
    assert_discovery(fixture, &source, &manifest);
    use crate::confirmed_replay::tests::snapshot_history::installed_recovery::WitnessService;
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let base =
        authenticate(&source, &fixture.params, manifest.checkpoint().parent).into_continuation();
    let proof_records: Vec<_> =
        crate::checkpoint_store::chain::load(&source, NetworkId::Dev, &fixture.params)
            .unwrap()
            .into_iter()
            .filter(|event| {
                event.certificate().checkpoint.height <= base.anchor().height()
                    && event
                        .selection()
                        .is_some_and(|selection| selection.beacon().is_some())
            })
            .map(|event| {
                let key = format!(
                    "checkpoint_chain/v1/dev/{}/{}",
                    base.anchor().genesis_hash(),
                    event.certificate().checkpoint.height
                );
                let bytes = source.meta_store().get_meta(&key).unwrap();
                (key, bytes, event.certificate().checkpoint.hash())
            })
            .collect();
    let proposer = (0..9)
        .find(|seed| address(*seed) == *base.anchor().proposer(0))
        .unwrap();
    let seeds: Vec<_> = std::iter::once(proposer)
        .chain((0..9).filter(|seed| *seed != proposer))
        .take(7)
        .collect();
    let mut directories = Vec::new();
    let mut stores = Vec::new();
    let mut sessions = Vec::new();
    let mut idle_workers = HashMap::new();
    let mut registries = Vec::new();
    let mut stops = Vec::new();
    let mut senders = Vec::new();
    let mut receivers = Vec::new();
    let mut account_snapshots = Vec::new();
    let mut proof_services = Vec::new();
    for seed in &seeds {
        let (dir, local) = cemented::sparse(&base, &source);
        for (key, bytes, hash) in &proof_records {
            if *seed != proposer
                && matches!(&manifest, ReplayManifest::Activation(activation) if activation.activation.selection == *hash)
            {
                continue;
            }
            local.meta_store().put_meta(key, bytes).unwrap();
        }
        let mut heights = HashMap::new();
        for block in &fixture.blocks {
            let height = heights.entry(block.account.clone()).or_insert(0u64);
            *height += 1;
            let floor = base
                .state()
                .account(&block.account)
                .map_or(0, |account| account.block_count);
            if *height > floor {
                if *seed == proposer {
                    cemented::append(&local, block, true);
                }
                local
                    .meta_store()
                    .put_meta(
                        &format!("checkpoint_history/v1/{}", block.hash),
                        &bincode::serialize(block).unwrap(),
                    )
                    .unwrap();
            }
        }
        local
            .meta_store()
            .put_meta(
                &format!(
                    "checkpoint_chain/v1/dev/{}/head",
                    base.anchor().genesis_hash()
                ),
                &base.anchor().height().to_be_bytes(),
            )
            .unwrap();
        let recovered = authenticate(&source, &fixture.params, manifest.checkpoint().parent)
            .recover_stored_successors(&local, |_, _, _| unreachable!())
            .unwrap();
        let local = Arc::new(local);
        account_snapshots.push(
            bincode::serialize(
                &local
                    .tx_begin_write()
                    .unwrap()
                    .bounded_accounts(131_072)
                    .unwrap(),
            )
            .unwrap(),
        );
        let proof_service = WitnessService::start(source.clone(), local.clone(), 0).await;
        let registry = proof_service.registry.clone();
        proof_services.push(proof_service);
        let settings = || Settings {
            store: local.clone(),
            network: NetworkId::Dev,
            initial_params: fixture.params.clone(),
            key: Some(Arc::new(
                burst_crypto::keypair_from_seed(&[*seed; 32]).private,
            )),
        };
        let session = if *seed == proposer {
            let before = local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap();
            let checkpoint_service::CompactStart::Idle(recovered) =
                Session::resume_or_discover_compact(
                    settings(),
                    registry.clone(),
                    recovered,
                    None,
                    Duration::from_secs(30),
                )
                .await
                .unwrap()
            else {
                panic!("fresh startup signed without a clock");
            };
            assert_eq!(
                local
                    .tx_begin_write()
                    .unwrap()
                    .checkpoint_metadata(b"")
                    .unwrap(),
                before
            );
            idle_workers.insert(*seed, (settings(), Arc::new(*recovered)));
            None
        } else if *seed == *seeds.last().unwrap() {
            idle_workers.insert(*seed, (settings(), Arc::new(recovered)));
            None
        } else {
            Some(
                Session::from_compact_candidate(
                    settings(),
                    registry.clone(),
                    recovered,
                    Some(alternative.clone()),
                    Duration::from_secs(30),
                )
                .await
                .unwrap(),
            )
        };
        sessions.push(session);
        let (sender, receiver) = mpsc::channel(checkpoint_service::INBOX_CAPACITY);
        registry.write().await.set_agreement_inbox(sender.clone());
        directories.push(dir);
        stores.push(local);
        registries.push(registry);
        stops.push(Arc::new(ShutdownController::new()));
        senders.push(sender);
        receivers.push(receiver);
    }
    let mut readers = tokio::task::JoinSet::new();
    for left in 0..seeds.len() {
        for right in left + 1..seeds.len() {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let (outgoing, incoming) = tokio::time::timeout(Duration::from_secs(5), async {
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
                readers.spawn(async move {
                    while let Ok(size) = reader.read_u32().await {
                        assert!(size as usize <= burst_types::MAX_P2P_MESSAGE_BYTES);
                        let mut body = vec![0; size as usize];
                        if reader.read_exact(&mut body).await.is_err() {
                            break;
                        }
                        let WireMessage::CheckpointAgreement(packet) =
                            WireMessage::decode(&body).unwrap()
                        else {
                            panic!("unexpected compact agreement frame")
                        };
                        let _ = sender.try_send(Incoming {
                            peer: peer.to_string(),
                            packet,
                        });
                    }
                });
            }
        }
    }
    let mut workers = Vec::new();
    let mut pending_clock = None;
    for (index, (session, receiver)) in sessions.into_iter().zip(receivers).enumerate() {
        let clock: Clock = Arc::new(move || {
            Some(crate::checkpoint_replay::SigningClock {
                now: clock_at,
                error_bound: 0,
            })
        });
        if let Some(session) = session {
            workers.push(tokio::spawn(checkpoint_service::run(
                session,
                registries[index].clone(),
                receiver,
                clock,
                stops[index].clone(),
            )));
        } else {
            let (settings, recovered) = idle_workers.remove(&seeds[index]).unwrap();
            let attempts = std::sync::atomic::AtomicUsize::new(0);
            let now = manifest.checkpoint().cutoff
                + u64::from(!matches!(manifest, ReplayManifest::Selection(_)));
            let clock: Clock = if index == 0 {
                Arc::new(move || {
                    (attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0).then_some(
                        crate::checkpoint_replay::SigningClock {
                            now,
                            error_bound: 0,
                        },
                    )
                })
            } else {
                let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
                pending_clock = Some((index, ready.clone()));
                Arc::new(move || {
                    ready.load(std::sync::atomic::Ordering::SeqCst).then_some(
                        crate::checkpoint_replay::SigningClock {
                            now: clock_at,
                            error_bound: 0,
                        },
                    )
                })
            };
            workers.push(tokio::spawn(checkpoint_service::run_compact_height(
                settings,
                recovered,
                registries[index].clone(),
                receiver,
                clock,
                stops[index].clone(),
                Duration::from_secs(30),
            )));
        }
    }
    let completed = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some((index, ready)) = &pending_clock {
                let has_journal = !stores[*index]
                    .tx_begin_write()
                    .unwrap()
                    .checkpoint_metadata(b"checkpoint_round/v1/")
                    .unwrap()
                    .is_empty();
                if has_journal {
                    ready.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            if stores.iter().all(|store| {
                CertifiedCheckpoint::stored_tip(store, NetworkId::Dev, &fixture.params).unwrap()
                    == manifest.checkpoint().height
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    for stop in &stops {
        stop.shutdown();
    }
    for worker in workers {
        tokio::time::timeout(Duration::from_secs(5), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    for registry in &registries {
        registry.write().await.close();
    }
    drop(registries);
    let mut proof_requests = 0;
    for service in proof_services {
        proof_requests += service.stop().await;
    }
    readers.abort_all();
    while let Some(result) = readers.join_next().await {
        if let Err(error) = result {
            assert!(error.is_cancelled(), "socket reader failed: {error}");
        }
    }
    completed.expect("compact service did not persist a human-quorum decision");
    if !matches!(manifest, ReplayManifest::Selection(_)) {
        assert!(proof_requests > 0);
    }
    drop(stores);
    for (index, dir) in directories.iter().enumerate() {
        let local = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
        let state = checkpoint_agreement::read_state(&local, base.anchor(), &address(seeds[index]))
            .unwrap()
            .unwrap();
        let decision = state.decision().unwrap();
        if index == 0 {
            Box::pin(observer::assert_observer(
                fixture,
                source.clone(),
                &manifest,
                decision,
            ))
            .await;
        }
        base.anchor().verify_certificate(decision).unwrap();
        assert_eq!(&decision.checkpoint, manifest.checkpoint());
        assert_eq!(
            local.meta_store().payment_epoch().unwrap(),
            base.anchor().hash()
        );
        assert_eq!(
            CertifiedCheckpoint::stored_tip(&local, NetworkId::Dev, &fixture.params).unwrap(),
            manifest.checkpoint().height
        );
        assert_eq!(
            bincode::serialize(
                &local
                    .tx_begin_write()
                    .unwrap()
                    .bounded_accounts(131_072)
                    .unwrap()
            )
            .unwrap(),
            account_snapshots[index]
        );
        let recovered = authenticate(&source, &fixture.params, manifest.checkpoint().parent)
            .recover_stored_epoch(&local, base.anchor().hash(), |_, _, _| unreachable!())
            .unwrap();
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let proof_service = WitnessService::start(source.clone(), local.clone(), 0).await;
        let resumed = Session::resume_or_discover_compact(
            Settings {
                store: local.clone(),
                network: NetworkId::Dev,
                initial_params: fixture.params.clone(),
                key: Some(Arc::new(
                    burst_crypto::keypair_from_seed(&[seeds[index]; 32]).private,
                )),
            },
            proof_service.registry.clone(),
            recovered,
            None,
            Duration::from_secs(30),
        )
        .await
        .unwrap();
        assert!(matches!(
            resumed,
            checkpoint_service::CompactStart::Ready(_)
        ));
        drop(resumed);
        proof_service.stop().await;
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

fn assert_discovery(fixture: &Fixture, source: &LmdbStore, expected: &ReplayManifest) {
    let snapshot = authenticate(source, &fixture.params, expected.checkpoint().parent);
    let mut selections = snapshot.recover_selections().unwrap();
    let events =
        crate::checkpoint_store::chain::load(source, NetworkId::Dev, &fixture.params).unwrap();
    for selection in &mut selections {
        if let Some(beacon) = events
            .iter()
            .filter_map(crate::checkpoint_store::chain::CertifiedEvent::selection)
            .find(|event| {
                event.certificate().checkpoint.hash() == selection.certificate().checkpoint.hash()
            })
            .and_then(|event| event.beacon())
        {
            selection.attach_compact_beacon(beacon.clone()).unwrap();
        }
    }
    let base = snapshot.into_continuation();
    let (_dir, local) = cemented::sparse(&base, source);
    let mut heights = HashMap::new();
    for block in &fixture.blocks {
        let height = heights.entry(block.account.clone()).or_insert(0u64);
        *height += 1;
        if *height
            > base
                .state()
                .account(&block.account)
                .map_or(0, |account| account.block_count)
        {
            cemented::append(&local, block, true);
        }
    }
    let cemented = cemented::finish(&base, &local, source);
    let before = base.state().snapshot_bytes().unwrap();
    let metadata = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let now =
        expected.checkpoint().cutoff + u64::from(!matches!(expected, ReplayManifest::Selection(_)));
    let candidate = base
        .prepare_next_cemented_candidate(
            &local,
            &cemented,
            &selections.iter().collect::<Vec<_>>(),
            crate::checkpoint_replay::SigningClock {
                now,
                error_bound: 0,
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(&candidate.input().manifest(), expected);
    assert_eq!(base.state().snapshot_bytes().unwrap(), before);
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        metadata
    );
}
