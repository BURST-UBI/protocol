use super::governance_certified::{installed, stop};
use super::*;
use burst_governance::{
    key_delegation::view::{Direction, Query},
    GovernableParam,
};

// Explicitly invoked: needs the built Tauri wallet, Vite, Xvfb and WebKitWebDriver.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a native desktop wallet and WebKitWebDriver"]
async fn desktop_scoped_signing_counts_and_recovers_on_certified_nodes() {
    desktop_journey(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a native desktop wallet and WebKitWebDriver"]
async fn desktop_constitution_authoring_and_votes_activate_and_recover() {
    desktop_journey(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a native desktop wallet and WebKitWebDriver"]
async fn desktop_parameter_authoring_and_votes_activate_and_recover() {
    desktop_journey(false, true).await;
}

async fn desktop_journey(constitution: bool, parameter_authoring: bool) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let initial_time = Timestamp::now().as_secs() - 10;
    let clock = Arc::new(AtomicU64::new(initial_time));
    let real_clock = clock.clone();
    let use_wall_clock = Arc::new(AtomicBool::new(false));
    let ticking = use_wall_clock.clone();
    let ticker = tokio::spawn(async move {
        loop {
            if ticking.load(Ordering::SeqCst) {
                real_clock.store(Timestamp::now().as_secs(), Ordering::SeqCst);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    let params = ProtocolParams {
        brn_rate: BRN_UNIT,
        min_work_difficulty: 0,
        bootstrap_exit_threshold: 4,
        endorsement_burn_amount: 1,
        new_wallet_spending_limit: 0,
        new_wallet_tx_limit_per_day: 0,
        governance_proposal_cost: 1,
        governance_proposal_duration_secs: if constitution || parameter_authoring {
            15
        } else {
            3
        },
        governance_exploration_duration_secs: 240,
        governance_cooldown_duration_secs: 1,
        governance_propagation_buffer_secs: 1,
        governance_proposal_endorsements: 1,
        governance_supermajority_bps: 6000,
        governance_quorum_bps: 6000,
        consti_supermajority_bps: 6000,
        consti_quorum_bps: 6000,
        ..ProtocolParams::burst_defaults()
    };
    let ports = [port(), port(), port()];
    let mut configs = Vec::new();
    let mut clients = Vec::new();
    let mut stores = Vec::new();
    let mut workers = Vec::new();
    for index in 0..3 {
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
            enable_faucet: false,
            nts_sources: vec![],
            representative_key_file: Some(key_file),
            params: params.clone(),
            bootstrap_peers: ports
                .iter()
                .enumerate()
                .filter(|(n, _)| *n != index)
                .map(|(_, p)| format!("127.0.0.1:{p}"))
                .collect(),
            ..NodeConfig::default()
        };
        clients.push(NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap());
        configs.push(config.clone());
        let mut node = fixture_node(config, &clock, "http://127.0.0.1:1").await;
        stores.push(node.store.clone());
        workers.push((
            node.shutdown.clone(),
            tokio::spawn(async move {
                node.start().await.unwrap();
                node.stop().await.unwrap();
            }),
        ));
    }
    let rpc_url = format!("http://127.0.0.1:{}", configs[0].rpc_port);
    let journey_clients = clients.clone();
    let journey_stores = stores.clone();
    let journey_params = params.clone();
    let journey_clock = clock.clone();
    let desktop_result = directory.path().join("desktop-result.json");
    let mut journey = tokio::spawn(async move {
        let clients = journey_clients;
        let keys: Vec<_> = (0..3)
            .map(|n| burst_crypto::keypair_from_seed(&[n; 32]))
            .collect();
        let addresses: Vec<_> = keys
            .iter()
            .map(|key| burst_crypto::derive_address(&key.public))
            .collect();
        for client in &clients {
            while !client
                .rpc_call("peers", json!({}))
                .await
                .is_ok_and(|v| v["count"].as_u64().unwrap_or(0) >= 2)
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        for index in 1..3 {
            let tx = Transaction::Endorse(
                build_endorse_tx(
                    &addresses[0],
                    &addresses[index],
                    1,
                    Timestamp::new(initial_time),
                )
                .unwrap(),
            );
            submit(
                &clients,
                &signed_block(&clients[0], &keys[0], tx, &journey_params).await,
            )
            .await;
            let tx = Transaction::Burn(
                build_burn_tx(
                    &addresses[0],
                    &addresses[index],
                    1,
                    Timestamp::new(initial_time),
                )
                .unwrap(),
            );
            let source = signed_block(&clients[0], &keys[0], tx, &journey_params).await;
            submit(&clients, &source).await;
            let tx = Transaction::Receive(
                build_receive_tx(
                    &addresses[index],
                    source.hash.into_tx_hash(),
                    1,
                    Timestamp::new(initial_time),
                )
                .unwrap(),
            );
            submit(
                &clients,
                &signed_block(&clients[0], &keys[index], tx, &journey_params).await,
            )
            .await;
        }
        // Finish the setup batch before the roster cut retires its payment epoch.
        journey_clock.store(Timestamp::now().as_secs(), Ordering::SeqCst);
        installed(&journey_stores, 1, &journey_params).await;
        wait_human_authority(&journey_stores, 3).await;
        use_wall_clock.store(true, Ordering::SeqCst);
        let proposal = if !constitution && !parameter_authoring {
            let result = clients[0]
                .propose_parameter_signed(&keys[0], GovernableParam::BrnRate, 2 * BRN_UNIT)
                .await
                .unwrap();
            let hash = burst_wallet_core::signed_client::parse_hash(&result.hash).unwrap();
            clients[0]
                .endorse_proposal_signed(&keys[0], hash.into_tx_hash(), 1)
                .await
                .unwrap();
            governance_content::wait_phase(&clients, &result.hash, "Exploration").await;
            result.hash
        } else {
            String::new()
        };
        let runner = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../wallet/tests/native-session.mjs");
        let output = tokio::process::Command::new("node")
            .arg(runner)
            .arg(if parameter_authoring {
                "--parameter-fixture"
            } else if constitution {
                "--constitution-fixture"
            } else {
                "--scoped-delegation-fixture"
            })
            .env("NATIVE_FIXTURE_RPC_URL", rpc_url)
            .env("NATIVE_PROPOSAL", &proposal)
            .env("NATIVE_RESULT_PATH", &desktop_result)
            .kill_on_drop(true)
            .output()
            .await
            .unwrap();
        eprintln!("{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "native desktop signing failed");
        let mut withdrawn = None;
        let proposal = if constitution || parameter_authoring {
            let result: serde_json::Value =
                serde_json::from_slice(&std::fs::read(desktop_result).unwrap()).unwrap();
            if parameter_authoring {
                withdrawn = Some(result["withdrawn"].as_str().unwrap().to_string());
                check_withdrawal(&clients, withdrawn.as_deref().unwrap()).await;
            }
            result["proposal"].as_str().unwrap().to_string()
        } else {
            check_grants(&clients).await;
            proposal
        };
        let info = clients[0]
            .rpc_call("governance_proposal_info", json!({"hash": proposal}))
            .await
            .unwrap();
        assert_eq!(info["phase"], "Exploration");
        let deadline = info["phase_deadline"].as_u64().unwrap();
        (proposal, deadline, withdrawn)
    });
    let result = tokio::time::timeout(Duration::from_secs(300), &mut journey).await;
    if result.is_err() {
        journey.abort();
        let _ = journey.await;
    }
    ticker.abort();
    let _ = ticker.await;
    if !matches!(&result, Ok(Ok(_))) {
        for (index, store) in stores.iter().enumerate() {
            let authority = crate::payment_authority::PaymentAuthority::read(store).unwrap();
            let events =
                crate::checkpoint_store::chain::load(store, NetworkId::Dev, &params).unwrap();
            eprintln!(
                "node {index}: events={}, authority={:?}",
                events.len(),
                authority.map(|a| (a.epoch, a.roster.total_humans()))
            );
        }
        stop(&mut workers).await;
        panic!("desktop journey failed: {result:?}");
    }
    let (proposal, deadline, withdrawn) = result.unwrap().unwrap();
    clock.store(deadline + 5, Ordering::SeqCst);
    let check_stores = stores.clone();
    let check_clients = clients.clone();
    let check_params = params.clone();
    let check_proposal = proposal.clone();
    let checked = tokio::spawn(async move {
        governance_content::wait_phase(&check_clients, &check_proposal, "Activated").await;
        let height =
            crate::checkpoint_store::chain::load(&check_stores[0], NetworkId::Dev, &check_params)
                .unwrap()
                .len();
        let epoch = installed(&check_stores, height, &check_params).await;
        check_result(&check_clients, &check_proposal).await;
        if constitution {
            check_document(&check_clients).await;
        } else if parameter_authoring {
            check_parameter(&check_clients).await;
        }
        (epoch, height)
    })
    .await;
    stop(&mut workers).await;
    let (epoch, height) = checked.unwrap();
    drop(stores);
    drop(clients);
    let mut observer = configs[0].clone();
    observer.data_dir = directory.path().join("fresh-observer");
    observer.port = port();
    observer.rpc_port = port();
    observer.enable_representative = false;
    observer.representative_key_file = None;
    observer.bootstrap_peers = ports.iter().map(|p| format!("127.0.0.1:{p}")).collect();
    configs.push(observer);
    let mut clients = Vec::new();
    let mut stores = Vec::new();
    for config in configs {
        clients.push(NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap());
        let mut node = fixture_node(config, &clock, "http://127.0.0.1:1").await;
        stores.push(node.store.clone());
        workers.push((
            node.shutdown.clone(),
            tokio::spawn(async move {
                node.start().await.unwrap();
                node.stop().await.unwrap();
            }),
        ));
    }
    let recovered = tokio::spawn(async move {
        assert_eq!(installed(&stores, height, &params).await, epoch);
        if constitution {
            check_document(&clients).await;
        } else if parameter_authoring {
            check_parameter(&clients).await;
        } else {
            check_grants(&clients).await;
        }
        check_result(&clients, &proposal).await;
        if let Some(withdrawn) = withdrawn {
            check_withdrawal(&clients, &withdrawn).await;
        }
    })
    .await;
    stop(&mut workers).await;
    recovered.unwrap();
}

async fn check_withdrawal(clients: &[NodeClient], proposal: &str) {
    for client in clients {
        let info = client
            .rpc_call("governance_proposal_info", json!({"hash":proposal}))
            .await
            .unwrap();
        assert_eq!(info["phase"], "Withdrawn");
        assert_eq!(info["votes_yea"], 0);
        assert_eq!(info["votes_nay"], 0);
    }
}

async fn check_parameter(clients: &[NodeClient]) {
    for client in clients {
        let params = client.rpc_call("protocol_params", json!({})).await.unwrap();
        assert_eq!(params["brn_rate"], (2 * BRN_UNIT + 1).to_string());
        let rate = params["governable_parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == "brn_rate")
            .unwrap();
        assert_eq!(rate["current"], (2 * BRN_UNIT + 1).to_string());
        assert_eq!(
            client.rpc_call("consti", json!({})).await.unwrap()["version"],
            0
        );
    }
}

async fn check_document(clients: &[NodeClient]) {
    for client in clients {
        let document = client.rpc_call("consti", json!({})).await.unwrap();
        assert_eq!(document["version"], 1);
        let articles = document["articles"].as_array().unwrap();
        assert_eq!(articles.len(), 3);
        assert_eq!(articles[0]["id"], "1");
        assert_eq!(articles[0]["text"], "人は平等です 🌍");
        assert_eq!(articles[1]["id"], "3");
        assert_eq!(articles[2]["id"], "4");
        assert_eq!(articles[2]["title"], "共同の権利");
        assert_eq!(
            articles[2]["text"],
            "Every verified person has one vote. 🌍"
        );
        let params = client.rpc_call("protocol_params", json!({})).await.unwrap();
        assert_eq!(params["brn_rate"], BRN_UNIT.to_string());
    }
}

async fn check_grants(clients: &[NodeClient]) {
    let person = |n| {
        burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[n; 32]).public).to_string()
    };
    for client in clients {
        let outgoing = client
            .delegation_keys(&Query {
                account: person(1),
                direction: Direction::Outgoing,
                scope: None,
                cursor: None,
            })
            .await
            .unwrap();
        assert_eq!(outgoing.entries.len(), 1);
        assert_eq!(outgoing.entries[0].generation, "2");
        assert!(outgoing.entries[0].grant.is_none() && outgoing.entries[0].confirmed);
        let incoming = client
            .delegation_keys(&Query {
                account: person(0),
                direction: Direction::Incoming,
                scope: None,
                cursor: None,
            })
            .await
            .unwrap();
        assert_eq!(incoming.entries.len(), 1);
        assert_eq!(incoming.entries[0].owner, person(2));
        assert!(incoming.entries[0].confirmed);
    }
}

async fn check_result(clients: &[NodeClient], proposal: &str) {
    for client in clients {
        let view = client
            .rpc_call("governance_proposal_info", json!({"hash": proposal}))
            .await
            .unwrap();
        assert_eq!(view["phase"], "Activated", "{view}");
        assert_eq!(
            view["votes_yea"], 2,
            "revoked owner's ballot must not count: {view}"
        );
        assert_eq!(view["votes_nay"], 0);
        assert_eq!(view["votes_abstain"], 0);
    }
}
