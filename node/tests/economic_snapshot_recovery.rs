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
            let mut node = BurstNode::new(config.clone()).await.unwrap();
            let error = tokio::time::timeout(std::time::Duration::from_secs(10), node.start())
                .await
                .unwrap()
                .unwrap_err();
            assert!(
                error.to_string().contains(if missing {
                    "missing persisted"
                } else {
                    "invalid persisted"
                }),
                "{key}: {error}"
            );
            let batch = node.store.tx_begin_write().unwrap();
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
            let error = tokio::time::timeout(std::time::Duration::from_secs(10), node.start())
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
            let error = tokio::time::timeout(std::time::Duration::from_secs(10), node.start())
                .await
                .unwrap()
                .unwrap_err();
            assert!(
                error.to_string().contains("missing persisted"),
                "{key}: {error}"
            );
            assert!(error.to_string().contains("known-good backup"));
            assert!(matches!(
                node.store.meta_store().get_meta(key),
                Err(burst_store::StoreError::NotFound(_))
            ));
        }
    }
}
