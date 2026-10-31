use super::*;
use crate::checkpoint_store::CertifiedCheckpoint;
use crate::confirmed_replay::snapshot::{
    archive::AuthenticatedSnapshot,
    export::{SnapshotAttestation, SnapshotExport},
};
use burst_store::meta::MetaStore;

mod framed_archive;
mod long_transfer;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn snapshot_peer_transfer_stages_authenticated_archive_over_tcp_and_restarts_full_nodes() {
    use crate::{
        checkpoint_sync::snapshot::{Config, Fetch},
        BurstNode, NodeConfig,
    };
    use std::{
        sync::{atomic::AtomicU64, Arc},
        time::Duration,
    };
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let fixture = Fixture::selection();
    let files = tempfile::tempdir().unwrap();
    let source = files.path().join("source.snapshot");
    let output = files.path().join("download.snapshot");
    let mut directories = Vec::new();
    let mut archive = Vec::new();
    let mut tip_archive = Vec::new();
    let mut epoch = BlockHash::ZERO;
    let mut expected_state = Vec::new();
    for index in 0..2 {
        let (dir, store, heads) = fixture.store();
        let roster = crate::checkpoint_roster::PreparedRoster::prepare_next(
            &store,
            NetworkId::Dev,
            &fixture.params,
            1_595_434_049,
        )
        .unwrap()
        .unwrap();
        let archive_epoch = roster.manifest().checkpoint.hash();
        crate::checkpoint_store::chain::accept_manifest(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certified_votes::certify(&roster.manifest().checkpoint, &[0]),
            &crate::checkpoint_agreement::ReplayManifest::Roster(roster.manifest().clone()),
        )
        .unwrap();
        let candidate = CertifiedCheckpoint::prepare(
            &store,
            NetworkId::Dev,
            &fixture.params,
            1_595_434_050,
            &heads,
            &address(20),
        )
        .unwrap();
        let certificate = certified_votes::certify(candidate.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]);
        epoch = certificate.checkpoint.hash();
        CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certificate,
            heads,
            address(20),
        )
        .unwrap();
        while let Some(installation) =
            crate::confirmed_replay::installation::PreparedInstallation::next(
                &store,
                NetworkId::Dev,
                &fixture.params,
            )
            .unwrap()
        {
            installation.commit(&store).unwrap();
        }
        if index == 0 {
            let export =
                SnapshotExport::prepare(&store, NetworkId::Dev, &fixture.params, archive_epoch)
                    .unwrap();
            let signatures = (0..7)
                .map(|seed| {
                    export
                        .attest(
                            &store,
                            &burst_crypto::keypair_from_seed(&[seed; 32]).private,
                        )
                        .unwrap()
                })
                .collect::<Vec<_>>();
            export.write(&signatures, &mut archive).unwrap();
            std::fs::write(&source, &archive).unwrap();
            let tip =
                SnapshotExport::prepare(&store, NetworkId::Dev, &fixture.params, epoch).unwrap();
            let signatures = (0..7)
                .map(|seed| {
                    tip.attest(
                        &store,
                        &burst_crypto::keypair_from_seed(&[seed; 32]).private,
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            tip.write(&signatures, &mut tip_archive).unwrap();
            let events =
                crate::checkpoint_store::chain::load(&store, NetworkId::Dev, &fixture.params)
                    .unwrap();
            expected_state = ConfirmedReplay::replay_with_events(
                &fixture.history(1_595_434_050),
                NetworkId::Dev,
                &fixture.params,
                &events,
            )
            .unwrap()
            .snapshot_bytes()
            .unwrap();
        }
        store.force_sync().unwrap();
        drop(store);
        directories.push(dir);
    }
    let reservations = (0..4)
        .map(|_| std::net::TcpListener::bind("0.0.0.0:0").unwrap())
        .collect::<Vec<_>>();
    let ports = reservations
        .iter()
        .map(|listener| listener.local_addr().unwrap().port())
        .collect::<Vec<_>>();
    let mut configs = (0..2)
        .map(|index| NodeConfig {
            data_dir: directories[index].path().to_path_buf(),
            network: NetworkId::Dev,
            params: fixture.params.clone(),
            port: ports[index * 2],
            rpc_port: ports[index * 2 + 1],
            enable_upnp: false,
            enable_peer_reachout: false,
            enable_representative: false,
            nts_sources: vec![],
            drand_relays: vec!["http://127.0.0.1:1".into()],
            snapshot_transfer: if index == 0 {
                Config {
                    max_archive_bytes: Config::default().max_archive_bytes,
                    serve: Some(source.clone()),
                    fetch: None,
                }
            } else {
                Config {
                    max_archive_bytes: Config::default().max_archive_bytes,
                    serve: None,
                    fetch: Some(Fetch {
                        epoch: None,
                        minimum_height: 1,
                        output: output.clone(),
                    }),
                }
            },
            bootstrap_peers: if index == 0 {
                vec![]
            } else {
                vec![format!("127.0.0.1:{}", ports[0])]
            },
            ..NodeConfig::default()
        })
        .collect::<Vec<_>>();
    drop(reservations);
    let mut tasks = Vec::new();
    let mut stops = Vec::new();
    let mut stores = Vec::new();
    let mut registries = Vec::new();
    for config in &configs {
        let mut node = BurstNode::new(config.clone()).await.unwrap();
        node.block_processor.lock().await.test_time = Some(Arc::new(AtomicU64::new(1_595_434_051)));
        node.verification_fixture_clock = Some(Arc::new(|| None));
        stops.push(node.shutdown.clone());
        stores.push(node.store.clone());
        registries.push(node.connection_registry.clone());
        tasks.push(tokio::spawn(async move {
            let result = node
                .start_with_services(burst_daemon::start_public_services)
                .await;
            (node, result)
        }));
    }
    let downloaded = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if tasks.iter().any(|task| task.is_finished()) {
                return Err("node exited before transfer");
            }
            if output.is_file() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await;
    let proof = if downloaded.as_ref().is_ok_and(|result| result.is_ok()) {
        let genesis = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            NetworkId::Dev,
            &fixture.params,
        )
        .unwrap();
        let authenticated = AuthenticatedSnapshot::read(&genesis, &mut archive.as_slice()).unwrap();
        let cursor = authenticated.history_cursor(&address(0)).unwrap().unwrap();
        Some(
            tokio::time::timeout(Duration::from_secs(30), async {
                let proof = crate::checkpoint_sync::witness::fetch(
                    &registries[1],
                    cursor,
                    vec![fixture.blocks[0].hash],
                )
                .await?;
                let base = AuthenticatedSnapshot::read(&genesis, &mut tip_archive.as_slice())
                    .map_err(|error| burst_store::StoreError::Corruption(error.to_string()))?
                    .into_continuation();
                let (_dir, suffix) = snapshot_history::suffix_store(&[]);
                let full = fixture.history(base.anchor().cutoff());
                let plan = base
                    .plan_tail(&suffix, base.anchor().cutoff(), full.heads())
                    .map_err(|error| burst_store::StoreError::Corruption(error.to_string()))?;
                let retained =
                    crate::checkpoint_sync::witness::fetch_plan(&registries[1], &base, &plan)
                        .await?;
                let accounts = retained
                    .iter()
                    .map(|proof| &proof.block().account)
                    .collect::<HashSet<_>>()
                    .len();
                let tail = plan
                    .finish(retained)
                    .map_err(|error| burst_store::StoreError::Corruption(error.to_string()))?;
                let empty_tail = tail.block_count() == 0;
                let prefix = format!("checkpoint_chain/v1/dev/{}", fixture.blocks[0].hash);
                for key in [format!("{prefix}/2"), format!("{prefix}/head")] {
                    suffix
                        .meta_store()
                        .put_meta(&key, &stores[0].meta_store().get_meta(&key)?)?;
                }
                let suffix = Arc::new(suffix);
                let before = suffix.tx_begin_write()?.checkpoint_metadata(b"")?;
                let authenticated = AuthenticatedSnapshot::read(&genesis, &mut archive.as_slice())
                    .map_err(|error| burst_store::StoreError::Corruption(error.to_string()))?;
                let recovered = authenticated
                    .recover_stored_successors_from_peers(
                        Arc::clone(&suffix),
                        Arc::clone(&registries[1]),
                    )
                    .await?;
                let same_state = recovered
                    .continuation
                    .state()
                    .snapshot_bytes()
                    .map_err(|error| burst_store::StoreError::Corruption(error.to_string()))?
                    == expected_state;
                let unchanged = suffix.tx_begin_write()?.checkpoint_metadata(b"")? == before;
                let mut installed_checks = true;
                for state_archive in [&archive, &tip_archive] {
                    let state =
                        AuthenticatedSnapshot::read(&genesis, &mut state_archive.as_slice())
                            .map_err(|error| {
                                burst_store::StoreError::Corruption(error.to_string())
                            })?
                            .into_continuation();
                    let (_dir, compact) = snapshot_history::cemented::sparse(&state, &stores[0]);
                    let retained = stores[0].block_store().indexed_blocks_txn(
                        &stores[0].tx_begin_read()?,
                        131_072,
                        64 << 20,
                    )?;
                    let old = retained
                        .iter()
                        .find(|row| {
                            state
                                .state()
                                .account(&row.account)
                                .is_some_and(|account| row.height + 1 < account.block_count)
                        })
                        .ok_or_else(|| {
                            burst_store::StoreError::Corruption(
                                "missing sparse-history fixture".into(),
                            )
                        })?;
                    let mut batch = compact.tx_begin_write()?;
                    batch.put_block_with_account(
                        &old.hash,
                        &old.bytes,
                        &old.account,
                        old.height,
                    )?;
                    batch.commit()?;
                    compact.meta_store().put_meta(
                        crate::confirmed_replay::snapshot::installed::BASE_KEY,
                        &archive,
                    )?;
                    for key in [format!("{prefix}/2"), format!("{prefix}/head")] {
                        compact
                            .meta_store()
                            .put_meta(&key, &stores[0].meta_store().get_meta(&key)?)?;
                    }
                    let compact = Arc::new(compact);
                    let before = compact.tx_begin_write()?.checkpoint_metadata(b"")?;
                    let rows = compact.block_store().block_count()?;
                    let checked =
                        crate::confirmed_replay::snapshot::installed::recover_metadata_from_peers(
                            Arc::clone(&compact),
                            NetworkId::Dev,
                            fixture.params.clone(),
                            Arc::clone(&registries[1]),
                        )
                        .await?
                        .ok_or_else(|| {
                            burst_store::StoreError::Corruption("compact base missing".into())
                        })?;
                    checked.verify_unchanged(&compact.tx_begin_write()?)?;
                    installed_checks &= checked.recovered().continuation.anchor().hash()
                        == state.anchor().hash()
                        && checked
                            .recovered()
                            .continuation
                            .state()
                            .snapshot_bytes()
                            .map_err(|error| {
                                burst_store::StoreError::Corruption(error.to_string())
                            })?
                            == state.state().snapshot_bytes().map_err(|error| {
                                burst_store::StoreError::Corruption(error.to_string())
                            })?
                        && compact.tx_begin_write()?.checkpoint_metadata(b"")? == before
                        && compact.block_store().block_count()? == rows;
                }
                Ok::<_, burst_store::StoreError>((
                    proof,
                    accounts,
                    empty_tail,
                    same_state,
                    recovered.continuation.anchor().hash(),
                    unchanged,
                    suffix.block_store().block_count()?,
                    installed_checks,
                ))
            })
            .await,
        )
    } else {
        None
    };
    for stop in stops {
        stop.shutdown();
    }
    for task in tasks {
        let (node, result) = tokio::time::timeout(Duration::from_secs(15), task)
            .await
            .unwrap()
            .unwrap();
        result.unwrap();
        assert_eq!(node.store.meta_store().payment_epoch().unwrap(), epoch);
        drop(node);
    }
    downloaded
        .expect("full-node snapshot transfer timed out")
        .unwrap();
    let (
        proof,
        accounts,
        empty_tail,
        same_state,
        recovered_epoch,
        unchanged,
        block_count,
        installed_checks,
    ) = proof
        .expect("archive downloaded")
        .expect("witness transfer timed out")
        .unwrap();
    assert_eq!(proof.len(), 1);
    assert_eq!(proof[0].block().hash, fixture.blocks[0].hash);
    assert!(accounts > 1);
    assert!(empty_tail && same_state && unchanged);
    assert_eq!(recovered_epoch, epoch);
    assert_eq!(block_count, 0);
    assert!(installed_checks);
    assert_eq!(std::fs::read(&output).unwrap(), archive);
    drop(stores);
    // Reopen the receiving node without a serving peer. Its completed archive
    // must be reauthenticated locally; this is not snapshot ledger installation.
    let config = configs.get_mut(1).unwrap();
    config.bootstrap_peers.clear();
    let client =
        burst_wallet_core::NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port))
            .unwrap();
    let mut node = BurstNode::new(config.clone()).await.unwrap();
    node.block_processor.lock().await.test_time = Some(Arc::new(AtomicU64::new(1_595_434_051)));
    node.verification_fixture_clock = Some(Arc::new(|| None));
    let stop = node.shutdown.clone();
    let task = tokio::spawn(async move {
        let result = node
            .start_with_services(burst_daemon::start_public_services)
            .await;
        (node, result)
    });
    let ready = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if task.is_finished() {
                return Err("restarted node exited before readiness");
            }
            if client
                .rpc_call("params_hash", serde_json::json!({}))
                .await
                .is_ok()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await;
    stop.shutdown();
    let (node, result) = tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .unwrap()
        .unwrap();
    result.unwrap();
    ready.expect("restarted node did not become ready").unwrap();
    assert_eq!(node.store.meta_store().payment_epoch().unwrap(), epoch);
    assert_eq!(std::fs::read(output).unwrap(), archive);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn snapshot_peer_transfer_stages_authenticated_archive_over_tcp_and_restarts() {
    snapshot_pair(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn snapshot_peer_transfer_stages_authenticated_archive_over_tcp_and_restarts_with_discovery()
{
    snapshot_pair(true).await;
}

async fn snapshot_pair(discovery: bool) {
    use crate::{
        checkpoint_sync::{
            self,
            snapshot::{Config, Fetch},
        },
        connection_registry::ConnectionRegistry,
        shutdown::ShutdownController,
        wire_message::WireMessage,
    };
    use std::{sync::Arc, time::Duration};
    use tokio::{
        io::AsyncReadExt,
        net::{TcpListener, TcpStream},
        sync::{mpsc, RwLock},
        task::JoinSet,
    };
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let fixture = Fixture::selection();
    let mut stores = Vec::new();
    let mut dirs = Vec::new();
    let mut epoch = BlockHash::ZERO;
    for _ in 0..2 {
        let (dir, store, heads) = fixture.store();
        let candidate = CertifiedCheckpoint::prepare(
            &store,
            NetworkId::Dev,
            &fixture.params,
            1_595_434_050,
            &heads,
            &address(20),
        )
        .unwrap();
        let certificate = certified_votes::certify(candidate.checkpoint(), &[0]);
        epoch = certificate.checkpoint.hash();
        CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certificate,
            heads,
            address(20),
        )
        .unwrap();
        stores.push(Arc::new(store));
        dirs.push(dir);
    }
    let export =
        SnapshotExport::prepare(&stores[0], NetworkId::Dev, &fixture.params, epoch).unwrap();
    let signatures = (0..7)
        .map(|seed| {
            export
                .attest(
                    &stores[0],
                    &burst_crypto::keypair_from_seed(&[seed; 32]).private,
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mut archive = Vec::new();
    export.write(&signatures, &mut archive).unwrap();
    let base = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap();
    reject_bad_peer_archives(&base, epoch, &archive);
    assert!(
        archive.len() > 16 * 1024,
        "fixture must exercise multiple frames"
    );
    let files = tempfile::tempdir().unwrap();
    let source = files.path().join("served.snapshot");
    let output = files.path().join("download.snapshot");
    std::fs::write(&source, &archive).unwrap();
    let fetch = Config {
        max_archive_bytes: Config::default().max_archive_bytes,
        serve: None,
        fetch: Some(Fetch {
            epoch: (!discovery).then(|| epoch.to_string()),
            minimum_height: 1,
            output: output.clone(),
        }),
    };
    let configs = [
        Config {
            max_archive_bytes: Config::default().max_archive_bytes,
            serve: Some(source),
            fetch: None,
        },
        fetch.clone(),
    ];
    let previous = stores
        .iter()
        .map(|store| store.meta_store().payment_epoch().unwrap())
        .collect::<Vec<_>>();
    let mut registries = Vec::new();
    let mut senders = Vec::new();
    let mut receivers = Vec::new();
    for _ in 0..2 {
        registries.push(Arc::new(RwLock::new(ConnectionRegistry::new())));
        let (sender, receiver) = mpsc::channel(checkpoint_sync::INBOX_CAPACITY);
        senders.push(sender);
        receivers.push(receiver);
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let outgoing = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (incoming, _) = listener.accept().await.unwrap();
    let mut readers = JoinSet::new();
    for (owner, socket) in [outgoing, incoming].into_iter().enumerate() {
        let peer = (1 - owner).to_string();
        let (mut reader, writer) = socket.into_split();
        registries[owner].write().await.insert(peer.clone(), writer);
        let sender = senders[owner].clone();
        readers.spawn(async move {
            while let Ok(size) = reader.read_u32().await {
                assert!(size as usize <= burst_types::MAX_P2P_MESSAGE_BYTES);
                let mut bytes = vec![0; size as usize];
                if reader.read_exact(&mut bytes).await.is_err() {
                    break;
                }
                let WireMessage::CheckpointSync(message) = WireMessage::decode(&bytes).unwrap()
                else {
                    panic!("unexpected frame")
                };
                if sender
                    .send(checkpoint_sync::Incoming {
                        peer: peer.clone(),
                        message,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
    }
    let shutdown = Arc::new(ShutdownController::new());
    let mut workers = JoinSet::new();
    for (index, receiver) in receivers.into_iter().enumerate() {
        workers.spawn(checkpoint_sync::run(
            stores[index].clone(),
            checkpoint_sync::Settings {
                network: NetworkId::Dev,
                params: fixture.params.clone(),
                snapshots: configs[index].clone(),
                recovery: None,
            },
            registries[index].clone(),
            receiver,
            shutdown.clone(),
            Arc::new(crate::checkpoint_store::historical::HistoricalAdmissions::default()),
        ));
    }
    let received = tokio::time::timeout(Duration::from_secs(15), async {
        while !output.is_file() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(result) = workers.join_next().await {
            result.unwrap();
        }
    })
    .await
    .expect("snapshot workers must stop");
    readers.abort_all();
    while let Some(result) = readers.join_next().await {
        if let Err(error) = result {
            assert!(error.is_cancelled(), "{error}");
        }
    }
    received.expect("snapshot transfer timed out");
    assert_eq!(std::fs::read(&output).unwrap(), archive);
    for (index, store) in stores.iter().enumerate() {
        assert_eq!(store.meta_store().payment_epoch().unwrap(), previous[index]);
    }
    let base = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap();
    let mut restarted = checkpoint_sync::snapshot::Transfer::new(
        fetch.clone(),
        base.clone(),
        output.parent().unwrap(),
    )
    .unwrap();
    assert!(restarted
        .poll(vec!["peer".into()], std::time::Instant::now())
        .is_none());
    let mut stale = fetch.clone();
    stale.fetch.as_mut().unwrap().minimum_height = 2;
    assert!(checkpoint_sync::snapshot::Transfer::new(
        stale,
        base.clone(),
        output.parent().unwrap()
    )
    .is_err());
    let mut wrong = fetch;
    wrong.fetch.as_mut().unwrap().epoch = Some(BlockHash::new([9; 32]).to_string());
    assert!(
        checkpoint_sync::snapshot::Transfer::new(wrong, base, output.parent().unwrap()).is_err()
    );
    assert_eq!(std::fs::read(output).unwrap(), archive);
}

fn reject_bad_peer_archives(
    base: &burst_consensus::checkpoint::CheckpointAnchor,
    epoch: BlockHash,
    archive: &[u8],
) {
    use crate::checkpoint_sync::{
        snapshot::{Config, Fetch, Transfer},
        CheckpointSyncMessage, Incoming,
    };
    use std::time::Instant;
    let header_end = 24 + u64::from_be_bytes(archive[16..24].try_into().unwrap()) as usize;
    for case in 0..7 {
        let files = tempfile::tempdir().unwrap();
        let output = files.path().join("snapshot");
        let config = Config {
            max_archive_bytes: Config::default().max_archive_bytes,
            serve: None,
            fetch: Some(Fetch {
                epoch: (case < 5).then(|| epoch.to_string()),
                minimum_height: if case == 6 { 2 } else { 1 },
                output: output.clone(),
            }),
        };
        let mut receiver = Transfer::new(config, base.clone(), files.path()).unwrap();
        let now = Instant::now();
        receiver.poll(vec!["source".into()], now).unwrap();
        let mut bytes = archive.to_vec();
        match case {
            0 => bytes[header_end - 1] ^= 1,      // header signature
            1 => *bytes.last_mut().unwrap() ^= 1, // signed body, with recomputed transport hash
            2 => {
                bytes.pop();
            } // authenticated length mismatch
            3 => std::fs::write(&output, b"existing file created during download").unwrap(),
            4 => bytes.push(0), // appended data
            _ => (),            // authenticated height must match offer and freshness floor
        }
        let digest = BlockHash::new(burst_crypto::blake2b_256(&bytes));
        if case >= 5 {
            receiver
                .handle(
                    Incoming {
                        peer: "source".into(),
                        message: CheckpointSyncMessage::SnapshotOffer {
                            epoch,
                            height: 2,
                            total: bytes.len() as u64,
                            digest,
                        },
                    },
                    now,
                )
                .unwrap()
                .unwrap();
        }
        let mut rejected_at = None;
        for (index, data) in bytes.chunks(16 * 1024).enumerate() {
            let message = CheckpointSyncMessage::SnapshotChunk {
                epoch,
                offset: (index * 16 * 1024) as u64,
                total: bytes.len() as u64,
                digest,
                data: data.to_vec(),
            };
            if receiver
                .handle(
                    Incoming {
                        peer: "source".into(),
                        message,
                    },
                    now,
                )
                .is_err()
            {
                rejected_at = Some((index + 1) * 16 * 1024);
                break;
            }
        }
        let rejected_at = rejected_at.expect("invalid peer archive was accepted");
        if matches!(case, 0 | 2 | 4 | 5 | 6) {
            assert!(
                rejected_at <= header_end.div_ceil(16 * 1024) * 16 * 1024,
                "reject bad authority/length before fetching more body"
            );
        }
        if case == 3 {
            assert_eq!(
                std::fs::read(&output).unwrap(),
                b"existing file created during download"
            );
        } else {
            assert!(!output.exists());
        }
    }
}

#[test]
#[ignore = "requires BURST_SNAPSHOT_DAEMON pointing to a freshly built executable"]
fn daemon_snapshot_attest_export_verify_journey() {
    let daemon = std::env::var_os("BURST_SNAPSHOT_DAEMON").expect("set BURST_SNAPSHOT_DAEMON");
    let fixture = Fixture::selection();
    let (dir, store, heads) = fixture.store();
    let candidate = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        1_595_434_050,
        &heads,
        &address(20),
    )
    .unwrap();
    let certificate = certified_votes::certify(candidate.checkpoint(), &[0]);
    let epoch = certificate.checkpoint.hash().to_string();
    CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certificate,
        heads,
        address(20),
    )
    .unwrap();
    store
        .meta_store()
        .put_meta("ledger_network", b"dev")
        .unwrap();
    store
        .meta_store()
        .put_meta(
            "genesis_params/v1",
            &bincode::serialize(&fixture.params).unwrap(),
        )
        .unwrap();
    store.force_sync().unwrap();
    drop(store);
    let files = tempfile::tempdir().unwrap();
    let key = files.path().join("representative.key");
    let config_path = files.path().join("node.toml");
    std::fs::write(&config_path, "database_map_size_bytes = 1610612736\n").unwrap();
    let invoke = |args: &[String], success: bool| {
        let output = std::process::Command::new(&daemon)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            success,
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let base = vec![
        "--config".to_owned(),
        config_path.to_str().unwrap().to_owned(),
        "--network".to_owned(),
        "dev".into(),
        "--data-dir".into(),
        dir.path().to_str().unwrap().into(),
    ];
    let mut signatures = Vec::new();
    for seed in 0..7u8 {
        std::fs::write(&key, hex::encode([seed; 32])).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let output = files.path().join(format!("{seed}.attestation"));
        let mut args = base.clone();
        args.extend([
            "--representative-key-file".into(),
            key.to_str().unwrap().into(),
            "snapshot".into(),
            "attest".into(),
            "--epoch".into(),
            epoch.clone(),
            "--output".into(),
            output.to_str().unwrap().into(),
        ]);
        invoke(&args, true);
        let original = std::fs::read(&output).unwrap();
        invoke(&args, false);
        assert_eq!(std::fs::read(&output).unwrap(), original);
        signatures.push(output);
    }
    let archive = files.path().join("economy.snapshot");
    let params = files.path().join("genesis.json");
    let mut args = base.clone();
    args.extend([
        "snapshot".into(),
        "export".into(),
        "--epoch".into(),
        epoch.clone(),
        "--output".into(),
        archive.to_str().unwrap().into(),
        "--genesis-params-output".into(),
        params.to_str().unwrap().into(),
    ]);
    for path in &signatures[..6] {
        args.extend(["--attestation".into(), path.to_str().unwrap().into()]);
    }
    invoke(&args, false);
    assert!(!archive.exists());
    assert!(!params.exists());
    args.extend([
        "--attestation".into(),
        signatures[6].to_str().unwrap().into(),
    ]);
    invoke(&args, true);
    let empty_ledger = files.path().join("must-not-be-created");
    let mut verify = vec![
        "--network".into(),
        "dev".into(),
        "--data-dir".into(),
        empty_ledger.to_str().unwrap().into(),
        "snapshot".into(),
        "verify".into(),
        "--input".into(),
        archive.to_str().unwrap().into(),
        "--genesis-params".into(),
        params.to_str().unwrap().into(),
        "--expected-epoch".into(),
        epoch,
    ];
    invoke(&verify, true);
    assert!(!empty_ledger.exists());
    verify.extend(["--minimum-height".into(), "2".into()]);
    invoke(&verify, false);
    assert!(!empty_ledger.exists());
}

#[test]
fn independent_snapshot_attestations_survive_restart_and_require_matching_human_quorum() {
    let fixture = Fixture::selection();
    let mut attestations = Vec::new();
    let mut final_export = None;
    let mut epoch = BlockHash::ZERO;
    for seed in 0..7 {
        let (dir, store, heads) = fixture.store();
        let candidate = CertifiedCheckpoint::prepare(
            &store,
            NetworkId::Dev,
            &fixture.params,
            1_595_434_050,
            &heads,
            &address(20),
        )
        .unwrap();
        let certificate = certified_votes::certify(candidate.checkpoint(), &[0]);
        epoch = certificate.checkpoint.hash();
        CertifiedCheckpoint::accept(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certificate,
            heads,
            address(20),
        )
        .unwrap();
        let export =
            SnapshotExport::prepare(&store, NetworkId::Dev, &fixture.params, epoch).unwrap();
        let key = burst_crypto::keypair_from_seed(&[seed; 32]).private;
        let signed = export.attest(&store, &key).unwrap();
        assert_eq!(export.attest(&store, &key).unwrap(), signed);
        assert!(export
            .attest(&store, &burst_crypto::keypair_from_seed(&[99; 32]).private)
            .is_err());
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let export =
            SnapshotExport::prepare(&store, NetworkId::Dev, &fixture.params, epoch).unwrap();
        assert_eq!(export.attest(&store, &key).unwrap(), signed);
        let mut bytes = Vec::new();
        signed.write(&mut bytes).unwrap();
        assert_eq!(
            SnapshotAttestation::read(&mut bytes.as_slice()).unwrap(),
            signed
        );
        bytes.push(0);
        assert!(SnapshotAttestation::read(&mut bytes.as_slice()).is_err());
        assert!(SnapshotAttestation::read(&mut &[0; 4097][..]).is_err());
        if seed == 0 {
            let name = format!(
                "snapshot_attestation/v1/dev/{}/{}/{}",
                export.manifest().genesis,
                epoch,
                address(seed)
            );
            let original = store.meta_store().get_meta(&name).unwrap();
            // Corrupt this disposable signer's durable choice: never overwrite it.
            store
                .meta_store()
                .put_meta(&name, b"different prior state")
                .unwrap();
            assert!(export.attest(&store, &key).is_err());
            assert_eq!(
                store.meta_store().get_meta(&name).unwrap(),
                b"different prior state"
            );
            store.meta_store().put_meta(&name, &original).unwrap();
        }
        attestations.push(signed);
        final_export = Some(export);
    }
    let export = final_export.unwrap();
    let mut archive = Vec::new();
    assert!(export.write(&attestations[..6], &mut archive).is_err());
    assert!(archive.is_empty());
    export.write(&attestations, &mut archive).unwrap();
    let base = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap();
    let imported = AuthenticatedSnapshot::read(&base, &mut archive.as_slice()).unwrap();
    assert_eq!(imported.anchor().hash(), epoch);
    assert_eq!(&imported.certificate().manifest, export.manifest());
    attestations.reverse();
    let mut reordered = Vec::new();
    export.write(&attestations, &mut reordered).unwrap();
    assert_eq!(archive, reordered);
    for mutation in 0..4 {
        let mut bad = attestations.clone();
        match mutation {
            0 => bad[0] = bad[1].clone(),
            1 => bad[0].manifest.epoch = BlockHash::new([9; 32]),
            2 => bad[0].manifest.state_hash = BlockHash::new([9; 32]),
            _ => bad[0].signer.signature = burst_types::Signature([0; 64]),
        }
        let mut output = Vec::new();
        assert!(export.write(&bad, &mut output).is_err());
        assert!(output.is_empty());
    }
}
