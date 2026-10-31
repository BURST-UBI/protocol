//! Startup must retain private outcome causes, not just visible holdings.
use burst_node::{BurstNode, NodeConfig};
use burst_trst::{engine::WalletRecord, merger_graph::GraphRecord, TrstEngine};
use burst_types::{BlockHash, NetworkId, Timestamp, TxHash, WalletAddress};
use std::time::Duration;

fn stored_engine(node: &BurstNode) -> TrstEngine {
    let batch = node.store.tx_begin_write().unwrap();
    let mut engine = TrstEngine::from_wallet_records(
        batch
            .checkpoint_metadata_entries(WalletRecord::STORAGE_PREFIX.as_bytes())
            .unwrap()
            .map(|row| {
                let (key, value) = row.map_err(|error| error.to_string())?;
                WalletRecord::from_storage_entry(std::str::from_utf8(key).unwrap(), value)
            }),
        node.config.params.trst_expiry_secs,
    )
    .unwrap();
    engine.merger_graph = burst_trst::MergerGraph::from_records(
        batch
            .checkpoint_metadata_entries(GraphRecord::STORAGE_PREFIX.as_bytes())
            .unwrap()
            .map(|row| {
                let (key, value) = row.map_err(|error| error.to_string())?;
                GraphRecord::from_storage_entry(std::str::from_utf8(key).unwrap(), value)
            }),
    )
    .unwrap();
    engine
}

async fn restart(config: &NodeConfig, owner: &WalletAddress, origin: TxHash) -> TrstEngine {
    let mut node = BurstNode::new(config.clone()).await.unwrap();
    let engine = node.trst_engine.clone();
    let shutdown = node.shutdown.clone();
    let task = tokio::spawn(async move {
        node.start_with_services(burst_daemon::start_public_services).await.unwrap();
        node.stop().await.unwrap();
    });
    let snapshot = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let trst = engine.lock().await;
            if trst
                .wallet_origins
                .get(owner)
                .is_some_and(|origins| origins.contains(&origin))
            {
                break trst.clone();
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
    snapshot.expect("node did not restore the seeded economic snapshot")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_startup_preserves_independent_outcome_and_identity_revocations() {
    for identity_revoked in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let config = NodeConfig {
            network: NetworkId::Dev,
            data_dir: directory.path().to_path_buf(),
            port: 0,
            enable_rpc: false,
            enable_upnp: false,
            enable_representative: false,
            bootstrap_peers: vec![],
            nts_sources: vec![],
            ..NodeConfig::default()
        };
        let owner = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[0; 32]).public);
        let origin = TxHash::new([91; 32]);
        let origins = [(owner.clone(), origin)];
        let first = BlockHash::new([92; 32]);
        let second = BlockHash::new([93; 32]);
        let now = Timestamp::now();
        let (wallets, graph) = {
            let node = BurstNode::new(config.clone()).await.unwrap();
            node.initialize_genesis().unwrap();
            let mut brn = node.brn_engine.lock().await;
            brn.track_wallet(
                owner.clone(),
                burst_brn::BrnWalletState::new(Timestamp::EPOCH),
            );
            let mut trst = node.trst_engine.lock().await;
            let token = trst
                .mint(origin, owner.clone(), 100, owner.clone(), now)
                .unwrap();
            trst.track_token(token);
            if identity_revoked {
                trst.revoke_by_originator(&owner);
            }
            trst.invalidate_outcome_origins(first, &origins).unwrap();
            trst.invalidate_outcome_origins(second, &origins).unwrap();
            let mut batch = node.store.tx_begin_write().unwrap();
            burst_node::ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
            batch.commit().unwrap();
            (trst.save_wallets(), trst.merger_graph.to_bytes())
        };
        // This fixture seeds local snapshots to isolate startup behavior. It
        // does not authorize appeals or replace the certified-network tests.
        for _ in 0..2 {
            let mut restored = restart(&config, &owner, origin).await;
            assert_eq!(
                restored.save_wallets(),
                wallets,
                "startup lost outcome causes"
            );
            assert_eq!(restored.merger_graph.to_bytes(), graph);
            let mut reverified = restored.clone();
            assert!(reverified.restore_by_originator(&owner, now).unwrap().is_empty());
            assert!(reverified.merger_graph.is_origin_revoked(&origin));
            assert!(restored
                .restore_outcome_origins(first, &origins, now)
                .unwrap()
                .is_empty());
            assert!(restored.merger_graph.is_origin_revoked(&origin));
            let released = restored
                .restore_outcome_origins(second, &origins, now)
                .unwrap();
            assert_eq!(
                released.iter().map(|event| event.amount).sum::<u128>(),
                if identity_revoked { 0 } else { 100 }
            );
            assert_eq!(
                restored.merger_graph.is_origin_revoked(&origin),
                identity_revoked
            );
            if identity_revoked {
                assert_eq!(
                    restored
                        .restore_by_originator(&owner, now).unwrap()
                        .iter()
                        .map(|event| event.amount)
                        .sum::<u128>(),
                    100
                );
            }
            assert_eq!(restored.transferable_balance(&owner, now), Some(100));
            assert!(restored
                .restore_outcome_origins(second, &origins, now)
                .unwrap()
                .is_empty());
        }
        let mut node = BurstNode::new(config).await.unwrap();
        assert_eq!(
            stored_engine(&node).save_wallets(),
            wallets,
            "shutdown lost outcome causes"
        );
        let mut inconsistent_graph = burst_trst::MergerGraph::from_bytes(&graph).unwrap();
        inconsistent_graph.apply_restoration(&origin);
        let inconsistent_bytes = inconsistent_graph.to_bytes();
        let mut inconsistent = stored_engine(&node);
        inconsistent.merger_graph = inconsistent_graph;
        let brn = burst_brn::BrnEngine::load_from_store(&node.store.brn_store()).unwrap();
        let mut batch = node.store.tx_begin_write().unwrap();
        burst_node::ledger_updater::persist_economics(&mut batch, &brn, &inconsistent).unwrap();
        batch.commit().unwrap();
        let error = tokio::time::timeout(Duration::from_secs(5), node.start_with_services(burst_daemon::start_public_services))
            .await
            .expect("inconsistent snapshot should fail before services start")
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("outcome-invalidated origin is active"),
            "{error}"
        );
        assert_eq!(
            stored_engine(&node).merger_graph.to_bytes(),
            inconsistent_bytes
        );
        assert_eq!(stored_engine(&node).save_wallets(), wallets);
    }
}
