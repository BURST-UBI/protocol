//! Exercise fork installation over authenticated TCP; this is not a BFT partition test.
use burst_consensus::vote_signing_payload;
use burst_node::{BurstNode, NodeConfig, WireMessage, WireVote};
use burst_types::{NetworkId, Signature, Timestamp, TxHash};
use burst_wallet_core::{
    transaction_builder::{
        build_change_rep_tx, build_merge_tx, build_state_block, sign_state_block,
    },
    NodeClient,
};
use serde_json::json;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_wire_vote_replaces_a_merge_without_losing_its_input_holdings() {
    let directory = tempfile::tempdir().unwrap();
    let p2p = [port(), port()];
    let keys = burst_crypto::keypair_from_seed(&[0; 32]);
    let address = burst_crypto::derive_address(&keys.public);
    let mut nodes = Vec::new();
    let mut clients = Vec::new();
    let mut registries = Vec::new();
    let mut elections = Vec::new();
    let mut signers = Vec::new();
    let mut stores = Vec::new();
    let mut params_hash = None;
    for index in 0..2 {
        let mut config = NodeConfig {
            network: NetworkId::Dev,
            data_dir: directory.path().join(format!("node{index}")),
            port: p2p[index],
            rpc_port: port(),
            enable_upnp: false,
            enable_representative: index == 0,
            bootstrap_peers: vec![format!("127.0.0.1:{}", p2p[1 - index])],
            ..NodeConfig::default()
        };
        config.params.min_work_difficulty = 0;
        params_hash = Some(config.params.params_hash());
        clients.push(NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap());
        let mut node = BurstNode::new(config).await.unwrap();
        stores.push(node.store.clone());
        registries.push(node.connection_registry.clone());
        elections.push(node.active_elections.clone());
        signers.push(node.vote_generator.clone());
        let shutdown = node.shutdown.clone();
        nodes.push((
            shutdown,
            tokio::spawn(async move {
                node.start_with_services(burst_daemon::start_public_services)
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
                    .is_ok_and(|v| v["count"].as_u64().unwrap_or(0) >= 1)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        let mut inputs = Vec::new();
        for amount in [20, 30] {
            let burn = clients[0]
                .pay_signed(&keys, &address, amount)
                .await
                .unwrap();
            for client in &clients {
                client
                    .wait_confirmed(&burn.hash, Duration::from_secs(10))
                    .await
                    .unwrap();
            }
            let received = clients[0].receive_signed(&keys, &burn.hash).await.unwrap();
            for client in &clients {
                client
                    .wait_confirmed(&received.hash, Duration::from_secs(10))
                    .await
                    .unwrap();
            }
            inputs.push(
                burst_wallet_core::signed_client::parse_hash(&received.hash)
                    .unwrap()
                    .into_tx_hash(),
            );
        }
        signers[0].lock().await.set_representative(false);
        let previous = clients[0].signing_state(&address).await.unwrap();
        let merge_tx = burst_transactions::Transaction::Merge(
            build_merge_tx(&address, inputs.clone(), Timestamp::now()).unwrap(),
        );
        let merge = sign_state_block(
            build_state_block(&previous, &merge_tx, TxHash::ZERO, params_hash.unwrap()).unwrap(),
            &keys.private,
        );
        let change_tx = burst_transactions::Transaction::ChangeRepresentative(
            build_change_rep_tx(&address, &address, Timestamp::now()).unwrap(),
        );
        let replacement = sign_state_block(
            build_state_block(&previous, &change_tx, TxHash::ZERO, params_hash.unwrap()).unwrap(),
            &keys.private,
        );
        assert_eq!(merge.previous, replacement.previous);
        assert_ne!(merge.hash, replacement.hash);
        assert!(
            clients[0]
                .process(&serde_json::to_string(&merge).unwrap())
                .await
                .unwrap()
                .accepted
        );
        for client in &clients {
            loop {
                if client.account_info(address.as_str()).await.unwrap().head
                    == merge.hash.to_string()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        let fork = clients[1]
            .process(&serde_json::to_string(&replacement).unwrap())
            .await
            .unwrap();
        assert!(!fork.accepted);
        assert!(fork
            .detail
            .unwrap_or_default()
            .to_lowercase()
            .contains("fork"));
        for active in &elections {
            loop {
                if active
                    .read()
                    .await
                    .candidate_root(&replacement.hash)
                    .is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        let mut vote = WireVote {
            voter: address.clone(),
            block_hashes: vec![replacement.hash],
            is_final: true,
            timestamp: Timestamp::now().as_secs() * 1000,
            sequence: 10_000,
            signature: Signature([0; 64]),
        };
        let forged = WireMessage::Vote(vote.clone()).encode().unwrap();
        for registry in &registries {
            let registry = registry.read().await;
            for peer in registry.peer_ids() {
                assert!(registry.send(peer, forged.clone()));
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        for client in &clients {
            assert_eq!(
                client.account_info(address.as_str()).await.unwrap().head,
                merge.hash.to_string()
            );
            assert_eq!(
                client
                    .rpc_call("block_info", json!({"hash":merge.hash.to_string()}))
                    .await
                    .unwrap()["confirmed"],
                false
            );
        }
        vote.signature = burst_crypto::sign_message(
            &vote_signing_payload(
                &vote.voter,
                true,
                vote.timestamp,
                vote.sequence,
                &vote.block_hashes,
            ),
            &keys.private,
        );
        let signed = WireMessage::Vote(vote).encode().unwrap();
        for registry in &registries {
            let registry = registry.read().await;
            for peer in registry.peer_ids() {
                assert!(registry.send(peer, signed.clone()));
            }
        }
        for client in &clients {
            client
                .wait_confirmed(&replacement.hash.to_string(), Duration::from_secs(15))
                .await
                .unwrap();
            let account = client.account_info(address.as_str()).await.unwrap();
            assert_eq!(account.head, replacement.hash.to_string());
            assert_eq!(account.trst_balance, "50");
            let portfolio = client
                .rpc_call("trst_portfolio", json!({"account":address.as_str()}))
                .await
                .unwrap();
            let batches = portfolio["batches"].as_array().unwrap();
            assert_eq!(batches.len(), 2);
            for id in &inputs {
                assert!(batches.iter().any(|batch| batch["id"] == id.to_string()));
            }
            assert!(
                !client
                    .process(&serde_json::to_string(&merge).unwrap())
                    .await
                    .unwrap()
                    .accepted
            );
            assert_eq!(
                client.account_info(address.as_str()).await.unwrap().head,
                replacement.hash.to_string()
            );
        }
        for store in &stores {
            loop {
                if store
                    .meta_store()
                    .fork_candidate(&replacement.hash)
                    .unwrap()
                    .is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        signers[0].lock().await.set_representative(true);
        let next = clients[0].pay_signed(&keys, &address, 1).await.unwrap();
        for client in &clients {
            client
                .wait_confirmed(&next.hash, Duration::from_secs(10))
                .await
                .unwrap();
        }
    })
    .await;
    for (shutdown, _) in &nodes {
        shutdown.shutdown();
    }
    for (_, task) in nodes {
        tokio::time::timeout(Duration::from_secs(15), task)
            .await
            .unwrap()
            .unwrap();
    }
    result.expect("wire merge-fork validation timed out");
}

#[path = "../support/ports.rs"]
mod ports;
use ports::available_port as port;
