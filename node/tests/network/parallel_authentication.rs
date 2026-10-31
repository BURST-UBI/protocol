//! Real RPC admission and restart through the parallel authentication pipeline.
use burst_crypto::{derive_address, keypair_from_seed, sign_message};
use burst_ledger::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
use burst_node::{BurstNode, NodeConfig};
use burst_store::{account::AccountStore, block::BlockStore, meta::MetaStore};
use burst_types::{BlockHash, NetworkId, Signature, Timestamp, TxHash};
use burst_wallet_core::NodeClient;
use serde_json::json;
use std::time::Duration;



async fn submit(
    client: &NodeClient,
    block: &StateBlock,
) -> burst_wallet_core::wallet::ProcessResponse {
    let encoded = serde_json::to_string(block).unwrap();
    for _ in 0..100 {
        match client.process(&encoded).await {
            Ok(result) => return result,
            Err(error) if error.to_string().contains("block queue full") => {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(error) => panic!("RPC submission failed: {error}"),
        }
    }
    panic!("block queue remained full");
}

fn signed_open(seed: u8, epoch: BlockHash, params: BlockHash) -> StateBlock {
    let keys = keypair_from_seed(&[seed; 32]);
    let address = derive_address(&keys.public);
    let mut block = StateBlock {
        signing_grant: burst_types::BlockHash::ZERO,
        version: CURRENT_BLOCK_VERSION,
        payment_epoch: epoch,
        block_type: BlockType::Open,
        account: address.clone(),
        representative: address,
        previous: BlockHash::ZERO,
        brn_spent: 0,
        trst_balance: 0,
        link: BlockHash::ZERO,
        origin: TxHash::ZERO,
        transaction: TxHash::ZERO,
        target_head: BlockHash::ZERO,
        merge_sources: vec![],
        release_sources: vec![],
        delegation_payload: vec![],
        timestamp: Timestamp::now(),
        params_hash: params,
        work: 0,
        signature: Signature([0; 64]),
        hash: BlockHash::ZERO,
    };
    block.hash = block.compute_hash();
    block.signature = sign_message(block.hash.as_bytes(), &keys.private);
    block
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_accounts_recover_gaps_reject_invalid_blocks_and_persist() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = NodeConfig {
        network: NetworkId::Dev,
        data_dir: directory.path().to_path_buf(),
        port: port(),
        rpc_port: port(),
        enable_upnp: false,
        enable_representative: false,
        bootstrap_peers: vec![],
        ..NodeConfig::default()
    };
    config.params.min_work_difficulty = 0;
    let params = config.params.params_hash();
    let mut node = BurstNode::new(config.clone()).await.unwrap();
    node.initialize_genesis().unwrap();
    let store = node.store.clone();
    let epoch = store.meta_store().payment_epoch().unwrap();
    let shutdown = node.shutdown.clone();
    let task = tokio::spawn(async move {
        node.start_with_services(burst_daemon::start_public_services).await.unwrap();
        node.stop().await.unwrap();
    });
    let url = format!("http://127.0.0.1:{}", config.rpc_port);
    let client = NodeClient::new(url.clone()).unwrap();
    let mut expected = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(40), async {
        while client.rpc_call("params_hash", json!({})).await.is_err() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let mut children = tokio::task::JoinSet::new();
        let mut parents = Vec::new();
        for seed in 1..=24 {
            let open = signed_open(seed, epoch, params);
            let mut child = open.clone();
            child.block_type = BlockType::ChangeRepresentative;
            child.previous = open.hash;
            child.hash = child.compute_hash();
            child.signature = sign_message(
                child.hash.as_bytes(),
                &keypair_from_seed(&[seed; 32]).private,
            );
            expected.push(child.clone());
            parents.push(open);
            let client = NodeClient::new(url.clone()).unwrap();
            children.spawn(async move {
                let result = submit(&client, &child).await;
                assert!(
                    result.accepted,
                    "RPC should acknowledge queue admission: {result:?}"
                );
            });
        }
        while let Some(result) = children.join_next().await {
            result.unwrap();
        }
        for child in &expected {
            assert!(
                !store.block_store().exists(&child.hash).unwrap(),
                "missing predecessor entered ledger"
            );
        }
        let mut submissions = tokio::task::JoinSet::new();
        for parent in parents {
            let client = NodeClient::new(url.clone()).unwrap();
            submissions.spawn(async move {
                let result = submit(&client, &parent).await;
                assert!(result.accepted, "{result:?}");
            });
        }
        while let Some(result) = submissions.join_next().await {
            result.unwrap();
        }
        for child in &expected {
            loop {
                if store
                    .account_store()
                    .get_account(&child.account)
                    .is_ok_and(|info| info.head == child.hash)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(store.block_store().exists(&child.hash).unwrap());
        }
        for self_reported_balance in [false, true] {
            let mut invalid = signed_open(100 + u8::from(self_reported_balance), epoch, params);
            if self_reported_balance {
                invalid.trst_balance = 1;
                invalid.hash = invalid.compute_hash();
                invalid.signature = sign_message(
                    invalid.hash.as_bytes(),
                    &keypair_from_seed(&[101; 32]).private,
                );
            } else {
                invalid.signature = Signature([7; 64]);
            }
            let result = client
                .process(&serde_json::to_string(&invalid).unwrap())
                .await
                .unwrap();
            assert!(!result.accepted, "{result:?}");
            assert!(!store.block_store().exists(&invalid.hash).unwrap());
            assert!(store.account_store().get_account(&invalid.account).is_err());
        }
    })
    .await;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .unwrap()
        .unwrap();
    result.expect("parallel authentication RPC test timed out");
    drop(store);
    let reopened = BurstNode::new(config).await.unwrap();
    reopened.initialize_genesis().unwrap();
    for child in expected {
        let account = reopened
            .store
            .account_store()
            .get_account(&child.account)
            .unwrap();
        assert_eq!(account.head, child.hash);
        assert_eq!(account.block_count, 2);
        assert_eq!(account.trst_balance, 0);
    }
}

#[path = "../support/ports.rs"]
mod ports;
use ports::available_port as port;
