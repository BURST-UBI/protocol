use super::*;

pub(in crate::confirmed_replay::tests) async fn assert_successor(
    fixture: Fixture,
    directory: tempfile::TempDir,
    store: LmdbStore,
    base_height: usize,
) {
    let events =
        crate::checkpoint_store::chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let last = &events.last().unwrap().certificate().checkpoint;
    let epoch = last.hash();
    let now = last.cutoff + 1;
    let transfers: Vec<_> = events
        .iter()
        .skip(base_height)
        .map(|event| {
            let height = event.certificate().checkpoint.height;
            (
                height,
                CertifiedCheckpoint::export_transfer(
                    &store,
                    NetworkId::Dev,
                    &fixture.params,
                    height,
                )
                .unwrap()
                .unwrap(),
            )
        })
        .collect();
    assert!(!transfers.is_empty());
    let prepared = prepare_lifecycle_archive(fixture, directory, store, Some(base_height));
    let store = LmdbStore::open(&prepared.source_config.data_dir, 32, 32 << 20).unwrap();
    let bytes = export_bytes(&store, &prepared.params, prepared.epoch);
    drop(store);
    let archive_dir = tempfile::tempdir().unwrap();
    let path = archive_dir.path().join("bootstrap.snapshot");
    std::fs::write(&path, &bytes).unwrap();
    let mut config = prepared.target_config.clone();
    config.snapshot_bootstrap = Some(path);
    let source = Running::start(prepared.source_config.clone(), now).await;
    source.ready(prepared.epoch).await;
    let target = Running::start(config.clone(), now).await;
    target.ready(prepared.epoch).await;
    assert_lifecycle(&source.store, &target.store, None);
    for (height, transfer) in transfers {
        CertifiedCheckpoint::import_transfer(
            &source.store,
            NetworkId::Dev,
            &prepared.params,
            height,
            &transfer,
            &crate::checkpoint_store::historical::HistoricalAdmissions::default(),
        )
        .unwrap();
    }
    source.ready(epoch).await;
    target.ready(epoch).await;
    assert_lifecycle(&source.store, &target.store, None);
    assert_economics(
        &source.store,
        &target.store,
        prepared.params.trst_expiry_secs,
    );
    assert_eq!(
        installed::read_stored_base_bytes(&target.store)
            .unwrap()
            .unwrap(),
        bytes
    );
    assert!(
        crate::checkpoint_store::chain::load(&target.store, NetworkId::Dev, &prepared.params)
            .is_err()
    );
    let first = burn(&target, &prepared.params, epoch, prepared.epoch, now).await;
    target.stop().await;
    let target = Running::start(config, now + 1).await;
    target.ready(epoch).await;
    assert_lifecycle(&source.store, &target.store, Some(first.into_tx_hash()));
    assert!(target.store.block_store().exists(&first).unwrap());
    burn(&target, &prepared.params, epoch, prepared.epoch, now + 1).await;
    target.stop().await;
    source.stop().await;
}
