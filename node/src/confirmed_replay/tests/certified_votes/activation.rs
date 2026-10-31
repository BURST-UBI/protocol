use super::*;
use crate::{
    checkpoint_activation::PreparedActivation,
    checkpoint_agreement::ReplayManifest,
    checkpoint_store::{chain, historical::HistoricalAdmissions},
    confirmed_replay::installation::PreparedInstallation,
    verification_activation,
};

mod deadline;
mod governance;
mod retry;

#[test]
fn compact_activation_persists_its_selection_beacon_atomically() {
    use crate::confirmed_replay::tests::snapshot_history::{authenticate, compact::retained};
    for snapshot_backed in [false, true] {
        let (fixture, dir, store, selection, _) = setup();
        let manifest =
            PreparedActivation::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 31)
                .unwrap()
                .unwrap()
                .manifest()
                .clone();
        let snapshot = authenticate(&store, &fixture.params, manifest.checkpoint.parent);
        let witnesses = retained(&snapshot, &fixture, &store, &[]);
        let base = snapshot.into_continuation();
        let tail = base
            .read_tail(
                &store,
                manifest.checkpoint.cutoff,
                &manifest.heads,
                witnesses,
            )
            .unwrap();
        let coverage = base
            .activation_vote_coverage_for_manifest(&tail, &manifest, &[&selection])
            .unwrap();
        let key = format!("checkpoint_chain/v1/dev/{}/1", manifest.checkpoint.genesis);
        let mut record: (
            u8,
            CheckpointCertificate,
            Vec<HistoryHead>,
            WalletAddress,
            Option<burst_vrf::RandomOutput>,
        ) = bincode::deserialize(&store.meta_store().get_meta(&key).unwrap()).unwrap();
        record.4 = None;
        store
            .meta_store()
            .put_meta(&key, &bincode::serialize(&record).unwrap())
            .unwrap();
        if snapshot_backed {
            let bytes =
                crate::confirmed_replay::tests::snapshot_history::installed_recovery::archive(
                    &store,
                    &fixture,
                    manifest.checkpoint.parent,
                );
            store
                .meta_store()
                .put_meta(
                    crate::confirmed_replay::snapshot::installed::BASE_KEY,
                    &bytes,
                )
                .unwrap();
        }
        assert!(selection
            .require_persisted(&store.tx_begin_write().unwrap())
            .is_err());
        let certificate = certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 6]);
        let beacon_key = format!(
            "snapshot_selection_beacon/v1/{}",
            selection.certificate().checkpoint.hash()
        );
        store
            .meta_store()
            .put_meta(&beacon_key, b"conflicting proof")
            .unwrap();
        assert!(crate::checkpoint_store::compact::accept_candidate(
            &store,
            base.anchor(),
            crate::checkpoint_agreement::CompactReplayInput::Activation(&coverage),
            &certificate,
        )
        .is_err());
        assert_eq!(
            CertifiedCheckpoint::stored_tip(&store, NetworkId::Dev, &fixture.params).unwrap(),
            1
        );
        assert_eq!(
            store.meta_store().get_meta(&beacon_key).unwrap(),
            b"conflicting proof"
        );
        store.meta_store().delete_meta(&beacon_key).unwrap();
        crate::checkpoint_store::compact::accept_candidate(
            &store,
            base.anchor(),
            crate::checkpoint_agreement::CompactReplayInput::Activation(&coverage),
            &certificate,
        )
        .unwrap();
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        selection
            .require_persisted(&store.tx_begin_write().unwrap())
            .unwrap();
        // Reaccepting the certified activation restores its missing beacon
        // without another committee selection or vote.
        store.meta_store().delete_meta(&beacon_key).unwrap();
        assert!(selection
            .require_persisted(&store.tx_begin_write().unwrap())
            .is_err());
        crate::checkpoint_store::compact::accept_candidate(
            &store,
            base.anchor(),
            crate::checkpoint_agreement::CompactReplayInput::Activation(&coverage),
            &certificate,
        )
        .unwrap();
        selection
            .require_persisted(&store.tx_begin_write().unwrap())
            .unwrap();
        store.meta_store().delete_meta(&beacon_key).unwrap();
        let epoch = store.meta_store().payment_epoch().unwrap();
        let accounts = bincode::serialize(
            &store
                .tx_begin_write()
                .unwrap()
                .bounded_accounts(131_072)
                .unwrap(),
        )
        .unwrap();
        let mut recovered = selection.clone();
        let beacon = recovered.beacon().unwrap().clone();
        recovered
            .attach_beacon(&store, &fixture.params, beacon.clone())
            .unwrap();
        recovered
            .require_persisted(&store.tx_begin_write().unwrap())
            .unwrap();
        let revision = store.env().info().last_txn_id;
        recovered
            .attach_beacon(&store, &fixture.params, beacon.clone())
            .unwrap();
        assert_eq!(
            store.env().info().last_txn_id,
            revision,
            "recovered proofs must not keep committing empty transactions"
        );
        let mut forged = beacon;
        forged.value[0] ^= 1;
        assert!(recovered
            .attach_beacon(&store, &fixture.params, forged)
            .is_err());
        assert_eq!(store.env().info().last_txn_id, revision);
        assert_eq!(store.meta_store().payment_epoch().unwrap(), epoch);
        assert_eq!(
            bincode::serialize(
                &store
                    .tx_begin_write()
                    .unwrap()
                    .bounded_accounts(131_072)
                    .unwrap()
            )
            .unwrap(),
            accounts
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_activation_service_agrees_and_recovers_over_tcp() {
    let (fixture, _dir, source, _, _) = setup();
    let prepare = |cutoff| {
        PreparedActivation::prepare_next(&source, NetworkId::Dev, &fixture.params, cutoff)
            .unwrap()
            .unwrap()
            .manifest()
            .clone()
    };
    let manifest = prepare(CUTOFF + 31);
    let alternative = prepare(CUTOFF + 32);
    crate::confirmed_replay::tests::snapshot_history::selection::compact_service::assert_service(
        &fixture,
        std::sync::Arc::new(source),
        ReplayManifest::Activation(manifest),
        ReplayManifest::Activation(alternative),
        CUTOFF + 35,
    )
    .await;
}

fn setup() -> (
    Fixture,
    tempfile::TempDir,
    LmdbStore,
    CertifiedCheckpoint,
    Vec<u8>,
) {
    setup_with(60)
}

fn setup_with(
    timeout: u64,
) -> (
    Fixture,
    tempfile::TempDir,
    LmdbStore,
    CertifiedCheckpoint,
    Vec<u8>,
) {
    setup_config(|params| params.verification_timeout_secs = timeout)
}

fn setup_config(
    configure: impl FnOnce(&mut ProtocolParams),
) -> (
    Fixture,
    tempfile::TempDir,
    LmdbStore,
    CertifiedCheckpoint,
    Vec<u8>,
) {
    setup_fixture(Fixture::selection_with(configure))
}

fn setup_fixture(
    fixture: Fixture,
) -> (
    Fixture,
    tempfile::TempDir,
    LmdbStore,
    CertifiedCheckpoint,
    Vec<u8>,
) {
    let (dir, store, heads) = fixture.store();
    let replay = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF,
        &heads,
        &address(20),
    )
    .unwrap();
    let mut selection = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certify(replay.checkpoint(), &[0]),
        heads,
        address(20),
    )
    .unwrap();
    let without_beacon =
        CertifiedCheckpoint::export_transfer(&store, NetworkId::Dev, &fixture.params, 1)
            .unwrap()
            .unwrap();
    PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    selection
        .attach_beacon(
            &store,
            &fixture.params,
            burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap(),
        )
        .unwrap();
    (fixture, dir, store, selection, without_beacon)
}

fn activate(fixture: &Fixture, store: &LmdbStore) -> BlockHash {
    let candidate =
        PreparedActivation::prepare_next(store, NetworkId::Dev, &fixture.params, CUTOFF + 31)
            .unwrap()
            .unwrap();
    let manifest = candidate.manifest().clone();
    assert!(
        PreparedActivation::replay_manifest(store, &fixture.params, &manifest)
            .unwrap()
            .certify(certify(&manifest.checkpoint, &[0]))
            .is_err()
    );
    chain::accept_manifest(
        store,
        NetworkId::Dev,
        &fixture.params,
        certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 6]),
        &ReplayManifest::Activation(manifest.clone()),
    )
    .unwrap();
    PreparedInstallation::next(store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(store)
        .unwrap();
    manifest.checkpoint.hash()
}

#[test]
fn activation_installs_one_network_deadline_and_rejects_late_votes() {
    let (mut fixture, _dir, store, selection, _) = setup();
    assert!(
        PreparedActivation::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 1)
            .unwrap()
            .is_none()
    );
    let epoch = activate(&fixture, &store);
    let context = selection
        .replayed()
        .request()
        .vote_context(&selection.certificate().checkpoint)
        .unwrap();
    let bytes = store
        .meta_store()
        .get_meta(&verification_activation::key(context))
        .unwrap();
    let activation = verification_activation::decode(&bytes).unwrap();
    assert_eq!(
        (activation.started_at, activation.deadline),
        (CUTOFF + 31, CUTOFF + 91)
    );
    assert_eq!(activation.target, address(20));
    assert!(
        PreparedActivation::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 32)
            .unwrap()
            .is_none()
    );
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    assert!(ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 90),
        NetworkId::Dev,
        &fixture.params,
        &events
    )
    .is_ok());
    let error = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 91),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .err()
    .unwrap();
    assert!(
        error
            .to_string()
            .contains("deadline requires agreed completion"),
        "{error}"
    );
    let seed = panel(&selection)[0];
    vote_choice_in_epoch(&mut fixture, &selection, seed, CUTOFF + 32, 1, epoch);
    let state = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 32),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    assert_eq!(
        state
            .verification
            .get_state(&address(20))
            .unwrap()
            .votes
            .len(),
        1
    );
    let last = fixture.blocks.last_mut().unwrap();
    last.timestamp = Timestamp::new(CUTOFF + 91);
    sign(seed, last);
    let error = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 91),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("deadline has passed"), "{error}");
    assert_eq!(
        store
            .meta_store()
            .get_meta(&verification_activation::key(context))
            .unwrap(),
        bytes
    );
}

#[test]
fn compact_snapshot_restores_active_panel_and_cannot_skip_its_deadline() {
    use crate::confirmed_replay::tests::snapshot_history::{
        authenticate,
        compact::{assert_same, retained},
        suffix_store,
    };
    for overdue in [false, true] {
        let (mut fixture, _dir, store, selection, _) = setup();
        let epoch = activate(&fixture, &store);
        let snapshot = authenticate(&store, &fixture.params, epoch);
        let witnesses = retained(&snapshot, &fixture, &store, &[]);
        let start = fixture.blocks.len();
        vote_choice_in_epoch(
            &mut fixture,
            &selection,
            panel(&selection)[0],
            CUTOFF + 32,
            1,
            epoch,
        );
        let cutoff = if overdue { CUTOFF + 91 } else { CUTOFF + 32 };
        let full_history = fixture.history(cutoff);
        let (_tail_dir, tail_store) = suffix_store(&fixture.blocks[start..]);
        let tail = snapshot
            .read_tail(&tail_store, cutoff, full_history.heads(), witnesses)
            .unwrap();
        let compact = snapshot.replay_tail(tail, &[selection]);
        if overdue {
            assert!(compact
                .err()
                .unwrap()
                .to_string()
                .contains("deadline requires agreed completion"));
        } else {
            let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
            let full = ConfirmedReplay::replay_with_events(
                &full_history,
                NetworkId::Dev,
                &fixture.params,
                &events,
            )
            .unwrap();
            assert_same(&compact.unwrap(), &full);
        }
    }
}

#[test]
fn activation_transfers_its_beacon_and_recovers_deadline_without_a_relay() {
    let (fixture, _dir, store, selection, first) = setup();
    let epoch = activate(&fixture, &store);
    let second = CertifiedCheckpoint::export_transfer(&store, NetworkId::Dev, &fixture.params, 2)
        .unwrap()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let fresh = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let mut batch = fresh.tx_begin_write().unwrap();
    crate::payment_epoch::initialize(&mut batch, fixture.blocks[0].hash).unwrap();
    batch.commit().unwrap();
    for (height, transfer) in [(1, first), (2, second)] {
        CertifiedCheckpoint::import_transfer(
            &fresh,
            NetworkId::Dev,
            &fixture.params,
            height,
            &transfer,
            &HistoricalAdmissions::default(),
        )
        .unwrap();
        if height == 1 {
            let events = chain::load(&fresh, NetworkId::Dev, &fixture.params).unwrap();
            assert!(events[0].selection().unwrap().beacon().is_none());
        }
        PreparedInstallation::next(&fresh, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap()
            .commit(&fresh)
            .unwrap();
    }
    drop(fresh);
    let fresh = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let events = chain::load(&fresh, NetworkId::Dev, &fixture.params).unwrap();
    assert!(events[0].selection().unwrap().beacon().is_some());
    assert_eq!(fresh.meta_store().payment_epoch().unwrap(), epoch);
    let context = selection
        .replayed()
        .request()
        .vote_context(&selection.certificate().checkpoint)
        .unwrap();
    let key = verification_activation::key(context);
    assert_eq!(
        fresh.meta_store().get_meta(&key).unwrap(),
        store.meta_store().get_meta(&key).unwrap()
    );
    assert!(
        PreparedInstallation::next(&fresh, NetworkId::Dev, &fixture.params)
            .unwrap()
            .is_none()
    );
}

#[test]
fn activation_rejects_forged_proof_cutoff_domain_and_parent() {
    let (fixture, _dir, store, _, _) = setup();
    let candidate =
        PreparedActivation::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 31)
            .unwrap()
            .unwrap();
    let original = candidate.manifest().clone();
    for mutation in 0..6 {
        let mut manifest = original.clone();
        match mutation {
            0 => manifest.activation.value[0] ^= 1,
            1 => manifest.activation.proof[0] ^= 1,
            2 => manifest.activation.selection = BlockHash::ZERO,
            3 => manifest.checkpoint.cutoff = CUTOFF + 1,
            4 => manifest.checkpoint.selection_root = BlockHash::ZERO,
            _ => manifest.checkpoint.parent = BlockHash::ZERO,
        }
        assert!(PreparedActivation::replay_manifest(&store, &fixture.params, &manifest).is_err());
    }
    assert_eq!(
        chain::load(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn activation_fresh_signatures_reject_missing_stale_and_uncertain_time() {
    use crate::{
        checkpoint_agreement::ReplayedCandidate, checkpoint_driver::CheckpointDriver,
        checkpoint_replay::SigningClock,
    };
    let (fixture, _dir, store, selection, _) = setup();
    let candidate =
        PreparedActivation::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 31)
            .unwrap()
            .unwrap();
    let proposer = (0..=8)
        .find(|seed| address(*seed) == *candidate.parent().proposer(0))
        .unwrap();
    let store = std::sync::Arc::new(store);
    let mut driver = CheckpointDriver::new_candidate(
        store.clone(),
        fixture.params.clone(),
        std::sync::Arc::new(burst_crypto::keypair_from_seed(&[proposer; 32]).private),
        ReplayedCandidate::Activation(Box::new(candidate)),
    )
    .unwrap();
    for clock in [
        None,
        Some(SigningClock {
            now: CUTOFF + 91,
            error_bound: 0,
        }),
        Some(SigningClock {
            now: CUTOFF + 32,
            error_bound: 59,
        }),
    ] {
        let error = driver.propose(clock).unwrap_err().to_string();
        assert!(
            error.contains("trusted clock") || error.contains("signing window has elapsed"),
            "{error}"
        );
    }
    assert!(!driver
        .propose(Some(SigningClock {
            now: CUTOFF + 32,
            error_bound: 0
        }))
        .unwrap()
        .is_empty());
    assert_eq!(
        store.meta_store().payment_epoch().unwrap(),
        selection.certificate().checkpoint.hash()
    );
    assert_eq!(
        chain::load(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn activation_preserves_votes_already_cast_by_the_certified_committee() {
    let (mut fixture, _dir, store, selection, _) = setup();
    let voter = panel(&selection)[0];
    vote(&mut fixture, &selection, voter, CUTOFF + 31);
    fixture.append_confirmed_prefix(&store, CUTOFF + 31);
    activate(&fixture, &store);
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let state = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 31),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    assert_eq!(
        state
            .verification
            .get_state(&address(20))
            .unwrap()
            .votes
            .len(),
        1
    );
    assert_eq!(
        state.account(&address(voter)).unwrap().total_brn_staked,
        fixture.params.verifier_stake_amount
    );
}

#[test]
fn activation_rejects_zero_or_overflowing_voting_windows_without_advancing() {
    for timeout in [0, u64::MAX] {
        let (fixture, _dir, store, selection, _) = setup_with(timeout);
        assert!(PreparedActivation::prepare_next(
            &store,
            NetworkId::Dev,
            &fixture.params,
            CUTOFF + 31
        )
        .is_err());
        assert_eq!(
            store.meta_store().payment_epoch().unwrap(),
            selection.certificate().checkpoint.hash()
        );
        assert_eq!(
            chain::load(&store, NetworkId::Dev, &fixture.params)
                .unwrap()
                .len(),
            1
        );
    }
}
