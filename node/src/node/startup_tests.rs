use super::*;
use crate::checkpoint_sync::CheckpointSyncMessage;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn connector_context(node: &BurstNode) -> crate::peer_connector::PeerConnectorContext {
    let (feedback, _receiver) = tokio::sync::mpsc::channel(1);
    crate::peer_connector::PeerConnectorContext {
        peer_manager: node.peer_manager.clone(),
        connection_registry: node.connection_registry.clone(),
        block_queue: node.block_queue.clone(),
        metrics: node.metrics.clone(),
        active_elections: node.active_elections.clone(),
        rep_weights: node.rep_weights.clone(),
        vote_generator: node.vote_generator.clone(),
        message_dedup: node.message_dedup.clone(),
        online_weight_sampler: node.online_weight_sampler.clone(),
        store: node.store.clone(),
        node_private_key: burst_types::PrivateKey(node.node_private_key.0),
        node_address: node.node_address.clone(),
        params_hash: node.config.params.params_hash(),
        network: node.config.network,
        peering_port: node.config.port,
        bootstrap_feedback: feedback,
    }
}

#[tokio::test]
async fn outbound_rejects_a_listener_claiming_an_unproved_identity() {
    let directory = tempfile::tempdir().unwrap();
    let node = BurstNode::new(config(directory.path(), 0)).await.unwrap();
    let context = connector_context(&node);
    for invalid in 0..3 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let victim_key = burst_crypto::keypair_from_seed(&[91; 32]);
        let challenge = crate::wire_message::HandshakeMsg {
            node_id: burst_crypto::derive_address(&victim_key.public),
            cookie: Some([7; 32]),
            cookie_signature: None,
            params_hash: context.params_hash,
            network_id: context.network,
            peering_port: 0,
            block_version: CURRENT_BLOCK_VERSION,
            wire_version: burst_types::PROTOCOL_VERSION,
        };
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            send(&mut stream, WireMessage::Handshake(challenge.clone())).await;
            let WireMessage::Handshake(response) = receive(&mut stream).await else {
                panic!("expected dialer response");
            };
            if invalid != 0 {
                let attacker_key = burst_crypto::keypair_from_seed(&[92; 32]);
                let key = if invalid == 1 {
                    &attacker_key.private
                } else {
                    &victim_key.private
                };
                let mut proof = challenge.clone();
                proof.cookie = None;
                proof.cookie_signature = Some(
                    crate::peer_handshake::sign(
                        crate::peer_handshake::Role::Listener,
                        &challenge,
                        &response,
                        key,
                    )
                    .unwrap(),
                );
                if invalid == 2 {
                    proof.peering_port = 1;
                }
                send(&mut stream, WireMessage::Handshake(proof)).await;
            }
        });
        let result = crate::peer_connector::connect_to_peer(&address, &context).await;
        server.await.unwrap();
        assert!(
            result.is_err(),
            "accepted missing, forged or changed listener proof: {invalid}"
        );
        assert!(node.connection_registry.read().await.is_empty());
    }
}

#[tokio::test]
async fn outbound_stalled_handshake_times_out_and_retries_without_leaking_dial_slot() {
    let directory = tempfile::tempdir().unwrap();
    let node = BurstNode::new(config(directory.path(), 0)).await.unwrap();
    let context = connector_context(&node);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let (mut stalled, _) = listener.accept().await.unwrap();
        stalled.write_u32(32).await.unwrap();
        stalled.write_all(&[0]).await.unwrap();
        // Hold the incomplete first challenge while accepting the later retry.
        let (retry, _) = listener.accept().await.unwrap();
        drop(retry);
        drop(stalled);
    });
    let first = tokio::time::timeout(
        Duration::from_secs(20),
        crate::peer_connector::connect_to_peer(&address, &context),
    )
    .await
    .unwrap();
    assert!(first.err().unwrap().contains("attempt deadline exceeded"));
    let blocked = crate::peer_connector::connect_to_peer(&address, &context).await;
    assert!(blocked.err().unwrap().contains("waiting for retry"));
    tokio::time::sleep(Duration::from_secs(2)).await;
    let retry = tokio::time::timeout(
        Duration::from_secs(2),
        crate::peer_connector::connect_to_peer(&address, &context),
    )
    .await
    .unwrap();
    assert!(!retry.err().unwrap().contains("waiting for retry"));
    server.await.unwrap();
}

#[tokio::test]
async fn snapshot_bootstrap_rejects_closed_gate_and_damaged_archive_before_admission() {
    for enabled in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("damaged.snapshot");
        std::fs::write(&path, b"not an authenticated archive").unwrap();
        let mut settings = config(&directory.path().join("node"), 0);
        settings.snapshot_bootstrap = Some(path);
        let mut node = BurstNode::new(settings).await.unwrap();
        if enabled {
            node.verification_fixture_clock = Some(Arc::new(|| None));
        }
        let result = tokio::time::timeout(Duration::from_secs(10), node.start())
            .await
            .unwrap();
        assert!(result.is_err());
        if !enabled {
            assert!(result.unwrap_err().to_string().contains("release gate"));
        }
        assert_eq!(
            node.store.block_store().block_count().unwrap(),
            u64::from(enabled)
        );
        assert!(node
            .store
            .meta_store()
            .get_meta_txn(
                &node.store.tx_begin_read().unwrap(),
                crate::confirmed_replay::snapshot::installed::BASE_KEY,
            )
            .unwrap()
            .is_none());
    }
}

fn config(directory: &std::path::Path, port: u16) -> NodeConfig {
    NodeConfig {
        data_dir: directory.to_path_buf(),
        network: burst_types::NetworkId::Dev,
        params: burst_ledger::genesis::initial_params(&burst_types::NetworkId::Dev),
        port,
        enable_rpc: false,
        enable_websocket: false,
        enable_upnp: false,
        enable_peer_reachout: false,
        enable_representative: false,
        nts_sources: vec![],
        bootstrap_peers: vec![],
        drand_relays: vec!["http://127.0.0.1:1".into()],
        ..NodeConfig::default()
    }
}

#[tokio::test]
async fn synchronous_submission_uses_bounded_queue_without_publishing_acceptance() {
    let directory = tempfile::tempdir().unwrap();
    let mut node = BurstNode::new(config(directory.path(), 0)).await.unwrap();
    node.initialize_genesis().unwrap();
    let budget = crate::block_retention::BlockRetention::new(1);
    node.block_processor.lock().await.set_retention(budget.clone());
    node.block_queue = Arc::new(BlockPriorityQueue::with_retention(1, budget));
    let genesis = crate::genesis_state::block(node.config.network, &node.initial_params);
    let before = node.frontier.read().await.get_head(&genesis.account).copied();
    let mut candidate = genesis.clone();
    candidate.previous = genesis.hash;
    candidate.block_type = BlockType::ChangeRepresentative;
    candidate.timestamp = Timestamp::now();
    candidate.hash = candidate.compute_hash();
    assert_eq!(node.process_block(&candidate).unwrap(), ProcessResult::Queued);
    assert!(node.process_block(&candidate).is_err());
    assert!(!node.store.block_store().exists(&candidate.hash).unwrap());
    assert_eq!(node.frontier.read().await.get_head(&genesis.account).copied(), before);
    assert_eq!(node.block_queue.pop_retained().await.hash, candidate.hash);
    assert_eq!(node.process_block(&candidate).unwrap(), ProcessResult::Queued);
}

#[tokio::test]
async fn genesis_only_brn_recovery_is_durable_before_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let settings = config(directory.path(), 0);
    let address = genesis_key::genesis_address(settings.network);
    {
        let node = BurstNode::new(settings.clone()).await.unwrap();
        node.initialize_genesis().unwrap();
        assert_eq!(
            node.store.meta_store().get_schema_version().unwrap(),
            burst_store_lmdb::CURRENT_SCHEMA_VERSION
        );
        node.store
            .brn_store()
            .delete_wallet_state(&address)
            .unwrap();
    }
    let mut node = BurstNode::new(settings).await.unwrap();
    assert_eq!(
        node.store.meta_store().get_schema_version().unwrap(),
        burst_store_lmdb::CURRENT_SCHEMA_VERSION
    );
    let store = node.store.clone();
    let shutdown = node.shutdown.clone();
    let task = tokio::spawn(async move {
        let result = node.start().await;
        (node, result)
    });
    let recovered = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(bytes) = store.brn_store().get_wallet_state(&address).unwrap() {
                let wallet: burst_brn::BrnWalletState = bincode::deserialize(&bytes).unwrap();
                assert_eq!(wallet, burst_brn::BrnWalletState::new(Timestamp::EPOCH));
                break;
            }
            assert!(
                !task.is_finished(),
                "startup exited before baseline persistence"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    shutdown.shutdown();
    let (mut node, started) = tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();
    started.unwrap();
    node.stop().await.unwrap();
    recovered.expect("genesis wallet baseline was not persisted during startup");
}

#[tokio::test]
async fn missing_genesis_brn_after_spending_is_not_reconstructed() {
    let directory = tempfile::tempdir().unwrap();
    let settings = config(directory.path(), 0);
    let address = genesis_key::genesis_address(settings.network);
    {
        let node = BurstNode::new(settings.clone()).await.unwrap();
        node.initialize_genesis().unwrap();
        let mut account = node.store.account_store().get_account(&address).unwrap();
        account.total_brn_burned = 1;
        node.store.account_store().put_account(&account).unwrap();
        node.store
            .brn_store()
            .delete_wallet_state(&address)
            .unwrap();
    }
    let mut node = BurstNode::new(settings).await.unwrap();
    let error = tokio::time::timeout(Duration::from_secs(10), node.start())
        .await
        .unwrap()
        .unwrap_err();
    assert!(error.to_string().contains("missing BRN history"), "{error}");
    assert!(node
        .store
        .brn_store()
        .get_wallet_state(&address)
        .unwrap()
        .is_none());
    assert_eq!(
        node.store
            .account_store()
            .get_account(&address)
            .unwrap()
            .total_brn_burned,
        1
    );
}

async fn send(stream: &mut tokio::net::TcpStream, message: WireMessage) {
    let bytes = message.encode().unwrap();
    stream.write_u32(bytes.len() as u32).await.unwrap();
    stream.write_all(&bytes).await.unwrap();
}

async fn receive(stream: &mut tokio::net::TcpStream) -> WireMessage {
    let size = stream.read_u32().await.unwrap() as usize;
    assert!(size <= burst_types::MAX_P2P_MESSAGE_BYTES);
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).await.unwrap();
    WireMessage::decode(&bytes).unwrap()
}

#[tokio::test]
async fn recovery_listener_serves_proofs_before_restoration_and_cleans_up_on_failure() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let directory = tempfile::tempdir().unwrap();
        let reservation = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = reservation.local_addr().unwrap().port();
        let mut node = BurstNode::new(config(directory.path(), port))
            .await
            .unwrap();
        node.initialize_genesis().unwrap();
        node.store
            .meta_store()
            .put_meta("consti_engine", b"invalid")
            .unwrap();
        let engine = node.brn_engine.clone();
        let pause = engine.lock().await;
        let queue = node.block_queue.clone();
        let registry = node.connection_registry.clone();
        let genesis = crate::genesis_state::block(node.config.network, &node.initial_params);
        drop(reservation);
        let startup = tokio::spawn(async move {
            let result = node.start().await;
            (node, result)
        });
        let mut stream = loop {
            match tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await {
                Ok(stream) => break stream,
                Err(_) => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        };
        let WireMessage::Handshake(mut challenge) = receive(&mut stream).await else {
            panic!("expected authenticated handshake");
        };
        let key = burst_crypto::keypair_from_seed(&[41; 32]);
        let mut old_response = None;
        for invalid in 0..4 {
            let mut response = crate::wire_message::HandshakeMsg {
                node_id: burst_crypto::derive_address(&key.public),
                cookie: Some(burst_protocol::handshake::create_cookie()),
                cookie_signature: None,
                params_hash: challenge.params_hash,
                network_id: challenge.network_id,
                peering_port: 0,
                block_version: CURRENT_BLOCK_VERSION,
                wire_version: burst_types::PROTOCOL_VERSION,
            };
            response.cookie_signature = Some(
                crate::peer_handshake::sign(
                    crate::peer_handshake::Role::Dialer,
                    &challenge,
                    &response,
                    &key.private,
                )
                .unwrap(),
            );
            if invalid == 0 {
                old_response = Some(response.clone());
            }
            match invalid {
                0 => {
                    response.cookie_signature = Some(burst_crypto::sign_message(
                        &challenge.cookie.unwrap(),
                        &key.private,
                    ))
                }
                1 => {
                    response.node_id = burst_crypto::derive_address(
                        &burst_crypto::keypair_from_seed(&[42; 32]).public,
                    )
                }
                2 => response.network_id = burst_types::NetworkId::Live,
                _ => response = old_response.clone().unwrap(),
            }
            send(&mut stream, WireMessage::Handshake(response)).await;
            assert!(
                tokio::time::timeout(Duration::from_secs(2), stream.read_u32())
                    .await
                    .unwrap()
                    .is_err()
            );
            assert!(registry.read().await.is_empty());
            stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            let WireMessage::Handshake(next) = receive(&mut stream).await else {
                panic!("expected fresh listener challenge");
            };
            challenge = next;
        }
        let mut response = crate::wire_message::HandshakeMsg {
            node_id: burst_crypto::derive_address(&key.public),
            cookie: Some(burst_protocol::handshake::create_cookie()),
            cookie_signature: None,
            params_hash: challenge.params_hash,
            network_id: challenge.network_id,
            peering_port: 0,
            block_version: CURRENT_BLOCK_VERSION,
            wire_version: burst_types::PROTOCOL_VERSION,
        };
        response.cookie_signature = Some(
            crate::peer_handshake::sign(
                crate::peer_handshake::Role::Dialer,
                &challenge,
                &response,
                &key.private,
            )
            .unwrap(),
        );
        send(&mut stream, WireMessage::Handshake(response.clone())).await;
        let WireMessage::Handshake(proof) = receive(&mut stream).await else {
            panic!("expected listener proof");
        };
        crate::peer_handshake::verify(
            crate::peer_handshake::Role::Listener,
            &challenge,
            &response,
            &proof.cookie_signature.unwrap(),
        )
        .unwrap();
        send(&mut stream, WireMessage::Block(Box::new(genesis.clone()))).await;
        send(
            &mut stream,
            WireMessage::CheckpointSync(CheckpointSyncMessage::WitnessRequest {
                start: genesis.hash,
                offset: 0,
                digest: None,
            }),
        )
        .await;
        loop {
            match receive(&mut stream).await {
                WireMessage::Keepalive(_) => continue,
                WireMessage::CheckpointSync(CheckpointSyncMessage::WitnessChunk {
                    start,
                    data,
                    ..
                }) => {
                    assert_eq!(start, genesis.hash);
                    assert!(!data.is_empty());
                    break;
                }
                other => panic!("unexpected recovery reply: {other:?}"),
            }
        }
        assert!(queue.is_empty().await);
        let proofs = registry.read().await.witness_requests().unwrap();
        assert!(!proofs.is_closed());
        drop(pause);
        let (mut node, result) = startup.await.unwrap();
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid persisted constitution"));
        assert!(node.task_handles.is_empty());
        assert!(registry.write().await.take_tasks().is_empty());
        assert!(proofs.is_closed());
        assert!(registry.read().await.is_empty());
        assert!(node
            .start()
            .await
            .unwrap_err()
            .to_string()
            .contains("already been started"));
        assert_eq!(
            node.store.meta_store().get_meta("consti_engine").unwrap(),
            b"invalid"
        );
        let _rebound =
            std::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn occupied_listener_fails_startup_without_leaving_workers() {
    let directory = tempfile::tempdir().unwrap();
    let reservation = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
    let mut node = BurstNode::new(config(
        directory.path(),
        reservation.local_addr().unwrap().port(),
    ))
    .await
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), node.start())
        .await
        .unwrap();
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("cannot bind P2P listener"));
    assert!(node.task_handles.is_empty());
    assert!(node
        .connection_registry
        .write()
        .await
        .take_tasks()
        .is_empty());
}

#[tokio::test]
async fn recovery_dials_seed_and_cached_peers_before_restoration() {
    use burst_store::peer::PeerStore;
    tokio::time::timeout(Duration::from_secs(15), async {
        for cached in [false, true] {
            let source_dir = tempfile::tempdir().unwrap();
            let target_dir = tempfile::tempdir().unwrap();
            let source_port = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
            let target_port = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
            let address = format!("127.0.0.1:{}", source_port.local_addr().unwrap().port());
            let mut source = BurstNode::new(config(source_dir.path(), source_port.local_addr().unwrap().port())).await.unwrap();
            source.initialize_genesis().unwrap();
            let genesis = crate::genesis_state::block(source.config.network, &source.initial_params);
            let (feedback, _feedback_rx) = mpsc::channel(4);
            drop(source_port);
            source.start_recovery_listener(&feedback).await.unwrap();
            crate::checkpoint_sync::witness::start(source.store.clone(), source.connection_registry.clone(), source.shutdown.clone()).await;
            let mut target_config = config(target_dir.path(), target_port.local_addr().unwrap().port());
            if !cached {
                target_config.bootstrap_peers = vec![address.clone()];
            }
            let mut target = BurstNode::new(target_config).await.unwrap();
            target.initialize_genesis().unwrap();
            if cached {
                target.store.peer_store().put_peer(&address, unix_now_secs()).unwrap();
            }
            target.store.meta_store().put_meta("consti_engine", b"invalid").unwrap();
            let engine = target.brn_engine.clone();
            let pause = engine.lock().await;
            let registry = target.connection_registry.clone();
            drop(target_port);
            let startup = tokio::spawn(async move {
                let result = target.start().await;
                (target, result)
            });
            loop {
                if registry.read().await.len() == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert!(registry.read().await.is_recovering());
            let (sender, mut incoming) = mpsc::channel(4);
            registry.write().await.set_witness_inbox(sender);
            assert!(registry.read().await.send(source.node_address.as_str(), WireMessage::CheckpointSync(CheckpointSyncMessage::WitnessRequest {
                start: genesis.hash,
                offset: 0,
                digest: None,
            }).encode().unwrap()));
            let reply = incoming.recv().await.unwrap();
            assert_eq!(reply.peer, source.node_address.as_str());
            assert!(matches!(reply.message, CheckpointSyncMessage::WitnessChunk { start, .. } if start == genesis.hash));
            drop(pause);
            let (target, result) = startup.await.unwrap();
            assert!(result.unwrap_err().to_string().contains("invalid persisted constitution"));
            assert!(target.task_handles.is_empty());
            assert!(registry.read().await.is_empty());
            source.stop().await.unwrap();
        }
    }).await.unwrap();
}
