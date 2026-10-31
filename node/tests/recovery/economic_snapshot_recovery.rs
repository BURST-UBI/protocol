//! Corrupt local recovery data must fail before admission workers start.
use burst_node::{BurstNode, NodeConfig};
use burst_store::meta::MetaStore;
use burst_store::{block::BlockStore, BrnStore};
use burst_trst::{MergeNode, MergeSource, MergerGraph, TrstEngine};
use burst_types::{NetworkId, Timestamp, TxHash};

fn hash(value: u8) -> TxHash {
    TxHash::new([value; 32])
}

#[tokio::test]
async fn startup_rejects_incomplete_or_noncanonical_lifecycle_without_repairing_it() {
    for (key, missing) in [
        ("consti_engine", true),
        ("delegation_engine_state", true),
        ("consti_engine", false),
        ("delegation_engine_state", false),
        ("governance_engine", false),
        ("verification_orchestrator", true),
        ("verification_orchestrator", false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let config = NodeConfig {
            network: NetworkId::Dev,
            data_dir: directory.path().to_path_buf(),
            port: 0,
            enable_rpc: false,
            enable_upnp: false,
            enable_representative: false,
            enable_faucet: false,
            nts_sources: vec![],
            bootstrap_peers: vec![],
            ..NodeConfig::default()
        };
        let table = if key == "governance_engine" {
            burst_store_lmdb::block_undo::UndoTable::BrnMeta
        } else {
            burst_store_lmdb::block_undo::UndoTable::Meta
        };
        let expected = {
            let node = BurstNode::new(config.clone()).await.unwrap();
            node.initialize_genesis().unwrap();
            let mut batch = node.store.tx_begin_write().unwrap();
            if missing {
                batch.delete_meta(key).unwrap();
            } else {
                let mut bytes = batch.undo_value(table, key.as_bytes()).unwrap().unwrap();
                bytes.push(0);
                if key == "governance_engine" {
                    batch.put_brn_meta(key.as_bytes(), &bytes).unwrap();
                } else {
                    batch.put_meta(key, &bytes).unwrap();
                }
            }
            let expected = batch.undo_value(table, key.as_bytes()).unwrap();
            batch.commit().unwrap();
            expected
        };
        for _ in 0..2 {
            let error = match BurstNode::new(config.clone()).await {
                Ok(mut node) => {
                    tokio::time::timeout(std::time::Duration::from_secs(10), node.start_with_services(burst_daemon::start_public_services))
                        .await
                        .unwrap()
                        .unwrap_err()
                }
                Err(error) => error,
            };
            assert!(
                error.to_string().contains(if missing {
                    "missing persisted"
                } else if key == "governance_engine" {
                    "persisted governance engine"
                } else {
                    "invalid persisted"
                }),
                "{key}: {error}"
            );
            let store = burst_store_lmdb::LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
            let batch = store.tx_begin_write().unwrap();
            assert_eq!(batch.undo_value(table, key.as_bytes()).unwrap(), expected);
        }
    }
}

#[tokio::test]
async fn genesis_preserves_orphaned_lifecycle_and_initializes_complete_snapshots() {
    for key in [
        "governance_engine",
        "consti_engine",
        "delegation_engine_state",
        "verification_orchestrator",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let config = NodeConfig {
            network: NetworkId::Dev,
            data_dir: directory.path().to_path_buf(),
            enable_rpc: false,
            enable_upnp: false,
            enable_representative: false,
            enable_faucet: false,
            ..NodeConfig::default()
        };
        let node = BurstNode::new(config.clone()).await.unwrap();
        let mut batch = node.store.tx_begin_write().unwrap();
        if key == "governance_engine" {
            batch
                .put_brn_meta(key.as_bytes(), b"retained evidence")
                .unwrap();
        } else {
            batch.put_meta(key, b"retained evidence").unwrap();
        }
        batch.commit().unwrap();
        assert!(node
            .initialize_genesis()
            .unwrap_err()
            .to_string()
            .contains("explicit ledger replay required"));
        assert_eq!(node.store.block_store().block_count().unwrap(), 0);
        let batch = node.store.tx_begin_write().unwrap();
        let table = if key == "governance_engine" {
            burst_store_lmdb::block_undo::UndoTable::BrnMeta
        } else {
            burst_store_lmdb::block_undo::UndoTable::Meta
        };
        assert_eq!(
            batch.undo_value(table, key.as_bytes()).unwrap().unwrap(),
            b"retained evidence"
        );
        drop(batch);
        let node = BurstNode::new(NodeConfig {
            data_dir: directory.path().join("fresh"),
            ..config
        })
        .await
        .unwrap();
        node.initialize_genesis().unwrap();
        let governance = node
            .store
            .brn_store()
            .get_meta(b"governance_engine")
            .unwrap()
            .unwrap();
        let constitution = node.store.meta_store().get_meta("consti_engine").unwrap();
        let delegation = node
            .store
            .meta_store()
            .get_meta(burst_governance::DelegationEngine::meta_key())
            .unwrap();
        bincode::deserialize::<burst_governance::GovernanceEngine>(&governance).unwrap();
        bincode::deserialize::<burst_consti::ConstiEngine>(&constitution).unwrap();
        burst_governance::DelegationEngine::load_state(&delegation).unwrap();
        let verification = node
            .store
            .meta_store()
            .get_meta("verification_orchestrator")
            .unwrap();
        let restored = burst_verification::VerificationOrchestrator::restore(
            burst_verification::OrchestratorSnapshot::decode(&verification).unwrap(),
        );
        assert!(restored
            .get_state(&burst_node::genesis_key::genesis_address(NetworkId::Dev))
            .is_some());
        node.initialize_genesis().unwrap();
        assert_eq!(
            node.store
                .meta_store()
                .get_meta("verification_orchestrator")
                .unwrap(),
            verification
        );
        assert_eq!(node.store.block_store().block_count().unwrap(), 1);
        assert_eq!(
            node.store
                .brn_store()
                .get_meta(b"governance_engine")
                .unwrap()
                .unwrap(),
            governance
        );
        assert_eq!(
            node.store.meta_store().get_meta("consti_engine").unwrap(),
            constitution
        );
        assert_eq!(
            node.store
                .meta_store()
                .get_meta(burst_governance::DelegationEngine::meta_key())
                .unwrap(),
            delegation
        );
    }
}

fn broken_snapshot(mutation: u8) -> (&'static str, Vec<u8>, &'static str) {
    if mutation < 2 {
        let mut graph = MergerGraph::new();
        graph.record_merge(MergeNode {
            merge_tx: hash(10),
            source_origins: vec![MergeSource {
                origin: hash(1),
                amount: 100,
            }],
            total_amount: if mutation == 0 { 101 } else { 100 },
            revoked_contribs: Default::default(),
        });
        if mutation == 1 {
            graph.record_downstream(hash(10), hash(99));
        }
        (
            "merger_graph",
            graph.to_bytes(),
            if mutation == 0 {
                "source total mismatch"
            } else {
                "unexpected merger graph reverse edge"
            },
        )
    } else {
        let owner = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[0; 32]).public);
        let mut engine = TrstEngine::new();
        let token = engine
            .mint(
                hash(1),
                owner.clone(),
                100,
                owner.clone(),
                Timestamp::new(100),
            )
            .unwrap();
        engine.track_token(token);
        let portfolio = engine.get_portfolio_mut(&owner).unwrap();
        if mutation == 2 {
            portfolio.cached_transferable = 101;
        } else if mutation == 3 {
            portfolio.tokens.push(portfolio.tokens[0].clone());
            portfolio.cached_transferable = 200;
        } else if mutation == 4 {
            portfolio.earliest_expiry = Some(Timestamp::new(999));
        } else {
            engine.wallet_origins.clear();
        }
        (
            TrstEngine::meta_key(),
            engine.save_wallets(),
            if mutation == 2 {
                "balance cache mismatch"
            } else if mutation == 3 {
                "duplicate TRST holding"
            } else if mutation == 4 {
                "expiry cache differs from governed period"
            } else {
                "TRST holding differs from its burn ownership"
            },
        )
    }
}

#[tokio::test]
async fn corrupt_economic_snapshots_fail_startup_and_remain_available_for_diagnosis() {
    for mutation in 0..6 {
        let directory = tempfile::tempdir().unwrap();
        let config = NodeConfig {
            network: NetworkId::Dev,
            data_dir: directory.path().to_path_buf(),
            port: 0,
            enable_rpc: false,
            enable_upnp: false,
            enable_representative: false,
            enable_faucet: false,
            nts_sources: vec![],
            bootstrap_peers: vec![],
            ..NodeConfig::default()
        };
        let (key, bytes, expected) = broken_snapshot(mutation);
        {
            let node = BurstNode::new(config.clone()).await.unwrap();
            node.initialize_genesis().unwrap();
            node.store.meta_store().put_meta(key, &bytes).unwrap();
        }
        for _ in 0..2 {
            let mut node = BurstNode::new(config.clone()).await.unwrap();
            let error = tokio::time::timeout(std::time::Duration::from_secs(10), node.start_with_services(burst_daemon::start_public_services))
                .await
                .expect("corrupt snapshot startup must terminate")
                .unwrap_err();
            assert!(
                error.to_string().contains(expected),
                "mutation {mutation}: {error}"
            );
            assert_eq!(node.store.meta_store().get_meta(key).unwrap(), bytes);
        }
    }
}

#[tokio::test]
async fn initialized_ledgers_never_replace_missing_economic_snapshots_with_empty_state() {
    for key in [TrstEngine::meta_key(), "merger_graph"] {
        let directory = tempfile::tempdir().unwrap();
        let config = NodeConfig {
            network: NetworkId::Dev,
            data_dir: directory.path().to_path_buf(),
            port: 0,
            enable_rpc: false,
            enable_upnp: false,
            enable_representative: false,
            enable_faucet: false,
            nts_sources: vec![],
            bootstrap_peers: vec![],
            ..NodeConfig::default()
        };
        {
            let node = BurstNode::new(config.clone()).await.unwrap();
            node.initialize_genesis().unwrap();
            assert!(node
                .store
                .meta_store()
                .get_meta(TrstEngine::meta_key())
                .is_ok());
            assert!(node.store.meta_store().get_meta("merger_graph").is_ok());
            node.store.meta_store().delete_meta(key).unwrap();
        }
        for _ in 0..2 {
            let mut node = BurstNode::new(config.clone()).await.unwrap();
            let error = tokio::time::timeout(std::time::Duration::from_secs(10), node.start_with_services(burst_daemon::start_public_services))
                .await
                .unwrap()
                .unwrap_err();
            assert!(
                error.to_string().contains("missing persisted"),
                "{key}: {error}"
            );
            assert!(
                error.to_string().contains("known-good backup"),
                "{key}: {error}"
            );
            assert!(matches!(
                node.store.meta_store().get_meta(key),
                Err(burst_store::StoreError::NotFound(_))
            ));
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn populated_legacy_governance_migrates_on_start_and_survives_second_restart() {
    use burst_store_lmdb::block_undo::UndoTable;
    use burst_wallet_core::NodeClient;
    use serde_json::json;
    use std::time::Duration;

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
        enable_faucet: false,
        nts_sources: vec![],
        bootstrap_peers: vec![],
        ..NodeConfig::default()
    };
    config.params.min_work_difficulty = 0;
    config.params.governance_proposal_cost = 0;
    let keys = burst_crypto::keypair_from_seed(&[0; 32]);
    let mut proposal = None;
    let mut expected = None;
    for generation in 0..3 {
        let mut node = BurstNode::new(config.clone()).await.unwrap();
        let shutdown = node.shutdown.clone();
        let store = node.store.clone();
        let worker = tokio::spawn(async move {
            node.start_with_services(burst_daemon::start_public_services).await.unwrap();
            node.stop().await.unwrap();
        });
        let client = NodeClient::new(format!("http://127.0.0.1:{port}")).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                assert!(!worker.is_finished(), "startup exited before RPC readiness");
                if client.rpc_call("node_info", json!({})).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            if generation == 0 {
                let submitted = client
                    .propose_parameter_signed(&keys, burst_governance::GovernableParam::BrnRate, 2)
                    .await
                    .unwrap();
                assert!(submitted.accepted);
                client
                    .wait_confirmed(&submitted.hash, Duration::from_secs(10))
                    .await
                    .unwrap();
                proposal =
                    Some(burst_wallet_core::signed_client::parse_hash(&submitted.hash).unwrap());
            }
            let batch = store.tx_begin_write().unwrap();
            assert!(batch.uses_governance_records().unwrap());
            assert_eq!(
                batch.get_meta("schema_version").unwrap(),
                Some(burst_store_lmdb::CURRENT_SCHEMA_VERSION.to_le_bytes().to_vec())
            );
            assert!(batch
                .undo_value_ref(UndoTable::BrnMeta, b"governance_engine")
                .unwrap()
                .is_none());
            let actual = batch.read_governance_records().unwrap();
            assert_eq!(actual.all_proposals().count(), 1);
            assert!(actual
                .get_proposal(&proposal.unwrap().into_tx_hash())
                .is_some());
            if let Some(ref expected) = expected {
                assert_eq!(&actual, expected);
            }
            expected = Some(actual);
        })
        .await;
        shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(20), worker)
            .await
            .unwrap()
            .unwrap();
        result.expect("migration/restart session timed out");
        if generation == 0 {
            // Materialize the supported legacy layout from a real confirmed signed
            // history. No provisional keyed journal may be stranded by this fixture.
            let mut batch = store.tx_begin_write().unwrap();
            assert!(batch
                .checkpoint_metadata(b"block_undo/")
                .unwrap()
                .is_empty());
            assert_eq!(store.block_store().block_count().unwrap(), 2);
            for prefix in ["governance_state/v1/", "governance_revision/v1/"] {
                for (key, _) in batch.checkpoint_metadata(prefix.as_bytes()).unwrap() {
                    batch
                        .delete_meta(std::str::from_utf8(&key).unwrap())
                        .unwrap();
                }
            }
            batch.delete_meta("governance_records_format").unwrap();
            batch
                .put_brn_meta(
                    b"governance_engine",
                    &bincode::serialize(expected.as_ref().unwrap()).unwrap(),
                )
                .unwrap();
            batch
                .put_meta("schema_version", &3u32.to_le_bytes())
                .unwrap();
            batch.commit().unwrap();
            store.force_sync().unwrap();
        }
    }
}
