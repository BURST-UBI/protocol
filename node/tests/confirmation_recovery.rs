//! Recover a persisted consensus handoff without peers or fresh votes.
use burst_ledger::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
use burst_node::{BurstNode, NodeConfig};
use burst_store::account::{AccountInfo, AccountStore};
use burst_types::{BlockHash, NetworkId, Signature, Timestamp, TxHash, WalletState};
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pooled_rpc_client_cannot_submit_to_a_stopped_node_after_restart() {
    use burst_wallet_core::NodeClient;
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut config = NodeConfig {
        network: NetworkId::Dev,
        data_dir: directory.path().to_path_buf(),
        port: 0,
        rpc_port: port,
        enable_upnp: false,
        nts_sources: vec![],
        bootstrap_peers: vec![],
        ..NodeConfig::default()
    };
    config.params.min_work_difficulty = 0;
    let client = NodeClient::new(format!("http://127.0.0.1:{port}")).unwrap();
    for generation in 0..2 {
        let mut node = BurstNode::new(config.clone()).await.unwrap();
        let shutdown = node.shutdown.clone();
        let worker = tokio::spawn(async move {
            node.start().await.unwrap();
            node.stop().await.unwrap();
        });
        let session = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if client.rpc_call("node_info", json!({})).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            if generation == 1 {
                let creator = burst_crypto::keypair_from_seed(&[0; 32]);
                let recipient = burst_crypto::derive_address(
                    &burst_crypto::keypair_from_seed(&[88; 32]).public,
                );
                let payment = client.pay_signed(&creator, &recipient, 1).await.unwrap();
                assert!(payment.accepted);
                client
                    .wait_confirmed(&payment.hash, Duration::from_secs(10))
                    .await
                    .unwrap();
            }
        })
        .await;
        shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(20), worker)
            .await
            .unwrap()
            .unwrap();
        session.expect("pooled RPC session did not reconnect to the current node");
        assert!(
            client.rpc_call("node_info", json!({})).await.is_err(),
            "a stopped RPC session must not retain the retired node state"
        );
    }
}

#[tokio::test]
async fn startup_never_reopens_or_repairs_unproven_bootstrap_authority() {
    use burst_store::BrnStore;
    use burst_verification::bootstrap::BootstrapPhase;
    let dir = tempfile::tempdir().unwrap();
    let config = NodeConfig {
        network: NetworkId::Dev,
        data_dir: dir.path().to_path_buf(),
        port: 0,
        enable_rpc: false,
        enable_upnp: false,
        enable_representative: false,
        bootstrap_peers: vec![],
        ..NodeConfig::default()
    };
    let mut node = BurstNode::new(config).await.unwrap();
    node.initialize_genesis().unwrap();
    node.store
        .brn_store()
        .put_meta(BootstrapPhase::KEY, &BootstrapPhase::Complete.encoding())
        .unwrap();
    node.initialize_genesis().unwrap();
    assert_eq!(
        node.store
            .brn_store()
            .get_meta(BootstrapPhase::KEY)
            .unwrap()
            .unwrap(),
        BootstrapPhase::Complete.encoding()
    );
    node.store
        .brn_store()
        .put_meta(BootstrapPhase::KEY, &[])
        .unwrap();
    let error = node.start().await.unwrap_err();
    assert!(
        error.to_string().contains("invalid bootstrap phase"),
        "{error}"
    );
    assert_eq!(
        node.store
            .brn_store()
            .get_meta(BootstrapPhase::KEY)
            .unwrap(),
        Some(vec![])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_restores_origin_ownership_even_without_a_portfolio() {
    let dir = tempfile::tempdir().unwrap();
    let config = NodeConfig {
        network: NetworkId::Dev,
        data_dir: dir.path().to_path_buf(),
        port: 0,
        enable_rpc: false,
        enable_upnp: false,
        enable_representative: false,
        bootstrap_peers: vec![],
        ..NodeConfig::default()
    };
    let owner = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[0; 32]).public);
    let origin = TxHash::new([91; 32]);
    {
        let node = BurstNode::new(config.clone()).await.unwrap();
        node.initialize_genesis().unwrap();
        let mut brn = node.brn_engine.lock().await;
        brn.track_wallet(
            owner.clone(),
            burst_brn::BrnWalletState::new(Timestamp::new(0)),
        );
        let mut trst = node.trst_engine.lock().await;
        trst.mint(origin, owner.clone(), 100, owner.clone(), Timestamp::now())
            .unwrap();
        assert!(trst.wallets.is_empty());
        let mut batch = node.store.tx_begin_write().unwrap();
        burst_node::ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
        batch.commit().unwrap();
    }
    let mut node = BurstNode::new(config).await.unwrap();
    let engine = node.trst_engine.clone();
    let shutdown = node.shutdown.clone();
    let task = tokio::spawn(async move {
        node.start().await.unwrap();
        node.stop().await.unwrap();
    });
    let recovered = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let mut trst = engine.lock().await;
            if trst
                .wallet_origins
                .get(&owner)
                .is_some_and(|origins| origins.contains(&origin))
            {
                assert!(trst.wallets.is_empty());
                assert!(trst.revoke_by_origin(&owner).is_empty());
                assert!(trst.merger_graph.is_origin_revoked(&origin));
                break;
            }
            drop(trst);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();
    recovered.expect("startup lost an origin with no current holdings");
}

#[tokio::test]
async fn legacy_trst_snapshot_requires_replay_without_overwriting_its_data() {
    use burst_store::meta::MetaStore;
    let dir = tempfile::tempdir().unwrap();
    let config = NodeConfig {
        network: NetworkId::Dev,
        data_dir: dir.path().to_path_buf(),
        port: 0,
        enable_rpc: false,
        enable_upnp: false,
        enable_representative: false,
        bootstrap_peers: vec![],
        ..NodeConfig::default()
    };
    let mut node = BurstNode::new(config).await.unwrap();
    let legacy = bincode::serialize(&burst_trst::TrstEngine::new().wallets).unwrap();
    node.store
        .meta_store()
        .put_meta(burst_trst::TrstEngine::meta_key(), &legacy)
        .unwrap();
    let error = node.start().await.unwrap_err();
    assert!(error
        .to_string()
        .contains("explicit ledger replay required"));
    assert_eq!(
        node.store
            .meta_store()
            .get_meta(burst_trst::TrstEngine::meta_key())
            .unwrap(),
        legacy
    );
    node.stop().await.unwrap();
    assert_eq!(
        node.store
            .meta_store()
            .get_meta(burst_trst::TrstEngine::meta_key())
            .unwrap(),
        legacy,
        "cleanup after a failed startup must not overwrite the evidence needed for recovery"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_preserves_committed_payments_when_engine_caches_are_stale() {
    use burst_store::{meta::MetaStore, BrnStore};
    use burst_wallet_core::NodeClient;
    use serde_json::json;
    let directory = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut config = NodeConfig {
        network: NetworkId::Dev,
        data_dir: directory.path().to_path_buf(),
        port: 0,
        rpc_port: port,
        enable_upnp: false,
        nts_sources: vec![],
        bootstrap_peers: vec![],
        ..NodeConfig::default()
    };
    config.params.min_work_difficulty = 0;
    let mut node = BurstNode::new(config.clone()).await.unwrap();
    let store = node.store.clone();
    let brn = node.brn_engine.clone();
    let shutdown = node.shutdown.clone();
    let task = tokio::spawn(async move {
        node.start().await.unwrap();
        node
    });
    let client = NodeClient::new(format!("http://127.0.0.1:{port}")).unwrap();
    let keys = burst_crypto::keypair_from_seed(&[0; 32]);
    let creator = burst_crypto::derive_address(&keys.public);
    let recipient_keys = burst_crypto::keypair_from_seed(&[88; 32]);
    let recipient = burst_crypto::derive_address(&recipient_keys.public);
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        while client.rpc_call("node_info", json!({})).await.is_err() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let stale_brn = brn.lock().await.clone();
        let payment = client.pay_signed(&keys, &recipient, 7).await.unwrap();
        client
            .wait_confirmed(&payment.hash, Duration::from_secs(10))
            .await
            .unwrap();
        (stale_brn, payment.hash)
    })
    .await;
    shutdown.shutdown();
    let mut node = tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();
    let (stale_brn, payment_hash) = result.expect("payment fixture timed out");
    let wallet_before = store.brn_store().get_wallet_state(&creator).unwrap();
    let meta_keys = [
        burst_trst::TrstEngine::meta_key(),
        "merger_graph",
        "verification_orchestrator",
        burst_governance::DelegationEngine::meta_key(),
    ];
    let before: Vec<_> = meta_keys
        .iter()
        .map(|key| store.meta_store().get_meta(key).ok())
        .collect();
    // Simulate a commit whose cache publication did not complete. The database
    // contains an accepted, confirmed payment; stale caches are not authority.
    *node.brn_engine.lock().await = stale_brn;
    *node.trst_engine.lock().await = burst_trst::TrstEngine::new();
    *node.verification_orchestrator.lock().await =
        burst_verification::VerificationOrchestrator::new();
    node.stop().await.unwrap();
    assert_eq!(
        store.brn_store().get_wallet_state(&creator).unwrap(),
        wallet_before
    );
    for (key, expected) in meta_keys.iter().zip(before) {
        assert_eq!(store.meta_store().get_meta(key).ok(), expected, "{key}");
    }
    drop(node);
    let mut node = BurstNode::new(config).await.unwrap();
    let brn = node.brn_engine.clone();
    let shutdown = node.shutdown.clone();
    let task = tokio::spawn(async move {
        node.start().await.unwrap();
        node.stop().await.unwrap();
    });
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        while client.rpc_call("node_info", json!({})).await.is_err() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            brn.lock().await.get_wallet(&creator).unwrap().total_burned,
            7
        );
        let received = client
            .receive_signed(&recipient_keys, &payment_hash)
            .await
            .unwrap();
        client
            .wait_confirmed(&received.hash, Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(
            store
                .account_store()
                .get_account(&recipient)
                .unwrap()
                .trst_balance,
            7
        );
    })
    .await;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();
    result.expect("committed payment was not usable after stale-cache shutdown");
}

#[test]
fn malformed_or_incomplete_brn_metadata_cannot_reset_stakes_or_earnings() {
    use burst_store::BrnStore;
    for sequence in [vec![], vec![1], vec![0; 8], vec![1; 9]] {
        let dir = tempfile::tempdir().unwrap();
        let store = burst_store_lmdb::LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
        store
            .brn_store()
            .put_meta(b"next_stake_id", &sequence)
            .unwrap();
        assert!(burst_brn::BrnEngine::load_from_store(&store.brn_store()).is_err());
    }
    let dir = tempfile::tempdir().unwrap();
    let store = burst_store_lmdb::LmdbStore::open(dir.path(), 32, 16 * 1024 * 1024).unwrap();
    assert!(burst_brn::BrnEngine::load_from_store(&store.brn_store()).is_ok());
    store
        .brn_store()
        .put_meta(b"next_stake_id", &5u64.to_be_bytes())
        .unwrap();
    assert!(burst_brn::BrnEngine::load_from_store(&store.brn_store()).is_err());
    let mut rates = burst_brn::RateHistory::new(5, Timestamp::new(0));
    rates.apply_rate_change(9, Timestamp::new(10)).unwrap();
    store
        .brn_store()
        .put_meta(b"rate_history", &bincode::serialize(&rates).unwrap())
        .unwrap();
    assert!(burst_brn::BrnEngine::load_from_store(&store.brn_store()).is_ok());
    rates.segments[0].end = Some(Timestamp::new(11));
    store
        .brn_store()
        .put_meta(b"rate_history", &bincode::serialize(&rates).unwrap())
        .unwrap();
    assert!(burst_brn::BrnEngine::load_from_store(&store.brn_store()).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_resumes_cementation_only_after_startup_restoration() {
    recover_cementation(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recovered_cementation_retires_an_existing_unconfirmed_election() {
    recover_cementation(true).await;
}

async fn recover_cementation(existing_election: bool) {
    let dir = tempfile::tempdir().unwrap();
    let config = NodeConfig {
        network: NetworkId::Dev,
        data_dir: dir.path().to_path_buf(),
        port: 0,
        enable_rpc: false,
        enable_upnp: false,
        enable_representative: false,
        bootstrap_peers: vec![],
        ..NodeConfig::default()
    };
    let keys = burst_crypto::keypair_from_seed(&[0; 32]);
    let address = burst_crypto::derive_address(&keys.public);
    let mut genesis = StateBlock {
        signing_grant: burst_types::BlockHash::ZERO,
        delegation_payload: Vec::new(),
        version: CURRENT_BLOCK_VERSION,
        payment_epoch: BlockHash::ZERO,
        block_type: BlockType::Open,
        account: address.clone(),
        representative: address.clone(),
        previous: BlockHash::ZERO,
        brn_balance: 0,
        trst_balance: 0,
        link: BlockHash::ZERO,
        origin: TxHash::ZERO,
        transaction: TxHash::ZERO,
        timestamp: Timestamp::new(0),
        params_hash: config.params.params_hash(),
        target_head: burst_types::BlockHash::ZERO,
        merge_sources: vec![],
        release_sources: Vec::new(),
        work: 0,
        signature: Signature([0; 64]),
        hash: BlockHash::ZERO,
    };
    genesis.hash = genesis.compute_hash();
    genesis.signature = burst_crypto::sign_message(genesis.hash.as_bytes(), &keys.private);
    let mut next = genesis.clone();
    next.payment_epoch = genesis.hash;
    next.block_type = BlockType::ChangeRepresentative;
    next.previous = genesis.hash;
    next.timestamp = Timestamp::now();
    next.hash = next.compute_hash();
    next.signature = burst_crypto::sign_message(next.hash.as_bytes(), &keys.private);
    {
        let node = BurstNode::new(config.clone()).await.unwrap();
        node.initialize_genesis().unwrap();
        let mut brn = node.brn_engine.lock().await;
        brn.track_wallet(
            address.clone(),
            burst_brn::BrnWalletState::new(Timestamp::new(0)),
        );
        let trst = node.trst_engine.lock().await;
        let mut batch = node.store.tx_begin_write().unwrap();
        burst_node::ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
        batch.commit().unwrap();
        let mut batch = node.store.tx_begin_write().unwrap();
        batch
            .begin_block_undo(next.hash, address.clone(), 2)
            .unwrap();
        batch
            .put_block_with_account(&next.hash, &bincode::serialize(&next).unwrap(), &address, 2)
            .unwrap();
        batch.put_frontier(&address, &next.hash).unwrap();
        batch
            .put_account_info(
                &AccountInfo {
                    address: address.clone(),
                    state: WalletState::Verified,
                    verified_at: Some(Timestamp::new(0)),
                    head: next.hash,
                    block_count: 2,
                    confirmation_height: 1,
                    representative: address.clone(),
                    total_brn_burned: 0,
                    trst_balance: 0,
                    total_brn_staked: 0,
                    expired_trst: 0,
                    revoked_trst: 0,
                    epoch: 0,
                    verifier_opted_in_at: None,
                    orv_evicted: false,
                },
                false,
            )
            .unwrap();
        burst_node::ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
        batch.commit().unwrap();
        assert_eq!(
            node.rollback_block(&next).unwrap(),
            burst_node::block_processor::RollbackResult::RequiresLedgerRollback
        );
        assert_eq!(
            node.store
                .account_store()
                .get_account(&address)
                .unwrap()
                .head,
            next.hash
        );
        assert!(node
            .store
            .tx_begin_write()
            .unwrap()
            .get_block(&next.hash)
            .is_ok());
        node.store
            .meta_store()
            .enqueue_confirmation(&next.hash)
            .unwrap();
    }
    let mut node = BurstNode::new(config).await.unwrap();
    let store = node.store.clone();
    let shutdown = node.shutdown.clone();
    let elections = node.active_elections.clone();
    if existing_election {
        elections
            .write()
            .await
            .register_candidate(
                burst_node::ledger_updater::election_root(&next),
                next.hash,
                Timestamp::now(),
            )
            .unwrap();
        assert_eq!(elections.read().await.election_count(), 1);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        store
            .account_store()
            .get_account(&address)
            .unwrap()
            .confirmation_height,
        1
    );
    assert_eq!(
        store.meta_store().pending_confirmations().unwrap(),
        vec![next.hash]
    );
    let task = tokio::spawn(async move {
        node.start().await.unwrap();
        node.stop().await.unwrap();
    });
    let recovered = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if store
                .account_store()
                .get_account(&address)
                .unwrap()
                .confirmation_height
                == 2
                && store
                    .meta_store()
                    .pending_confirmations()
                    .unwrap()
                    .is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();
    recovered.expect("persisted handoff did not resume without new votes");
    assert_eq!(elections.read().await.election_count(), 0);
}
