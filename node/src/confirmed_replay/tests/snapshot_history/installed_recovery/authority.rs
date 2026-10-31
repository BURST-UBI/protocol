use super::*;

fn verification_tamper_row(store: &LmdbStore) -> (String, Vec<u8>) {
    let batch = store.tx_begin_write().unwrap();
    if batch.uses_verification_records().unwrap() {
        let (key, value) = batch
            .checkpoint_metadata(b"verification_state/")
            .unwrap()
            .into_iter()
            .next()
            .expect("fixture must contain a persisted verification record");
        (String::from_utf8(key).unwrap(), value)
    } else {
        let key = crate::endorsement_commit::SNAPSHOT_KEY;
        (
            key.to_owned(),
            batch
                .get_meta(key)
                .unwrap()
                .expect("fixture verification snapshot"),
        )
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn batched_recovery_beacons_publish_only_after_complete_authentication() {
    let (fixture, _directory, store, _, certificate) = selection::setup();
    let store = Arc::new(store);
    let service = WitnessService::start(store.clone(), store.clone(), 0).await;
    let mut cache = installed::recover_current_tip_with_cache(
        store.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
        None,
    )
    .await
    .unwrap();
    service.stop().await;
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
    let hash = certificate.checkpoint.hash();
    let index = cache
        .recovered
        .selections
        .iter_selections()
        .map(Result::unwrap)
        .position(|selection| selection.certificate().checkpoint.hash() == hash)
        .unwrap();
    let mut original_hints = cache.recovered.selections.refresh_cursor().unwrap();
    let missing_height = certificate.checkpoint.height;
    let before = store
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let mut forged = beacon.clone();
    forged.value[0] ^= 1;
    for batch in [
        vec![
            Ok((hash, beacon.clone())),
            Ok((BlockHash::new([99; 32]), beacon.clone())),
        ],
        vec![
            Ok((hash, beacon.clone())),
            Err(burst_store::StoreError::Corruption(
                "truncated input".into(),
            )),
        ],
        vec![Ok((hash, forged.clone()))],
        vec![Ok((hash, beacon.clone())), Ok((hash, forged))],
    ] {
        assert!(cache
            .recovered
            .selections
            .attach_beacons(store.env().path(), batch)
            .is_err());
        assert!(cache
            .recovered
            .selections
            .read_selection(index)
            .unwrap()
            .beacon()
            .is_none());
        assert_eq!(
            cache
                .recovered
                .selections
                .refresh_cursor()
                .unwrap()
                .next(&store)
                .unwrap(),
            Some(missing_height),
            "failed batches must not publish hint updates"
        );
    }
    let mut single = cache.recovered.fork(store.env().path()).unwrap();
    single
        .selections
        .attach_beacon(hash, beacon.clone())
        .unwrap();
    assert_eq!(
        single
            .selections
            .refresh_cursor()
            .unwrap()
            .next(&store)
            .unwrap(),
        None
    );
    assert_eq!(
        cache
            .recovered
            .selections
            .refresh_cursor()
            .unwrap()
            .next(&store)
            .unwrap(),
        Some(missing_height)
    );
    cache
        .recovered
        .selections
        .attach_beacons(
            store.env().path(),
            [Ok((hash, beacon.clone())), Ok((hash, beacon.clone()))],
        )
        .unwrap();
    let bytes = cache.recovered.selections.bytes_for_test();
    for _ in 0..3 {
        cache
            .recovered
            .selections
            .attach_beacons(store.env().path(), [Ok((hash, beacon.clone()))])
            .unwrap();
        cache
            .recovered
            .selections
            .attach_beacon(hash, beacon.clone())
            .unwrap();
    }
    assert_eq!(
        cache.recovered.selections.bytes_for_test(),
        bytes,
        "rechecking existing proofs must not append duplicate private rows"
    );
    assert_eq!(
        bincode::serialize(
            cache
                .recovered
                .selections
                .read_selection(index)
                .unwrap()
                .beacon()
                .unwrap()
        )
        .unwrap(),
        bincode::serialize(&beacon).unwrap(),
    );
    assert_eq!(
        store
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    // Scheduling owns only small derived rows; it does not decode damaged receipts.
    cache.recovered.selections.truncate_for_test();
    assert!(cache.recovered.selections.read_selection(index).is_err());
    assert_eq!(
        cache
            .recovered
            .selections
            .refresh_cursor()
            .unwrap()
            .next(&store)
            .unwrap(),
        None
    );
    assert_eq!(
        original_hints.next(&store).unwrap(),
        Some(missing_height),
        "existing hint cursors keep their frozen view after enrichment"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn cached_selection_accepts_only_authenticated_beacon_enrichment_without_replay() {
    let (fixture, _directory, store, _, certificate) = selection::setup();
    let store = Arc::new(store);
    let service = WitnessService::start(store.clone(), store.clone(), 0).await;
    let cache = installed::recover_current_tip_with_cache(
        store.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
        None,
    )
    .await
    .unwrap();
    let key = format!(
        "checkpoint_chain/v1/dev/{}/{}",
        fixture.blocks[0].hash, certificate.checkpoint.height
    );
    let original = store.meta_store().get_meta(&key).unwrap();
    let (version, certificate, heads, target, _): (
        u8,
        burst_consensus::checkpoint::CheckpointCertificate,
        Vec<crate::confirmed_history::HistoryHead>,
        WalletAddress,
        Option<burst_vrf::RandomOutput>,
    ) = bincode::deserialize(&original).unwrap();
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
    service.stop().await;
    let registry = Arc::new(RwLock::new(ConnectionRegistry::new()));
    for valid in [false, true] {
        let mut proof = beacon.clone();
        if !valid {
            proof.value[0] ^= 1;
        }
        let bytes =
            bincode::serialize(&(version, &certificate, &heads, &target, Some(proof))).unwrap();
        store.meta_store().put_meta(&key, &bytes).unwrap();
        let recovered = cache
            .fork(store.env().path())
            .unwrap()
            .recover(
                cache.base,
                store.clone(),
                registry.clone(),
                certificate.checkpoint.hash(),
            )
            .await;
        if valid {
            let recovered = recovered
                .unwrap()
                .expect("valid late beacon must preserve cached replay");
            let selection = recovered
                .selections
                .iter_selections()
                .map(Result::unwrap)
                .find(|selection| selection.certificate().checkpoint == certificate.checkpoint)
                .unwrap();
            assert_eq!(selection.beacon().unwrap().round, beacon.round);
            recovered
                .verify_records_unchanged(&store.tx_begin_write().unwrap())
                .unwrap();
        } else {
            assert!(
                recovered.is_err(),
                "forged beacon must not preserve cached authority"
            );
        }
        assert_eq!(
            store.meta_store().get_meta(&key).unwrap(),
            bytes,
            "recovery must not mutate storage"
        );
    }
    let changed =
        bincode::serialize(&(version, &certificate, &heads, address(99), Some(beacon))).unwrap();
    store.meta_store().put_meta(&key, &changed).unwrap();
    assert!(
        cache
            .fork(store.env().path())
            .unwrap()
            .recover(
                cache.base,
                store.clone(),
                registry,
                certificate.checkpoint.hash(),
            )
            .await
            .unwrap()
            .is_none(),
        "a valid beacon cannot authorize changed selection fields"
    );
    assert_eq!(store.meta_store().get_meta(&key).unwrap(), changed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn recovery_forks_keep_late_beacons_private_without_changing_durable_authority() {
    let (fixture, _directory, store, _, certificate) = selection::setup();
    let store = Arc::new(store);
    let service = WitnessService::start(store.clone(), store.clone(), 0).await;
    let cache = installed::recover_current_tip_with_cache(
        store.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
        None,
    )
    .await
    .unwrap();
    let height = certificate.checkpoint.height;
    let find = |recovered: &installed::RecoveredSnapshot| {
        recovered
            .selections
            .iter_selections()
            .map(Result::unwrap)
            .position(|selection| selection.certificate().checkpoint.height == height)
            .unwrap()
    };
    let original_index = find(&cache.recovered);
    assert!(cache
        .recovered
        .selections
        .read_selection(original_index)
        .unwrap()
        .beacon()
        .is_none());
    let before = store
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let requests = service.requests.load(Ordering::SeqCst);
    let mut first = cache.fork(store.env().path()).unwrap();
    let second = cache.fork(store.env().path()).unwrap();
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
    let index = find(&first.recovered);
    first
        .recovered
        .selections
        .attach_beacon(certificate.checkpoint.hash(), beacon.clone())
        .unwrap();
    assert_eq!(
        first
            .recovered
            .selections
            .read_selection(index)
            .unwrap()
            .beacon()
            .unwrap()
            .round,
        beacon.round
    );
    assert!(cache
        .recovered
        .selections
        .read_selection(original_index)
        .unwrap()
        .beacon()
        .is_none());
    assert!(second
        .recovered
        .selections
        .read_selection(find(&second.recovered))
        .unwrap()
        .beacon()
        .is_none());
    let child = first.fork(store.env().path()).unwrap();
    assert_eq!(
        child
            .recovered
            .selections
            .read_selection(find(&child.recovered))
            .unwrap()
            .beacon()
            .unwrap()
            .round,
        beacon.round
    );
    let mut wrong = beacon;
    wrong.round += 1;
    assert!(first
        .recovered
        .selections
        .attach_beacon(certificate.checkpoint.hash(), wrong)
        .is_err());
    for recovered in [
        &cache.recovered,
        &first.recovered,
        &second.recovered,
        &child.recovered,
    ] {
        recovered
            .verify_records_unchanged(&store.tx_begin_write().unwrap())
            .unwrap();
        assert_eq!(
            recovered.continuation.anchor().hash(),
            certificate.checkpoint.hash()
        );
    }
    assert_eq!(
        store
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    assert_eq!(service.requests.load(Ordering::SeqCst), requests);
    first.recovered.selections.truncate_for_test();
    assert!(first.recovered.selections.read_selection(index).is_err());
    assert!(cache
        .recovered
        .selections
        .read_selection(original_index)
        .is_err());
    assert!(second
        .recovered
        .selections
        .iter_selections()
        .next()
        .unwrap()
        .is_err());
    assert!(child
        .recovered
        .selections
        .read_selection(usize::MAX)
        .is_err());
    assert_eq!(
        store
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn replay_cache_accepts_only_an_identical_durable_beacon_copy() {
    let (fixture, _directory, store, _, certificate) = selection::setup();
    let mut selection = crate::checkpoint_beacon::selection_checkpoints(
        crate::checkpoint_store::chain::load(&store, NetworkId::Dev, &fixture.params).unwrap(),
    )
    .pop()
    .unwrap();
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
    selection
        .attach_beacon(&store, &fixture.params, beacon.clone())
        .unwrap();
    let store = Arc::new(store);
    let service = WitnessService::start(store.clone(), store.clone(), 0).await;
    let cache = installed::recover_current_tip_with_cache(
        store.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
        None,
    )
    .await
    .unwrap();
    let requests = service.requests.load(Ordering::SeqCst);
    let key = crate::checkpoint_store::archive_selection::beacon_key(certificate.checkpoint.hash());
    assert!(store
        .tx_begin_write()
        .unwrap()
        .get_meta(&key)
        .unwrap()
        .is_none());
    let base = cache.base;
    let target = certificate.checkpoint.hash();
    let mut wrong = beacon.clone();
    wrong.round += 1;
    store
        .meta_store()
        .put_meta(&key, &bincode::serialize(&wrong).unwrap())
        .unwrap();
    assert!(
        cache
            .fork(store.env().path())
            .unwrap()
            .recover(base, store.clone(), service.registry.clone(), target,)
            .await
            .unwrap()
            .is_none(),
        "different beacon must invalidate reuse"
    );
    store
        .meta_store()
        .put_meta(&key, &bincode::serialize(&beacon).unwrap())
        .unwrap();
    let before = store
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let recovered = cache
        .recover(base, store.clone(), service.registry.clone(), target)
        .await
        .unwrap()
        .expect("an identical durable copy must not trigger full replay");
    recovered
        .verify_records_unchanged(&store.tx_begin_write().unwrap())
        .unwrap();
    assert_eq!(service.requests.load(Ordering::SeqCst), requests);
    assert_eq!(
        store
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    let cache = installed::ReplayCache { base, recovered };
    store
        .meta_store()
        .put_meta(&key, &bincode::serialize(&wrong).unwrap())
        .unwrap();
    assert!(
        cache
            .recover(base, store.clone(), service.registry.clone(), target,)
            .await
            .unwrap()
            .is_none(),
        "changing an observed proof must invalidate reuse"
    );
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn known_selection_refresh_reuses_checked_receipt_without_peer_replay() {
    use crate::checkpoint_agreement::ReplayManifest;
    for with_beacon in [false, true] {
        let (fixture, _source_dir, source, manifest, certificate) = selection::setup();
        let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
            "../../../../../tests/fixtures/selection-beacons.json"
        ))
        .unwrap();
        let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
        if with_beacon {
            let mut selection = crate::checkpoint_beacon::selection_checkpoints(
                crate::checkpoint_store::chain::load(&source, NetworkId::Dev, &fixture.params)
                    .unwrap(),
            )
            .pop()
            .unwrap();
            selection
                .attach_beacon(&source, &fixture.params, beacon.clone())
                .unwrap();
        }
        let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
        let old =
            authenticate(&source, &fixture.params, manifest.checkpoint.parent).into_continuation();
        let source = Arc::new(source);
        let (_dir, local) = cemented::sparse(&old, &source);
        copy_installed_records(&local, &source, &fixture, &bytes);
        let local = Arc::new(local);
        let service = WitnessService::start(source.clone(), local.clone(), 0).await;
        let cache = installed::recover_current_tip_with_cache(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
            None,
        )
        .await
        .unwrap();
        let expected = ReplayManifest::Selection(manifest.clone());
        let proof = with_beacon.then_some(&beacon);
        assert!(cache
            .refresh_known_selection(
                &local,
                cache.base,
                &certificate,
                &expected,
                proof,
                &fixture.params
            )
            .unwrap());
        assert!(!cache
            .refresh_known_selection(
                &local,
                None,
                &certificate,
                &expected,
                proof,
                &fixture.params
            )
            .unwrap());
        let mut forged = certificate.clone();
        forged.signatures[0].signature.0[0] ^= 1;
        assert!(cache
            .refresh_known_selection(
                &local,
                cache.base,
                &forged,
                &expected,
                proof,
                &fixture.params
            )
            .is_err());
        let mut changed = manifest.clone();
        changed.target = address(99);
        assert!(cache
            .refresh_known_selection(
                &local,
                cache.base,
                &certificate,
                &ReplayManifest::Selection(changed),
                proof,
                &fixture.params,
            )
            .is_err());
        let unchanged = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        for field in 0..3 {
            let mut wrong = beacon.clone();
            match field {
                0 => wrong.round += 1,
                1 => wrong.value[0] ^= 1,
                _ => wrong.proof[0] ^= 1,
            }
            assert!(cache
                .refresh_known_selection(
                    &local,
                    cache.base,
                    &certificate,
                    &expected,
                    Some(&wrong),
                    &fixture.params,
                )
                .is_err());
            assert_eq!(
                local
                    .tx_begin_write()
                    .unwrap()
                    .checkpoint_metadata(b"")
                    .unwrap(),
                unchanged
            );
        }
        local
            .meta_store()
            .put_meta("test/unrelated-refresh", b"unchanged authority")
            .unwrap();
        assert!(cache
            .refresh_known_selection(
                &local,
                cache.base,
                &certificate,
                &expected,
                proof,
                &fixture.params
            )
            .unwrap());
        let retained = cache.fork(local.env().path()).unwrap();
        let transfer = CertifiedCheckpoint::export_transfer_for_request(
            &source,
            NetworkId::Dev,
            &fixture.params,
            certificate.checkpoint.height,
            true,
        )
        .unwrap()
        .unwrap();
        service.stop().await;
        let registry = Arc::new(RwLock::new(ConnectionRegistry::new()));
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let mut imports =
            crate::checkpoint_store::historical::snapshot_transfer::Session::from_recovery(Some(
                cache,
            ));
        for _ in 0..2 {
            assert!(imports
                .import(
                    local.clone(),
                    NetworkId::Dev,
                    fixture.params.clone(),
                    certificate.checkpoint.height,
                    transfer.clone(),
                    registry.clone(),
                )
                .await
                .unwrap()
                .is_none());
        }
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
        let key = format!("checkpoint_chain/v1/dev/{}/2", fixture.blocks[0].hash);
        let original = local.meta_store().get_meta(&key).unwrap();
        local
            .meta_store()
            .put_meta(&key, b"changed record")
            .unwrap();
        assert!(!retained
            .refresh_known_selection(
                &local,
                retained.base,
                &certificate,
                &expected,
                proof,
                &fixture.params
            )
            .unwrap());
        assert!(
            imports
                .import(
                    local.clone(),
                    NetworkId::Dev,
                    fixture.params.clone(),
                    certificate.checkpoint.height,
                    transfer,
                    registry.clone(),
                )
                .await
                .is_err(),
            "stale receipt must not bypass replay"
        );
        assert_eq!(
            local.meta_store().get_meta(&key).unwrap(),
            b"changed record"
        );
        local.meta_store().put_meta(&key, &original).unwrap();
        let original_base = local.meta_store().get_meta(installed::BASE_KEY).unwrap();
        local
            .meta_store()
            .put_meta(installed::BASE_KEY, b"changed base")
            .unwrap();
        assert!(retained
            .refresh_known_selection(
                &local,
                retained.base,
                &certificate,
                &expected,
                proof,
                &fixture.params
            )
            .is_err());
        local
            .meta_store()
            .put_meta(installed::BASE_KEY, &original_base)
            .unwrap();
        if !with_beacon {
            let mut imports =
                crate::checkpoint_store::historical::snapshot_transfer::Session::from_recovery(
                    Some(retained),
                );
            let mut selection = crate::checkpoint_beacon::selection_checkpoints(
                crate::checkpoint_store::chain::load(&source, NetworkId::Dev, &fixture.params)
                    .unwrap(),
            )
            .pop()
            .unwrap();
            selection
                .attach_beacon(&source, &fixture.params, beacon.clone())
                .unwrap();
            let with_proof = CertifiedCheckpoint::export_transfer_for_request(
                &source,
                NetworkId::Dev,
                &fixture.params,
                certificate.checkpoint.height,
                true,
            )
            .unwrap()
            .unwrap();
            assert!(
                imports
                    .import(
                        local.clone(),
                        NetworkId::Dev,
                        fixture.params.clone(),
                        certificate.checkpoint.height,
                        with_proof.clone(),
                        registry.clone(),
                    )
                    .await
                    .unwrap()
                    .is_none(),
                "a new valid beacon must attach without peers"
            );
            let persisted = local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap();
            assert_ne!(persisted, before, "the new beacon must be durable");
            assert!(imports
                .import(
                    local.clone(),
                    NetworkId::Dev,
                    fixture.params.clone(),
                    certificate.checkpoint.height,
                    with_proof,
                    registry.clone(),
                )
                .await
                .unwrap()
                .is_none());
            assert_eq!(
                local
                    .tx_begin_write()
                    .unwrap()
                    .checkpoint_metadata(b"")
                    .unwrap(),
                persisted
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn recovery_progress_survives_failed_journal_checks_without_authorizing_them() {
    let (fixture, _source_dir, source, manifest, _) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let old =
        authenticate(&source, &fixture.params, manifest.checkpoint.parent).into_continuation();
    let source = Arc::new(source);
    let (_dir, local) = cemented::sparse(&old, &source);
    copy_installed_records(&local, &source, &fixture, &bytes);
    let local = Arc::new(local);
    let service = WitnessService::start(source.clone(), local.clone(), 0).await;
    let mut progress = None;
    let bad_key = "consensus_final/unsupported";
    local
        .meta_store()
        .put_meta(bad_key, b"invalid promise")
        .unwrap();
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    for _ in 0..2 {
        assert!(
            installed::recover_metadata_retaining_progress(
                local.clone(),
                NetworkId::Dev,
                fixture.params.clone(),
                service.registry.clone(),
                &mut progress,
            )
            .await
            .is_err(),
            "retained replay must not authorize an invalid signer journal"
        );
        assert_eq!(
            progress
                .as_ref()
                .unwrap()
                .recovered
                .continuation
                .anchor()
                .hash(),
            old.anchor().hash()
        );
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
    }
    local.meta_store().delete_meta(bad_key).unwrap();
    let sequence_key = format!("consensus_sequence/v1/{}", address(0));
    local
        .meta_store()
        .put_meta(&sequence_key, &12u64.to_be_bytes())
        .unwrap();
    let checked = installed::recover_metadata_retaining_progress(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
        &mut progress,
    )
    .await
    .unwrap()
    .unwrap();
    compact::assert_same(checked.recovered().continuation.state(), old.state());
    checked
        .verify_unchanged(&local.tx_begin_write().unwrap())
        .unwrap();
    let reused = installed::recover_metadata_retaining_progress(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
        &mut progress,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        checked.shares_projection(&reused),
        "unchanged recovery must reuse the projection"
    );
    let (retained_hash, retained_bytes) = {
        let batch = local.tx_begin_write().unwrap();
        let row = batch
            .indexed_block_entries(131_072, 64 << 20)
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        (row.hash, row.bytes.to_vec())
    };
    {
        let mut batch = local.tx_begin_write().unwrap();
        batch
            .put_block(&retained_hash, b"corrupt retained body")
            .unwrap();
        batch.commit().unwrap();
    }
    assert!(
        installed::recover_metadata_retaining_progress(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
            &mut progress,
        )
        .await
        .is_err(),
        "shared tails must not hide changed retained block bytes"
    );
    {
        let mut batch = local.tx_begin_write().unwrap();
        batch.put_block(&retained_hash, &retained_bytes).unwrap();
        batch.commit().unwrap();
    }
    let body_restored = installed::recover_metadata_retaining_progress(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
        &mut progress,
    )
    .await
    .unwrap()
    .unwrap();
    compact::assert_same(body_restored.recovered().continuation.state(), old.state());
    let (snapshot_key, snapshot) = verification_tamper_row(&local);
    local
        .meta_store()
        .put_meta(&snapshot_key, b"corrupt applied snapshot")
        .unwrap();
    assert!(
        installed::recover_metadata_retaining_progress(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
            &mut progress,
        )
        .await
        .is_err(),
        "a cached projection must not hide a changed economic snapshot"
    );
    assert_eq!(
        local.meta_store().get_meta(&snapshot_key).unwrap(),
        b"corrupt applied snapshot",
        "failed recovery must not repair the stored verification record"
    );
    local
        .meta_store()
        .put_meta(&snapshot_key, &snapshot)
        .unwrap();
    let restored = installed::recover_metadata_retaining_progress(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
        &mut progress,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        checked.shares_projection(&restored),
        "exactly restored contents may reuse a revalidated shared projection"
    );
    restored
        .verify_unchanged(&local.tx_begin_write().unwrap())
        .unwrap();
    compact::assert_same(restored.recovered().continuation.state(), old.state());
    local
        .meta_store()
        .put_meta(&sequence_key, &13u64.to_be_bytes())
        .unwrap();
    checked
        .verify_unchanged(&local.tx_begin_write().unwrap())
        .unwrap();
    let advanced_sequence = installed::recover_metadata_retaining_progress(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
        &mut progress,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        restored.shares_projection(&advanced_sequence),
        "valid sequence advances must retain reusable projections"
    );
    local
        .meta_store()
        .put_meta(&sequence_key, &11u64.to_be_bytes())
        .unwrap();
    assert!(checked
        .verify_unchanged(&local.tx_begin_write().unwrap())
        .is_err());
    local
        .meta_store()
        .put_meta(&sequence_key, &13u64.to_be_bytes())
        .unwrap();
    local
        .meta_store()
        .put_meta(bad_key, b"changed promise")
        .unwrap();
    assert!(checked
        .verify_unchanged(&local.tx_begin_write().unwrap())
        .is_err());
    assert!(
        installed::recover_metadata_retaining_progress(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
            &mut progress,
        )
        .await
        .is_err(),
        "projection reuse must still validate signer journals"
    );
    local.meta_store().delete_meta(bad_key).unwrap();
    let prepared = checked
        .prepare_next_from_peers(local.clone(), service.registry.clone())
        .await
        .unwrap()
        .unwrap();
    local
        .meta_store()
        .put_meta(&sequence_key, &14u64.to_be_bytes())
        .unwrap();
    let installed = prepared.commit(&local).unwrap();
    assert_eq!(installed.anchor.hash(), manifest.checkpoint.hash());
    assert_eq!(
        local.meta_store().get_meta(&sequence_key).unwrap(),
        14u64.to_be_bytes()
    );
    let advanced_epoch = installed::recover_metadata_retaining_progress(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
        &mut progress,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        !restored.shares_projection(&advanced_epoch),
        "new epochs must not reuse old tails or economics"
    );
    assert_eq!(
        advanced_epoch.recovered().continuation.anchor().hash(),
        manifest.checkpoint.hash()
    );
    local
        .meta_store()
        .put_meta(installed::BASE_KEY, b"changed base")
        .unwrap();
    assert!(
        installed::recover_metadata_retaining_progress(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
            &mut progress,
        )
        .await
        .is_err(),
        "retained progress must not bypass a changed recovery base"
    );
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn agreement_replay_cache_rechecks_installed_authority_and_metadata() {
    use crate::checkpoint_service::{compact_authority::recover_authority, Settings};
    let (fixture, _source_dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let old =
        authenticate(&source, &fixture.params, manifest.checkpoint.parent).into_continuation();
    let source = Arc::new(source);
    for change in 0..4 {
        let (_dir, local) = cemented::sparse(&old, &source);
        copy_installed_records(&local, &source, &fixture, &bytes);
        let local = Arc::new(local);
        let service = WitnessService::start(source.clone(), local.clone(), 0).await;
        let settings = Settings {
            store: local.clone(),
            network: NetworkId::Dev,
            initial_params: fixture.params.clone(),
            key: None,
        };
        let authority = recover_authority(&settings, service.registry.clone(), None, None)
            .await
            .unwrap()
            .unwrap();
        let cached = authority.replay_copy(&local).await.unwrap();
        drop(authority);
        let requests = service.requests.load(Ordering::SeqCst);
        match change {
            0 => local
                .meta_store()
                .put_meta("test/unrelated-revision", b"changed")
                .unwrap(),
            1 => local
                .meta_store()
                .put_meta(installed::BASE_KEY, b"corrupt")
                .unwrap(),
            2 => local
                .meta_store()
                .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, b"corrupt")
                .unwrap(),
            3 => local
                .meta_store()
                .put_meta(
                    burst_store::meta::PAYMENT_EPOCH_KEY,
                    certificate.checkpoint.hash().as_bytes(),
                )
                .unwrap(),
            _ => unreachable!(),
        }
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let result =
            recover_authority(&settings, service.registry.clone(), None, Some(cached)).await;
        if change == 0 {
            let checked = result.unwrap().unwrap();
            compact::assert_same(checked.base.continuation.state(), old.state());
            assert_eq!(service.requests.load(Ordering::SeqCst), requests);
        } else {
            assert!(
                result.is_err(),
                "cached agreement authority accepted changed input {change}"
            );
        }
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
        service.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_idle_signer_recovers_after_late_beacon_changes_its_read_set() {
    use crate::checkpoint_service::{self, Settings};
    use std::time::Duration;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let (fixture, _directory, store, _, _) = selection::setup();
    let mut selection = crate::checkpoint_beacon::selection_checkpoints(
        crate::checkpoint_store::chain::load(&store, NetworkId::Dev, &fixture.params).unwrap(),
    )
    .pop()
    .unwrap();
    for _ in 0..2 {
        crate::confirmed_replay::installation::PreparedInstallation::next(
            &store,
            NetworkId::Dev,
            &fixture.params,
        )
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    }
    let store = Arc::new(store);
    let service = WitnessService::start(store.clone(), store.clone(), 0).await;
    let mut settings = Settings {
        store: store.clone(),
        network: NetworkId::Dev,
        initial_params: fixture.params.clone(),
        key: None,
    };
    let authority = checkpoint_service::compact_authority::recover_authority(
        &settings,
        service.registry.clone(),
        None,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    let base = authority.base.clone();
    let anchor = base.continuation.anchor().clone();
    let seed = (0..9)
        .find(|seed| address(*seed) == *anchor.proposer(0))
        .unwrap();
    settings.key = Some(Arc::new(
        burst_crypto::keypair_from_seed(&[seed; 32]).private,
    ));
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    selection
        .attach_beacon(
            &store,
            &fixture.params,
            burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        base.verify_records_unchanged(&store.tx_begin_write().unwrap()),
        Err(burst_store::StoreError::StaleRead(_))
    ));
    let stop = Arc::new(crate::shutdown::ShutdownController::new());
    let (_sender, receiver) = mpsc::channel(checkpoint_service::INBOX_CAPACITY);
    let now = selection.replayed().request().beacon_time().unwrap() + 2;
    let fresh =
        checkpoint_service::recover_compact_authority(&settings, service.registry.clone(), None)
            .await
            .unwrap()
            .unwrap();
    assert!(
        fresh
            .discover_cemented_candidate_from_peers(
                store.clone(),
                service.registry.clone(),
                crate::checkpoint_replay::SigningClock {
                    now,
                    error_bound: 0
                },
            )
            .await
            .unwrap()
            .is_some(),
        "late-beacon fixture must have a signable lifecycle candidate"
    );
    let worker = tokio::spawn(
        checkpoint_service::compact_observer::run_compact_height_with_authority(
            settings,
            checkpoint_service::compact_observer::HeightAuthority::Installed(authority),
            service.registry.clone(),
            receiver,
            Arc::new(move || {
                Some(crate::checkpoint_replay::SigningClock {
                    now,
                    error_bound: 0,
                })
            }),
            stop.clone(),
            Duration::from_secs(30),
        ),
    );
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(state) =
                crate::checkpoint_agreement::read_state(&store, &anchor, &address(seed)).unwrap()
            {
                if state.proposal().is_some() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    stop.shutdown();
    worker.await.unwrap().unwrap();
    service.stop().await;
    result.expect("idle signer kept retrying stale observations after a valid late beacon");
    assert_eq!(store.meta_store().payment_epoch().unwrap(), anchor.hash());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn cached_recovery_matches_full_replay_and_rejects_changed_history() {
    let (fixture, _directory, store, _, certificate) = selection::setup();
    let epoch = certificate.checkpoint.hash();
    let expected = installed::RecoveredSnapshot::recover_genesis_locally(
        &store,
        NetworkId::Dev,
        &fixture.params,
        epoch,
    )
    .unwrap();
    let store = Arc::new(store);
    let service = WitnessService::start(store.clone(), store.clone(), 0).await;
    let recovered = installed::RecoveredSnapshot::genesis(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .advance_from_peers(store.clone(), service.registry.clone(), epoch)
        .await
        .unwrap();
    assert_eq!(recovered.continuation.anchor().hash(), epoch);
    assert_eq!(
        recovered.continuation.state().snapshot_bytes().unwrap(),
        expected.continuation.state().snapshot_bytes().unwrap()
    );
    let recovered = recovered
        .advance_from_peers(store.clone(), service.registry.clone(), epoch)
        .await
        .unwrap();
    let key = format!("checkpoint_chain/v1/dev/{}/1", fixture.blocks[0].hash);
    let original = store.meta_store().get_meta(&key).unwrap();
    store.meta_store().put_meta(&key, &[0]).unwrap();
    assert!(matches!(
        recovered
            .advance_from_peers(store.clone(), service.registry.clone(), epoch)
            .await,
        Err(burst_store::StoreError::StaleRead(_))
    ));
    assert_eq!(store.meta_store().get_meta(&key).unwrap(), [0]);
    store.meta_store().put_meta(&key, &original).unwrap();
    let archive = format!(
        "checkpoint_record_archive/v1/dev/{}/1",
        fixture.blocks[0].hash
    );
    {
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(&archive, &original).unwrap();
        batch.delete_meta(&key).unwrap();
        batch.commit().unwrap();
    }
    let expected_state = expected.continuation.state().snapshot_bytes().unwrap();
    let recovered = expected
        .advance_from_peers(store.clone(), service.registry.clone(), epoch)
        .await
        .unwrap();
    assert_eq!(
        recovered.continuation.state().snapshot_bytes().unwrap(),
        expected_state
    );
    assert_eq!(store.meta_store().get_meta(&archive).unwrap(), original);
    store.meta_store().put_meta(&archive, &[0]).unwrap();
    assert!(matches!(
        recovered
            .advance_from_peers(store.clone(), service.registry.clone(), epoch)
            .await,
        Err(burst_store::StoreError::StaleRead(_))
    ));
    assert_eq!(store.meta_store().get_meta(&archive).unwrap(), [0]);
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn genesis_record_transfer_fetches_missing_suffix() {
    genesis_transfer(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn genesis_record_transfer_replays_attached_suffix() {
    genesis_transfer(false).await;
}

async fn genesis_transfer(record_request: bool) {
    let (fixture, _source_dir, source, _, _) = selection::setup();
    let directory = tempfile::tempdir().unwrap();
    let local = Arc::new(LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
    let genesis = &fixture.blocks[0];
    {
        let mut batch = local.tx_begin_write().unwrap();
        batch
            .put_block_with_account(
                &genesis.hash,
                &bincode::serialize(genesis).unwrap(),
                &genesis.account,
                1,
            )
            .unwrap();
        let mut account =
            crate::genesis_state::account_to_write(None, genesis.account.clone(), genesis.hash)
                .unwrap();
        account.confirmation_height = 1;
        batch.put_account_info(&account, false).unwrap();
        crate::payment_epoch::initialize(&mut batch, genesis.hash).unwrap();
        batch.commit().unwrap();
    }
    crate::payment_authority::initialize(&local, NetworkId::Dev, &fixture.params).unwrap();
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let source = Arc::new(source);
    let service = WitnessService::start(source.clone(), local.clone(), 0).await;
    let record_only = CertifiedCheckpoint::export_transfer_for_request(
        &source,
        NetworkId::Dev,
        &fixture.params,
        1,
        true,
    )
    .unwrap()
    .unwrap();
    // Supplied legacy bodies must be replayed without first persisting them.
    let full = CertifiedCheckpoint::export_transfer(&source, NetworkId::Dev, &fixture.params, 1)
        .unwrap()
        .unwrap();
    for mutation in 0..3 {
        let (version, record, mut blocks, contents): (
            u8,
            Vec<u8>,
            Vec<StateBlock>,
            Vec<crate::confirmed_history::ProposalWitness>,
        ) = bincode::deserialize(&full).unwrap();
        match mutation {
            0 => blocks.clear(),
            1 => blocks.push(blocks[1].clone()),
            _ => blocks[1].signature = Signature([0; 64]),
        }
        let altered = bincode::serialize(&(version, record, blocks, contents)).unwrap();
        assert!(
            crate::checkpoint_store::historical::snapshot_transfer::import(
                local.clone(),
                NetworkId::Dev,
                fixture.params.clone(),
                1,
                altered,
                service.registry.clone(),
            )
            .await
            .is_err()
        );
        assert_eq!(local.block_store().block_count().unwrap(), 1);
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
    }
    crate::checkpoint_store::historical::snapshot_transfer::import(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        1,
        if record_request {
            record_only.clone()
        } else {
            full
        },
        service.registry.clone(),
    )
    .await
    .unwrap();
    assert_eq!(local.meta_store().payment_epoch().unwrap(), genesis.hash);
    assert_eq!(local.block_store().block_count().unwrap(), 1);
    crate::checkpoint_store::historical::snapshot_transfer::import(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        1,
        record_only,
        service.registry.clone(),
    )
    .await
    .unwrap();
    crate::confirmed_replay::installation::PreparedInstallation::next(
        &local,
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap()
    .unwrap()
    .commit(&local)
    .unwrap();
    assert_ne!(local.meta_store().payment_epoch().unwrap(), genesis.hash);
    assert!(local.meta_store().get_meta(installed::BASE_KEY).is_err());
    let requests = service.stop().await;
    if record_request {
        assert!(
            requests > 0,
            "cold start must retrieve missing suffix from peer"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn archived_selection_accepts_a_late_beacon_and_publishes_after_reopen() {
    let (fixture, directory, store, _, certificate) = selection::setup();
    let mut selection = crate::checkpoint_beacon::selection_checkpoints(
        crate::checkpoint_store::chain::load(&store, NetworkId::Dev, &fixture.params).unwrap(),
    )
    .pop()
    .unwrap();
    let mut waiting = None;
    for _ in 0..2 {
        waiting = Some(
            crate::confirmed_replay::installation::PreparedInstallation::next(
                &store,
                NetworkId::Dev,
                &fixture.params,
            )
            .unwrap()
            .unwrap()
            .commit(&store)
            .unwrap()
            .verification,
        );
    }
    let active = format!("checkpoint_chain/v1/dev/{}/2", fixture.blocks[0].hash);
    let archived = format!(
        "checkpoint_record_archive/v1/dev/{}/2",
        fixture.blocks[0].hash
    );
    let original = store.meta_store().get_meta(&active).unwrap();
    {
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(&archived, &original).unwrap();
        batch.delete_meta(&active).unwrap();
        batch.commit().unwrap();
    }
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
    let mut wrong = beacon.clone();
    wrong.value[0] ^= 1;
    assert!(selection
        .attach_beacon(&store, &fixture.params, wrong.clone())
        .is_err());
    let proof_key =
        crate::checkpoint_store::archive_selection::beacon_key(certificate.checkpoint.hash());
    assert!(store.meta_store().get_meta(&proof_key).is_err());
    let store = Arc::new(store);
    let service = WitnessService::start(store.clone(), store.clone(), 0).await;
    let before = installed::RecoveredSnapshot::recover_genesis_from_peers(
        store.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
        None,
    )
    .await
    .unwrap();
    assert!(before
        .selections
        .read_selection(before.selections.len() - 1)
        .unwrap()
        .beacon()
        .is_none());
    let (version, transferred_certificate, heads, target, _): (
        u8,
        burst_consensus::checkpoint::CheckpointCertificate,
        Vec<crate::confirmed_history::HistoryHead>,
        WalletAddress,
        Option<burst_vrf::RandomOutput>,
    ) = bincode::deserialize(&original).unwrap();
    for valid in [false, true, true] {
        let record = bincode::serialize(&(
            version,
            &transferred_certificate,
            &heads,
            &target,
            Some(if valid { &beacon } else { &wrong }),
        ))
        .unwrap();
        let transfer = bincode::serialize(&(
            3u8,
            record,
            Vec::<burst_ledger::StateBlock>::new(),
            Vec::<crate::confirmed_history::ProposalWitness>::new(),
        ))
        .unwrap();
        let result = crate::checkpoint_store::historical::snapshot_transfer::import(
            store.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            2,
            transfer,
            service.registry.clone(),
        )
        .await;
        assert_eq!(result.is_ok(), valid, "{result:?}");
        if !valid {
            assert!(store.meta_store().get_meta(&proof_key).is_err());
        }
        assert_eq!(store.meta_store().get_meta(&archived).unwrap(), original);
    }
    selection
        .attach_beacon(&store, &fixture.params, beacon.clone())
        .unwrap();
    selection
        .attach_beacon(&store, &fixture.params, beacon.clone())
        .unwrap();
    assert_eq!(store.meta_store().get_meta(&archived).unwrap(), original);
    let exported = CertifiedCheckpoint::export_record(&store, NetworkId::Dev, &fixture.params, 2)
        .unwrap()
        .unwrap();
    let mut export_recovery = crate::checkpoint_store::ExportRecovery::default();
    for _ in 0..2 {
        assert_eq!(
            CertifiedCheckpoint::export_record_with_recovery(
                &store,
                NetworkId::Dev,
                &fixture.params,
                2,
                &mut export_recovery,
            )
            .unwrap()
            .unwrap(),
            exported
        );
    }
    let (_, exported_certificate, _, _, exported_beacon): (
        u8,
        burst_consensus::checkpoint::CheckpointCertificate,
        Vec<crate::confirmed_history::HistoryHead>,
        WalletAddress,
        Option<burst_vrf::RandomOutput>,
    ) = bincode::deserialize(&exported).unwrap();
    assert_eq!(exported_certificate, certificate);
    assert_eq!(
        bincode::serialize(&exported_beacon).unwrap(),
        bincode::serialize(&Some(beacon.clone())).unwrap()
    );
    assert!(before
        .verify_records_unchanged(&store.tx_begin_write().unwrap())
        .is_err());
    service.stop().await;
    drop(store);

    let store = Arc::new(LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
    let service = WitnessService::start(store.clone(), store.clone(), 0).await;
    let recovered = installed::RecoveredSnapshot::recover_genesis_from_peers(
        store.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
        None,
    )
    .await
    .unwrap();
    let selected = recovered
        .selections
        .read_selection(recovered.selections.len() - 1)
        .unwrap()
        .into_owned();
    assert_eq!(selected.beacon().unwrap().round, beacon.round);
    selected
        .require_persisted(&store.tx_begin_write().unwrap())
        .unwrap();
    let context = selected
        .replayed()
        .request()
        .vote_context(&certificate.checkpoint)
        .unwrap();
    let live = Arc::new(tokio::sync::Mutex::new(waiting.unwrap()));
    crate::checkpoint_selection::install(
        store.clone(),
        live.clone(),
        selected,
        fixture.params.clone(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        live.lock().await.selection_context(&address(20)),
        Some(context)
    );
    assert!(store.meta_store().get_meta(installed::BASE_KEY).is_err());
    let earlier_base = archive(&store, &fixture, certificate.checkpoint.parent);
    store
        .meta_store()
        .put_meta(installed::BASE_KEY, &earlier_base)
        .unwrap();
    assert_eq!(
        CertifiedCheckpoint::export_record_with_recovery(
            &store,
            NetworkId::Dev,
            &fixture.params,
            2,
            &mut export_recovery,
        )
        .unwrap()
        .unwrap(),
        exported
    );
    assert_eq!(
        export_recovery.height(),
        None,
        "snapshot handoff retires genesis export progress"
    );
    assert_eq!(
        CertifiedCheckpoint::export_record(&store, NetworkId::Dev, &fixture.params, 2)
            .unwrap()
            .unwrap(),
        exported
    );
    store
        .meta_store()
        .put_meta(&proof_key, &bincode::serialize(&wrong).unwrap())
        .unwrap();
    assert!(installed::RecoveredSnapshot::recover_genesis_from_peers(
        store.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
        None,
    )
    .await
    .is_err());
    assert!(
        CertifiedCheckpoint::export_record(&store, NetworkId::Dev, &fixture.params, 2).is_err()
    );
    assert_eq!(store.meta_store().get_meta(&archived).unwrap(), original);
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn incremental_installed_recovery_and_successor_do_not_require_snapshot_creation() {
    // End the live-import task before reopening the same store. Keeping both
    // phases in one async body reserves their combined poll temporaries.
    let (fixture, _source_dir, directory, source, certificate) = tokio::spawn(async move {
        let (fixture, _source_dir, source, manifest, certificate) = selection::setup();
        let (directory, local, _) = fixture.store();
        for height in 1..=1 {
            let transfer = CertifiedCheckpoint::export_transfer(
                &source,
                NetworkId::Dev,
                &fixture.params,
                height,
            )
            .unwrap()
            .unwrap();
            CertifiedCheckpoint::import_transfer(
                &local,
                NetworkId::Dev,
                &fixture.params,
                height,
                &transfer,
                &crate::checkpoint_store::historical::HistoricalAdmissions::default(),
            )
            .unwrap();
        }
        crate::confirmed_replay::installation::PreparedInstallation::next(
            &local,
            NetworkId::Dev,
            &fixture.params,
        )
        .unwrap()
        .unwrap()
        .commit(&local)
        .unwrap();
        let local = Arc::new(local);
        let source = Arc::new(source);
        let service = WitnessService::start(source.clone(), local.clone(), 0).await;
        assert!(crate::checkpoint_store::uses_incremental_replay(
            &local,
            NetworkId::Dev,
            &fixture.params,
        )
        .unwrap());
        let transfer =
            CertifiedCheckpoint::export_transfer(&source, NetworkId::Dev, &fixture.params, 2)
                .unwrap()
                .unwrap();
        let startup = installed::recover_current_tip_with_cache(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
            None,
        )
        .await
        .unwrap();
        let requests = service.requests.load(Ordering::SeqCst);
        let forked = startup.fork(local.env().path()).unwrap();
        drop(startup);
        let mut authority = crate::checkpoint_sync::authority::authenticate_with_cache(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
            Some(forked),
        )
        .await
        .unwrap();
        assert_eq!(
            service.requests.load(Ordering::SeqCst),
            requests,
            "sync startup must reuse authenticated ancestry while rechecking its observations"
        );
        let cache = authority
            .take_recovery()
            .expect("sync startup retains authenticated replay");
        assert_eq!(
            cache.recovered.continuation.anchor().hash(),
            manifest.checkpoint.parent
        );
        assert!(
            authority.take_recovery().is_none(),
            "replay ownership transfers only once"
        );
        let mut imports =
            crate::checkpoint_store::historical::snapshot_transfer::Session::from_recovery(Some(
                cache,
            ));
        let before_rejection = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let mut forged_transfer = transfer.clone();
        let signature = &certificate.signatures[0].signature.0;
        let positions: Vec<_> = forged_transfer
            .windows(signature.len())
            .enumerate()
            .filter_map(|(offset, bytes)| (bytes == signature).then_some(offset))
            .collect();
        assert_eq!(
            positions.len(),
            1,
            "locate the certificate signature, not arbitrary payload bytes"
        );
        forged_transfer[positions[0]] ^= 1;
        assert!(imports
            .import(
                local.clone(),
                NetworkId::Dev,
                fixture.params.clone(),
                2,
                forged_transfer,
                service.registry.clone(),
            )
            .await
            .is_err());
        assert_eq!(
            imports.recovery_anchor(),
            Some(manifest.checkpoint.parent),
            "failed acceptance must retain its authenticated parent for retry"
        );
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before_rejection,
            "retaining replay progress must not publish a rejected certificate"
        );
        for _ in 0..2 {
            imports
                .import(
                    local.clone(),
                    NetworkId::Dev,
                    fixture.params.clone(),
                    2,
                    transfer.clone(),
                    service.registry.clone(),
                )
                .await
                .unwrap();
        }
        // The first import retained the parent; refreshing the newly accepted
        // selection must advance that replay once and retain it for later refreshes.
        let before_refresh = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let disconnected = Arc::new(RwLock::new(ConnectionRegistry::new()));
        assert!(imports
            .import(
                local.clone(),
                NetworkId::Dev,
                fixture.params.clone(),
                2,
                transfer.clone(),
                disconnected,
            )
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before_refresh
        );
        let recovered = installed::recover_metadata_from_peers(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            recovered.recovered().continuation.anchor().hash(),
            manifest.checkpoint.parent
        );
        let next = recovered
            .prepare_next_from_peers(local.clone(), service.registry.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next.anchor().hash(), certificate.checkpoint.hash());
        next.commit(&local).unwrap();
        assert!(local.meta_store().get_meta(installed::BASE_KEY).is_err());
        service.stop().await;
        drop(local);
        (fixture, _source_dir, directory, source, certificate)
    })
    .await
    .expect("incremental recovery live-import task failed");

    tokio::spawn(async move {
        // Keep original certificate bytes but retire the active prefix without
        // creating a snapshot. Startup must authenticate them from genesis.
        {
            let local = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
            let mut batch = local.tx_begin_write().unwrap();
            for height in 1..=2 {
                let key = format!(
                    "checkpoint_chain/v1/dev/{}/{height}",
                    fixture.blocks[0].hash
                );
                let archived = format!(
                    "checkpoint_record_archive/v1/dev/{}/{height}",
                    fixture.blocks[0].hash
                );
                let bytes = batch.get_meta(&key).unwrap().unwrap();
                batch.put_meta(&archived, &bytes).unwrap();
                batch.delete_meta(&key).unwrap();
            }
            batch.commit().unwrap();
            local.force_sync().unwrap();
        }
        let local = Arc::new(LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
        let service = WitnessService::start(source.clone(), local.clone(), 0).await;
        let recovered = installed::recover_metadata_from_peers(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            recovered.recovered().continuation.anchor().hash(),
            certificate.checkpoint.hash()
        );
        let expected =
            authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_state();
        compact::assert_same(recovered.recovered().continuation.state(), &expected);
        let archived = format!(
            "checkpoint_record_archive/v1/dev/{}/1",
            fixture.blocks[0].hash
        );
        let original = {
            let mut batch = local.tx_begin_write().unwrap();
            recovered.verify_unchanged(&batch).unwrap();
            let original = batch.get_meta(&archived).unwrap().unwrap();
            batch
                .put_meta(&archived, b"changed archived certificate")
                .unwrap();
            assert!(recovered.verify_unchanged(&batch).is_err());
            batch.put_meta(&archived, &original).unwrap();
            recovered.verify_unchanged(&batch).unwrap();
            let active = format!("checkpoint_chain/v1/dev/{}/1", fixture.blocks[0].hash);
            batch.put_meta(&active, &original).unwrap();
            assert!(recovered.verify_unchanged(&batch).is_err());
            batch.delete_meta(&active).unwrap();
            recovered.verify_unchanged(&batch).unwrap();
            batch
                .put_meta(installed::BASE_KEY, b"changed recovery source")
                .unwrap();
            assert!(recovered.verify_unchanged(&batch).is_err());
            original
        };
        drop(recovered);
        assert!(local.meta_store().get_meta(installed::BASE_KEY).is_err());
        crate::checkpoint_sync::authority::authenticate(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
        )
        .await
        .unwrap();
        local
            .meta_store()
            .put_meta(&archived, b"damaged retained history")
            .unwrap();
        assert!(crate::checkpoint_sync::authority::authenticate(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
        )
        .await
        .is_err());
        local.meta_store().put_meta(&archived, &original).unwrap();
        crate::checkpoint_sync::authority::authenticate(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
        )
        .await
        .unwrap();
        service.stop().await;
    })
    .await
    .expect("incremental recovery retired-history task failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_runtime_starts_from_sparse_state_and_relays_the_previous_decision() {
    assert_previous_decision(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_runtime_switches_from_full_history_without_restarting_service() {
    assert_previous_decision(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_runtime_rejects_corrupt_live_base_without_returning_to_full_history() {
    assert_previous_decision(true, true).await;
}

async fn assert_previous_decision(start_with_full_history: bool, corrupt: bool) {
    use crate::checkpoint_store::chain;
    use crate::{
        checkpoint_service,
        checkpoint_transport::{Incoming, Transport},
        shutdown::ShutdownController,
        wire_message::WireMessage,
    };
    use tokio::{
        io::AsyncReadExt,
        net::{TcpListener, TcpStream},
    };
    let (fixture, _source_dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let old =
        authenticate(&source, &fixture.params, manifest.checkpoint.parent).into_continuation();
    let next =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    let (_dir, local) = if start_with_full_history {
        let (dir, local, _) = fixture.store();
        for height in 1..=2 {
            let transfer = CertifiedCheckpoint::export_transfer(
                &source,
                NetworkId::Dev,
                &fixture.params,
                height,
            )
            .unwrap()
            .unwrap();
            CertifiedCheckpoint::import_transfer(
                &local,
                NetworkId::Dev,
                &fixture.params,
                height,
                &transfer,
                &crate::checkpoint_store::historical::HistoricalAdmissions::default(),
            )
            .unwrap();
            crate::confirmed_replay::installation::PreparedInstallation::next(
                &local,
                NetworkId::Dev,
                &fixture.params,
            )
            .unwrap()
            .unwrap()
            .commit(&local)
            .unwrap();
        }
        assert!(chain::load(&local, NetworkId::Dev, &fixture.params).is_ok());
        (dir, local)
    } else {
        let (dir, local) = cemented::sparse(&next, &source);
        copy_installed_records(&local, &source, &fixture, &bytes);
        assert!(chain::load(&local, NetworkId::Dev, &fixture.params).is_err());
        (dir, local)
    };
    let local = Arc::new(local);
    let service = WitnessService::start(Arc::new(source), local.clone(), 0).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (mut client, server) = tokio::try_join!(
        TcpStream::connect(listener.local_addr().unwrap()),
        listener.accept()
    )
    .unwrap();
    let (_reader, writer) = server.0.into_split();
    service
        .registry
        .write()
        .await
        .insert("catchup".into(), writer);
    let (sender, receiver) = mpsc::channel(checkpoint_service::INBOX_CAPACITY);
    let shutdown = Arc::new(ShutdownController::new());
    let worker = tokio::spawn(checkpoint_service::run_node(
        checkpoint_service::Settings {
            store: local.clone(),
            network: NetworkId::Dev,
            initial_params: fixture.params.clone(),
            key: None,
        },
        service.registry.clone(),
        receiver,
        Arc::new(|| None),
        shutdown.clone(),
    ));
    if start_with_full_history {
        // A frame before archive publication proves the genesis-backed service has
        // started. Its existing receiver and peer connection must survive.
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let size = client.read_u32().await.unwrap();
            assert!(size as usize <= burst_types::MAX_P2P_MESSAGE_BYTES);
            let mut frame = vec![0; size as usize];
            client.read_exact(&mut frame).await.unwrap();
            assert!(matches!(
                WireMessage::decode(&frame).unwrap(),
                WireMessage::CheckpointAgreement(_)
            ));
        })
        .await
        .unwrap();
        let mut batch = local.tx_begin_write().unwrap();
        batch
            .put_meta(
                installed::BASE_KEY,
                if corrupt { b"invalid archive" } else { &bytes },
            )
            .unwrap();
        batch.commit().unwrap();
        local
            .meta_store()
            .delete_meta(&format!(
                "checkpoint_chain/v1/dev/{}/1",
                old.anchor().genesis_hash()
            ))
            .unwrap();
        assert!(chain::load(&local, NetworkId::Dev, &fixture.params).is_err());
    }
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    if corrupt {
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), worker)
            .await
            .unwrap()
            .unwrap();
        assert!(
            result.is_err(),
            "corrupt base must not resume a full-history signer"
        );
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
        shutdown.shutdown();
        service.registry.write().await.remove("catchup");
        service.stop().await;
        return;
    }
    let mut transport = Transport::new(
        crate::checkpoint_transport::context_from_anchor(old.anchor()).unwrap(),
        std::time::Instant::now(),
    );
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        if start_with_full_history {
            while service.requests.load(Ordering::SeqCst) == 0 {
                assert!(
                    !worker.is_finished(),
                    "compact handoff stopped before proof recovery"
                );
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
        loop {
            let size = client.read_u32().await.unwrap();
            assert!(size as usize <= burst_types::MAX_P2P_MESSAGE_BYTES);
            let mut bytes = vec![0; size as usize];
            client.read_exact(&mut bytes).await.unwrap();
            let WireMessage::CheckpointAgreement(packet) = WireMessage::decode(&bytes).unwrap()
            else {
                panic!("unexpected previous-decision frame")
            };
            let (reply, received) = transport
                .handle("runtime", packet, std::time::Instant::now())
                .unwrap();
            let mut sends = reply.into_iter().collect::<Vec<_>>();
            sends.extend(transport.poll(&["runtime".into()], std::time::Instant::now()));
            for send in sends {
                sender
                    .send(Incoming {
                        peer: "catchup".into(),
                        packet: send.packet,
                    })
                    .await
                    .unwrap();
            }
            if let Some(received) = received {
                let crate::checkpoint_driver::Message::Decision {
                    certificate: actual,
                    manifest: replay,
                } = received.message
                else {
                    panic!("previous relay supplied a non-decision")
                };
                assert_eq!(actual, certificate);
                assert_eq!(
                    replay,
                    crate::checkpoint_agreement::ReplayManifest::Selection(manifest.clone())
                );
                break;
            }
        }
    })
    .await;
    shutdown.shutdown();
    tokio::time::timeout(std::time::Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    service.registry.write().await.remove("catchup");
    let requests = service.stop().await;
    if start_with_full_history {
        assert!(requests > 0);
    }
    // A sparse startup may relay its authenticated local decision directly.
    result.expect("compact previous decision was not relayed");
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_runtime_rotates_only_after_checked_economic_installation() {
    use crate::checkpoint_service::{compact_runtime::CompactRuntime, Settings};
    // Separate installation from replay checks: the combined fixture poll frame
    // otherwise nests 775KiB of temporaries above recovery on the test thread.
    let (
        fixture,
        directories,
        local,
        service,
        old,
        next,
        mut runtime,
        mut behind,
        cache,
        handoff,
        shared,
        initial,
    ) = tokio::spawn(async move {
        let (fixture, _source_dir, source, manifest, certificate) = selection::setup();
        let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
        let old =
            authenticate(&source, &fixture.params, manifest.checkpoint.parent).into_continuation();
        let next = authenticate(&source, &fixture.params, certificate.checkpoint.hash())
            .into_continuation();
        let (_dir, local) = cemented::sparse(&old, &source);
        copy_installed_records(&local, &source, &fixture, &bytes);
        let local = Arc::new(local);
        let service = WitnessService::start(Arc::new(source), local.clone(), 0).await;
        let startup = installed::recover_metadata_from_peers(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
        )
        .await
        .unwrap()
        .unwrap()
        .into_checkpoint_recovery(&local, None)
        .unwrap();
        let behind = Some(startup.fork(local.env().path()).unwrap());
        let requests = service.requests.load(Ordering::SeqCst);
        let mut runtime = CompactRuntime::new(
            Settings {
                store: local.clone(),
                network: NetworkId::Dev,
                initial_params: fixture.params.clone(),
                key: None,
            },
            service.registry.clone(),
            Arc::new(|| None),
            Some(startup),
        )
        .await
        .unwrap();
        assert_eq!(
            service.requests.load(Ordering::SeqCst),
            requests,
            "agreement startup must reuse authenticated history"
        );
        assert_eq!(runtime.anchor().hash(), old.anchor().hash());
        assert!(runtime.previous_context().is_none());
        runtime.refresh().await.unwrap();
        assert_eq!(runtime.anchor().hash(), old.anchor().hash());

        local
            .meta_store()
            .put_meta(
                burst_store::meta::PAYMENT_EPOCH_KEY,
                next.anchor().hash().as_bytes(),
            )
            .unwrap();
        assert!(runtime.refresh().await.is_err());
        assert_eq!(runtime.anchor().hash(), old.anchor().hash());
        assert!(runtime.previous_context().is_none());

        local
            .meta_store()
            .put_meta(
                burst_store::meta::PAYMENT_EPOCH_KEY,
                old.anchor().hash().as_bytes(),
            )
            .unwrap();
        let (_, cache, handoff) = installed::recover_metadata_from_peers(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
        )
        .await
        .unwrap()
        .unwrap()
        .prepare_next_from_peers(local.clone(), service.registry.clone())
        .await
        .unwrap()
        .unwrap()
        .commit_with_recovery(&local)
        .unwrap();
        assert_eq!(
            cache.recovered.continuation.anchor().hash(),
            next.anchor().hash()
        );
        compact::assert_same(cache.recovered.continuation.state(), next.state());
        let shared = service.registry.read().await.recovery_projection.clone();
        handoff.clone().publish(&shared, &local).unwrap();
        let initial = fixture.params.params_hash();
        assert!(shared
            .replay(
                &local,
                NetworkId::Dev,
                initial,
                cache.base,
                old.anchor().hash()
            )
            .unwrap()
            .is_none());
        assert!(shared
            .replay(
                &local,
                NetworkId::Dev,
                BlockHash::new([91; 32]),
                cache.base,
                next.anchor().hash()
            )
            .unwrap()
            .is_none());
        assert!(shared
            .replay(
                &local,
                NetworkId::Test,
                initial,
                cache.base,
                next.anchor().hash()
            )
            .unwrap()
            .is_none());
        assert!(cache.base.is_some());
        assert!(shared
            .replay(&local, NetworkId::Dev, initial, None, next.anchor().hash())
            .unwrap()
            .is_none());
        let other_directory = tempfile::tempdir().unwrap();
        let other = Arc::new(LmdbStore::open(other_directory.path(), 32, 32 << 20).unwrap());
        assert!(shared
            .replay(
                &other,
                NetworkId::Dev,
                initial,
                cache.base,
                next.anchor().hash()
            )
            .unwrap()
            .is_none());
        assert!(shared
            .selection_refresh(&other, NetworkId::Dev, initial, None)
            .unwrap()
            .is_none());
        assert!(shared
            .selection_refresh(&local, NetworkId::Test, initial, None)
            .unwrap()
            .is_none());
        assert!(shared
            .selection_refresh(&local, NetworkId::Dev, BlockHash::new([99; 32]), None)
            .unwrap()
            .is_none());
        let (hint_epoch, _) = shared
            .selection_refresh(&local, NetworkId::Dev, initial, None)
            .unwrap()
            .expect("committed replay supplies selection hints");
        assert_eq!(hint_epoch, next.anchor().hash());
        assert!(shared
            .selection_refresh(&local, NetworkId::Dev, initial, Some(hint_epoch))
            .unwrap()
            .is_none());
        (
            fixture,
            (_source_dir, _dir),
            local,
            service,
            old,
            next,
            runtime,
            behind,
            cache,
            handoff,
            shared,
            initial,
        )
    })
    .await
    .unwrap();
    tokio::spawn(async move {
        let reused = shared
            .replay(
                &local,
                NetworkId::Dev,
                initial,
                cache.base,
                next.anchor().hash(),
            )
            .unwrap()
            .expect("committed installation retains its exact replay parent");
        let reused = reused
            .recover(
                cache.base,
                local.clone(),
                service.registry.clone(),
                next.anchor().hash(),
            )
            .await
            .unwrap()
            .unwrap();
        compact::assert_same(reused.continuation.state(), next.state());
        let handoffs = shared.replay_handoffs();
        let checked_handoff = installed::recover_metadata_from_peers(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            shared.replay_handoffs(),
            handoffs + 1,
            "metadata without caller progress must consume the installed replay"
        );
        assert!(
            checked_handoff.uses_committed_projection(&handoff),
            "first recovery after commit must reuse the writer-captured projection"
        );
        checked_handoff
            .verify_unchanged(&local.tx_begin_write().unwrap())
            .unwrap();
        let handoffs = shared.replay_handoffs();
        let advanced = installed::recover_metadata_retaining_progress(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
            &mut behind,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            shared.replay_handoffs(),
            handoffs + 1,
            "installed replay must replace an older worker's progress"
        );
        compact::assert_same(advanced.recovered().continuation.state(), next.state());
        advanced
            .verify_unchanged(&local.tx_begin_write().unwrap())
            .unwrap();
        let handoffs = shared.replay_handoffs();
        installed::recover_metadata_retaining_progress(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
            &mut behind,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            shared.replay_handoffs(),
            handoffs,
            "already-current caller progress must not fork the shared replay again"
        );
        assert!(
            shared
                .replay(
                    &local,
                    NetworkId::Dev,
                    initial,
                    cache.base,
                    next.anchor().hash()
                )
                .unwrap()
                .is_some(),
            "ordinary projection refresh must preserve the same committed replay"
        );
        let replay_key = format!(
            "checkpoint_chain/v1/dev/{}/{}",
            next.anchor().genesis_hash(),
            next.anchor().height()
        );
        let replay_bytes = local.meta_store().get_meta(&replay_key).unwrap();
        let stale = shared
            .replay(
                &local,
                NetworkId::Dev,
                initial,
                cache.base,
                next.anchor().hash(),
            )
            .unwrap()
            .unwrap();
        local
            .meta_store()
            .put_meta(&replay_key, b"changed after replay handoff")
            .unwrap();
        assert!(
            stale
                .recover(
                    cache.base,
                    local.clone(),
                    service.registry.clone(),
                    next.anchor().hash()
                )
                .await
                .unwrap()
                .is_none(),
            "shared replay must reject changed durable history"
        );
        assert!(
            installed::recover_metadata_from_peers(
                local.clone(),
                NetworkId::Dev,
                fixture.params.clone(),
                service.registry.clone(),
            )
            .await
            .is_err(),
            "metadata handoff must not hide changed durable history"
        );
        local
            .meta_store()
            .put_meta(&replay_key, &replay_bytes)
            .unwrap();
        let (snapshot_key, snapshot) = verification_tamper_row(&local);
        local
            .meta_store()
            .put_meta(&snapshot_key, b"changed after committed handoff")
            .unwrap();
        assert!(
            installed::recover_metadata_from_peers(
                local.clone(),
                NetworkId::Dev,
                fixture.params.clone(),
                service.registry.clone(),
            )
            .await
            .is_err(),
            "committed handoff must not conceal a later economic mutation"
        );
        local
            .meta_store()
            .put_meta(&snapshot_key, &snapshot)
            .unwrap();
        let restored_handoff = installed::recover_metadata_from_peers(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
        )
        .await
        .unwrap()
        .unwrap();
        // A pinned-reader miss must not evict a newer shared projection. Once the
        // exact contents are restored, reuse is valid only after the normal guards.
        assert!(restored_handoff.uses_committed_projection(&handoff));
        restored_handoff
            .verify_unchanged(&local.tx_begin_write().unwrap())
            .unwrap();
        compact::assert_same(
            restored_handoff.recovered().continuation.state(),
            next.state(),
        );
        let commitment = cache.base.clone();
        let recovered = cache
            .recover(
                commitment,
                local.clone(),
                service.registry.clone(),
                next.anchor().hash(),
            )
            .await
            .unwrap()
            .unwrap();
        compact::assert_same(recovered.continuation.state(), next.state());
        let record_key = format!(
            "checkpoint_chain/v1/dev/{}/{}",
            next.anchor().genesis_hash(),
            next.anchor().height()
        );
        let record = local.meta_store().get_meta(&record_key).unwrap();
        local.meta_store().put_meta(&record_key, &[0]).unwrap();
        assert!(matches!(
            recovered
                .advance_from_peers(
                    local.clone(),
                    service.registry.clone(),
                    next.anchor().hash()
                )
                .await,
            Err(burst_store::StoreError::StaleRead(_))
        ));
        local.meta_store().put_meta(&record_key, &record).unwrap();
        let installed = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        runtime.refresh().await.unwrap();
        assert_eq!(runtime.anchor().hash(), next.anchor().hash());
        assert_eq!(
            runtime.previous_context().unwrap(),
            crate::checkpoint_transport::context_from_anchor(old.anchor()).unwrap()
        );
        runtime.refresh().await.unwrap();
        runtime.stop().await.unwrap();
        drop(runtime);
        assert!(service.stop().await > 0);
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            installed
        );
        drop(directories);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_worker_authority_uses_installed_descendants_and_rechecks_handoff() {
    static CHANGE_EPOCH: std::sync::Mutex<Option<Arc<LmdbStore>>> = std::sync::Mutex::new(None);
    let (fixture, _source_dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let old =
        authenticate(&source, &fixture.params, manifest.checkpoint.parent).into_continuation();
    let next =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    let source = Arc::new(source);
    for case in 0..10 {
        let expected = if matches!(case, 0 | 2 | 4 | 6) {
            &old
        } else {
            &next
        };
        let (_dir, local) = cemented::sparse(expected, &source);
        if case < 8 {
            copy_installed_records(&local, &source, &fixture, &bytes);
        }
        if case == 6 {
            local
                .meta_store()
                .put_meta(
                    burst_store::meta::PAYMENT_EPOCH_KEY,
                    next.anchor().hash().as_bytes(),
                )
                .unwrap();
        }
        let previous = match case {
            0 | 8 => None,
            3 | 4 => Some(next.anchor().clone()),
            5 => Some(
                crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
                    NetworkId::Test,
                    &fixture.params,
                )
                .unwrap(),
            ),
            _ => Some(old.anchor().clone()),
        };
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let local = Arc::new(local);
        let service = WitnessService::start(source.clone(), local.clone(), 0).await;
        if case == 7 {
            // Captured recovery may satisfy witnesses locally. Inject at the
            // witness boundary itself so the mutation cannot silently be skipped.
            *CHANGE_EPOCH.lock().unwrap() = Some(local.clone());
            service.registry.write().await.recovery_probe = Some(|stage| {
                if stage == "private-recovery:confirmed-witnesses" {
                    if let Some(store) = CHANGE_EPOCH.lock().unwrap().take() {
                        store
                            .meta_store()
                            .put_meta(
                                burst_store::meta::PAYMENT_EPOCH_KEY,
                                BlockHash::new([77; 32]).as_bytes(),
                            )
                            .unwrap();
                    }
                }
            });
        }
        let result = crate::checkpoint_service::recover_compact_authority(
            &crate::checkpoint_service::Settings {
                store: local.clone(),
                network: NetworkId::Dev,
                initial_params: fixture.params.clone(),
                key: None,
            },
            service.registry.clone(),
            previous,
        )
        .await;
        service.stop().await;
        match case {
            0 | 1 => {
                let recovered = result.unwrap().unwrap();
                assert_eq!(
                    recovered.continuation.anchor().hash(),
                    expected.anchor().hash()
                );
                compact::assert_same(recovered.continuation.state(), expected.state());
            }
            // A non-genesis installed epoch with neither an archive nor its
            // certified records is corrupt, not an uninitialized full node.
            _ => assert!(result.is_err(), "handoff case {case}"),
        }
        let after = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let unchanged = |rows: Vec<(Vec<u8>, Vec<u8>)>| {
            rows.into_iter()
                .filter(|(key, _)| {
                    case != 7 || key.as_slice() != burst_store::meta::PAYMENT_EPOCH_KEY.as_bytes()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(unchanged(before), unchanged(after), "handoff case {case}");
        if case == 7 {
            assert!(
                CHANGE_EPOCH.lock().unwrap().is_none(),
                "epoch change was not exercised"
            );
        }
    }
}
