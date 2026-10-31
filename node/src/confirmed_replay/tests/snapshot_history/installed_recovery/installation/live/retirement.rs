use super::*;

mod process;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_retires_full_record_inventory_without_snapshots_and_restarts_payments() {
    retired_history_catchup(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_cold_start_crosses_retired_history_without_snapshots() {
    retired_history_catchup(true).await;
}

async fn retired_history_catchup(from_genesis: bool) {
    process::initialize_diagnostics();
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let target_directory = tempfile::tempdir().unwrap();
    let mut old_epoch = BlockHash::ZERO;
    let mut epochs = std::collections::HashMap::new();
    let (fixture, source_directory, epoch) =
        crate::confirmed_replay::tests::snapshot_history::reconciliation::rollover::prepare_retired_rosters(
            |height, store, installed| {
                epochs.insert(installed, height);
                if height == 128 {
                    if !from_genesis {
                        store.env().copy_to_file(
                        target_directory.path().join("data.mdb"),
                        heed::CompactionOption::Disabled,
                    ).unwrap();
                    }
                    old_epoch = installed;
                }
            },
        ).await;
    assert!(!old_epoch.is_zero());
    let params = fixture.params.clone();
    let now = fixture.blocks.last().unwrap().timestamp.as_secs() + 10;
    let prefix = format!("checkpoint_chain/v1/dev/{}", fixture.blocks[0].hash);
    let archived = format!(
        "checkpoint_record_archive/v1/dev/{}",
        fixture.blocks[0].hash
    );
    let originals = {
        let source = LmdbStore::open(source_directory.path(), 32, 32 << 20).unwrap();
        [1, 2].map(|height| {
            source
                .meta_store()
                .get_meta(&format!("{archived}/{height}"))
                .unwrap()
        })
    };
    if !from_genesis {
        let target = LmdbStore::open(target_directory.path(), 32, 32 << 20).unwrap();
        assert_eq!(target.meta_store().payment_epoch().unwrap(), old_epoch);
        assert!(target.meta_store().get_meta(installed::BASE_KEY).is_err());
        for (index, height) in [1, 2].into_iter().enumerate() {
            assert_eq!(
                target
                    .meta_store()
                    .get_meta(&format!("{prefix}/{height}"))
                    .unwrap(),
                originals[index]
            );
        }
    }
    let reservations: Vec<_> = (0..4)
        .map(|_| std::net::TcpListener::bind("0.0.0.0:0").unwrap())
        .collect();
    let ports: Vec<_> = reservations
        .iter()
        .map(|listener| listener.local_addr().unwrap().port())
        .collect();
    let config = |index: usize, directory: &std::path::Path| crate::NodeConfig {
        network: NetworkId::Dev,
        data_dir: directory.to_path_buf(),
        params: params.clone(),
        port: ports[index * 2],
        rpc_port: ports[index * 2 + 1],
        enable_upnp: false,
        enable_peer_reachout: false,
        enable_representative: false,
        enable_faucet: false,
        nts_sources: vec![],
        drand_relays: vec!["http://127.0.0.1:1".into()],
        bootstrap_peers: if index == 0 {
            vec![]
        } else {
            vec![format!("127.0.0.1:{}", ports[0])]
        },
        ..crate::NodeConfig::default()
    };
    let source_config = config(0, source_directory.path());
    let target_config = config(1, target_directory.path());
    drop(reservations);
    let source = Running::start(source_config.clone(), now).await;
    source.ready(epoch).await;
    let (withheld, _agreement_receiver) = tokio::sync::mpsc::channel(32);
    source.registry.write().await.set_agreement_inbox(withheld);
    if from_genesis {
        process::cold_receiver(&target_config, epoch, now).await;
    }
    let target = Running::start(target_config.clone(), now).await;
    {
        let ready = target.ready(epoch);
        tokio::pin!(ready);
        let mut progress = tokio::time::interval(Duration::from_secs(10));
        loop {
            tokio::select! {
                _ = &mut ready => break,
                _ = progress.tick() => {
                    let accepted = CertifiedCheckpoint::stored_tip(&target.store, NetworkId::Dev, &params);
                    let installed = target.store.meta_store().payment_epoch().ok().and_then(|epoch| epochs.get(&epoch).copied());
                    eprintln!("retired-history catch-up: accepted={accepted:?}, installed={installed:?}");
                }
            }
        }
    }
    for node in [&source, &target] {
        assert_eq!(
            node.store
                .account_store()
                .get_account(&address(1))
                .unwrap()
                .trst_balance,
            4
        );
        assert!(node
            .store
            .meta_store()
            .get_meta(installed::BASE_KEY)
            .is_err());
        assert_eq!(
            CertifiedCheckpoint::stored_tip(&node.store, NetworkId::Dev, &params).unwrap(),
            130
        );
        assert_eq!(
            node.store
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(format!("{prefix}/").as_bytes())
                .unwrap()
                .len(),
            129
        );
        for (index, height) in [1, 2].into_iter().enumerate() {
            assert!(node
                .store
                .meta_store()
                .get_meta(&format!("{prefix}/{height}"))
                .is_err());
            assert_eq!(
                node.store
                    .meta_store()
                    .get_meta(&format!("{archived}/{height}"))
                    .unwrap(),
                originals[index]
            );
        }
    }
    {
        let mut expected =
            crate::payment_transition::restore(&source.store.tx_begin_write().unwrap(), &params)
                .unwrap();
        let mut actual =
            crate::payment_transition::restore(&target.store.tx_begin_write().unwrap(), &params)
                .unwrap();
        assert_eq!(actual.brn.wallets, expected.brn.wallets);
        assert_eq!(actual.brn.rate_history, expected.brn.rate_history);
        assert_eq!(actual.brn.next_stake_id(), expected.brn.next_stake_id());
        // Startup and checkpoint publication may flush expiry at different clocks.
        // Compare rights at one time, as recovery's economic afterimage check does.
        expected
            .trst
            .set_expiry_period(params.trst_expiry_secs, Timestamp::new(now)).unwrap();
        actual
            .trst
            .set_expiry_period(params.trst_expiry_secs, Timestamp::new(now)).unwrap();
        for seed in [0, 1] {
            assert_eq!(
                actual.trst.get_portfolio(&address(seed)),
                expected.trst.get_portfolio(&address(seed))
            );
        }
        assert!(actual.trst.wallet_snapshot_eq(&expected.trst));
        assert!(actual.trst.merger_graph == expected.trst.merger_graph);
    }
    let payment = burn(&target, &params, epoch, old_epoch, now).await;
    target.stop().await;
    source.stop().await;
    let source = Running::start(source_config, now + 1).await;
    source.ready(epoch).await;
    let target = Running::start(target_config, now + 1).await;
    target.ready(epoch).await;
    assert_eq!(
        target
            .store
            .account_store()
            .get_account(&address(1))
            .unwrap()
            .trst_balance,
        4
    );
    assert!(target.store.block_store().exists(&payment).unwrap());
    burn(&target, &params, epoch, old_epoch, now + 1).await;
    assert!(target
        .store
        .meta_store()
        .get_meta(installed::BASE_KEY)
        .is_err());
    target.stop().await;
    source.stop().await;
}
