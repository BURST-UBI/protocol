use super::*;
use burst_store::brn::BrnStore;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_download_crashes_recover_without_partial_installation() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let (prepared, _) = prepare_active();
    let files = tempfile::tempdir().unwrap();
    let serve = files.path().join("source.snapshot");
    let store = LmdbStore::open(&prepared.source_config.data_dir, 32, 32 << 20).unwrap();
    let bytes = export_bytes(&store, &prepared.params, prepared.epoch);
    let expected = authenticate(&store, &prepared.params, prepared.epoch).into_state();
    assert!(
        bytes.len() > 16 * 1024,
        "fixture must span multiple transport chunks"
    );
    std::fs::write(&serve, &bytes).unwrap();
    let mut source_config = prepared.source_config.clone();
    source_config.snapshot_transfer.serve = Some(serve);
    for stage in [
        "snapshot_download_partial",
        "snapshot_download_prepared",
        "snapshot_download_published",
    ] {
        // Each resumed target sends a real payment to its peers. Start its
        // source from the same immutable database, not the preceding run.
        let source_directory = tempfile::tempdir().unwrap();
        store
            .env()
            .copy_to_file(
                source_directory.path().join("data.mdb"),
                heed::CompactionOption::Disabled,
            )
            .unwrap();
        let mut source_config = source_config.clone();
        source_config.data_dir = source_directory.path().to_path_buf();
        let source = Running::start(source_config, prepared.now + 1).await;
        source.ready(prepared.epoch).await;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("download.snapshot");
        let mut config = prepared.target_config.clone();
        config.data_dir = directory.path().join("node");
        config.snapshot_bootstrap = Some(path.clone());
        config.snapshot_transfer.fetch = Some(crate::checkpoint_sync::snapshot::Fetch {
            epoch: Some(prepared.epoch.to_string()),
            minimum_height: 1,
            output: path.clone(),
        });
        crash::kill_at(&config, prepared.now, stage).await;
        if stage == "snapshot_download_published" {
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        } else {
            assert!(!path.exists());
        }
        let local = LmdbStore::open(&config.data_dir, 32, 32 << 20).unwrap();
        let genesis = crate::genesis_state::block(NetworkId::Dev, &prepared.params);
        assert_eq!(local.meta_store().payment_epoch().unwrap(), genesis.hash);
        assert_eq!(local.block_store().block_count().unwrap(), 1);
        assert!(
            installed::load_base(&local, NetworkId::Dev, &prepared.params)
                .unwrap()
                .is_none()
        );
        drop(local);
        let target = Running::start(config, prepared.now).await;
        target.ready(prepared.epoch).await;
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            installed::read_stored_base_bytes(&target.store)
                .unwrap()
                .unwrap(),
            bytes
        );
        assert_eq!(
            restore_lifecycle(&target.store)
                .snapshot()
                .encode()
                .unwrap(),
            expected.verification.snapshot().encode().unwrap()
        );
        for (wallet, state) in &expected.brn.wallets {
            let bytes = target
                .store
                .brn_store()
                .get_wallet_state(wallet)
                .unwrap()
                .unwrap();
            assert_eq!(
                &bincode::deserialize::<burst_brn::BrnWalletState>(&bytes).unwrap(),
                state
            );
        }
        assert_eq!(
            target.store.pending_store().pending_count().unwrap(),
            expected.pending.len() as u64
        );
        for ((wallet, hash), info) in &expected.pending {
            assert_eq!(
                bincode::serialize(
                    &target
                        .store
                        .pending_store()
                        .get_pending(wallet, hash)
                        .unwrap()
                )
                .unwrap(),
                bincode::serialize(info).unwrap()
            );
        }
        burn(
            &target,
            &prepared.params,
            prepared.epoch,
            prepared.old_epoch,
            prepared.now,
        )
        .await;
        target.stop().await;
        source.stop().await;
        eprintln!("{stage}: SIGKILL, genesis-only ledger, restart and payment pass");
    }
}
