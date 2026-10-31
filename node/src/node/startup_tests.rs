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
        let result = tokio::time::timeout(Duration::from_secs(10), node.start_with_services(burst_daemon::start_public_services))
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
async fn final_quorum_handoff_progresses_while_local_signer_is_held() {
    use burst_store::{account::AccountStore, block::BlockStore, meta::MetaStore};
    let directory = tempfile::tempdir().unwrap();
    let mut settings = config(directory.path(), 0);
    settings.params.min_work_difficulty = 0;
    let mut node = BurstNode::new(settings).await.unwrap();
    node.initialize_genesis().unwrap();
    let creator = genesis_key::genesis_address(node.config.network);
    let store = node.store.clone();
    let genesis = store.account_store().get_account(&creator).unwrap().head;
    let mut block: StateBlock = bincode::deserialize(&store.block_store().get_block(&genesis).unwrap()).unwrap();
    let key = burst_crypto::keypair_from_seed(&[17; 32]);
    block.account = burst_crypto::derive_address(&key.public);
    block.representative = block.account.clone();
    block.payment_epoch = store.meta_store().payment_epoch().unwrap();
    block.timestamp = Timestamp::now();
    block.hash = block.compute_hash();
    block.signature = burst_crypto::sign_message(block.hash.as_bytes(), &key.private);
    let queue = node.block_queue();
    let results = node.submission_results.clone();
    let elections = node.active_elections.clone();
    let weights = node.rep_weights.clone();
    let generator = node.vote_generator.clone();
    let registry = node.connection_registry.clone();
    let shutdown = node.shutdown.clone();
    let task = tokio::spawn(async move {
        let result = node.start_with_services(burst_daemon::start_public_services).await;
        (node, result)
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        while registry.read().await.is_recovering() {
            assert!(!task.is_finished());
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    let held = generator.lock_owned().await;
    let sequence = held.sequence();
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        let payload = bincode::serialize(&block).unwrap();
        results.track(&payload);
        assert!(queue.push(block.clone()).await);
        assert_eq!(results.wait(&payload).await, Some(burst_rpc::ProcessResult::Accepted));
        let root = crate::ledger_updater::election_root(&block);
        let key = genesis_key::genesis_signing_key(burst_types::NetworkId::Dev).unwrap();
        let mut external = burst_consensus::VoteGenerator::new(creator.clone(), key.private.0);
        let vote = external.generate_final_vote(block.hash);
        assert!(burst_crypto::verify_signature(&burst_consensus::vote_signing_payload(
            &vote.voter, vote.is_final, vote.timestamp, vote.sequence, &[vote.block_hash]),
            &vote.signature, &key.public));
        let weight = weights.read().await.weight(&creator);
        assert!(weight > 0);
        let now = Timestamp::now();
        assert!(elections.write().await.process_signed_vote(&root,
            burst_consensus::VoteInfo::new(vote.voter, block.hash, weight, true,
                Timestamp::new(vote.timestamp / 1000), vote.sequence), now).unwrap().is_some());
        loop {
            let account = store.account_store().get_account(&block.account).unwrap();
            if account.confirmation_height == 1 && elections.read().await.get_election(&root).is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(held.sequence(), sequence, "local signer must remain unused");
    }).await;
    drop(held);
    shutdown.shutdown();
    let (mut node, result) = tokio::time::timeout(Duration::from_secs(10), task).await.unwrap().unwrap();
    result.unwrap();
    node.stop().await.unwrap();
    assert!(outcome.is_ok(), "final quorum could not cement while local signer was held");
}

#[tokio::test]
async fn synchronous_submission_uses_bounded_queue_without_publishing_acceptance() {
    let directory = tempfile::tempdir().unwrap();
    let mut node = BurstNode::new(config(directory.path(), 0)).await.unwrap();
    node.initialize_genesis().unwrap();
    let budget = crate::block_retention::BlockRetention::new(1);
    node.block_processor
        .lock()
        .await
        .set_retention(budget.clone());
    node.block_queue = Arc::new(BlockPriorityQueue::with_retention(1, budget));
    let genesis = crate::genesis_state::block(node.config.network, &node.initial_params);
    let before = node
        .frontier
        .read()
        .await
        .get_head(&genesis.account)
        .copied();
    let mut candidate = genesis.clone();
    candidate.previous = genesis.hash;
    candidate.block_type = BlockType::ChangeRepresentative;
    candidate.timestamp = Timestamp::now();
    candidate.hash = candidate.compute_hash();
    assert_eq!(
        node.process_block(&candidate).unwrap(),
        ProcessResult::Queued
    );
    assert_eq!(node.process_block(&candidate).unwrap(), ProcessResult::Queued);
    assert_eq!(node.block_queue.len().await, 1);
    let mut distinct_payload = candidate.clone();
    distinct_payload.work ^= 1;
    assert!(node.process_block(&distinct_payload).is_err());
    assert!(!node.store.block_store().exists(&candidate.hash).unwrap());
    assert_eq!(
        node.frontier
            .read()
            .await
            .get_head(&genesis.account)
            .copied(),
        before
    );
    assert_eq!(node.block_queue.pop_retained().await.hash, candidate.hash);
    assert_eq!(
        node.process_block(&candidate).unwrap(),
        ProcessResult::Queued
    );
}

#[tokio::test]
async fn prepared_admission_advances_independent_accounts_and_reports_snapshot_faults() {
    prepared_admission_workload(2, 1, false, false, None).await;
}

#[tokio::test]
#[ignore = "explicit capped live admission diagnostic; not consensus or release throughput"]
async fn prepared_admission_stage_scaling() {
    if std::env::var_os("BURST_ADMISSION_STAGE_LOG").is_some() {
        tracing_subscriber::fmt()
            .with_env_filter("burst_node::node=debug")
            .with_ansi(false)
            .without_time()
            .with_test_writer()
            .try_init()
            .expect("isolated admission diagnostic subscriber");
    }
    prepared_admission_workload(64, 32, true, false, None).await;
}

#[tokio::test]
async fn prepared_admission_progresses_with_concurrent_http_queries() {
    prepared_admission_workload(8, 8, true, true, None).await;
}

#[tokio::test]
async fn grouped_admission_abort_keeps_state_and_replies_private_then_isolates_serially() {
    prepared_admission_workload(1, 1, false, false, Some(GroupCase::PreparedAbort)).await;
}

#[tokio::test]
async fn native_growth_pressure_keeps_signed_group_private_then_retries_once() {
    prepared_admission_workload(1, 1, false, false, Some(GroupCase::PressureAbort)).await;
}

#[tokio::test]
async fn native_growth_commit_pressure_keeps_signed_group_private_then_retries_once() {
    prepared_admission_workload(1, 1, false, false, Some(GroupCase::PressureCommit)).await;
}

#[tokio::test]
async fn unprepared_group_follower_aborts_privately_and_retries_serially() {
    prepared_admission_workload(1, 1, false, false, Some(GroupCase::UnpreparedAbort)).await;
}

#[tokio::test]
async fn unprepared_group_follower_commits_after_private_staging() {
    prepared_admission_workload(1, 1, false, false, Some(GroupCase::UnpreparedCommit)).await;
}

#[derive(Clone, Copy)]
enum GroupCase {
    PreparedAbort,
    PressureAbort,
    PressureCommit,
    UnpreparedAbort,
    UnpreparedCommit,
    GapAbort,
    GapCommit,
    SourceGapAbort,
    SourceGapCommit,
}

#[tokio::test]
async fn ready_gap_does_not_prevent_independent_group_commit_or_abort() {
    for case in [GroupCase::GapAbort, GroupCase::GapCommit, GroupCase::SourceGapAbort, GroupCase::SourceGapCommit] {
        prepared_admission_workload(1, 1, false, false, Some(case)).await;
    }
}

async fn prepared_admission_workload(
    account_rounds: usize, payment_rounds: usize, report: bool, concurrent_queries: bool,
    group_case: Option<GroupCase>,
) {
    let pressure = matches!(group_case, Some(GroupCase::PressureAbort | GroupCase::PressureCommit));
    let commit_pressure = matches!(group_case, Some(GroupCase::PressureCommit));
    let prepare_follower = matches!(group_case, Some(GroupCase::PreparedAbort));
    let abort_group = matches!(group_case, Some(GroupCase::PreparedAbort | GroupCase::PressureAbort | GroupCase::PressureCommit | GroupCase::UnpreparedAbort | GroupCase::GapAbort | GroupCase::SourceGapAbort));
    let source_gap = matches!(group_case, Some(GroupCase::SourceGapAbort | GroupCase::SourceGapCommit));
    let inject_gap = source_gap || matches!(group_case, Some(GroupCase::GapAbort | GroupCase::GapCommit));
    let directory = tempfile::tempdir().unwrap();
    let mut settings = config(directory.path(), 0);
    settings.params.min_work_difficulty = 0;
    let query_url = if concurrent_queries {
        settings.enable_rpc = true;
        settings.rpc_bind = "127.0.0.1".into();
        settings.rpc_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap()
            .local_addr().unwrap().port();
        Some(format!("http://127.0.0.1:{}", settings.rpc_port))
    } else { None };

    let mut node = BurstNode::new(settings).await.unwrap();
    let (probe, mut probe_controller) = if group_case.is_some() {
        let (probe, controller) = admission_group::AbortProbe::new();
        node.admission_group_probe = Some(probe.clone());
        (Some(probe), Some(controller))
    } else { (None, None) };
    let admission_processor = node.block_processor.clone();
    let admission_frontier = node.frontier.clone();
    node.initialize_genesis().unwrap();
    assert!(node.store.tx_begin_write().unwrap().uses_stable_trst_records().unwrap());
    node.initialize_genesis().unwrap();
    assert!(node.store.tx_begin_write().unwrap().uses_stable_trst_records().unwrap());
    let store = node.store.clone();
    let queue = node.block_queue();
    let results = node.submission_results.clone();
    let shutdown = node.shutdown.clone();
    let live_trst = node.trst_engine.clone();
    let params = node.config.params.clone();
    let creator = genesis_key::genesis_address(node.config.network);
    let genesis = store.account_store().get_account(&creator).unwrap().head;
    let base: StateBlock = bincode::deserialize(&store.block_store().get_block(&genesis).unwrap()).unwrap();
    let epoch = store.meta_store().payment_epoch().unwrap();
    let sign = |block: &mut StateBlock, seed: u8| {
        block.hash = block.compute_hash();
        block.signature = burst_crypto::sign_message(block.hash.as_bytes(),
            &burst_crypto::keypair_from_seed(&[seed; 32]).private);
    };
    let task = tokio::spawn(async move {
        let started = node.start_with_services(burst_daemon::start_public_services).await;
        (node, started)
    });
    let outcome = tokio::time::timeout(Duration::from_secs(60), async {
        let second = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[17; 32]).public);
        let mut open = base.clone();
        open.account = second.clone();
        open.representative = second.clone();
        open.payment_epoch = epoch;
        open.timestamp = Timestamp::now();
        sign(&mut open, 17);
        let payload = bincode::serialize(&open).unwrap();
        results.track(&payload);
        assert!(queue.push(open).await);
        assert_eq!(tokio::time::timeout(Duration::from_secs(10), results.wait(&payload)).await.unwrap(),
            Some(burst_rpc::ProcessResult::Accepted));
        {
            let batch = store.tx_begin_write().unwrap();
            assert!(batch.uses_stable_trst_records().unwrap());
            assert!(batch.uses_graph_records().unwrap());
        }
        let query_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let query_task = if let Some(url) = &query_url {
            let client = burst_wallet_core::NodeClient::new(url.clone()).unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    assert!(!task.is_finished(), "node exited before RPC readiness");
                    if client.rpc_call("peers", serde_json::json!({})).await.is_ok() { break; }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }).await.expect("RPC readiness");
            let stop = query_stop.clone();
            let query_store = store.clone();
            let query_account = creator.clone();
            let (ready, started) = tokio::sync::oneshot::channel();
            let queries = tokio::spawn(async move {
                let mut ready = Some(ready);
                let mut accepted = [0usize; 4];
                let mut rejected = [0usize; 4];
                let mut heads = std::collections::HashSet::new();
                while !stop.load(std::sync::atomic::Ordering::Acquire) {
                    for (index, method) in ["governance_proposals", "verification_activity",
                        "merger_graph_stats", "telemetry"].into_iter().enumerate() {
                        let response = tokio::time::timeout(Duration::from_secs(10),
                            client.rpc_call(method, serde_json::json!({}))).await
                            .expect("query stalled during signed admission");
                        match response {
                            Ok(value) => {
                                assert!(value.is_object(), "{method}: {value}");
                                accepted[index] += 1;
                                heads.insert(query_store.account_store().get_account(&query_account).unwrap().head);
                            }
                            Err(error) if error.to_string().contains("rate limited") => rejected[index] += 1,
                            Err(error) => panic!("{method} during signed admission: {error}"),
                        }
                    }
                    if let Some(ready) = ready.take() { let _ = ready.send(()); }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                (accepted, rejected, heads)
            });
            started.await.unwrap();
            Some(queries)
        } else { None };
        let gap_head = if source_gap {
            let mut open = base.clone();
            open.account = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[18; 32]).public);
            open.representative = open.account.clone();
            open.payment_epoch = epoch;
            open.timestamp = Timestamp::now();
            sign(&mut open, 18);
            let payload = bincode::serialize(&open).unwrap();
            results.track(&payload);
            assert!(queue.push(open.clone()).await);
            assert_eq!(tokio::time::timeout(Duration::from_secs(10), results.wait(&payload)).await.unwrap(),
                Some(burst_rpc::ProcessResult::Accepted));
            Some(open)
        } else { None };
        // Queue separate account chains together; each commits its own head and
        // metadata without replacing the other account's committed state.
        let account_start = std::time::Instant::now();
        let mut account_samples = Vec::new();
        for round in 0..account_rounds {
            let mut candidates = Vec::new();
            for (address, seed) in [(&creator, 0), (&second, 17)] {
                let head = store.account_store().get_account(address).unwrap().head;
                let mut block: StateBlock = bincode::deserialize(&store.block_store().get_block(&head).unwrap()).unwrap();
                block.previous = head;
                block.payment_epoch = epoch;
                block.block_type = BlockType::ChangeRepresentative;
                block.signing_grant = block.active_signing_grant();
                block.representative = if round % 2 == 0 { second.clone() } else { creator.clone() };
                block.timestamp = Timestamp::now();
                sign(&mut block, seed);
                let payload = bincode::serialize(&block).unwrap();
                results.track(&payload);
                let submitted_at = std::time::Instant::now();
                candidates.push((block, payload, submitted_at));
            }
            let expected_heights: std::collections::HashMap<_, _> = candidates.iter().map(|(block, _, _)| {
                (block.account.clone(), store.account_store().get_account(&block.account).unwrap().block_count + 1)
            }).collect();
            if let Some(controller) = probe_controller.take() {
                // Only scheduling is controlled: the injected ready head still
                // uses ordinary signature authentication; economics may be deferred.
                let mut ready = admission_processor.lock().await.authentication_rules()
                    .authenticate(candidates[1].0.clone());
                assert!(ready.result().is_ok());
                if prepare_follower {
                    let batch = store.tx_begin_write().unwrap();
                    let captured = crate::economic_candidate::prepared::CapturedPayment::capture(
                        &batch, ready.block(), &params).unwrap().unwrap();
                    batch.commit().unwrap();
                    ready.prepared = Some(captured.prepare().unwrap());
                } else {
                    assert!(ready.prepared.is_none());
                }
                let ready = tokio::spawn(async move { ready });
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !ready.is_finished() { tokio::task::yield_now().await; }
                }).await.unwrap();
                let mut heads = Vec::new();
                let gap = if inject_gap {
                    let keys = burst_crypto::keypair_from_seed(&[18; 32]);
                    let mut gap = candidates[1].0.clone();
                    gap.account = burst_crypto::derive_address(&keys.public);
                    if let Some(head) = &gap_head {
                        gap.previous = head.hash;
                        gap.block_type = BlockType::Receive;
                        gap.link = BlockHash::new([99; 32]);
                        gap.trst_balance = head.trst_balance + 1;
                    } else {
                        gap.previous = BlockHash::new([99; 32]);
                    }
                    sign(&mut gap, 18);
                    let authenticated = admission_processor.lock().await.authentication_rules()
                        .authenticate(gap.clone());
                    assert!(authenticated.result().is_ok());
                    let handle = tokio::spawn(async move { authenticated });
                    tokio::time::timeout(Duration::from_secs(2), async {
                        while !handle.is_finished() { tokio::task::yield_now().await; }
                    }).await.unwrap();
                    heads.push(handle);
                    Some(gap)
                } else { None };
                heads.push(ready);
                let capture = if pressure {
                    probe.as_ref().unwrap().pressure(commit_pressure);
                    let capture_store = store.clone();
                    let (ready, received) = std::sync::mpsc::channel();
                    let (release, released) = std::sync::mpsc::channel();
                    let worker = std::thread::spawn(move || {
                        let mut writer = capture_store.tx_begin_write().unwrap();
                        let ceiling = capture_store.env().real_disk_size().unwrap() as usize + (4 << 20);
                        let limit = writer.limit_file_extent(ceiling).unwrap();
                        let reader = capture_store.tx_begin_read().unwrap();
                        drop(writer);
                        ready.send(limit.watch()).unwrap();
                        released.recv_timeout(Duration::from_secs(5)).unwrap();
                        drop(reader);
                        drop(limit);
                    });
                    let watch = received.recv_timeout(Duration::from_secs(2)).unwrap();
                    Some((release, worker, watch))
                } else { None };
                probe.as_ref().unwrap().arm(candidates[0].0.hash, heads);
                assert!(queue.push(candidates[0].0.clone()).await);
                tokio::time::timeout(Duration::from_secs(2), controller.staged).await.unwrap().unwrap();
                let assert_private = || async {
                    let view = store.tx_begin_read().unwrap();
                    for (block, _, _) in &candidates {
                        assert!(!store.block_store().exists_txn(&view, &block.hash).unwrap());
                        assert!(store.meta_store().block_undo_txn(&view, &block.hash).unwrap().is_none());
                        assert_eq!(store.account_store().get_account_txn(&view, &block.account).unwrap().head, block.previous);
                    }
                    drop(view);
                    for (block, payload, _) in &candidates {
                        assert_eq!(admission_frontier.read().await.get_head(&block.account), Some(&block.previous));
                        assert!(tokio::time::timeout(Duration::from_millis(20), results.wait(payload)).await.is_err(),
                            "uncommitted/aborted group published a submission result");
                    }
                };
                assert_private().await;
                if let Some(gap) = &gap {
                    assert!(!store.block_store().exists(&gap.hash).unwrap());
                    assert_eq!(admission_processor.lock().await.unchecked_count(), 1);
                }
                controller.resume_staged.send(abort_group && !commit_pressure).unwrap();
                if abort_group {
                    tokio::time::timeout(Duration::from_secs(2), controller.aborted).await.unwrap().unwrap();
                    assert_private().await;
                    if pressure {
                        assert!(live_trst.try_lock().is_ok(), "pressure wait retained a live engine guard");
                        assert!(capture.as_ref().unwrap().2.was_exceeded());
                    }
                    controller.resume_aborted.send(()).unwrap();
                    if let Some((release, worker, watch)) = capture {
                        assert_private().await;
                        assert!(!watch.wait_released(Duration::ZERO));
                        release.send(()).unwrap();
                        worker.join().unwrap();
                        assert!(watch.wait_released(Duration::ZERO));
                    }
                }
            } else {
                for (block, _, _) in &candidates { assert!(queue.push(block.clone()).await); }
            }
            for (block, payload, submitted_at) in candidates {
                assert_eq!(tokio::time::timeout(Duration::from_secs(10), results.wait(&payload)).await.unwrap(),
                    Some(burst_rpc::ProcessResult::Accepted));
                account_samples.push(submitted_at.elapsed());
                let account = store.account_store().get_account(&block.account).unwrap();
                assert_eq!(account.head, block.hash);
                assert_eq!(account.representative, block.representative);
                assert_eq!(account.block_count, expected_heights[&block.account],
                    "serial isolation must apply each candidate exactly once");
            }
        }
        let account_elapsed = account_start.elapsed();
        let payment_start = std::time::Instant::now();
        let mut payment_samples = Vec::new();
        let mut payment_heads = std::collections::HashSet::new();
        let mut received = Vec::new();
        let endorsement_key = burst_crypto::keypair_from_seed(&[42; 32]);
        let endorsement_target = burst_crypto::derive_address(&endorsement_key.public);
        let mut burn_hash = BlockHash::ZERO;
        let mut merge_hash = BlockHash::ZERO;
        for kind in (0..payment_rounds).flat_map(|_| [BlockType::Burn, BlockType::Receive,
            BlockType::Burn, BlockType::Receive, BlockType::Merge]).chain([BlockType::Endorse]) {
            let head = store.account_store().get_account(&creator).unwrap().head;
            let mut block: StateBlock = bincode::deserialize(&store.block_store().get_block(&head).unwrap()).unwrap();
            block.previous = head;
            block.payment_epoch = epoch;
            block.signing_grant = block.active_signing_grant();
            block.block_type = kind.clone();
            block.timestamp = Timestamp::now();
            block.link = BlockHash::ZERO;
            block.origin = burst_types::TxHash::ZERO;
            block.merge_sources.clear();
            match kind {
                BlockType::Burn => {
                    block.brn_spent += 1;
                    block.link = BlockHash::new(burst_crypto::decode_address(creator.as_str()).unwrap());
                }
                BlockType::Receive => { block.link = burn_hash; block.trst_balance += 1; }
                BlockType::Merge => block.merge_sources = received.clone(),
                BlockType::Endorse => { block.brn_spent += 1; block.link = BlockHash::new(endorsement_key.public.0); }
                _ => unreachable!(),
            }
            sign(&mut block, 0);
            let payload = bincode::serialize(&block).unwrap();
            results.track(&payload);
            let submitted_at = std::time::Instant::now();
            assert!(queue.push(block.clone()).await);
            let result = tokio::time::timeout(Duration::from_secs(10), results.wait(&payload)).await.unwrap();
            assert_eq!(result, Some(burst_rpc::ProcessResult::Accepted), "operation {kind:?}");
            payment_samples.push(submitted_at.elapsed());
            payment_heads.insert(block.hash);
            match kind {
                BlockType::Burn => burn_hash = block.hash,
                BlockType::Receive => received.push(block.hash.into_tx_hash()),
                BlockType::Merge => { merge_hash = block.hash; received.clear(); },
                BlockType::Endorse => {},
                _ => unreachable!(),
            }
        }
        {
            let trst = live_trst.lock().await;
            assert!(trst.merger_graph.contains_merge(&merge_hash.into_tx_hash()));
            assert_eq!(trst.wallets[&creator].tokens.len(), payment_rounds);
            assert_eq!(trst.transferable_balance_snapshot(&creator), Some(2 * payment_rounds as u128));
        }
        {
            let batch = store.tx_begin_write().unwrap();
            let state = crate::payment_transition::restore(&batch, &params).unwrap();
            assert!(state.trst.merger_graph.contains_merge(&merge_hash.into_tx_hash()));
            assert_eq!(state.trst.wallets[&creator].tokens.len(), payment_rounds);
            assert!(state.brn.get_wallet(&endorsement_target).is_some());
            assert_eq!(batch.get_account(&endorsement_target).unwrap().state, burst_types::WalletState::Verified);
        }
        query_stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(queries) = query_task {
            let (accepted, rejected, heads) = tokio::time::timeout(Duration::from_secs(10), queries)
                .await.unwrap().unwrap();
            assert!(accepted.iter().all(|count| *count > 1), "each route must succeed across admission: {accepted:?}");
            assert!(heads.intersection(&payment_heads).count() > 1,
                "queries must observe multiple admitted signed payment heads");
            println!("admission_concurrent_queries accepted={accepted:?} rejected={rejected:?} distinct_heads={} payment_heads={}",
                heads.len(), heads.intersection(&payment_heads).count());
            let client = burst_wallet_core::NodeClient::new(query_url.as_ref().unwrap().clone()).unwrap();
            let telemetry = client.rpc_call("telemetry", serde_json::json!({})).await.unwrap();
            assert_eq!(telemetry["trst_circulating"], (2 * payment_rounds).to_string());
            let stats = client.rpc_call("merger_graph_stats", serde_json::json!({})).await.unwrap();
            assert_eq!(stats["total_merges"], serde_json::json!(payment_rounds));
        }
        if report {
            for (name, mut samples, elapsed) in [
                ("two_account_representative", account_samples, account_elapsed),
                ("burn_receive_merge_endorse", payment_samples, payment_start.elapsed()),
            ] {
                samples.sort();
                let at = |percent: usize| samples[(samples.len() - 1) * percent / 100].as_secs_f64() * 1000.0;
                println!("admission_workload {name} count={} elapsed_ms={:.3} observed_per_second={:.3} queue_to_observation_p50_ms={:.3} p95_ms={:.3} max_ms={:.3}",
                    samples.len(), elapsed.as_secs_f64() * 1000.0,
                    samples.len() as f64 / elapsed.as_secs_f64(), at(50), at(95), at(100));
            }
            if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
                for line in status.lines().filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:")) {
                    println!("admission_process_memory {line}");
                }
            }
        }
        let head = store.account_store().get_account(&creator).unwrap().head;
        let mut block: StateBlock = bincode::deserialize(&store.block_store().get_block(&head).unwrap()).unwrap();
        block.previous = head;
        block.block_type = BlockType::ChangeRepresentative;
        block.merge_sources.clear();
        block.link = BlockHash::ZERO;
        block.origin = burst_types::TxHash::ZERO;
        block.timestamp = Timestamp::now();
        sign(&mut block, 0);
        let rates = store.brn_store().get_meta(b"rate_history").unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_brn_meta(b"rate_history", b"corrupted").unwrap();
        batch.commit().unwrap();
        let payload = bincode::serialize(&block).unwrap();
        let mut competitor = block.clone();
        competitor.representative = second.clone();
        sign(&mut competitor, 0);
        let competing_payload = bincode::serialize(&competitor).unwrap();
        results.track(&payload);
        results.track(&competing_payload);
        assert!(queue.push(block.clone()).await);
        assert!(queue.push(competitor.clone()).await);
        for bytes in [&payload, &competing_payload] {
            let rejected = tokio::time::timeout(Duration::from_secs(10), results.wait(bytes)).await.unwrap();
            assert!(matches!(rejected, Some(burst_rpc::ProcessResult::Rejected(_))), "{rejected:?}");
        }
        assert!(!store.block_store().exists(&block.hash).unwrap());
        assert!(!store.block_store().exists(&competitor.hash).unwrap());
        assert_eq!(store.account_store().get_account(&creator).unwrap().head, head);
        let mut batch = store.tx_begin_write().unwrap();
        match rates {
            Some(rates) => batch.put_brn_meta(b"rate_history", &rates).unwrap(),
            None => batch.delete_brn_meta(b"rate_history").unwrap(),
        }
        batch.commit().unwrap();
        results.track(&payload);
        assert!(queue.push(block.clone()).await);
        assert_eq!(tokio::time::timeout(Duration::from_secs(10), results.wait(&payload)).await.unwrap(),
            Some(burst_rpc::ProcessResult::Accepted));
        assert_eq!(store.account_store().get_account(&creator).unwrap().head, block.hash);
    }).await;
    shutdown.shutdown();
    let (mut node, started) = tokio::time::timeout(Duration::from_secs(20), task).await.unwrap().unwrap();
    started.unwrap();
    assert!(store.tx_begin_write().unwrap().uses_normalized_verification_records().unwrap());
    assert!(store.tx_begin_write().unwrap().uses_brn_wallet_records().unwrap());
    node.stop().await.unwrap();
    outcome.expect("prepared admission did not complete");
}

#[tokio::test]
async fn configured_recovery_budget_is_active_before_start_and_survives_in_retained_files() {
    use crate::confirmed_replay::scratch_file::{ScratchFile, ScratchLimit};
    let directory = tempfile::tempdir().unwrap();
    let mut settings = config(directory.path(), 0);
    settings.recovery_scratch_bytes = Some(64 << 20);
    let node = BurstNode::new(settings.clone()).await.unwrap();
    assert!(node.recovery_scratch.is_some());
    let mut file = ScratchFile::new_in(directory.path()).unwrap();
    assert!(file.set_len(64 << 20).is_err(), "durable reservation must reduce scratch capacity");
    drop(node);
    assert!(file.set_len(64 << 20).is_err(), "retained file lost the node's policy");
    assert!(ScratchLimit::acquire(directory.path(), 64 << 20).is_err());
    let restarted = BurstNode::new(settings).await.unwrap();
    assert!(restarted.recovery_scratch.is_some(), "same budget must remain usable");
    drop(restarted);
    drop(file);
    drop(ScratchLimit::acquire(directory.path(), 1).unwrap());
}

#[tokio::test]
async fn genesis_only_brn_recovery_is_durable_before_shutdown() {
    let directory = tempfile::tempdir().unwrap();
    let mut settings = config(directory.path(), 0);
    settings.recovery_scratch_bytes = Some(64 << 20);
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
        let result = node.start_with_services(burst_daemon::start_public_services).await;
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
    assert!(store.tx_begin_write().unwrap().uses_normalized_verification_records().unwrap());
    {
        let live = node.brn_engine.lock().await;
        for _ in 0..2 {
            let batch = node.store.tx_begin_write().unwrap();
            let restored = batch.read_brn_engine(131_072, 64 * 1024 * 1024).unwrap();
            assert!(std::ptr::eq(
                live.get_wallet(&address).unwrap(),
                restored.get_wallet(&address).unwrap()
            ));
            assert!(batch.read_brn_engine(0, usize::MAX).is_err());
            assert!(batch.read_brn_engine(usize::MAX, 0).is_err());
            // Aborted evaluations must retain the startup-primed shared root.
            drop(batch);
        }
    }
    {
        let live = node.trst_engine.lock().await;
        for _ in 0..2 {
            let batch = node.store.tx_begin_write().unwrap();
            let restored = batch.read_trst_engine(node.config.params.trst_expiry_secs).unwrap();
            assert!(live.shares_wallet_snapshot_with(&restored));
            assert!(live.merger_graph.shares_storage_with(&restored.merger_graph));
        }
    }
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
    let error = tokio::time::timeout(Duration::from_secs(10), node.start_with_services(burst_daemon::start_public_services))
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
            let result = node.start_with_services(burst_daemon::start_public_services).await;
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
                cookie: Some(crate::transport::peer_handshake::create_cookie()),
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
            cookie: Some(crate::transport::peer_handshake::create_cookie()),
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
        let error = result.unwrap_err().to_string();
        assert!(error.contains("invalid persisted consti_engine"), "{error}");
        assert!(node.task_handles.is_empty());
        assert!(registry.write().await.take_tasks().is_empty());
        assert!(proofs.is_closed());
        assert!(registry.read().await.is_empty());
        assert!(node
            .start_with_services(burst_daemon::start_public_services)
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
    let result = tokio::time::timeout(Duration::from_secs(5), node.start_with_services(burst_daemon::start_public_services))
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
                let result = target.start_with_services(burst_daemon::start_public_services).await;
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
            let error = result.unwrap_err().to_string();
            assert!(error.contains("invalid persisted consti_engine"), "{error}");
            assert!(target.task_handles.is_empty());
            assert!(registry.read().await.is_empty());
            source.stop().await.unwrap();
        }
    }).await.unwrap();
}

#[tokio::test]
async fn certified_authority_refresh_reuses_only_unchanged_published_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let params = ProtocolParams::burst_defaults();
    let genesis = crate::genesis_state::block(burst_types::NetworkId::Dev, &params);
    let mut batch = store.tx_begin_write().unwrap();
    crate::payment_epoch::initialize(&mut batch, genesis.hash).unwrap();
    batch.commit().unwrap();
    crate::payment_authority::initialize(&store, burst_types::NetworkId::Dev, &params).unwrap();
    let weights = RwLock::new(RepWeightCache::new());
    let elections = RwLock::new(ActiveElections::new(16, 0));
    rebuild_rep_weights(&store, &weights, &elections).await.unwrap();
    let identity = weights.read().await.publication_identity();
    assert!(identity.is_some());
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta("unrelated_payment", b"one").unwrap();
    batch.commit().unwrap();
    rebuild_rep_weights(&store, &weights, &elections).await.unwrap();
    assert_eq!(weights.read().await.publication_identity(), identity);
    weights.write().await.rebuild_from_accounts(std::iter::empty());
    assert_eq!(weights.read().await.publication_identity(), None);
    rebuild_rep_weights(&store, &weights, &elections).await.unwrap();
    assert_eq!(weights.read().await.weight(&genesis.account), 10_000);
    elections.write().await.set_online_weight(0);
    assert_eq!(elections.read().await.publication_identity(), None);
    rebuild_rep_weights(&store, &weights, &elections).await.unwrap();
    assert_eq!(elections.read().await.publication_identity(), identity);
    let key = burst_store::meta::CERTIFIED_PAYMENT_AUTHORITY_KEY;
    let original = store.meta_store().get_meta(key).unwrap();
    let alternate = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[73; 32]).public);
    let changed = bincode::serialize(&(
        genesis.hash,
        vec![burst_consensus::checkpoint::HumanDelegation {
            human: genesis.account.clone(), representative: alternate,
        }],
        6700u16,
        genesis.timestamp.as_secs(),
    )).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(key, &changed).unwrap();
    batch.commit().unwrap();
    // Decodable descriptor with the same epoch must reach the existing
    // authority comparison, never reuse old published weights silently.
    assert!(crate::payment_authority::PaymentAuthority::read(&store).is_ok());
    assert!(rebuild_rep_weights(&store, &weights, &elections).await.is_err());
    assert_eq!(weights.read().await.weight(&genesis.account), 10_000);
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(key, &original).unwrap();
    batch.commit().unwrap();
    rebuild_rep_weights(&store, &weights, &elections).await.unwrap();
    // Force the same canonical genesis bytes through the paged representation.
    struct PagedAuthority;
    impl crate::paged_metadata::Location for PagedAuthority {
        fn descriptor_key(&self) -> &str { burst_store::meta::CERTIFIED_PAYMENT_AUTHORITY_KEY }
        fn page_key(&self, index: usize) -> String {
            format!("{}{:08x}", burst_store::meta::CERTIFIED_PAYMENT_AUTHORITY_PAGE_PREFIX, index)
        }
    }
    let mut batch = store.tx_begin_write().unwrap();
    crate::paged_metadata::write_bytes_at(PagedAuthority, &mut batch, &original).unwrap();
    batch.commit().unwrap();
    rebuild_rep_weights(&store, &weights, &elections).await.unwrap();
    let page = format!("{}00000000", burst_store::meta::CERTIFIED_PAYMENT_AUTHORITY_PAGE_PREFIX);
    let descriptor = store.meta_store().get_meta(key).unwrap();
    let identity = weights.read().await.publication_identity();
    let mut damaged = original.clone();
    damaged[0] ^= 1;
    {
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(&page, &damaged).unwrap();
    }
    rebuild_rep_weights(&store, &weights, &elections).await.unwrap();
    assert_eq!(weights.read().await.publication_identity(), identity);
    let pinned = store.tx_begin_read().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(&page, &damaged).unwrap();
    batch.commit().unwrap();
    assert!(crate::payment_authority::PaymentAuthority::read_at(
        |key| store.meta_store().get_meta_txn(&pinned, key)).unwrap().is_some());
    drop(pinned);
    assert_eq!(store.meta_store().get_meta(key).unwrap(), descriptor);
    assert!(rebuild_rep_weights(&store, &weights, &elections).await.is_err());
    assert_eq!(weights.read().await.weight(&genesis.account), 10_000);
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(&page, &original).unwrap();
    batch.commit().unwrap();
    rebuild_rep_weights(&store, &weights, &elections).await.unwrap();
    let identity = weights.read().await.publication_identity();
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(key, b"malformed").unwrap();
    drop(batch);
    rebuild_rep_weights(&store, &weights, &elections).await.unwrap();
    assert_eq!(weights.read().await.publication_identity(), identity);
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(key, b"malformed").unwrap();
    batch.commit().unwrap();
    assert!(rebuild_rep_weights(&store, &weights, &elections).await.is_err());
    assert_eq!(weights.read().await.weight(&genesis.account), 10_000);
}

#[tokio::test]
async fn configured_database_map_is_used_on_open_and_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let mut configuration = config(directory.path(), 0);
    configuration.database_map_size_bytes = 1536 << 20;
    for _ in 0..2 {
        let node = BurstNode::new(configuration.clone()).await.unwrap();
        assert_eq!(node.store.env().info().map_size, 1536 << 20);
        drop(node);
    }
    configuration.database_map_size_bytes = 0;
    assert!(matches!(BurstNode::new(configuration).await, Err(NodeError::Config(_))));
}
