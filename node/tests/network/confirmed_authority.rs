//! Independent representatives cannot replace an offline human with a smaller quorum.
use burst_node::{BurstNode, NodeConfig};
#[path = "../support/admin_rpc.rs"]
mod admin_rpc;
use burst_types::{BlockHash, NetworkId, Timestamp, TxHash};
use burst_wallet_core::{
    transaction_builder::{build_burn_tx, build_state_block, sign_state_block},
    NodeClient,
};
use serde_json::json;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_representative_stalls_finality_until_its_real_vote_returns() {
    pending_chain_recovers(Recovery::ReturnSigner).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_reactivates_a_pending_chain_already_stored_by_every_peer() {
    pending_chain_recovers(Recovery::Restart).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_pending_elections_are_rediscovered_without_resubmitting_payments() {
    pending_chain_recovers(Recovery::ExpireElections).await;
}

enum Recovery {
    ReturnSigner,
    Restart,
    ExpireElections,
}

async fn pending_chain_recovers(recovery: Recovery) {
    let dir = tempfile::tempdir().unwrap();
    let keys: Vec<_> = (0..3)
        .map(|n| burst_crypto::keypair_from_seed(&[n; 32]))
        .collect();
    let addresses: Vec<_> = keys
        .iter()
        .map(|key| burst_crypto::derive_address(&key.public))
        .collect();
    let mut configs = Vec::new();
    let p2p: Vec<_> = (0..3).map(|_| port()).collect();
    for i in 0..3 {
        let key_file = dir.path().join(format!("rep{i}.key"));
        std::fs::write(&key_file, hex::encode([i as u8; 32])).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut config = NodeConfig {
            network: NetworkId::Dev,
            data_dir: dir.path().join(format!("node{i}")),
            port: p2p[i],
            rpc_port: port(),
            enable_upnp: false,
            representative_key_file: Some(key_file),
            bootstrap_peers: p2p
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, port)| format!("127.0.0.1:{port}"))
                .collect(),
            ..NodeConfig::default()
        };
        config.params.min_work_difficulty = 0;
        configs.push(config);
    }
    let mut clients = Vec::new();
    let mut nodes = Vec::new();
    let mut signers = Vec::new();
    let mut elections = Vec::new();
    for config in &configs {
        let mut node = BurstNode::new(config.clone()).await.unwrap();
        clients.push(NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap());
        signers.push(node.vote_generator.clone());
        elections.push(node.active_elections.clone());
        let shutdown = node.shutdown.clone();
        nodes.push((
            shutdown,
            tokio::spawn(async move {
                node.start_with_services(admin_rpc::start_services)
                    .await
                    .unwrap();
                node.stop().await.unwrap();
            }),
        ));
    }
    let result = tokio::time::timeout(Duration::from_secs(90), async {
        for client in &clients {
            loop {
                if client
                    .rpc_call("peers", json!({}))
                    .await
                    .is_ok_and(|v| v["count"].as_u64().unwrap_or(0) >= 2)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        for address in &addresses[1..] {
            let reply = admin_rpc::endorse(&clients[0], address.as_str())
                .await
                .unwrap();
            let hash = reply["block_hash"].as_str().unwrap();
            for client in &clients {
                client
                    .wait_confirmed(hash, Duration::from_secs(20))
                    .await
                    .unwrap();
            }
        }
        // Leave the transport connected. It may relay votes, but it cannot sign.
        signers[2].lock().await.set_representative(false);
        let state = clients[0].signing_state(&addresses[0]).await.unwrap();
        let tx = burst_transactions::Transaction::Burn(
            build_burn_tx(&addresses[0], &addresses[1], 1_000_000, Timestamp::now()).unwrap(),
        );
        let block = sign_state_block(
            build_state_block(&state, &tx, TxHash::ZERO, configs[0].params.params_hash()).unwrap(),
            &keys[0].private,
        );
        assert_eq!(block.work, 0);
        assert_ne!(block.previous, BlockHash::ZERO);
        let hash = block.hash.to_string();
        let result = clients[0]
            .process(&serde_json::to_string(&block).unwrap())
            .await
            .unwrap();
        assert!(result.accepted);
        let next_state = clients[0].signing_state(&addresses[0]).await.unwrap();
        assert_eq!(next_state.head, block.hash);
        let next_tx = burst_transactions::Transaction::Burn(
            build_burn_tx(&addresses[0], &addresses[2], 1_000_000, Timestamp::now()).unwrap(),
        );
        let next = sign_state_block(
            build_state_block(
                &next_state,
                &next_tx,
                TxHash::ZERO,
                configs[0].params.params_hash(),
            )
            .unwrap(),
            &keys[0].private,
        );
        let next_hash = next.hash.to_string();
        assert!(
            clients[0]
                .process(&serde_json::to_string(&next).unwrap())
                .await
                .unwrap()
                .accepted
        );
        tokio::time::sleep(Duration::from_secs(4)).await;
        for client in &clients {
            for pending in [&hash, &next_hash] {
                let block = client
                    .rpc_call("block_info", json!({"hash":pending}))
                    .await
                    .unwrap();
                assert_eq!(
                    block["confirmed"], false,
                    "two of three people must not finalize alone"
                );
            }
        }
        if matches!(recovery, Recovery::Restart) {
            for (shutdown, _) in &nodes {
                shutdown.shutdown();
            }
            for (_, task) in nodes.drain(..) {
                tokio::time::timeout(Duration::from_secs(20), task)
                    .await
                    .unwrap()
                    .unwrap();
            }
            signers.clear();
            for config in &configs {
                let mut node = BurstNode::new(config.clone()).await.unwrap();
                let shutdown = node.shutdown.clone();
                nodes.push((
                    shutdown,
                    tokio::spawn(async move {
                        node.start_with_services(admin_rpc::start_services)
                            .await
                            .unwrap();
                        node.stop().await.unwrap();
                    }),
                ));
            }
        } else {
            if matches!(recovery, Recovery::ExpireElections) {
                for active in &elections {
                    let mut active = active.write().await;
                    assert!(active.candidate_root(&block.hash).is_some());
                    active.cleanup_expired(1, Timestamp::new(Timestamp::now().as_secs() + 301));
                    assert!(active.candidate_root(&block.hash).is_none());
                    assert!(active.candidate_root(&next.hash).is_none());
                }
            }
            signers[2].lock().await.set_representative(true);
        }
        for client in &clients {
            client
                .wait_confirmed(&hash, Duration::from_secs(25))
                .await
                .unwrap();
            client
                .wait_confirmed(&next_hash, Duration::from_secs(25))
                .await
                .unwrap();
        }
    })
    .await;
    for (shutdown, _) in &nodes {
        shutdown.shutdown();
    }
    for (_, task) in nodes {
        tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .unwrap()
            .unwrap();
    }
    result.expect("independent-signer quorum recovery timed out");
}

#[path = "../support/ports.rs"]
mod ports;
use ports::available_port as port;
