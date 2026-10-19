use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_record_cold_start_crashes_resume_without_snapshots() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let (mut prepared, mut fixture) = prepare_active();
    let (proposal, content) = record_catchup::add_full_proposal(&mut prepared, &mut fixture);
    let source = Running::start(prepared.source_config.clone(), prepared.now + 1).await;
    source.ready(prepared.epoch).await;
    let (withheld, _receiver) = tokio::sync::mpsc::channel(32);
    source.registry.write().await.set_agreement_inbox(withheld);
    for (stage, cached_content) in [
        ("checkpoint_suffix_received", false),
        ("checkpoint_content_received", false),
        ("checkpoint_content_received", true),
        ("checkpoint_record_accepted", false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut config = prepared.target_config.clone();
        config.data_dir = directory.path().join("node");
        config.snapshot_bootstrap = None;
        if cached_content {
            let store = LmdbStore::open(&config.data_dir, 32, 32 << 20).unwrap();
            store
                .meta_store()
                .put_meta(&crate::governance_content::key(proposal), &content)
                .unwrap();
            store.force_sync().unwrap();
        }
        crash::kill_running_at(&config, prepared.now + 1, stage).await;
        {
            let store = LmdbStore::open(&config.data_dir, 32, 32 << 20).unwrap();
            assert!(installed::read_stored_base_bytes(&store).unwrap().is_none());
            let tip =
                CertifiedCheckpoint::stored_tip(&store, NetworkId::Dev, &prepared.params).unwrap();
            if stage == "checkpoint_suffix_received" {
                assert_eq!(tip, 0);
                assert_eq!(
                    store.meta_store().payment_epoch().unwrap(),
                    fixture.blocks[0].hash
                );
            } else if stage == "checkpoint_content_received" {
                let source_tip = CertifiedCheckpoint::stored_tip(
                    &source.store,
                    NetworkId::Dev,
                    &prepared.params,
                )
                .unwrap();
                assert_eq!(tip, source_tip - 1);
                assert!(store
                    .meta_store()
                    .get_meta(&crate::confirmed_history::proposal_archive_key(proposal))
                    .is_err());
            } else {
                assert_eq!(tip, 1);
            }
        }
        let target = Running::start(config, prepared.now + 2).await;
        target.ready(prepared.epoch).await;
        assert!(installed::read_stored_base_bytes(&target.store)
            .unwrap()
            .is_none());
        assert_eq!(
            target
                .store
                .meta_store()
                .get_meta(&crate::confirmed_history::proposal_archive_key(proposal))
                .unwrap(),
            content
        );
        assert_lifecycle(&source.store, &target.store, None);
        // Each crash case starts from the same certified source. Keep its
        // post-recovery payment local so later cases do not inherit that tail.
        {
            let mut registry = target.registry.write().await;
            registry.partitioned_peers = registry.peer_ids().into_iter().cloned().collect();
        }
        burn(
            &target,
            &prepared.params,
            prepared.epoch,
            prepared.old_epoch,
            prepared.now + 2,
        )
        .await;
        target.stop().await;
        eprintln!("{stage} (cached={cached_content}): SIGKILL, snapshot-free restart, content and payment pass");
    }
    source.stop().await;
}
