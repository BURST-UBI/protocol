use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_replacement_crashes_preserve_history_and_signing_promises() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let (prepared, _fixture) = prepare_active();
    let store = LmdbStore::open(&prepared.source_config.data_dir, 32, 32 << 20).unwrap();
    let bytes = export_bytes(&store, &prepared.params, prepared.epoch);
    let expected = authenticate(&store, &prepared.params, prepared.epoch).into_state();
    drop(store);
    let source = Running::start(prepared.source_config.clone(), prepared.now + 1).await;
    source.ready(prepared.epoch).await;
    for stage in [
        "bootstrap_prepared",
        "bootstrap_committed",
        "bootstrap_synced",
        "economics",
        "complete",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut config = prepared.target_config.clone();
        config.data_dir = directory.path().join("node");
        std::fs::create_dir(&config.data_dir).unwrap();
        std::fs::copy(
            prepared.target_config.data_dir.join("data.mdb"),
            config.data_dir.join("data.mdb"),
        )
        .unwrap();
        let path = directory.path().join("bootstrap.snapshot");
        std::fs::write(&path, &bytes).unwrap();
        config.snapshot_bootstrap = Some(path);
        let local = LmdbStore::open(&config.data_dir, 32, 32 << 20).unwrap();
        let old = installed::load_base(&local, NetworkId::Dev, &prepared.params)
            .unwrap()
            .unwrap();
        local
            .meta_store()
            .reserve_consensus_vote_at(
                &old.anchor().hash(),
                &address(0),
                &address(30),
                &BlockHash::ZERO,
                &BlockHash::new([98; 32]),
                true,
                0,
                Some(old.anchor().cutoff()),
            )
            .unwrap();
        let old_epoch = local.meta_store().payment_epoch().unwrap();
        let old_count = local.block_store().block_count().unwrap();
        let old_metadata = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let retained: Vec<_> = old_metadata
            .iter()
            .filter(|(key, _)| {
                (key.starts_with(b"checkpoint_chain/") && !key.ends_with(b"/head"))
                    || key.starts_with(b"consensus_final/")
            })
            .cloned()
            .collect();
        assert!(retained
            .iter()
            .any(|(key, _)| key.starts_with(b"consensus_final/")));
        assert!(retained
            .iter()
            .any(|(key, _)| key.starts_with(b"checkpoint_chain/")));
        local.force_sync().unwrap();
        drop(local);

        crash::kill_at(&config, prepared.now, stage).await;
        let local = Arc::new(LmdbStore::open(&config.data_dir, 32, 32 << 20).unwrap());
        assert_retained(&local, &retained);
        if stage == "bootstrap_prepared" {
            assert_eq!(local.meta_store().payment_epoch().unwrap(), old_epoch);
            assert_eq!(local.block_store().block_count().unwrap(), old_count);
            assert_eq!(
                installed::read_stored_base_bytes(&local).unwrap().unwrap(),
                prepared.bytes
            );
            assert_retained(&local, &old_metadata);
        } else {
            assert_eq!(local.meta_store().payment_epoch().unwrap(), prepared.epoch);
            assert_eq!(
                installed::read_stored_base_bytes(&local).unwrap().unwrap(),
                bytes
            );
            let service = WitnessService::start(source.store.clone(), local.clone(), 0).await;
            let recovered = installed::recover_metadata_from_peers(
                local.clone(),
                NetworkId::Dev,
                prepared.params.clone(),
                service.registry.clone(),
            )
            .await
            .unwrap()
            .unwrap();
            compact::assert_same(recovered.recovered().continuation.state(), &expected);
            drop(recovered);
            service.stop().await;
        }
        drop(local);
        let target = Running::start(config, prepared.now + 1).await;
        target.ready(prepared.epoch).await;
        assert_retained(&target.store, &retained);
        burn(
            &target,
            &prepared.params,
            prepared.epoch,
            prepared.old_epoch,
            prepared.now + 1,
        )
        .await;
        target.stop().await;
        eprintln!("replacement {stage}: atomic base, retained certificates and promises, restarted payments pass");
    }
    source.stop().await;
}
