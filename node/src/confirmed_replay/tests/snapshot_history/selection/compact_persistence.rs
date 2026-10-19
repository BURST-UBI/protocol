use super::*;

#[test]
fn archived_refresh_pages_sparse_heights_and_restarts_after_cursor_deletion() {
    let (fixture, _dir, source, manifest, _) = setup();
    let active = format!(
        "checkpoint_chain/v1/dev/{}/{}",
        manifest.checkpoint.genesis, manifest.checkpoint.height
    );
    let mut record: (
        u8,
        CheckpointCertificate,
        Vec<HistoryHead>,
        WalletAddress,
        Option<burst_vrf::RandomOutput>,
    ) = bincode::deserialize(&source.meta_store().get_meta(&active).unwrap()).unwrap();
    let prefix = format!(
        "checkpoint_record_archive/v1/dev/{}/",
        manifest.checkpoint.genesis
    );
    for height in [1_000_000, 2_000_000] {
        record.1.checkpoint.height = height;
        source
            .meta_store()
            .put_meta(
                &format!("{prefix}{height}"),
                &bincode::serialize(&record).unwrap(),
            )
            .unwrap();
    }
    let mut cursor = crate::checkpoint_store::ArchivedRefresh::default();
    assert_eq!(
        cursor
            .next(&source, NetworkId::Dev, &fixture.params)
            .unwrap(),
        Some(1_000_000)
    );
    source
        .meta_store()
        .delete_meta(&format!("{prefix}1000000"))
        .unwrap();
    assert_eq!(
        cursor
            .next(&source, NetworkId::Dev, &fixture.params)
            .unwrap(),
        Some(2_000_000)
    );
    assert_eq!(
        cursor
            .next(&source, NetworkId::Dev, &fixture.params)
            .unwrap(),
        None
    );
    assert_eq!(
        cursor
            .next(&source, NetworkId::Dev, &fixture.params)
            .unwrap(),
        Some(2_000_000)
    );
    assert_eq!(
        cursor
            .next(&source, NetworkId::Dev, &fixture.params)
            .unwrap(),
        None
    );
    record.1.checkpoint.height = 3;
    source
        .meta_store()
        .put_meta(
            &format!("{prefix}2000000"),
            &bincode::serialize(&record).unwrap(),
        )
        .unwrap();
    assert!(cursor
        .next(&source, NetworkId::Dev, &fixture.params)
        .is_err());
}

#[test]
fn selection_refresh_visits_retained_heights_not_the_numeric_prefix() {
    let (fixture, _dir, source, manifest, _) = setup();
    let heights = crate::checkpoint_store::selection_refresh_heights;
    assert_eq!(
        heights(&source, NetworkId::Dev, &fixture.params).unwrap(),
        vec![manifest.checkpoint.height]
    );
    let prefix = format!("checkpoint_chain/v1/dev/{}", manifest.checkpoint.genesis);
    let old_key = format!("{prefix}/{}", manifest.checkpoint.height);
    let bytes = source.meta_store().get_meta(&old_key).unwrap();
    let mut record: (
        u8,
        CheckpointCertificate,
        Vec<HistoryHead>,
        WalletAddress,
        Option<burst_vrf::RandomOutput>,
    ) = bincode::deserialize(&bytes).unwrap();
    // Scheduling hints do not authorize these synthetic certificates for import.
    record.1.checkpoint.height = 1_000_000;
    source.meta_store().delete_meta(&old_key).unwrap();
    let key = format!("{prefix}/1000000");
    source
        .meta_store()
        .put_meta(&key, &bincode::serialize(&record).unwrap())
        .unwrap();
    source
        .meta_store()
        .put_meta(&format!("{prefix}/head"), &1_000_000u64.to_be_bytes())
        .unwrap();
    assert_eq!(
        heights(&source, NetworkId::Dev, &fixture.params).unwrap(),
        vec![1_000_000]
    );
    record.1.checkpoint.height -= 1;
    source
        .meta_store()
        .put_meta(&key, &bincode::serialize(&record).unwrap())
        .unwrap();
    assert!(heights(&source, NetworkId::Dev, &fixture.params).is_err());
}

#[test]
fn selection_refresh_skips_stored_proofs_and_resumes_when_missing() {
    let (fixture, _dir, source, manifest, _) = setup();
    let height = manifest.checkpoint.height;
    let active = format!(
        "checkpoint_chain/v1/dev/{}/{height}",
        manifest.checkpoint.genesis
    );
    let archived = format!(
        "checkpoint_record_archive/v1/dev/{}/{height}",
        manifest.checkpoint.genesis
    );
    let absent = source.meta_store().get_meta(&active).unwrap();
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
    let chain::CertifiedEvent::Selection(mut selection) =
        chain::load(&source, NetworkId::Dev, &fixture.params)
            .unwrap()
            .pop()
            .unwrap()
    else {
        panic!("selection tip")
    };
    selection
        .attach_beacon(&source, &fixture.params, beacon.clone())
        .unwrap();
    let present = source.meta_store().get_meta(&active).unwrap();
    let proof_key = format!(
        "snapshot_selection_beacon/v1/{}",
        manifest.checkpoint.hash()
    );
    for separate in [false, true] {
        let record = if separate { &absent } else { &present };
        source.meta_store().put_meta(&active, record).unwrap();
        source.meta_store().put_meta(&archived, record).unwrap();
        if separate {
            source
                .meta_store()
                .put_meta(&proof_key, &bincode::serialize(&beacon).unwrap())
                .unwrap();
        }
        assert!(crate::checkpoint_store::selection_refresh_heights(
            &source,
            NetworkId::Dev,
            &fixture.params
        )
        .unwrap()
        .is_empty());
        assert_eq!(
            crate::checkpoint_store::ArchivedRefresh::default()
                .next(&source, NetworkId::Dev, &fixture.params)
                .unwrap(),
            None
        );
    }
    source.meta_store().delete_meta(&proof_key).unwrap();
    assert_eq!(
        crate::checkpoint_store::selection_refresh_heights(
            &source,
            NetworkId::Dev,
            &fixture.params
        )
        .unwrap(),
        vec![height]
    );
    assert_eq!(
        crate::checkpoint_store::ArchivedRefresh::default()
            .next(&source, NetworkId::Dev, &fixture.params)
            .unwrap(),
        Some(height)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn snapshot_receipt_refreshes_beacon_without_pruned_record_or_peers() {
    use crate::checkpoint_store::historical::snapshot_transfer;
    use crate::confirmed_replay::tests::snapshot_history::installed_recovery::archive;
    use std::sync::Arc;
    let (fixture, _source_dir, source, manifest, _) = setup();
    let snapshot = authenticate(&source, &fixture.params, manifest.checkpoint.hash());
    let base = snapshot.into_continuation();
    let (_dir, local) = cemented::sparse(&base, &source);
    let prefix = format!("checkpoint_chain/v1/dev/{}", base.anchor().genesis_hash());
    local
        .meta_store()
        .put_meta(
            crate::confirmed_replay::snapshot::installed::BASE_KEY,
            &archive(&source, &fixture, base.anchor().hash()),
        )
        .unwrap();
    local
        .meta_store()
        .put_meta(
            &format!("{prefix}/head"),
            &base.anchor().height().to_be_bytes(),
        )
        .unwrap();
    let height = manifest.checkpoint.height;
    assert_eq!(
        crate::checkpoint_store::selection_refresh_heights(&local, NetworkId::Dev, &fixture.params)
            .unwrap(),
        vec![height]
    );
    assert!(local
        .tx_begin_write()
        .unwrap()
        .get_meta(&format!("{prefix}/{height}"))
        .unwrap()
        .is_none());
    let before =
        CertifiedCheckpoint::export_record(&local, NetworkId::Dev, &fixture.params, height)
            .unwrap()
            .unwrap();
    let (version, certificate, heads, target, absent): (
        u8,
        CheckpointCertificate,
        Vec<HistoryHead>,
        WalletAddress,
        Option<burst_vrf::RandomOutput>,
    ) = bincode::deserialize(&before).unwrap();
    assert!(absent.is_none());
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
    let local = Arc::new(local);
    let registry = Arc::new(tokio::sync::RwLock::new(
        crate::connection_registry::ConnectionRegistry::new(),
    ));
    for case in 0..5 {
        let valid = case >= 3;
        let mut candidate = beacon.clone();
        let mut candidate_heads = heads.clone();
        let mut candidate_certificate = certificate.clone();
        match case {
            0 => candidate.round += 1,
            1 => candidate_heads.clear(),
            2 => candidate_certificate.checkpoint.parent = BlockHash::new([99; 32]),
            _ => (),
        }
        let record = bincode::serialize(&(
            version,
            &candidate_certificate,
            &candidate_heads,
            &target,
            Some(candidate),
        ))
        .unwrap();
        let transfer = bincode::serialize(&(
            3u8,
            record,
            Vec::<StateBlock>::new(),
            Vec::<crate::confirmed_history::ProposalWitness>::new(),
        ))
        .unwrap();
        let result = snapshot_transfer::import(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            height,
            transfer,
            registry.clone(),
        )
        .await;
        if valid {
            result.unwrap();
        } else {
            assert!(result.is_err());
            assert_eq!(
                CertifiedCheckpoint::export_record(&local, NetworkId::Dev, &fixture.params, height)
                    .unwrap()
                    .unwrap(),
                before
            );
        }
    }
    let after = CertifiedCheckpoint::export_record(&local, NetworkId::Dev, &fixture.params, height)
        .unwrap()
        .unwrap();
    assert!(crate::checkpoint_store::selection_refresh_heights(
        &local,
        NetworkId::Dev,
        &fixture.params
    )
    .unwrap()
    .is_empty());
    assert_eq!(
        after,
        bincode::serialize(&(version, &certificate, &heads, &target, Some(&beacon))).unwrap()
    );
    assert!(local
        .tx_begin_write()
        .unwrap()
        .get_meta(&format!("{prefix}/{height}"))
        .unwrap()
        .is_none());
    assert_eq!(
        local.tx_begin_write().unwrap().payment_epoch().unwrap(),
        base.anchor().hash()
    );
    let key = format!("{prefix}/{height}");
    local.meta_store().put_meta(&key, &before).unwrap();
    assert_eq!(
        CertifiedCheckpoint::export_record(&local, NetworkId::Dev, &fixture.params, height)
            .unwrap()
            .unwrap(),
        after
    );
    assert_eq!(local.meta_store().get_meta(&key).unwrap(), before);
    let transfer =
        CertifiedCheckpoint::export_transfer(&local, NetworkId::Dev, &fixture.params, height)
            .unwrap()
            .unwrap();
    let (format, exported, blocks, contents): (
        u8,
        Vec<u8>,
        Vec<StateBlock>,
        Vec<crate::confirmed_history::ProposalWitness>,
    ) = bincode::deserialize(&transfer).unwrap();
    assert_eq!(format, 3);
    assert_eq!(exported, after);
    assert!(blocks.is_empty() && contents.is_empty());
    let beacon_key =
        crate::checkpoint_store::archive_selection::beacon_key(certificate.checkpoint.hash());
    let mut invalid_beacon = beacon.clone();
    invalid_beacon.round += 1;
    local
        .meta_store()
        .put_meta(&beacon_key, &bincode::serialize(&invalid_beacon).unwrap())
        .unwrap();
    assert!(
        CertifiedCheckpoint::export_record(&local, NetworkId::Dev, &fixture.params, height)
            .is_err()
    );
    local
        .meta_store()
        .put_meta(&beacon_key, &bincode::serialize(&beacon).unwrap())
        .unwrap();
    local.force_sync().unwrap();
    drop(local);
    let reopened = LmdbStore::open(_dir.path(), 32, 32 << 20).unwrap();
    assert_eq!(
        CertifiedCheckpoint::export_record(&reopened, NetworkId::Dev, &fixture.params, height)
            .unwrap()
            .unwrap(),
        after
    );
}

#[test]
fn compact_recovery_derives_only_the_certified_published_committee() {
    let (fixture, _source_dir, source, manifest, _) = setup();
    let chain::CertifiedEvent::Selection(mut selection) =
        chain::load(&source, NetworkId::Dev, &fixture.params)
            .unwrap()
            .pop()
            .unwrap()
    else {
        panic!("selection tip")
    };
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
    selection
        .attach_beacon(&source, &fixture.params, beacon.clone())
        .unwrap();
    let state = authenticate(&source, &fixture.params, manifest.checkpoint.hash()).into_state();
    assert!(state
        .verification
        .selection_context(&manifest.target)
        .is_none());
    let mut published = state.verification.clone();
    published
        .install_selection(
            &manifest.target,
            selection.parent(),
            selection.certificate(),
            &beacon,
        )
        .unwrap();
    published.drain_events();
    let selections = vec![*selection];
    for case in 0..5 {
        let mut candidate = state.clone();
        let mut actual = if case == 0 {
            state.verification.snapshot()
        } else {
            published.snapshot()
        };
        match case {
            2 => {
                actual
                    .states
                    .get_mut(&manifest.target)
                    .unwrap()
                    .selected_verifiers[0] = address(99)
            }
            3 => actual.states.get_mut(&manifest.target).unwrap().started_at = Timestamp::new(1),
            _ => (),
        }
        let before = candidate.verification.snapshot().encode().unwrap();
        let expected = actual.encode().unwrap();
        let supplied = if case == 4 {
            &[][..]
        } else {
            selections.as_slice()
        };
        let result = crate::confirmed_replay::snapshot::installed::publication::materialize(
            actual,
            &mut candidate,
            supplied,
        );
        if case < 2 {
            result.unwrap();
            assert_eq!(
                candidate.verification.snapshot().encode().unwrap(),
                expected
            );
        } else {
            assert!(result.is_err(), "case {case}");
            assert_eq!(candidate.verification.snapshot().encode().unwrap(), before);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_transfer_replays_legacy_and_sparse_exports_with_a_beacon() {
    use crate::confirmed_replay::tests::snapshot_history::installed_recovery::{archive, Service};
    use std::sync::Arc;
    let (fixture, _source_dir, source, manifest, _) = setup();
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
    let chain::CertifiedEvent::Selection(mut selection) =
        chain::load(&source, NetworkId::Dev, &fixture.params)
            .unwrap()
            .pop()
            .unwrap()
    else {
        panic!("selection tip")
    };
    selection
        .attach_beacon(&source, &fixture.params, beacon.clone())
        .unwrap();
    let source = Arc::new(source);
    let mut transfer = CertifiedCheckpoint::export_transfer(
        &source,
        NetworkId::Dev,
        &fixture.params,
        manifest.checkpoint.height,
    )
    .unwrap()
    .unwrap();
    for version in [2, 3] {
        assert_eq!(transfer[0], version);
        let base =
            authenticate(&source, &fixture.params, manifest.checkpoint.parent).into_continuation();
        let (_dir, local) = cemented::sparse(&base, &source);
        local
            .meta_store()
            .put_meta(
                crate::confirmed_replay::snapshot::installed::BASE_KEY,
                &archive(&source, &fixture, base.anchor().hash()),
            )
            .unwrap();
        let prefix = format!("checkpoint_chain/v1/dev/{}", base.anchor().genesis_hash());
        local
            .meta_store()
            .put_meta(
                &format!("{prefix}/head"),
                &base.anchor().height().to_be_bytes(),
            )
            .unwrap();
        let local = Arc::new(local);
        let service = Service::start(source.clone(), local.clone(), 0).await;
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        assert!(
            crate::checkpoint_store::historical::snapshot_transfer::import(
                local.clone(),
                NetworkId::Dev,
                fixture.params.clone(),
                manifest.checkpoint.height + 1,
                transfer.clone(),
                service.registry.clone(),
            )
            .await
            .is_err()
        );
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
        crate::checkpoint_store::historical::snapshot_transfer::import(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            manifest.checkpoint.height,
            transfer,
            service.registry.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            CertifiedCheckpoint::stored_tip(&local, NetworkId::Dev, &fixture.params).unwrap(),
            manifest.checkpoint.height
        );
        assert_eq!(
            local.meta_store().payment_epoch().unwrap(),
            base.anchor().hash()
        );
        let recovered = crate::confirmed_replay::snapshot::installed::load_base(
            &local,
            NetworkId::Dev,
            &fixture.params,
        )
        .unwrap()
        .unwrap()
        .recover_stored_successors_from_peers(local.clone(), service.registry.clone())
        .await
        .unwrap();
        assert_eq!(
            recovered.continuation.anchor().hash(),
            manifest.checkpoint.hash()
        );
        assert_eq!(
            recovered.selections.read_selection(recovered.selections.len() - 1).unwrap().beacon().unwrap().round,
            beacon.round
        );
        transfer = CertifiedCheckpoint::export_transfer(
            &local,
            NetworkId::Dev,
            &fixture.params,
            manifest.checkpoint.height,
        )
        .unwrap()
        .unwrap();
        service.stop().await;
    }
}

#[test]
fn compact_beacon_import_is_atomic_and_preserves_the_first_certificate() {
    use crate::checkpoint_agreement::CompactReplayInput;
    use crate::checkpoint_store::compact::accept_candidate_with_beacon_checked;
    let (fixture, _source_dir, source, manifest, certificate) = setup();
    let snapshot = authenticate(&source, &fixture.params, manifest.checkpoint.parent);
    let witnesses = compact::retained(&snapshot, &fixture, &source, &[fixture.blocks[0].hash]);
    let base = snapshot.into_continuation();
    let (_suffix_dir, suffix) = suffix_store(&[]);
    let tail = base
        .read_tail(&suffix, CUTOFF, &manifest.heads, witnesses)
        .unwrap();
    let coverage = base.selection_vote_coverage(tail, &manifest, &[]).unwrap();
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
    let anchor = base.anchor();
    let prefix = format!("checkpoint_chain/v1/dev/{}", anchor.genesis_hash());
    let key = format!("{prefix}/{}", certificate.checkpoint.height);
    for delayed in [false, true] {
        let (_dir, local) = cemented::sparse(&base, &source);
        local
            .meta_store()
            .put_meta(&format!("{prefix}/head"), &anchor.height().to_be_bytes())
            .unwrap();
        if delayed {
            crate::checkpoint_store::compact::accept_selection(
                &local,
                anchor,
                &coverage,
                &certificate,
            )
            .unwrap();
        }
        let rows = || {
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap()
        };
        let before = rows();
        for case in 0..4 {
            let mut wrong = beacon.clone();
            match case {
                0 => wrong.round += 1,
                1 => wrong.proof[0] ^= 1,
                2 => wrong.value[0] ^= 1,
                _ => wrong.proof.resize(4097, 0),
            }
            assert!(accept_candidate_with_beacon_checked(
                &local,
                anchor,
                CompactReplayInput::Selection(&coverage),
                &certificate,
                Some(&wrong),
                |_| Ok(())
            )
            .is_err());
            assert_eq!(rows(), before);
        }
        assert!(accept_candidate_with_beacon_checked(
            &local,
            anchor,
            CompactReplayInput::Selection(&coverage),
            &certificate,
            Some(&beacon),
            |_| Err(burst_store::StoreError::Corruption(
                "changed observation".into()
            ))
        )
        .is_err());
        assert_eq!(rows(), before);

        let alternative = certified_votes::certify(&manifest.checkpoint, &[1, 2, 3, 4, 5, 6, 7]);
        let incoming = if delayed { &alternative } else { &certificate };
        accept_candidate_with_beacon_checked(
            &local,
            anchor,
            CompactReplayInput::Selection(&coverage),
            incoming,
            Some(&beacon),
            |_| Ok(()),
        )
        .unwrap();
        let persisted = local.meta_store().get_meta(&key).unwrap();
        let (version, stored_certificate, heads, target, stored_beacon): (
            u8,
            CheckpointCertificate,
            Vec<crate::confirmed_history::HistoryHead>,
            WalletAddress,
            Option<burst_vrf::RandomOutput>,
        ) = bincode::deserialize(&persisted).unwrap();
        assert_eq!(version, 1);
        assert_eq!(stored_certificate, certificate);
        assert_eq!(heads, manifest.heads);
        assert_eq!(target, manifest.target);
        assert_eq!(
            bincode::serialize(&stored_beacon).unwrap(),
            bincode::serialize(&Some(&beacon)).unwrap()
        );
        assert_eq!(local.meta_store().payment_epoch().unwrap(), anchor.hash());
        let after = rows();
        accept_candidate_with_beacon_checked(
            &local,
            anchor,
            CompactReplayInput::Selection(&coverage),
            &alternative,
            Some(&beacon),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(rows(), after);
    }
}

#[test]
fn compact_decision_persistence_is_atomic_and_preserves_local_authority() {
    let (fixture, _source_dir, source, manifest, certificate) = setup();
    let snapshot = authenticate(&source, &fixture.params, manifest.checkpoint.parent);
    let witnesses = compact::retained(&snapshot, &fixture, &source, &[fixture.blocks[0].hash]);
    let base = snapshot.into_continuation();
    let (_suffix_dir, suffix) = suffix_store(&[]);
    let tail = base
        .read_tail(&suffix, CUTOFF, &manifest.heads, witnesses)
        .unwrap();
    let coverage = base.selection_vote_coverage(tail, &manifest, &[]).unwrap();
    assert!(coverage.archive_rows().next().unwrap().is_ok());
    let anchor = base.anchor();
    let prefix = format!("checkpoint_chain/v1/dev/{}", anchor.genesis_hash());
    for case in 0..6 {
        let (_dir, local) = cemented::sparse(&base, &source);
        local
            .meta_store()
            .put_meta(&format!("{prefix}/head"), &anchor.height().to_be_bytes())
            .unwrap();
        local
            .meta_store()
            .reserve_consensus_vote_at(
                &anchor.hash(),
                &address(0),
                &address(99),
                &BlockHash::ZERO,
                &BlockHash::new([99; 32]),
                true,
                0,
                Some(CUTOFF),
            )
            .unwrap();
        let mut incoming = certificate.clone();
        match case {
            1 => incoming.signatures.clear(),
            2 => local
                .meta_store()
                .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &[99; 32])
                .unwrap(),
            3 => local
                .meta_store()
                .put_meta(
                    &format!("{prefix}/head"),
                    &(anchor.height() + 1).to_be_bytes(),
                )
                .unwrap(),
            4 => local
                .meta_store()
                .put_meta(
                    &coverage.archive_rows().last().unwrap().unwrap().0,
                    b"conflict",
                )
                .unwrap(),
            5 => local
                .meta_store()
                .put_meta(&format!("{prefix}/3"), b"orphan")
                .unwrap(),
            _ => (),
        }
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let result = crate::checkpoint_store::compact::accept_selection(
            &local, anchor, &coverage, &incoming,
        );
        if case != 0 {
            assert!(result.is_err(), "case {case}");
            assert_eq!(
                local
                    .tx_begin_write()
                    .unwrap()
                    .checkpoint_metadata(b"")
                    .unwrap(),
                before
            );
            continue;
        }
        result.unwrap();
        assert_eq!(local.meta_store().payment_epoch().unwrap(), anchor.hash());
        let after = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let economic_and_promises = |rows: Vec<(Vec<u8>, Vec<u8>)>| {
            rows.into_iter()
                .filter(|(key, _)| {
                    !key.starts_with(b"checkpoint_chain/v1/")
                        && !key.starts_with(b"checkpoint_history/v1/")
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            economic_and_promises(before),
            economic_and_promises(after.clone())
        );
        let alternative = certified_votes::certify(&manifest.checkpoint, &[1, 2, 3, 4, 5, 6, 7]);
        crate::checkpoint_store::compact::accept_selection(&local, anchor, &coverage, &alternative)
            .unwrap();
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            after
        );
        for row in coverage.archive_rows() {
            let (key, bytes) = row.unwrap();
            assert_eq!(local.meta_store().get_meta(&key).unwrap(), bytes);
        }
    }
}
