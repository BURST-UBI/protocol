//! Bootstrap authority must close on every peer, including a fresh observer.
use burst_node::NodeConfig;
use burst_store::{account::AccountStore, BrnStore};
use burst_types::{NetworkId, Timestamp, TxHash, WalletAddress};
use burst_verification::bootstrap::BootstrapPhase;
use burst_wallet_core::transaction_builder::build_endorse_tx;
use serde_json::json;
use std::time::Duration;

#[path = "../support/running_node.rs"]
mod running_node;
use running_node::RunningNode;

impl RunningNode {
    fn phase(&self) -> BootstrapPhase {
        BootstrapPhase::decode(
            self.store
                .brn_store()
                .get_meta(BootstrapPhase::KEY)
                .unwrap()
                .as_deref(),
        )
        .unwrap()
    }
}

async fn ready(nodes: &[RunningNode]) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let mut all_ready = true;
            for node in nodes {
                assert!(!node.is_finished(), "node exited before readiness");
                all_ready &= node
                    .client
                    .rpc_call("peers", json!({}))
                    .await
                    .is_ok_and(|v| v["count"].as_u64().is_some_and(|n| n > 0));
            }
            if all_ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("nodes did not connect");
}

async fn assert_closed(node: &RunningNode, target: &WalletAddress, endorsement: &str) {
    node.client
        .wait_confirmed(endorsement, Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(node.phase(), BootstrapPhase::Complete);
    match node.store.account_store().get_account(target) {
        Ok(account) => assert!(!account.state.is_verified()),
        Err(burst_store::StoreError::NotFound(_)) => {}
        Err(error) => panic!("cannot read endorsed account: {error}"),
    }
    let status = node
        .client
        .rpc_call("verification_status", json!({"account":target.as_str()}))
        .await
        .unwrap();
    assert_eq!(status["endorser_count"], 1);
    assert_eq!(status["selection_available"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bootstrap_exit_survives_signed_endorsement_restart_and_fresh_sync() {
    let dir = tempfile::tempdir().unwrap();
    let genesis = burst_crypto::keypair_from_seed(&[0; 32]);
    let first_human =
        burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[1; 32]).public);
    let next_human =
        burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[2; 32]).public);
    let first_port = available_port();
    let mut configs = Vec::new();
    for i in 0..3 {
        let key_file = dir.path().join(format!("representative{i}.key"));
        if i < 2 {
            std::fs::write(&key_file, hex::encode([i as u8; 32])).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
            }
        }
        let mut config = NodeConfig {
            network: NetworkId::Dev,
            data_dir: dir.path().join(format!("node{i}")),
            port: if i == 0 { first_port } else { available_port() },
            rpc_port: available_port(),
            enable_upnp: false,
            enable_faucet: false,
            enable_representative: i < 2,
            representative_key_file: (i < 2).then_some(key_file),
            bootstrap_peers: if i == 0 {
                vec![]
            } else {
                vec![format!("127.0.0.1:{first_port}")]
            },
            ..NodeConfig::default()
        };
        config.params.bootstrap_exit_threshold = 2;
        config.params.min_work_difficulty = 0;
        configs.push(config);
    }
    let mut nodes = Vec::new();
    for config in &configs[..2] {
        nodes.push(RunningNode::launch_with_admin(config.clone()).await);
    }
    ready(&nodes).await;
    for node in &nodes {
        assert_eq!(node.phase(), BootstrapPhase::Active);
    }
    let admission = running_node::admin_rpc::endorse(&nodes[0].client, first_human.as_str())
        .await
        .unwrap();
    assert_eq!(admission["verified"], true);
    for node in &nodes {
        node.client
            .wait_confirmed(
                admission["block_hash"].as_str().unwrap(),
                Duration::from_secs(30),
            )
            .await
            .unwrap();
        assert_eq!(node.phase(), BootstrapPhase::Complete);
        assert!(node
            .store
            .account_store()
            .get_account(&first_human)
            .unwrap()
            .state
            .is_verified());
    }
    let denied = running_node::admin_rpc::endorse(&nodes[0].client, next_human.as_str())
        .await
        .unwrap_err();
    assert!(
        denied.to_string().contains("genesis bootstrap has ended"),
        "{denied}"
    );

    // RPC restrictions alone are insufficient: a holder can sign without that RPC.
    let endorsement = build_endorse_tx(
        &burst_crypto::derive_address(&genesis.public),
        &next_human,
        configs[0].params.endorsement_burn_amount,
        Timestamp::now(),
    )
    .unwrap();
    let submitted = nodes[0]
        .client
        .submit_signed(
            &genesis,
            &burst_transactions::Transaction::Endorse(endorsement),
            TxHash::ZERO,
        )
        .await
        .unwrap();
    for node in &nodes {
        assert_closed(node, &next_human, &submitted.hash).await;
    }
    for node in nodes.drain(..) {
        node.stop().await;
    }

    for config in &configs {
        nodes.push(RunningNode::launch_with_admin(config.clone()).await);
    }
    ready(&nodes).await;
    for node in &nodes {
        assert_closed(node, &next_human, &submitted.hash).await;
    }
    let denied = running_node::admin_rpc::endorse(&nodes[0].client, next_human.as_str())
        .await
        .unwrap_err();
    assert!(
        denied.to_string().contains("genesis bootstrap has ended"),
        "{denied}"
    );
    for node in nodes {
        node.stop().await;
    }
}

#[path = "../support/ports.rs"]
mod ports;
use ports::available_port;
