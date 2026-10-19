use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_refreshes_pre_base_beacon_after_startup_and_restart() {
    delayed_refresh(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_refreshes_pre_base_beacon_from_pruned_record_and_restarts_both_nodes() {
    delayed_refresh(true).await;
}

async fn delayed_refresh(prune_source_record: bool) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let prepared = prepare();
    let store = LmdbStore::open(&prepared.source_config.data_dir, 32, 32 << 20).unwrap();
    let bytes = export_bytes(&store, &prepared.params, prepared.epoch);
    let mut selection = authenticate(&store, &prepared.params, prepared.epoch)
        .recover_selections()
        .unwrap()
        .remove(0);
    assert!(selection.beacon().is_none());
    let height = selection.certificate().checkpoint.height;
    let beacon_key = crate::checkpoint_store::archive_selection::beacon_key(
        selection.certificate().checkpoint.hash(),
    );
    let record_key = format!(
        "checkpoint_chain/v1/dev/{}/{height}",
        selection.parent().genesis_hash()
    );
    drop(store);
    let source = Running::start(prepared.source_config.clone(), prepared.now + 1).await;
    tracing::debug!("waiting for delayed-refresh source readiness");
    source.ready(prepared.epoch).await;
    source
        .store
        .meta_store()
        .put_meta(installed::BASE_KEY, &bytes)
        .unwrap();
    if prune_source_record {
        source.store.meta_store().delete_meta(&record_key).unwrap();
        source.store.force_sync().unwrap();
        assert!(source
            .store
            .tx_begin_write()
            .unwrap()
            .get_meta(&record_key)
            .unwrap()
            .is_none());
    }
    let (withheld, _agreement_receiver) = tokio::sync::mpsc::channel(32);
    source.registry.write().await.set_agreement_inbox(withheld);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pending.snapshot");
    std::fs::write(&path, &bytes).unwrap();
    let mut config = prepared.target_config.clone();
    config.data_dir = directory.path().join("node");
    config.snapshot_bootstrap = Some(path);
    let target = Running::start(config.clone(), prepared.now + 1).await;
    tracing::debug!("waiting for delayed-refresh target readiness");
    let mut source_stopped = source.stop.subscribe();
    tokio::select! {
        biased;
        _ = source_stopped.recv() => panic!("snapshot source stopped during receiver startup: {:?}", source.stop.failure()),
        _ = target.ready(prepared.epoch) => (),
    }
    assert!(target
        .store
        .tx_begin_write()
        .unwrap()
        .get_meta(&record_key)
        .unwrap()
        .is_none());
    assert!(target
        .store
        .tx_begin_write()
        .unwrap()
        .get_meta(&beacon_key)
        .unwrap()
        .is_none());
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
    let mut expected_lifecycle = restore_lifecycle(&target.store);
    expected_lifecycle
        .install_selection(
            selection.replayed().request().target(),
            selection.parent(),
            selection.certificate(),
            &beacon,
        )
        .unwrap();
    expected_lifecycle.drain_events();
    let expected_lifecycle = expected_lifecycle.snapshot().encode().unwrap();
    selection
        .attach_beacon(&source.store, &prepared.params, beacon.clone())
        .unwrap();
    let expected = bincode::serialize(&beacon).unwrap();
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            assert!(
                !target.task.as_ref().unwrap().is_finished(),
                "receiver exited: {:?}",
                target.stop.failure()
            );
            if target
                .store
                .tx_begin_write()
                .unwrap()
                .get_meta(&beacon_key)
                .unwrap()
                .as_ref()
                == Some(&expected)
                && restore_lifecycle(&target.store)
                    .snapshot()
                    .encode()
                    .unwrap()
                    == expected_lifecycle
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("peer refresh did not publish the delayed pre-base committee");
    assert!(target
        .store
        .tx_begin_write()
        .unwrap()
        .get_meta(&record_key)
        .unwrap()
        .is_none());
    assert_eq!(
        target
            .client
            .signing_state(&address(0))
            .await
            .unwrap()
            .payment_epoch,
        prepared.epoch
    );
    target.stop().await;
    source.stop().await;
    let source = Running::start(prepared.source_config.clone(), prepared.now + 2).await;
    if prune_source_record {
        assert!(source
            .store
            .tx_begin_write()
            .unwrap()
            .get_meta(&record_key)
            .unwrap()
            .is_none());
    }
    config.snapshot_bootstrap = None;
    let target = Running::start(config, prepared.now + 2).await;
    source.ready(prepared.epoch).await;
    target.ready(prepared.epoch).await;
    assert_eq!(
        target.store.meta_store().get_meta(&beacon_key).unwrap(),
        expected
    );
    assert_eq!(
        restore_lifecycle(&target.store)
            .snapshot()
            .encode()
            .unwrap(),
        expected_lifecycle
    );
    assert!(target
        .store
        .tx_begin_write()
        .unwrap()
        .get_meta(&record_key)
        .unwrap()
        .is_none());
    target.stop().await;
    source.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_record_only_catchup_installs_successors_and_restarts() {
    record_only_catchup(true, true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_record_only_catchup_uses_a_source_without_snapshots() {
    record_only_catchup(false, true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_record_only_catchup_from_genesis_without_snapshots() {
    record_only_catchup(false, false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_record_only_catchup_from_genesis_with_full_proposal() {
    record_only_catchup(false, false, true).await;
}

pub(super) fn add_full_proposal(
    prepared: &mut Prepared,
    fixture: &mut Fixture,
) -> (BlockHash, Vec<u8>) {
    let source = LmdbStore::open(&prepared.source_config.data_dir, 32, 32 << 20).unwrap();
    let parent = authenticate(&source, &prepared.params, prepared.epoch);
    let signers: Vec<_> = (0..32)
        .filter(|seed| parent.anchor().roster().weight(&address(*seed)) > 0)
        .collect();
    let body = burst_governance::content::EncodedContent::new(
        &burst_governance::ProposalContent::ConstitutionalDiff {
            title: "Voting rights".into(),
            operations: vec![burst_governance::proposal::AmendmentOp::AddArticle {
                title: "Voting".into(),
                text: "Each verified human has one vote.".into(),
            }],
        },
    )
    .unwrap();
    let proposal = fixture.push(
        0,
        BlockType::GovernanceProposal,
        prepared.now + 1,
        |block| {
            block.payment_epoch = prepared.epoch;
            (block.link, block.origin, block.transaction) = body.manifest().block_fields().unwrap();
        },
    );
    source
        .meta_store()
        .put_meta(&crate::governance_content::key(proposal), body.bytes())
        .unwrap();
    fixture.push(
        0,
        BlockType::ChangeRepresentative,
        prepared.now + 2,
        |block| {
            block.payment_epoch = prepared.epoch;
            block.representative = address(1);
        },
    );
    let (_history_dir, history_store, heads) = fixture.store();
    history_store
        .meta_store()
        .put_meta(&crate::governance_content::key(proposal), body.bytes())
        .unwrap();
    let history = ConfirmedHistory::read(
        &history_store,
        NetworkId::Dev,
        fixture.blocks[0].hash,
        prepared.now + 3,
        &heads,
        &[],
    )
    .unwrap();
    fixture.append_confirmed_history(&source, &history);
    let roster = crate::checkpoint_roster::PreparedRoster::prepare_next(
        &source,
        NetworkId::Dev,
        &prepared.params,
        prepared.now + 4,
    )
    .unwrap()
    .unwrap()
    .manifest()
    .clone();
    let certificate = certified_votes::certify(&roster.checkpoint, &signers);
    crate::checkpoint_store::chain::accept_manifest(
        &source,
        NetworkId::Dev,
        &prepared.params,
        certificate.clone(),
        &crate::checkpoint_agreement::ReplayManifest::Roster(roster),
    )
    .unwrap();
    crate::confirmed_replay::installation::PreparedInstallation::next(
        &source,
        NetworkId::Dev,
        &prepared.params,
    )
    .unwrap()
    .unwrap()
    .commit(&source)
    .unwrap();
    prepared.epoch = certificate.checkpoint.hash();
    prepared.now = certificate.checkpoint.cutoff + 1;
    source.force_sync().unwrap();
    (proposal, body.bytes().to_vec())
}

async fn record_only_catchup(source_snapshot: bool, target_snapshot: bool, full_proposal: bool) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let (mut prepared, mut fixture) = prepare_active();
    let proposal = full_proposal.then(|| add_full_proposal(&mut prepared, &mut fixture));
    let source = Running::start(prepared.source_config.clone(), prepared.now + 1).await;
    source.ready(prepared.epoch).await;
    let source_store = &source.store;
    if source_snapshot {
        source_store
            .meta_store()
            .put_meta(installed::BASE_KEY, &prepared.bytes)
            .unwrap();
    } else {
        assert!(source_store
            .meta_store()
            .get_meta(installed::BASE_KEY)
            .is_err());
    }
    let tip =
        CertifiedCheckpoint::stored_tip(source_store, NetworkId::Dev, &prepared.params).unwrap();
    let transfer = CertifiedCheckpoint::export_transfer_for_request(
        source_store,
        NetworkId::Dev,
        &prepared.params,
        tip,
        true,
    )
    .unwrap()
    .unwrap();
    assert_eq!(transfer[0], 3);
    if !source_snapshot {
        let legacy = CertifiedCheckpoint::export_transfer(
            source_store,
            NetworkId::Dev,
            &prepared.params,
            tip,
        )
        .unwrap()
        .unwrap();
        assert_eq!(legacy[0], 2);
        assert!(transfer.len() < legacy.len());
    }
    source_store.force_sync().unwrap();
    // Prevent the separate agreement relay from supplying the final decision.
    // Checkpoint synchronization and the witness service remain connected.
    let (withheld, _agreement_receiver) = tokio::sync::mpsc::channel(32);
    source.registry.write().await.set_agreement_inbox(withheld);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("base.snapshot");
    let mut config = prepared.target_config.clone();
    config.data_dir = directory.path().join("node");
    config.snapshot_bootstrap = if target_snapshot {
        std::fs::write(&path, &prepared.bytes).unwrap();
        Some(path)
    } else {
        None
    };
    let target = Running::start(config.clone(), prepared.now + 1).await;
    target.ready(prepared.epoch).await;
    assert_eq!(
        CertifiedCheckpoint::stored_tip(&target.store, NetworkId::Dev, &prepared.params).unwrap(),
        tip
    );
    if target_snapshot {
        assert_eq!(
            installed::read_stored_base_bytes(&target.store)
                .unwrap()
                .unwrap(),
            prepared.bytes
        );
        assert!(crate::checkpoint_store::chain::load(
            &target.store,
            NetworkId::Dev,
            &prepared.params
        )
        .is_err());
    } else {
        assert!(installed::read_stored_base_bytes(&target.store)
            .unwrap()
            .is_none());
    }
    assert_lifecycle(&source.store, &target.store, None);
    if let Some((hash, bytes)) = &proposal {
        assert_eq!(
            target
                .store
                .meta_store()
                .get_meta(&crate::confirmed_history::proposal_archive_key(*hash))
                .unwrap(),
            *bytes
        );
    }
    let first = burn(
        &target,
        &prepared.params,
        prepared.epoch,
        prepared.old_epoch,
        prepared.now + 1,
    )
    .await;
    target.stop().await;
    config.snapshot_bootstrap = None;
    let target = Running::start(config, prepared.now + 2).await;
    target.ready(prepared.epoch).await;
    assert!(target.store.block_store().exists(&first).unwrap());
    if let Some((hash, bytes)) = &proposal {
        assert_eq!(
            target
                .store
                .meta_store()
                .get_meta(&crate::confirmed_history::proposal_archive_key(*hash))
                .unwrap(),
            *bytes
        );
    }
    if target_snapshot {
        assert_eq!(
            installed::read_stored_base_bytes(&target.store)
                .unwrap()
                .unwrap(),
            prepared.bytes
        );
    } else {
        assert!(installed::read_stored_base_bytes(&target.store)
            .unwrap()
            .is_none());
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
    source.stop().await;
}
