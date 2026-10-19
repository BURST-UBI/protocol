use super::*;
use crate::confirmed_replay::snapshot::installed;

#[test]
fn historical_export_observations_reject_records_changed_during_replay() {
    let (fixture, _dir, store, _, _) = selection::setup();
    let genesis = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap();
    let key = format!("checkpoint_chain/v1/dev/{}/1", genesis.genesis_hash());
    let mut records =
        crate::checkpoint_store::compact::ExportRecords::new(&store, genesis).unwrap();
    assert!(records.next().unwrap().is_some());
    // Replay needs to be able to open its own transaction between records.
    drop(store.tx_begin_read().unwrap());
    let observations = records.finish();
    let mut batch = store.tx_begin_write().unwrap();
    observations.verify(&batch).unwrap();
    let mut changed = batch.get_meta(&key).unwrap().unwrap();
    changed[0] ^= 1;
    batch.put_meta(&key, &changed).unwrap();
    assert!(observations.verify(&batch).is_err());
    drop(batch);
}

#[test]
fn genesis_incremental_export_matches_full_replay_without_creating_a_snapshot_base() {
    let (fixture, _dir, store, _, _) = selection::setup();
    let events =
        crate::checkpoint_store::chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let before = store
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    for count in 1..=events.len() {
        let prefix = &events[..count];
        let anchor =
            crate::checkpoint_store::chain::next_anchor(prefix, NetworkId::Dev, &fixture.params)
                .unwrap();
        let history = ConfirmedHistory::read_certified(
            &store,
            NetworkId::Dev,
            anchor.genesis_hash(),
            anchor.cutoff(),
            prefix.last().unwrap().heads(),
            &[],
        )
        .unwrap();
        let full =
            ConfirmedReplay::replay_with_events(&history, NetworkId::Dev, &fixture.params, prefix)
                .unwrap()
                .prepare_snapshot(&anchor)
                .unwrap();
        let incremental =
            SnapshotExport::prepare(&store, NetworkId::Dev, &fixture.params, anchor.hash())
                .unwrap();
        assert_eq!(incremental.manifest(), full.manifest());
        if count == events.len() {
            let at_tip = crate::confirmed_replay::snapshot::PreparedSnapshot::at_tip(
                &store,
                NetworkId::Dev,
                &fixture.params,
            )
            .unwrap();
            assert_eq!(at_tip.bytes(), full.bytes());
        }
    }
    assert_eq!(
        store
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    assert!(store.meta_store().get_meta(installed::BASE_KEY).is_err());
}

#[test]
fn compact_export_replays_successor_and_reexports_restarted_base_without_full_history() {
    let (fixture, _source_dir, source, manifest, certificate) = selection::setup();
    let snapshot = authenticate(&source, &fixture.params, manifest.checkpoint.parent);
    let base_epoch = snapshot.anchor().hash();
    let base = authenticate(&source, &fixture.params, base_epoch).into_continuation();
    let (dir, local) = cemented::sparse(&base, &source);
    local
        .meta_store()
        .put_meta(
            installed::BASE_KEY,
            &installed_recovery::archive(&source, &fixture, base_epoch),
        )
        .unwrap();
    let prefix = format!("checkpoint_chain/v1/dev/{}", base.anchor().genesis_hash());
    for key in [format!("{prefix}/2"), format!("{prefix}/head")] {
        local
            .meta_store()
            .put_meta(&key, &source.meta_store().get_meta(&key).unwrap())
            .unwrap();
    }
    let epoch = certificate.checkpoint.hash();
    let expected =
        SnapshotExport::prepare(&source, NetworkId::Dev, &fixture.params, epoch).unwrap();
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    for target in [base_epoch, epoch] {
        let compact =
            SnapshotExport::prepare(&local, NetworkId::Dev, &fixture.params, target).unwrap();
        let full =
            SnapshotExport::prepare(&source, NetworkId::Dev, &fixture.params, target).unwrap();
        assert_eq!(compact.manifest(), full.manifest());
    }
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    assert!(crate::checkpoint_store::chain::load(&local, NetworkId::Dev, &fixture.params).is_err());
    let export = SnapshotExport::prepare(&local, NetworkId::Dev, &fixture.params, epoch).unwrap();
    let attestations: Vec<_> = (0..7)
        .map(|seed| {
            export
                .attest(
                    &local,
                    &burst_crypto::keypair_from_seed(&[seed; 32]).private,
                )
                .unwrap()
        })
        .collect();
    let mut archive = Vec::new();
    export.write(&attestations, &mut archive).unwrap();
    let genesis = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap();
    let exported = AuthenticatedSnapshot::read(&genesis, &mut archive.as_slice()).unwrap();
    assert_eq!(exported.certificate().manifest, *expected.manifest());
    compact::assert_same(
        &exported.into_state(),
        &authenticate(&source, &fixture.params, epoch).into_state(),
    );
    local
        .meta_store()
        .put_meta(installed::BASE_KEY, &archive)
        .unwrap();
    local.force_sync().unwrap();
    drop(local);
    let local = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let reopened = SnapshotExport::prepare(&local, NetworkId::Dev, &fixture.params, epoch).unwrap();
    assert_eq!(reopened.manifest(), expected.manifest());
    assert_eq!(
        reopened
            .attest(&local, &burst_crypto::keypair_from_seed(&[0; 32]).private)
            .unwrap(),
        attestations[0]
    );
}

#[test]
fn compact_export_rejects_corrupt_installed_base_instead_of_falling_back_to_full_history() {
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    source
        .meta_store()
        .put_meta(
            installed::BASE_KEY,
            &installed_recovery::archive(&source, &fixture, manifest.checkpoint.parent),
        )
        .unwrap();
    source
        .meta_store()
        .put_meta(installed::BASE_KEY, b"corrupt")
        .unwrap();
    let before = source
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    assert!(SnapshotExport::prepare(
        &source,
        NetworkId::Dev,
        &fixture.params,
        certificate.checkpoint.hash()
    )
    .is_err());
    assert_eq!(
        source
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
}
