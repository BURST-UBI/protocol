use super::*;
use crate::{checkpoint_store::chain, verification_deadline};
use burst_store::brn::BrnStore;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deadline_partial_panel_installs_on_every_node_and_recovers_on_a_fresh_observer() {
    deadline_journey(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn governance_changes_preserve_an_active_panel_and_its_paid_stakes_through_recovery() {
    deadline_journey(true).await;
}

async fn change_panel_rules(
    clients: &[NodeClient],
    key: &KeyPair,
    params: &ProtocolParams,
    clock: &AtomicU64,
    at: u64,
) -> ProtocolParams {
    let owner = burst_crypto::derive_address(&key.public);
    let mut governed = params.clone();
    governed.brn_rate *= 2;
    governed.verifier_stake_amount *= 2;
    governed.verification_timeout_secs = 1;
    governed.min_verification_age_secs = 1;
    governed.min_verifier_brn_balance = u128::MAX;
    governed.neither_penalty_cooldown_secs = 7;
    governed.neither_penalty_threshold_bps = 10000;
    governed.neither_window_assignments = 4;
    governed.emergency_voting_duration_secs = 17;
    governed.emergency_quorum_bps = 7501;
    governed.emergency_supermajority_bps = 9001;
    clock.store(at, Ordering::SeqCst);
    let mut proposals = Vec::new();
    for (parameter, current_value, proposed_value) in [
        ("brn_rate", params.brn_rate, governed.brn_rate),
        (
            "verifier_stake_amount",
            params.verifier_stake_amount,
            governed.verifier_stake_amount,
        ),
        (
            "verification_timeout_secs",
            params.verification_timeout_secs as u128,
            1,
        ),
        (
            "min_verification_age_secs",
            params.min_verification_age_secs as u128,
            1,
        ),
        (
            "min_verifier_brn_balance",
            params.min_verifier_brn_balance,
            u128::MAX,
        ),
        (
            "neither_penalty_cooldown_secs",
            params.neither_penalty_cooldown_secs as u128,
            7,
        ),
        (
            "neither_penalty_threshold_bps",
            params.neither_penalty_threshold_bps as u128,
            10000,
        ),
        (
            "neither_window_assignments",
            params.neither_window_assignments as u128,
            4,
        ),
        (
            "emergency_voting_duration_secs",
            params.emergency_voting_duration_secs as u128,
            17,
        ),
        (
            "emergency_quorum_bps",
            params.emergency_quorum_bps as u128,
            7501,
        ),
        (
            "emergency_supermajority_bps",
            params.emergency_supermajority_bps as u128,
            9001,
        ),
    ] {
        let tx = Transaction::GovernanceProposal(
            build_governance_proposal_tx(
                &owner,
                burst_transactions::governance::ProposalContent::ParameterChange {
                    parameter: parameter.into(),
                    current_value,
                    proposed_value,
                },
                Timestamp::new(at),
            )
            .unwrap(),
        );
        let proposal = signed_block(&clients[0], key, tx, params).await;
        submit(clients, &proposal).await;
        let tx = Transaction::ChangeRepresentative(
            build_change_rep_tx(&owner, &owner, Timestamp::new(at)).unwrap(),
        );
        let mut support = signed_block(&clients[0], key, tx, params).await;
        support.block_type = BlockType::GovernanceEndorse;
        support.link = proposal.hash;
        support.brn_balance += params.governance_proposal_cost;
        support.hash = support.compute_hash();
        submit(clients, &sign_state_block(support, &key.private)).await;
        proposals.push(proposal.hash);
    }
    clock.store(at + 1, Ordering::SeqCst);
    for proposal in &proposals {
        let tx = Transaction::GovernanceVote(
            build_governance_vote_tx(
                &owner,
                proposal.into_tx_hash(),
                burst_transactions::governance::GovernanceVote::Yea,
                Timestamp::new(at + 1),
            )
            .unwrap(),
        );
        submit(clients, &signed_block(&clients[0], key, tx, params).await).await;
    }
    clock.store(at + 5, Ordering::SeqCst);
    let activated = tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let mut matching = 0;
            for client in clients {
                let current = client.rpc_call("params_hash", json!({})).await.unwrap();
                matching +=
                    usize::from(current["params_hash"] == governed.params_hash().to_string());
            }
            if matching == clients.len() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    if activated.is_err() {
        for (index, client) in clients.iter().enumerate() {
            eprintln!(
                "node {index}: current {:?}, wanted {}",
                client.rpc_call("params_hash", json!({})).await,
                governed.params_hash()
            );
            for proposal in &proposals {
                eprintln!(
                    "node {index}: proposal {:?}",
                    client
                        .rpc_call(
                            "governance_proposal_info",
                            json!({"hash":proposal.to_string()})
                        )
                        .await
                );
            }
        }
    }
    activated.expect("governance changes did not activate on every node");
    governed
}

async fn emergency_ballots(
    clients: &[NodeClient],
    keys: &[KeyPair],
    params: &mut ProtocolParams,
    clock: &AtomicU64,
) -> Vec<BlockHash> {
    let at = clock.load(Ordering::SeqCst) + 1;
    clock.store(at, Ordering::SeqCst);
    let mut proposals = Vec::new();
    for value in [23, 99] {
        // A non-genesis human submits; neither a foundation key nor endorsements are required.
        let proposer = &keys[1];
        let owner = burst_crypto::derive_address(&proposer.public);
        let state = clients[0].signing_state(&owner).await.unwrap();
        let content = burst_governance::ProposalContent::Emergency {
            description: format!("Change emergency duration to {value}"),
            param: burst_governance::GovernableParam::EmergencyVotingDurationSecs,
            new_value: value,
        };
        let (proposal, body) = burst_wallet_core::governance_content::build_signed_proposal(
            &state,
            proposer,
            &content,
            params.params_hash(),
            Timestamp::new(at),
        )
        .unwrap();
        let response = clients[0].rpc_call("process", json!({
            "block":serde_json::to_string(&proposal).unwrap(), "proposal_content":hex::encode(body.bytes()),
        })).await.unwrap();
        assert_eq!(response["accepted"], true, "{response}");
        for client in clients {
            client
                .wait_confirmed(&proposal.hash.to_string(), Duration::from_secs(25))
                .await
                .unwrap();
        }
        super::governance_content::wait_phase(clients, &proposal.hash.to_string(), "Exploration")
            .await;
        proposals.push(proposal.hash);
    }
    for (index, proposal) in proposals.iter().enumerate() {
        for key in &keys[..if index == 0 { 4 } else { 3 }] {
            let owner = burst_crypto::derive_address(&key.public);
            let tx = Transaction::GovernanceVote(
                build_governance_vote_tx(
                    &owner,
                    proposal.into_tx_hash(),
                    burst_transactions::governance::GovernanceVote::Yea,
                    Timestamp::new(at),
                )
                .unwrap(),
            );
            submit(clients, &signed_block(&clients[0], key, tx, params).await).await;
        }
    }
    // The service certifies cuts strictly before its local clock. Counting is
    // at +18 and activation at +19, so advance beyond both boundaries.
    clock.store(at + 20, Ordering::SeqCst);
    for (proposal, phase) in proposals.iter().zip(["Activated", "Rejected"]) {
        super::governance_content::wait_phase(clients, &proposal.to_string(), phase).await;
    }
    params.emergency_voting_duration_secs = 23;
    for client in clients {
        let current = client.rpc_call("params_hash", json!({})).await.unwrap();
        assert_eq!(current["params_hash"], params.params_hash().to_string());
    }
    proposals
}

fn eligible_at(
    store: &burst_store_lmdb::LmdbStore,
    params: &ProtocolParams,
    at: u64,
) -> Vec<WalletAddress> {
    let heads = crate::confirmed_history::ConfirmedHistory::heads_at(store, at).unwrap();
    let history = crate::confirmed_history::ConfirmedHistory::read(
        store,
        NetworkId::Dev,
        crate::genesis_state::block(NetworkId::Dev, params).hash,
        at,
        &heads,
        &[],
    )
    .unwrap();
    let events = chain::load(store, NetworkId::Dev, params).unwrap();
    crate::confirmed_replay::ConfirmedReplay::replay_with_events(
        &history,
        NetworkId::Dev,
        params,
        &events,
    )
    .unwrap()
    .eligible_verifiers()
    .unwrap()
}

fn check_governed_selection_state(
    store: &burst_store_lmdb::LmdbStore,
    params: &ProtocolParams,
    absent: &WalletAddress,
    deadline: u64,
    at: u64,
) {
    let current: ProtocolParams = bincode::deserialize(
        &store
            .brn_store()
            .get_meta(b"protocol_params")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(current.min_verification_age_secs, 1);
    assert_eq!(current.min_verifier_brn_balance, u128::MAX);
    assert_eq!(current.neither_penalty_cooldown_secs, 7);
    assert_eq!(current.neither_penalty_threshold_bps, 10000);
    assert_eq!(current.neither_window_assignments, 4);
    assert_eq!(current.emergency_voting_duration_secs, 23);
    assert_eq!(current.emergency_quorum_bps, 7501);
    assert_eq!(current.emergency_supermajority_bps, 9001);
    let snapshot = burst_verification::OrchestratorSnapshot::decode(
        &store
            .meta_store()
            .get_meta(crate::endorsement_commit::SNAPSHOT_KEY)
            .unwrap(),
    )
    .unwrap();
    assert!(!snapshot.penalized_verifiers.contains_key(absent));
    assert_eq!(snapshot.neither_tracker.total_assignments(absent), 1);
    assert_eq!(snapshot.neither_tracker.neither_count(absent), 1);
    assert!(!snapshot.neither_tracker.is_penalized(absent));
    let state = burst_verification::VerificationOrchestrator::restore(snapshot);
    assert!(!state.is_penalized(absent, deadline + 6));
    assert!(!state.is_penalized(absent, deadline + 7));
    assert!(eligible_at(store, params, at).is_empty());
}

async fn deadline_journey(change_rules: bool) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(AtomicU64::new(time(10).as_secs()));
    let released = Arc::new(AtomicBool::new(false));
    let relay = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_url = format!("http://{}", relay.local_addr().unwrap());
    let relay_release = released.clone();
    let relay_task = tokio::spawn(async move {
        let beacons: Vec<burst_vrf::DrandBeacon> =
            serde_json::from_str(include_str!("../../tests/fixtures/selection-beacons.json"))
                .unwrap();
        loop {
            let (mut socket, _) = relay.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 1024];
                let length = socket.read(&mut buffer).await.unwrap();
                if length == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..length]);
                if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    break;
                }
                assert!(request.len() <= 4096);
            }
            let path = std::str::from_utf8(&request)
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap();
            let round = path.rsplit('/').next().unwrap().parse::<u64>().unwrap();
            let beacon = beacons.iter().find(|beacon| beacon.round == round);
            let (status, body) = if relay_release.load(Ordering::SeqCst) {
                (
                    "200 OK",
                    serde_json::to_string(
                        beacon.unwrap_or_else(|| panic!("missing round {round}")),
                    )
                    .unwrap(),
                )
            } else {
                ("503 Service Unavailable", "{}".to_string())
            };
            let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let params = ProtocolParams {
        brn_rate: BRN_UNIT,
        min_work_difficulty: 0,
        bootstrap_exit_threshold: 4,
        endorsement_threshold: 1,
        endorsement_burn_amount: 1,
        num_verifiers: 3,
        verification_threshold_bps: 6000,
        verifier_stake_amount: 10 * BRN_UNIT,
        verification_timeout_secs: 60,
        min_verification_age_secs: 0,
        new_wallet_spending_limit: 0,
        new_wallet_tx_limit_per_day: 0,
        trst_expiry_secs: u64::MAX,
        governance_proposal_cost: 1,
        governance_proposal_endorsements: 1,
        governance_proposal_duration_secs: 1,
        governance_exploration_duration_secs: 1,
        governance_cooldown_duration_secs: 0,
        governance_propagation_buffer_secs: 1,
        governance_quorum_bps: 2000,
        ..ProtocolParams::burst_defaults()
    };
    let ports: Vec<_> = (0..4).map(|_| port()).collect();
    let mut clients = Vec::new();
    let mut workers = Vec::new();
    let mut orchestrators = Vec::new();
    let mut stores = Vec::new();
    let mut configs = Vec::new();
    for index in 0..4 {
        let key_file = directory.path().join(format!("representative-{index}.key"));
        std::fs::write(&key_file, hex::encode([index as u8; 32])).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let config = NodeConfig {
            network: NetworkId::Dev,
            data_dir: directory.path().join(format!("node-{index}")),
            port: ports[index],
            rpc_port: port(),
            enable_upnp: false,
            nts_sources: vec![],
            representative_key_file: Some(key_file),
            params: params.clone(),
            bootstrap_peers: ports
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, port)| format!("127.0.0.1:{port}"))
                .collect(),
            ..NodeConfig::default()
        };
        clients.push(NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap());
        configs.push(config.clone());
        let mut node = fixture_node(config, &clock, &relay_url).await;
        orchestrators.push(node.verification_orchestrator.clone());
        stores.push(node.store.clone());
        let shutdown = node.shutdown.clone();
        workers.push((
            shutdown,
            tokio::spawn(async move {
                node.start().await.unwrap();
                node.stop().await.unwrap();
            }),
        ));
    }

    let observed_stores = stores.clone();
    let observed_clock = clock.clone();
    let observed_params = params.clone();
    let mut journey = tokio::spawn(async move {
        for client in &clients {
            loop {
                if client
                    .rpc_call("peers", json!({}))
                    .await
                    .is_ok_and(|value| value["count"].as_u64().unwrap_or(0) >= 3)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let keys: Vec<_> = (0..5)
            .map(|index| burst_crypto::keypair_from_seed(&[index; 32]))
            .collect();
        let addresses: Vec<_> = keys
            .iter()
            .map(|key| burst_crypto::derive_address(&key.public))
            .collect();
        for index in 1..5 {
            if index < 4 {
                let tx = Transaction::Endorse(
                    build_endorse_tx(&addresses[0], &addresses[index], 1, time(10)).unwrap(),
                );
                submit(
                    &clients,
                    &signed_block(&clients[0], &keys[0], tx, &params).await,
                )
                .await;
            }
            let tx = Transaction::Burn(
                build_burn_tx(&addresses[0], &addresses[index], BRN_UNIT, time(10)).unwrap(),
            );
            let burn = signed_block(&clients[0], &keys[0], tx, &params).await;
            submit(&clients, &burn).await;
            let tx = Transaction::Receive(
                build_receive_tx(
                    &addresses[index],
                    burn.hash.into_tx_hash(),
                    BRN_UNIT,
                    time(10),
                )
                .unwrap(),
            );
            let opening = signed_block(&clients[0], &keys[index], tx, &params).await;
            submit(&clients, &opening).await;
            if index < 4 {
                let mut opt_in = opening.clone();
                opt_in.block_type = BlockType::VerifierOptIn;
                opt_in.previous = opening.hash;
                opt_in.link = BlockHash::ZERO;
                opt_in.origin = TxHash::ZERO;
                opt_in.transaction =
                    burst_crypto::hash_transaction(format!("fixture-opt-in-{index}").as_bytes());
                opt_in.hash = opt_in.compute_hash();
                submit(&clients, &sign_state_block(opt_in, &keys[index].private)).await;
            }
        }
        clock.store(time(20).as_secs(), Ordering::SeqCst);
        wait_human_authority(&stores, 4).await;
        let tx = Transaction::Endorse(
            build_endorse_tx(&addresses[0], &addresses[4], 1, time(100)).unwrap(),
        );
        clock.store(time(100).as_secs(), Ordering::SeqCst);
        submit(
            &clients,
            &signed_block(&clients[0], &keys[0], tx, &params).await,
        )
        .await;
        let request = loop {
            let chain =
                crate::checkpoint_store::chain::load(&stores[0], NetworkId::Dev, &params).unwrap();
            if let Some(checkpoint) = chain
                .iter()
                .find_map(crate::checkpoint_store::chain::CertifiedEvent::selection)
            {
                break checkpoint.replayed().request().clone();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert_eq!(request.round(), 102);
        for orchestrator in &orchestrators {
            assert!(orchestrator
                .lock()
                .await
                .selection_context(&addresses[4])
                .is_none());
        }
        clock.store(request.beacon_time().unwrap(), Ordering::SeqCst);
        released.store(true, Ordering::SeqCst);
        let mut contexts = Vec::new();
        for orchestrator in &orchestrators {
            loop {
                let state = orchestrator.lock().await;
                if let Some(context) = state.selection_context(&addresses[4]) {
                    let mut selected = state
                        .get_state(&addresses[4])
                        .unwrap()
                        .selected_verifiers
                        .clone();
                    selected.sort_by(|left, right| left.as_str().cmp(right.as_str()));
                    let mut expected = addresses[1..4].to_vec();
                    expected.sort_by(|left, right| left.as_str().cmp(right.as_str()));
                    assert_eq!(selected, expected);
                    contexts.push(context);
                    break;
                }
                drop(state);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        assert!(contexts.iter().all(|context| *context == contexts[0]));
        clock.store(request.beacon_time().unwrap() + 2, Ordering::SeqCst);
        let activation_key = crate::verification_activation::key(contexts[0]);
        let activation_bytes = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let installed: Vec<_> = stores
                    .iter()
                    .filter_map(|store| store.meta_store().get_meta(&activation_key).ok())
                    .collect();
                if installed.len() == stores.len()
                    && installed.iter().all(|bytes| *bytes == installed[0])
                {
                    let activation = crate::verification_activation::decode(&installed[0]).unwrap();
                    assert_eq!(activation.started_at, request.beacon_time().unwrap() + 1);
                    assert_eq!(
                        activation.deadline,
                        activation.started_at + params.verification_timeout_secs
                    );
                    for store in &stores {
                        let events =
                            crate::checkpoint_store::chain::load(store, NetworkId::Dev, &params)
                                .unwrap();
                        let last = events.last().unwrap();
                        assert!(matches!(
                            last,
                            crate::checkpoint_store::chain::CertifiedEvent::Activation(_)
                        ));
                        assert_eq!(
                            store.meta_store().payment_epoch().unwrap(),
                            last.certificate().checkpoint.hash()
                        );
                    }
                    break installed[0].clone();
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("committee activation did not install on every node");

        let activation = crate::verification_activation::decode(&activation_bytes).unwrap();
        let mut vote_params = params.clone();
        let mut emergencies = Vec::new();
        for index in 1..3 {
            if change_rules && index == 2 {
                for store in &stores {
                    assert_eq!(
                        eligible_at(store, &params, clock.load(Ordering::SeqCst)).len(),
                        3
                    );
                }
                vote_params = change_panel_rules(
                    &clients,
                    &keys[0],
                    &params,
                    &clock,
                    activation.started_at + 3,
                )
                .await;
                for store in &stores {
                    assert_eq!(
                        store.meta_store().get_meta(&activation_key).unwrap(),
                        activation_bytes
                    );
                    assert!(eligible_at(store, &params, clock.load(Ordering::SeqCst)).is_empty());
                }
                emergencies = emergency_ballots(&clients, &keys, &mut vote_params, &clock).await;
            }
            let before = Timestamp::new(clock.load(Ordering::SeqCst));
            let tx = Transaction::VerificationVote(
                build_verification_vote_tx(
                    &addresses[index],
                    &addresses[4],
                    contexts[0],
                    1,
                    vote_params.verifier_stake_amount,
                    before,
                )
                .unwrap(),
            );
            submit(
                &clients,
                &signed_block(&clients[0], &keys[index], tx, &vote_params).await,
            )
            .await;
        }
        for client in &clients {
            let status = client
                .rpc_call(
                    "verification_status",
                    json!({"account":addresses[4].as_str()}),
                )
                .await
                .unwrap();
            assert_ne!(status["status"], "verified");
            let components = client
                .rpc_call("brn_components", json!({"account":addresses[1].as_str()}))
                .await
                .unwrap();
            assert_eq!(components["total_staked"], (10 * BRN_UNIT).to_string());
        }
        clock.store(activation.deadline + 2, Ordering::SeqCst);
        let completion_key = verification_deadline::key(contexts[0]);
        let completion_bytes = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let values: Vec<_> = stores
                    .iter()
                    .filter_map(|store| store.meta_store().get_meta(&completion_key).ok())
                    .collect();
                if values.len() == stores.len() && values.iter().all(|value| *value == values[0]) {
                    break values[0].clone();
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("normal deadline did not install on every node");
        let completion = verification_deadline::decode(&completion_bytes).unwrap();
        assert_eq!(completion.absent, vec![addresses[3].clone()]);
        assert_eq!(completion.completed_at, activation.deadline + 1);
        wait_human_authority(&stores, 5).await;
        for client in &clients {
            let status = client
                .rpc_call(
                    "verification_status",
                    json!({"account":addresses[4].as_str()}),
                )
                .await
                .unwrap();
            assert_eq!(status["status"], "verified");
            assert_eq!(status["verified_at"], completion.completed_at);
            assert_eq!(status["selection_available"], false);
            for account in &addresses[1..4] {
                let components = client
                    .rpc_call("brn_components", json!({"account":account.as_str()}))
                    .await
                    .unwrap();
                assert_eq!(components["total_staked"], "0");
                assert_eq!(components["total_burned"], "0");
                assert!(components["release_sources"].as_array().unwrap().is_empty());
                let pending = client
                    .rpc_call("account_pending", json!({"account":account.as_str()}))
                    .await
                    .unwrap();
                assert!(pending["pending"].as_array().unwrap().is_empty());
            }
        }
        let spend_at = Timestamp::new(activation.deadline + 4);
        clock.store(spend_at.as_secs(), Ordering::SeqCst);
        let available = {
            let batch = stores[0].tx_begin_write().unwrap();
            let state = crate::payment_transition::restore(&batch, &params).unwrap();
            let wallet = state.brn.get_wallet(&addresses[1]).unwrap();
            let balance = state.brn.compute_balance(wallet, spend_at);
            assert_eq!(
                balance,
                wallet
                    .spendable_balance_checked(&state.brn.rate_history, spend_at)
                    .unwrap()
            );
            balance
        };
        let tx = Transaction::Burn(
            build_burn_tx(&addresses[1], &addresses[0], available, spend_at).unwrap(),
        );
        let spend = signed_block(&clients[0], &keys[1], tx, &vote_params).await;
        assert!(spend.release_sources.is_empty());
        submit(&clients, &spend).await;
        let target_tx = Transaction::Burn(
            build_burn_tx(&addresses[4], &addresses[0], BRN_UNIT, spend_at).unwrap(),
        );
        let target_spend = signed_block(&clients[0], &keys[4], target_tx, &vote_params).await;
        submit(&clients, &target_spend).await;
        if change_rules {
            for store in &stores {
                check_governed_selection_state(
                    store,
                    &params,
                    &addresses[3],
                    activation.deadline,
                    spend_at.as_secs(),
                );
            }
        }
        (
            addresses,
            completion_bytes,
            spend.hash,
            target_spend.hash,
            emergencies,
        )
    });
    let result = tokio::time::timeout(Duration::from_secs(240), &mut journey).await;
    if result.is_err() {
        journey.abort();
        let _ = journey.await;
    }
    for (shutdown, _) in &workers {
        shutdown.shutdown();
    }
    for (_, worker) in workers.drain(..) {
        tokio::time::timeout(Duration::from_secs(20), worker)
            .await
            .unwrap()
            .unwrap();
    }
    let (addresses, completion_bytes, spend, target_spend, emergencies) =
        result.expect("deadline live journey timed out").unwrap();
    for store in &observed_stores {
        let events = chain::load(store, NetworkId::Dev, &observed_params).unwrap();
        assert!(events
            .iter()
            .any(|event| matches!(event, chain::CertifiedEvent::Deadline(_))));
    }
    let mut observer = configs[0].clone();
    observer.data_dir = directory.path().join("deadline-observer");
    observer.port = port();
    observer.rpc_port = port();
    observer.enable_representative = false;
    observer.representative_key_file = None;
    let observer_client =
        NodeClient::new(format!("http://127.0.0.1:{}", observer.rpc_port)).unwrap();
    configs.push(observer);
    let mut recovered_stores = Vec::new();
    for config in configs {
        let mut node = fixture_node(config, &observed_clock, &relay_url).await;
        recovered_stores.push(node.store.clone());
        workers.push((
            node.shutdown.clone(),
            tokio::spawn(async move {
                node.start().await.unwrap();
                node.stop().await.unwrap();
            }),
        ));
    }
    let recovery = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            if observer_client
                .rpc_call(
                    "verification_status",
                    json!({"account":addresses[4].as_str()}),
                )
                .await
                .is_ok_and(|status| status["status"] == "verified")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        observer_client
            .wait_confirmed(&spend.to_string(), Duration::from_secs(30))
            .await
            .unwrap();
        observer_client
            .wait_confirmed(&target_spend.to_string(), Duration::from_secs(30))
            .await
            .unwrap();
        for (proposal, phase) in emergencies.iter().zip(["Activated", "Rejected"]) {
            super::governance_content::wait_phase(
                std::slice::from_ref(&observer_client),
                &proposal.to_string(),
                phase,
            )
            .await;
        }
        let completion = verification_deadline::decode(&completion_bytes).unwrap();
        for store in recovered_stores {
            assert_eq!(
                store
                    .meta_store()
                    .get_meta(&verification_deadline::key(completion.activation.context))
                    .unwrap(),
                completion_bytes
            );
            if change_rules {
                check_governed_selection_state(
                    &store,
                    &observed_params,
                    &addresses[3],
                    completion.activation.deadline,
                    observed_clock.load(Ordering::SeqCst),
                );
            }
        }
    })
    .await;
    for (shutdown, _) in &workers {
        shutdown.shutdown();
    }
    for (_, worker) in workers {
        tokio::time::timeout(Duration::from_secs(20), worker)
            .await
            .unwrap()
            .unwrap();
    }
    relay_task.abort();
    let _ = relay_task.await;
    recovery.expect("deadline fresh observer did not recover");
}
