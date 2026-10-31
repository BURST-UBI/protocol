//! Signed wallet context through real RPC, admission, durable voting and restart.
use burst_node::{BurstNode, NodeConfig};
use burst_store::{account::AccountStore, block::BlockStore, meta::MetaStore};
use burst_types::{BlockHash, NetworkId, Timestamp, TxHash};
use burst_wallet_core::{
    transaction_builder::{build_burn_tx, build_state_block, sign_state_block},
    NodeClient,
};
use serde_json::json;
use std::time::Duration;

#[path = "payment_epoch/offline_opening.rs"]
mod offline_opening;

fn config(path: &std::path::Path) -> NodeConfig {
    let mut config = NodeConfig {
        network: NetworkId::Dev,
        data_dir: path.to_path_buf(),
        port: port(),
        rpc_port: port(),
        enable_upnp: false,
        bootstrap_peers: vec![],
        ..NodeConfig::default()
    };
    config.params.min_work_difficulty = 0;
    config
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wallet_epoch_survives_restart_and_wrong_context_cannot_enter_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let keys = burst_crypto::keypair_from_seed(&[0; 32]);
    let account = burst_crypto::derive_address(&keys.public);
    let receiver = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[2; 32]).public);
    let client = NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap();
    let mut epoch = BlockHash::ZERO;
    let mut last_head = BlockHash::ZERO;
    for round in 0..2 {
        let mut node = BurstNode::new(config.clone()).await.unwrap();
        node.initialize_genesis().unwrap();
        let store = node.store.clone();
        let installed = store.meta_store().payment_epoch().unwrap();
        if round == 0 {
            epoch = installed;
        }
        assert_eq!(installed, epoch);
        let elections = node.active_elections.clone();
        let voter = node.vote_generator.clone();
        voter.lock().await.set_representative(false);
        let shutdown = node.shutdown.clone();
        let task = tokio::spawn(async move {
            node.start_with_services(burst_daemon::start_public_services)
                .await
                .unwrap();
            node.stop().await.unwrap();
        });
        let result = tokio::time::timeout(Duration::from_secs(40), async {
            loop {
                if client.rpc_call("params_hash", json!({})).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let state = client.signing_state(&account).await.unwrap();
            assert_eq!(state.payment_epoch, epoch);
            assert_eq!(state.head, if round == 0 { epoch } else { last_head });
            let tx = burst_transactions::Transaction::Burn(
                build_burn_tx(&account, &receiver, 10, Timestamp::now()).unwrap(),
            );
            let valid =
                build_state_block(&state, &tx, TxHash::ZERO, config.params.params_hash()).unwrap();
            let sequence_key = format!("consensus_sequence/v1/{account}");
            let sequence = store.meta_store().get_meta(&sequence_key).ok();
            for wrong_epoch in [BlockHash::ZERO, BlockHash::new([99; 32])] {
                let mut wrong = valid.clone();
                wrong.payment_epoch = wrong_epoch;
                wrong.hash = wrong.compute_hash();
                let wrong = sign_state_block(wrong, &keys.private);
                let response = client
                    .process(&serde_json::to_string(&wrong).unwrap())
                    .await
                    .unwrap();
                assert!(!response.accepted);
                assert!(
                    response
                        .detail
                        .as_deref()
                        .is_some_and(|detail| detail.contains("payment epoch")),
                    "{response:?}"
                );
                assert!(!store.block_store().exists(&wrong.hash).unwrap());
                assert!(store
                    .meta_store()
                    .fork_candidate(&wrong.hash)
                    .unwrap()
                    .is_none());
                assert!(elections.read().await.candidate_root(&wrong.hash).is_none());
                assert_eq!(
                    store.account_store().get_account(&account).unwrap().head,
                    state.head
                );
                assert_eq!(store.meta_store().get_meta(&sequence_key).ok(), sequence);
            }
            voter.lock().await.set_representative(true);
            let submitted = client
                .submit_signed(&keys, &tx, TxHash::ZERO)
                .await
                .unwrap();
            assert!(submitted.accepted);
            client
                .wait_confirmed(&submitted.hash, Duration::from_secs(20))
                .await
                .unwrap();
            let info = client
                .rpc_call("block_info", json!({"hash":submitted.hash}))
                .await
                .unwrap();
            assert_eq!(info["payment_epoch"], epoch.to_string());
            last_head = client.signing_state(&account).await.unwrap().head;
        })
        .await;
        shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .unwrap()
            .unwrap();
        result.expect("payment epoch RPC/restart test timed out");
    }
}

#[tokio::test]
async fn initialization_refuses_missing_or_corrupt_epoch_without_repairing_it() {
    let dir = tempfile::tempdir().unwrap();
    let node = BurstNode::new(config(dir.path())).await.unwrap();
    node.initialize_genesis().unwrap();
    let store = &node.store;
    let key = burst_store::meta::PAYMENT_EPOCH_KEY;
    let genesis = store.meta_store().payment_epoch().unwrap();
    let bytes = store.block_store().get_block(&genesis).unwrap();
    store.meta_store().delete_meta(key).unwrap();
    assert!(node.initialize_genesis().is_err());
    assert!(store.meta_store().get_meta(key).is_err());
    for invalid in [vec![], vec![0; 32], vec![1; 31], vec![1; 33], vec![99; 32]] {
        store.meta_store().put_meta(key, &invalid).unwrap();
        assert!(node.initialize_genesis().is_err());
        assert_eq!(store.meta_store().get_meta(key).unwrap(), invalid);
        assert_eq!(store.block_store().get_block(&genesis).unwrap(), bytes);
    }
    store
        .meta_store()
        .put_meta(key, genesis.as_bytes())
        .unwrap();
    node.initialize_genesis().unwrap();
    assert_eq!(store.meta_store().payment_epoch().unwrap(), genesis);
}

#[path = "../support/ports.rs"]
mod ports;
use ports::available_port as port;
