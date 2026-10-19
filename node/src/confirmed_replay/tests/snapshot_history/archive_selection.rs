use super::*;
use crate::confirmed_replay::snapshot::installed;

#[test]
fn archived_selection_beacon_and_committee_survive_reopen_without_checkpoint_records() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    let bytes = installed_recovery::archive(&source, &fixture, certificate.checkpoint.hash());
    let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
    let mut checkpoint = snapshot.recover_selections().unwrap().pop().unwrap();
    let base = snapshot.into_continuation();
    let (dir, local) = cemented::sparse(&base, &source);
    let head = format!(
        "checkpoint_chain/v1/dev/{}/head",
        base.anchor().genesis_hash()
    );
    local
        .meta_store()
        .put_meta(&head, &base.anchor().height().to_be_bytes())
        .unwrap();
    local
        .meta_store()
        .put_meta(installed::BASE_KEY, &bytes)
        .unwrap();
    let live = base.state().verification.clone();
    {
        let mut batch = local.tx_begin_write().unwrap();
        assert!(crate::checkpoint_selection::stage(
            &mut batch,
            &checkpoint,
            &fixture.params,
            &live
        )
        .unwrap()
        .is_none());
    }
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    let beacon = burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap();
    let mut wrong = beacon.clone();
    wrong.round += 1;
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    assert!(checkpoint
        .attach_beacon(&local, &fixture.params, wrong)
        .is_err());
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    checkpoint
        .attach_beacon(&local, &fixture.params, beacon)
        .unwrap();
    let mut batch = local.tx_begin_write().unwrap();
    let candidate =
        crate::checkpoint_selection::stage(&mut batch, &checkpoint, &fixture.params, &live)
            .unwrap()
            .unwrap();
    assert!(!candidate
        .get_state(&address(20))
        .unwrap()
        .selected_verifiers
        .is_empty());
    batch.commit().unwrap();
    local.force_sync().unwrap();
    drop(local);
    let local = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let restored = installed::load_base(&local, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .recover_stored_successors(&local, |_, _, _| {
            panic!("base has no successor dependencies")
        })
        .unwrap();
    let checkpoint = &restored.selections.read_selection(0).unwrap();
    assert!(checkpoint.beacon().is_some());
    let mut batch = local.tx_begin_write().unwrap();
    assert!(crate::checkpoint_selection::stage(
        &mut batch,
        checkpoint,
        &fixture.params,
        &candidate
    )
    .unwrap()
    .is_none());
    assert_eq!(
        batch
            .checkpoint_metadata(
                format!("checkpoint_chain/v1/dev/{}/", base.anchor().genesis_hash()).as_bytes()
            )
            .unwrap()
            .len(),
        1
    );
}
