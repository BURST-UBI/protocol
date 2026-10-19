use super::*;
use crate::confirmed_replay::snapshot::installed;

#[test]
fn staged_retirement_rechecks_each_body_and_aborts_partial_archiving() {
    let (fixture, _dir, local, _, certificate) = selection::setup();
    let snapshot = authenticate(&local, &fixture.params, certificate.checkpoint.hash());
    let events = chain::load(&local, NetworkId::Dev, &fixture.params).unwrap();
    let anchors: Vec<_> = (0..=events.len())
        .map(|end| chain::next_anchor(&events[..end], NetworkId::Dev, &fixture.params).unwrap())
        .collect();
    let prefix = format!("checkpoint_chain/v1/dev/{}", fixture.blocks[0].hash);
    let key = format!("{prefix}/2");
    let original = local.meta_store().get_meta(&key).unwrap();
    let archived = format!(
        "checkpoint_record_archive/v1/dev/{}/1",
        fixture.blocks[0].hash
    );
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    for case in 0..4 {
        let mut batch = local.tx_begin_write().unwrap();
        let records = crate::checkpoint_store::stage_snapshot_head(
            &mut batch,
            anchors.iter().cloned().map(Ok),
        )
        .unwrap();
        match case {
            1 => batch.put_meta(&key, b"changed").unwrap(),
            2 => batch.delete_meta(&key).unwrap(),
            3 => {
                batch.put_meta(&key, b"changed").unwrap();
                batch.put_meta(&key, &original).unwrap();
            }
            _ => (),
        }
        let result = records.archive(&mut batch, &snapshot);
        assert_eq!(result.is_ok(), case == 0 || case == 3);
        // The first body was archived before reaching the changed second body.
        assert!(batch.get_meta(&archived).unwrap().is_some());
        if result.is_ok() {
            assert!(batch.get_meta(&key).unwrap().is_none());
        }
        drop(batch);
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
    }
}

#[test]
fn snapshot_reconciliation_authenticates_archived_successors_before_retirement() {
    let (fixture, _dir, local, _, certificate) = selection::setup();
    let snapshot = authenticate(&local, &fixture.params, certificate.checkpoint.hash());
    let events = chain::load(&local, NetworkId::Dev, &fixture.params).unwrap();
    let anchors: Vec<_> = (0..=events.len())
        .map(|end| chain::next_anchor(&events[..end], NetworkId::Dev, &fixture.params).unwrap())
        .collect();
    let prefix = format!("checkpoint_chain/v1/dev/{}", fixture.blocks[0].hash);
    let archive = format!(
        "checkpoint_record_archive/v1/dev/{}",
        fixture.blocks[0].hash
    );
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    for case in 0..5 {
        let mut batch = local.tx_begin_write().unwrap();
        for height in 1..=2 {
            let active = format!("{prefix}/{height}");
            let bytes = batch.get_meta(&active).unwrap().unwrap();
            batch
                .put_meta(&format!("{archive}/{height}"), &bytes)
                .unwrap();
            batch.delete_meta(&active).unwrap();
        }
        let key = format!("{archive}/2");
        match case {
            1 => batch.delete_meta(&key).unwrap(),
            2 => batch.put_meta(&key, b"corrupt record").unwrap(),
            3 => {
                let wrong_height = batch.get_meta(&format!("{archive}/1")).unwrap().unwrap();
                batch.put_meta(&key, &wrong_height).unwrap();
            }
            _ => (),
        }
        let staged = crate::checkpoint_store::stage_snapshot_head(
            &mut batch,
            anchors.iter().cloned().map(Ok),
        );
        if (1..=3).contains(&case) {
            assert!(staged.is_err());
        } else {
            let staged = staged.unwrap();
            if case == 4 {
                batch
                    .put_meta(&key, b"changed after authentication")
                    .unwrap();
            }
            assert_eq!(staged.archive(&mut batch, &snapshot).is_ok(), case == 0);
            assert!(batch.get_meta(&format!("{archive}/1")).unwrap().is_some());
        }
        drop(batch);
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
    }
}

#[test]
fn snapshot_record_retirement_is_atomic_and_preserves_late_beacons_after_restart() {
    for case in 0..4 {
        let (fixture, dir, local, manifest, certificate) = selection::setup();
        let epoch = certificate.checkpoint.hash();
        let earlier = SnapshotExport::prepare(
            &local,
            NetworkId::Dev,
            &fixture.params,
            manifest.checkpoint.parent,
        )
        .unwrap()
        .manifest()
        .clone();
        let snapshot = authenticate(&local, &fixture.params, epoch);
        assert!(snapshot.recover_selections().unwrap()[0].beacon().is_none());
        let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
            "../../../../../tests/fixtures/selection-beacons.json"
        ))
        .unwrap();
        let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
        let before_beacon = authenticate(&local, &fixture.params, epoch)
            .recover_stored_successors(&local, |_, _, _| panic!("no successors"))
            .unwrap();
        {
            let mut batch = local.tx_begin_write().unwrap();
            before_beacon.verify_records_unchanged(&batch).unwrap();
            batch
                .put_meta(
                    &crate::checkpoint_store::archive_selection::beacon_key(epoch),
                    &bincode::serialize(&beacon).unwrap(),
                )
                .unwrap();
            assert!(before_beacon.verify_records_unchanged(&batch).is_err());
        }
        let mut selection = CertifiedCheckpoint::accept(
            &local,
            NetworkId::Dev,
            &fixture.params,
            certificate,
            manifest.heads,
            address(20),
        )
        .unwrap();
        selection
            .attach_beacon(&local, &fixture.params, beacon.clone())
            .unwrap();
        PreparedInstallation::next(&local, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap()
            .commit(&local)
            .unwrap();
        let prefix = format!("checkpoint_chain/v1/dev/{}", fixture.blocks[0].hash);
        let archived = format!(
            "checkpoint_record_archive/v1/dev/{}/2",
            fixture.blocks[0].hash
        );
        let original = local.meta_store().get_meta(&format!("{prefix}/2")).unwrap();
        if case == 2 {
            local
                .meta_store()
                .put_meta(&archived, b"conflicting archive")
                .unwrap();
        }
        if case == 3 {
            assert!(local.meta_store().get_meta(installed::BASE_KEY).is_err());
            let mut batch = local.tx_begin_write().unwrap();
            for height in 1..=2 {
                let active = format!("{prefix}/{height}");
                let archived = format!(
                    "checkpoint_record_archive/v1/dev/{}/{height}",
                    fixture.blocks[0].hash
                );
                let bytes = batch.get_meta(&active).unwrap().unwrap();
                batch.put_meta(&archived, &bytes).unwrap();
                batch.delete_meta(&active).unwrap();
            }
            batch.commit().unwrap();
        }
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let needs = snapshot.installation_needs(&local).unwrap();
        let hashes: Vec<_> = needs
            .snapshot_heads
            .iter()
            .chain(&needs.cemented_heads)
            .map(|head| head.hash)
            .collect();
        let proofs = witnesses(&snapshot, &fixture, &local, &hashes);
        let prepared = snapshot
            .reconcile_installation(
                local.tx_begin_write().unwrap(),
                NetworkId::Dev,
                &fixture.params,
                &proofs,
            )
            .unwrap()
            .project_ledger(&[])
            .unwrap()
            .project_economics();
        if case == 2 {
            assert!(prepared.is_err());
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
        let prepared = prepared.unwrap();
        assert_eq!(
            prepared.transaction().get_meta(&archived).unwrap(),
            Some(original.clone())
        );
        assert!(prepared
            .transaction()
            .get_meta(&format!("{prefix}/2"))
            .unwrap()
            .is_none());
        if case == 0 {
            drop(prepared);
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
        prepared.commit_for_startup().unwrap();
        local.force_sync().unwrap();
        drop(local);
        let local = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        assert_eq!(local.meta_store().get_meta(&archived).unwrap(), original);
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(format!("{prefix}/").as_bytes())
                .unwrap()
                .len(),
            1
        );
        let recovered = installed::load_base(&local, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap()
            .recover_stored_successors(&local, |_, _, _| panic!("no successors"))
            .unwrap();
        assert_eq!(recovered.continuation.anchor().hash(), epoch);
        assert_eq!(
            bincode::serialize(&recovered.selections.read_selection(0).unwrap().beacon()).unwrap(),
            bincode::serialize(&Some(beacon)).unwrap()
        );
        let export =
            SnapshotExport::prepare(&local, NetworkId::Dev, &fixture.params, epoch).unwrap();
        assert_eq!(export.manifest().epoch, epoch);
        assert_eq!(
            SnapshotExport::prepare(&local, NetworkId::Dev, &fixture.params, earlier.epoch)
                .unwrap()
                .manifest(),
            &earlier
        );
        assert_eq!(
            CertifiedCheckpoint::export_record(&local, NetworkId::Dev, &fixture.params, 2).unwrap(),
            Some(original)
        );
    }
}
