//! Real signed governance over RPC/P2P. Short timelines are isolated to this fixture.
use burst_governance::GovernableParam;
use burst_node::{BurstNode, NodeConfig};
use burst_store::meta::MetaStore;
use burst_transactions::{governance::GovernanceVote, Transaction};
use burst_types::{NetworkId, Timestamp, TxHash};
use burst_wallet_core::{
    signed_client::parse_hash, transaction_builder::build_governance_vote_tx, NodeClient,
};
use serde_json::{json, Value};
use std::time::Duration;



async fn launch(
    config: NodeConfig,
) -> (
    std::sync::Arc<burst_node::shutdown::ShutdownController>,
    tokio::task::JoinHandle<()>,
    std::sync::Arc<burst_store_lmdb::LmdbStore>,
) {
    let mut node = BurstNode::new(config).await.unwrap();
    let shutdown = node.shutdown.clone();
    let store = node.store.clone();
    let task = tokio::spawn(async move {
        node.start_with_services(burst_daemon::start_public_services).await.unwrap();
        tokio::time::timeout(Duration::from_secs(15), node.stop())
            .await
            .expect("node shutdown timed out")
            .unwrap();
    });
    (shutdown, task, store)
}

async fn wait_phase(clients: &[NodeClient], hash: &str, phase: &str) {
    let mut last = vec![String::new(); clients.len()];
    let result = tokio::time::timeout(Duration::from_secs(100), async {
        loop {
            let mut ready = true;
            for (index, client) in clients.iter().enumerate() {
                let proposal = client
                    .rpc_call("governance_proposal_info", json!({"hash":hash}))
                    .await;
                last[index] = format!("{proposal:?}");
                ready &= proposal.is_ok_and(|value| value["phase"] == phase);
            }
            if ready {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    if result.is_err() {
        for (index, client) in clients.iter().enumerate() {
            eprintln!(
                "node {index} peers: {:?}",
                client.rpc_call("peers", json!({})).await
            );
            let creator =
                burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[0; 32]).public);
            eprintln!(
                "node {index} account head: {:?}",
                client.signing_state(&creator).await.map(|state| state.head)
            );
        }
    }
    assert!(
        result.is_ok(),
        "governance {hash} never reached {phase}: {last:?}"
    );
}

async fn wait_ready(
    clients: &[NodeClient],
    nodes: &[(
        std::sync::Arc<burst_node::shutdown::ShutdownController>,
        tokio::task::JoinHandle<()>,
        std::sync::Arc<burst_store_lmdb::LmdbStore>,
    )],
) {
    let mut status = vec![String::new(); clients.len()];
    let ready = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let mut all_ready = true;
            for (i, client) in clients.iter().enumerate() {
                assert!(
                    !nodes[i].1.is_finished(),
                    "node {i} exited before RPC readiness"
                );
                match client.rpc_call("peers", json!({})).await {
                    Ok(value) if value["count"].as_u64().is_some_and(|n| n > 0) => {
                        status[i] = "ready".into()
                    }
                    result => {
                        status[i] = format!("{result:?}");
                        all_ready = false;
                    }
                }
            }
            if all_ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(
        ready.is_ok(),
        "node RPC/peer readiness timed out: {status:?}"
    );
}

async fn change_rate(clients: &[NodeClient], keys: &burst_types::KeyPair, rate: u128) -> String {
    let address = burst_crypto::derive_address(&keys.public);
    let submitted = clients[0]
        .propose_parameter_signed(keys, GovernableParam::BrnRate, rate)
        .await
        .unwrap();
    let id = parse_hash(&submitted.hash).unwrap().into_tx_hash();
    let support = clients[0]
        .endorse_proposal_signed(keys, id, 5)
        .await
        .unwrap();
    for client in clients {
        client
            .wait_confirmed(&support.hash, Duration::from_secs(30))
            .await
            .unwrap();
    }
    wait_phase(clients, &id.to_string(), "Exploration").await;
    let vote =
        build_governance_vote_tx(&address, id, GovernanceVote::Yea, Timestamp::now()).unwrap();
    let voted = clients[0]
        .submit_signed(keys, &Transaction::GovernanceVote(vote), TxHash::ZERO)
        .await
        .unwrap();
    for client in clients {
        client
            .wait_confirmed(&voted.hash, Duration::from_secs(30))
            .await
            .unwrap();
    }
    wait_phase(clients, &id.to_string(), "Activated").await;
    for client in clients {
        let params = client.rpc_call("protocol_params", json!({})).await.unwrap();
        assert_eq!(params["brn_rate"], rate.to_string());
    }
    id.to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_governance_reaches_activation_and_replays_on_a_fresh_node() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".into()))
        .with_test_writer()
        .try_init();
    let dir = tempfile::tempdir().unwrap();
    let mut nodes = Vec::new();
    let mut clients = Vec::new();
    let mut configs = Vec::new();
    let first_port = available_port();
    for i in 0..3 {
        let mut config = NodeConfig {
            network: NetworkId::Dev,
            data_dir: dir.path().join(format!("node{i}")),
            port: if i == 0 { first_port } else { available_port() },
            rpc_port: available_port(),
            enable_upnp: false,
            enable_faucet: false,
            bootstrap_peers: if i == 0 {
                vec![]
            } else {
                vec![format!("127.0.0.1:{first_port}")]
            },
            ..NodeConfig::default()
        };
        config.params.governance_proposal_duration_secs = 15;
        config.params.governance_exploration_duration_secs = 30;
        config.params.governance_cooldown_duration_secs = 5;
        config.params.governance_propagation_buffer_secs = 2;
        config.params.governance_proposal_endorsements = 1;
        config.params.governance_proposal_cost = 5;
        config.params.min_work_difficulty = 0;
        let node = launch(config.clone()).await;
        clients.push(NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap());
        configs.push(config);
        nodes.push(node);
    }
    wait_ready(&clients, &nodes).await;
    let keys = burst_crypto::keypair_from_seed(&[0; 32]);
    let address = burst_crypto::derive_address(&keys.public);
    let head = clients[0].signing_state(&address).await.unwrap().head;
    let constitution = nodes[0].2.meta_store().get_meta("consti_engine").unwrap();
    nodes[0]
        .2
        .meta_store()
        .delete_meta("consti_engine")
        .unwrap();
    let error = clients[0]
        .propose_parameter_signed(&keys, GovernableParam::BrnRate, 999)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("missing persisted consti_engine"),
        "{error}"
    );
    assert_eq!(clients[0].signing_state(&address).await.unwrap().head, head);
    assert!(nodes[0].2.meta_store().get_meta("consti_engine").is_err());
    // Restore only the exact saved bytes; admission must not invent replacement state.
    nodes[0]
        .2
        .meta_store()
        .put_meta("consti_engine", &constitution)
        .unwrap();
    let id = change_rate(&clients, &keys, 123).await;
    eprintln!("all nodes activated the approved BRN rate");
    let expected: Value = clients[0]
        .rpc_call("governance_proposal_info", json!({"hash":id.to_string()}))
        .await
        .unwrap();
    for client in &clients {
        let params = client.rpc_call("protocol_params", json!({})).await.unwrap();
        assert_eq!(params["brn_rate"], "123");
        assert_eq!(client.signing_state(&address).await.unwrap().brn_spent, 5);
        let proposal = client
            .rpc_call("governance_proposal_info", json!({"hash":id.to_string()}))
            .await
            .unwrap();
        assert_eq!(proposal, expected);
    }
    let mut config = configs[0].clone();
    config.data_dir = dir.path().join("late");
    config.port = available_port();
    config.rpc_port = available_port();
    config.bootstrap_peers = vec![format!("127.0.0.1:{first_port}")];
    let late_client = NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap();
    let late = launch(config).await;
    wait_phase(
        std::slice::from_ref(&late_client),
        &id.to_string(),
        "Activated",
    )
    .await;
    assert_eq!(
        late_client
            .rpc_call("governance_proposal_info", json!({"hash":id.to_string()}))
            .await
            .unwrap(),
        expected
    );
    assert_eq!(
        late_client
            .rpc_call("protocol_params", json!({}))
            .await
            .unwrap()["brn_rate"],
        "123"
    );
    assert_eq!(
        late_client
            .signing_state(&address)
            .await
            .unwrap()
            .brn_spent,
        5
    );
    let mut all_clients = clients.clone();
    all_clients.push(late_client.clone());
    let zero = change_rate(&all_clients, &keys, 0).await;
    eprintln!("all nodes activated zero accrual; existing BRN can still fund governance");
    let restored = change_rate(&all_clients, &keys, 123).await;
    eprintln!("all nodes reversed zero accrual through a second paid proposal");
    for client in &all_clients {
        assert_eq!(
            client.signing_state(&address).await.unwrap().brn_spent,
            15
        );
        for proposal in [&zero, &restored] {
            let info = client
                .rpc_call("governance_proposal_info", json!({"hash":proposal}))
                .await
                .unwrap();
            assert_eq!(info["phase"], "Activated");
        }
    }
    late.0.shutdown();
    tokio::time::timeout(Duration::from_secs(20), late.1)
        .await
        .expect("late-node start waiter did not stop")
        .unwrap();
    for node in &nodes {
        node.0.shutdown();
    }
    for node in nodes {
        tokio::time::timeout(Duration::from_secs(20), node.1)
            .await
            .expect("node start waiter did not stop")
            .unwrap();
    }
    let recovered = launch(configs[0].clone()).await;
    eprintln!("restarting the activated node from its persisted state");
    wait_phase(
        std::slice::from_ref(&clients[0]),
        &id.to_string(),
        "Activated",
    )
    .await;
    assert_eq!(
        clients[0]
            .rpc_call("governance_proposal_info", json!({"hash":id.to_string()}))
            .await
            .unwrap(),
        expected
    );
    assert_eq!(
        clients[0]
            .rpc_call("protocol_params", json!({}))
            .await
            .unwrap()["brn_rate"],
        "123"
    );
    assert_eq!(
        clients[0]
            .signing_state(&address)
            .await
            .unwrap()
            .brn_spent,
        15
    );
    for proposal in [&zero, &restored] {
        let info = clients[0]
            .rpc_call("governance_proposal_info", json!({"hash":proposal}))
            .await
            .unwrap();
        assert_eq!(info["phase"], "Activated");
    }
    recovered.0.shutdown();
    tokio::time::timeout(Duration::from_secs(20), recovered.1)
        .await
        .expect("recovered-node start waiter did not stop")
        .unwrap();
}

#[path = "../support/ports.rs"]
mod ports;
use ports::available_port;
