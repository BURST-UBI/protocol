use super::*;
use crate::{
    checkpoint_agreement::{ReplayManifest, ReplayedCandidate},
    checkpoint_driver::CheckpointDriver,
    checkpoint_replay::SigningClock,
    checkpoint_roster::PreparedRoster,
    checkpoint_store::{chain, historical::HistoricalAdmissions, CertifiedCheckpoint},
    confirmed_replay::installation::PreparedInstallation,
};

fn fixture() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.params.bootstrap_exit_threshold = 2;
    fixture.blocks[0] = crate::genesis_state::block(NetworkId::Dev, &fixture.params);
    fixture.push(0, BlockType::Endorse, 100, |block| {
        block.brn_balance = 1;
        block.link = destination(1);
    });
    fixture.push(1, BlockType::Open, 101, |_| {});
    fixture
}

fn candidate(store: &LmdbStore, params: &ProtocolParams, cutoff: u64) -> PreparedRoster {
    PreparedRoster::prepare_next(store, NetworkId::Dev, params, cutoff)
        .unwrap()
        .unwrap()
}

fn accept(store: &LmdbStore, params: &ProtocolParams, candidate: &PreparedRoster, seeds: &[u8]) {
    chain::accept_manifest(
        store,
        NetworkId::Dev,
        params,
        certified_votes::certify(&candidate.manifest().checkpoint, seeds),
        &ReplayManifest::Roster(candidate.manifest().clone()),
    )
    .unwrap();
}

#[test]
fn stale_action_recovery_requires_certified_cut_and_survives_snapshot_replay() {
    let mut fixture = fixture();
    let (_dir, store, _) = fixture.store();
    let first = candidate(&store, &fixture.params, 110);
    let epoch = first.manifest().checkpoint.hash();
    accept(&store, &fixture.params, &first, &[0]);
    PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap().unwrap().commit(&store).unwrap();
    assert!(PreparedRoster::prepare_next(&store, NetworkId::Dev, &fixture.params, 200)
        .unwrap().is_none(), "idle time alone must not request a recovery cut");
    fixture.push(0, BlockType::ChangeRepresentative, 111, |block| {
        block.payment_epoch = epoch;
        block.representative = address(0);
    });
    let pending = fixture.blocks.pop().unwrap();
    let query = burst_types::payment_recovery::ExclusionQuery {
        account: pending.account.to_string(), previous: pending.previous.to_string(),
        payment_epoch: epoch.to_string(),
    };
    assert!(!crate::payment_exclusion::read(&store, query.clone()).unwrap().excluded);
    crate::checkpoint_recovery::request(&store, &pending).unwrap();
    assert!(!crate::payment_exclusion::read(&store, query.clone()).unwrap().excluded,
        "a local recovery request must not exclude an action");
    let recovery = candidate(&store, &fixture.params, 200);
    let electorate = recovery.parent().roster().clone();
    assert_eq!(recovery.manifest().checkpoint.selection_root,
        crate::checkpoint_recovery::commitment());
    let mut wrong_domain = recovery.manifest().clone();
    wrong_domain.checkpoint.selection_root = crate::checkpoint_roster::commitment();
    assert!(PreparedRoster::replay_manifest(&store, &fixture.params, &wrong_domain).is_err());
    assert!(PreparedRoster::replay_manifest(&store, &fixture.params, recovery.manifest())
        .unwrap().certify(certified_votes::certify(&recovery.manifest().checkpoint, &[0])).is_err(),
        "one of two humans cannot close the epoch alone");
    accept(&store, &fixture.params, &recovery, &[0, 1]);
    let installed = PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap().unwrap().commit(&store).unwrap();
    assert_eq!(installed.anchor.roster(), &electorate);
    assert!(crate::payment_exclusion::read(&store, query).unwrap().excluded);
    assert!(PreparedRoster::prepare_next(&store, NetworkId::Dev, &fixture.params, 300)
        .unwrap().is_none(), "a demand from the closed epoch must not repeat");
    crate::confirmed_replay::tests::snapshot_history::boundaries::assert_chain(&fixture, &store);
}

#[test]
fn roster_checkpoint_replays_delegation_and_installs_across_archival_catchup() {
    let mut fixture = fixture();
    let (dir, store, _) = fixture.store();
    let first = candidate(&store, &fixture.params, 110);
    assert_eq!(first.parent().roster().total_humans(), 1);
    let epoch = first.manifest().checkpoint.hash();
    // The newly verified human cannot self-authorize the enlarged roster.
    assert!(
        PreparedRoster::replay_manifest(&store, &fixture.params, first.manifest())
            .unwrap()
            .certify(certified_votes::certify(&first.manifest().checkpoint, &[1]))
            .is_err()
    );
    accept(&store, &fixture.params, &first, &[0]);
    let installed = PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    assert_eq!(installed.anchor.hash(), epoch);
    assert_eq!(installed.anchor.roster().weight(&address(1)), 1);
    assert!(
        PreparedRoster::prepare_next(&store, NetworkId::Dev, &fixture.params, 111)
            .unwrap()
            .is_none()
    );
    assert!(crate::checkpoint_beacon::pending_selections(
        chain::load(&store, NetworkId::Dev, &fixture.params).unwrap()
    )
    .is_empty());

    fixture.push(1, BlockType::ChangeRepresentative, 112, |block| {
        block.payment_epoch = epoch;
        block.representative = address(0);
    });
    fixture.append_confirmed_prefix(&store, 112);
    let second = candidate(&store, &fixture.params, 114);
    assert_eq!(second.parent().roster().weight(&address(0)), 1);
    // Both old representatives must sign even though the new roster delegates
    // both human units to the genesis representative.
    assert!(
        PreparedRoster::replay_manifest(&store, &fixture.params, second.manifest())
            .unwrap()
            .certify(certified_votes::certify(
                &second.manifest().checkpoint,
                &[0]
            ))
            .is_err()
    );
    accept(&store, &fixture.params, &second, &[0, 1]);
    crate::confirmed_replay::tests::snapshot_history::boundaries::assert_chain(&fixture, &store);
    let installed = PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    assert_eq!(installed.anchor.roster().total_humans(), 2);
    assert_eq!(installed.anchor.roster().weight(&address(0)), 2);
    assert_eq!(installed.anchor.roster().weight(&address(1)), 0);
    let expected_epoch = installed.anchor.hash();
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let authority =
        crate::payment_authority::initialize(&store, NetworkId::Dev, &fixture.params).unwrap();
    assert_eq!(authority.epoch, expected_epoch);
    assert_eq!(authority.roster.weight(&address(0)), 2);

    let fresh_dir = tempfile::tempdir().unwrap();
    let fresh = LmdbStore::open(fresh_dir.path(), 32, 32 << 20).unwrap();
    let mut batch = fresh.tx_begin_write().unwrap();
    crate::payment_epoch::initialize(&mut batch, fixture.blocks[0].hash).unwrap();
    batch.commit().unwrap();
    for height in 1..=2 {
        let transfer =
            CertifiedCheckpoint::export_transfer(&store, NetworkId::Dev, &fixture.params, height)
                .unwrap()
                .unwrap();
        CertifiedCheckpoint::import_transfer(
            &fresh,
            NetworkId::Dev,
            &fixture.params,
            height,
            &transfer,
            &HistoricalAdmissions::default(),
        )
        .unwrap();
        PreparedInstallation::next(&fresh, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap()
            .commit(&fresh)
            .unwrap();
    }
    let authority =
        crate::payment_authority::initialize(&fresh, NetworkId::Dev, &fixture.params).unwrap();
    assert_eq!(authority.epoch, expected_epoch);
    assert_eq!(authority.roster.weight(&address(0)), 2);
    for account in [address(0), address(1)] {
        assert_eq!(
            bincode::serialize(&fresh.account_store().get_account(&account).unwrap()).unwrap(),
            bincode::serialize(&store.account_store().get_account(&account).unwrap()).unwrap()
        );
    }
}

#[test]
fn roster_checkpoint_rejects_forged_manifest_and_untrusted_clock() {
    let fixture = fixture();
    let (_dir, store, _) = fixture.store();
    let prepared = candidate(&store, &fixture.params, 110);
    for field in 0..4 {
        let mut manifest = prepared.manifest().clone();
        match field {
            0 => manifest.checkpoint.selection_root = BlockHash::new([77; 32]),
            1 => manifest.checkpoint.next_roster = BlockHash::new([77; 32]),
            2 => manifest.checkpoint.next_quorum_bps = 6667,
            _ => manifest.heads.pop().map(|_| ()).unwrap(),
        }
        assert!(PreparedRoster::replay_manifest(&store, &fixture.params, &manifest).is_err());
        assert!(chain::accept_manifest(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certified_votes::certify(&manifest.checkpoint, &[0]),
            &ReplayManifest::Roster(manifest)
        )
        .is_err());
        assert_eq!(
            CertifiedCheckpoint::stored_tip(&store, NetworkId::Dev, &fixture.params).unwrap(),
            0
        );
    }
    let store = std::sync::Arc::new(store);
    let key = burst_crypto::keypair_from_seed(&[0; 32]);
    let mut driver = CheckpointDriver::new_candidate(
        store.clone(),
        fixture.params.clone(),
        key.private,
        ReplayedCandidate::Roster(Box::new(prepared)),
    )
    .unwrap();
    assert!(driver.propose(None).is_err());
    assert!(driver
        .propose(Some(SigningClock {
            now: 109,
            error_bound: 0
        }))
        .is_err());
    let output = driver
        .propose(Some(SigningClock {
            now: 120,
            error_bound: 0,
        }))
        .unwrap();
    assert!(!output.is_empty());
    assert!(driver.persist_decision().unwrap());
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    assert!(matches!(&events[0], chain::CertifiedEvent::Roster(_)));
    assert!(crate::checkpoint_agreement::read_candidates(
        &store,
        driver.anchor(),
        &address(0),
        &fixture.params
    )
    .unwrap()
    .iter()
    .all(|value| matches!(value, ReplayedCandidate::Roster(_))));
}

#[test]
fn roster_checkpoint_does_not_advance_an_unchanged_genesis_roster() {
    let fixture = Fixture::new();
    let (_dir, store, _) = fixture.store();
    assert!(
        PreparedRoster::prepare_next(&store, NetworkId::Dev, &fixture.params, 110)
            .unwrap()
            .is_none()
    );
}

#[test]
fn roster_checkpoint_dilutes_genesis_before_bootstrap_exit_and_replays_later_admission() {
    let mut fixture = Fixture::new();
    fixture.params.bootstrap_exit_threshold = 3;
    fixture.blocks[0] = crate::genesis_state::block(NetworkId::Dev, &fixture.params);
    fixture.push(0, BlockType::Endorse, 100, |block| {
        block.brn_balance = 1;
        block.link = destination(1);
    });
    fixture.push(1, BlockType::Open, 101, |_| {});
    let (_dir, store, _) = fixture.store();
    let first = candidate(&store, &fixture.params, 110);
    accept(&store, &fixture.params, &first, &[0]);
    let installed = PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    assert_eq!(installed.anchor.roster().total_humans(), 2);
    let epoch = installed.anchor.hash();
    fixture.push(0, BlockType::Endorse, 112, |block| {
        block.payment_epoch = epoch;
        block.brn_balance += 1;
        block.link = destination(2);
    });
    fixture.push(2, BlockType::Open, 111, |_| {});
    fixture.push(2, BlockType::Burn, 114, |block| {
        block.brn_balance = 1;
        block.link = destination(0);
    });
    fixture.append_confirmed_prefix(&store, 115);
    let second = candidate(&store, &fixture.params, 116);
    accept(&store, &fixture.params, &second, &[0, 1]);
    let installed = PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    assert_eq!(installed.anchor.roster().total_humans(), 3);
    assert_eq!(installed.anchor.roster().weight(&address(0)), 1);
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let state = ConfirmedReplay::replay_with_events(
        &fixture.history(116),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    assert!(state.bootstrap_complete());
    assert_eq!(state.account(&address(2)).unwrap().total_brn_burned, 1);
}

#[test]
fn roster_checkpoint_cannot_sign_away_a_prior_final_payment_promise() {
    let fixture = fixture();
    let (_dir, store, _) = fixture.store();
    let prepared = candidate(&store, &fixture.params, 110);
    let epoch = prepared.parent().hash();
    let head = fixture
        .blocks
        .iter()
        .rev()
        .find(|block| block.account == address(0))
        .unwrap()
        .hash;
    let promised = BlockHash::new([61; 32]);
    store
        .meta_store()
        .reserve_consensus_vote(&epoch, &address(0), &address(0), &head, &promised, true, 0)
        .unwrap();
    let store = std::sync::Arc::new(store);
    let key = burst_crypto::keypair_from_seed(&[0; 32]);
    let mut driver = CheckpointDriver::new_candidate(
        store.clone(),
        fixture.params.clone(),
        key.private,
        ReplayedCandidate::Roster(Box::new(prepared)),
    )
    .unwrap();
    assert!(driver
        .propose(Some(SigningClock {
            now: 120,
            error_bound: 0
        }))
        .is_err());
    assert!(driver.state().unwrap().is_none());
    assert_eq!(store.meta_store().payment_epoch().unwrap(), epoch);
    assert_eq!(
        CertifiedCheckpoint::stored_tip(&store, NetworkId::Dev, &fixture.params).unwrap(),
        0
    );
}

#[test]
fn roster_checkpoint_installer_preserves_a_later_cemented_payment() {
    let mut fixture = fixture();
    let (_dir, store, _) = fixture.store();
    let prepared = candidate(&store, &fixture.params, 110);
    accept(&store, &fixture.params, &prepared, &[0]);
    let hash = fixture.push(0, BlockType::Burn, 112, |block| {
        block.brn_balance += 1;
        block.link = destination(1);
    });
    let block = fixture.blocks.last().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    let mut account = batch.get_account(&address(0)).unwrap();
    account.block_count += 1;
    account.confirmation_height = account.block_count;
    account.head = hash;
    batch
        .put_block_with_account(
            &hash,
            &bincode::serialize(block).unwrap(),
            &address(0),
            account.block_count,
        )
        .unwrap();
    batch.put_account_info(&account, false).unwrap();
    batch.put_frontier(&address(0), &hash).unwrap();
    batch.commit().unwrap();
    let before =
        bincode::serialize(&store.account_store().get_account(&address(0)).unwrap()).unwrap();
    let prepared = PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap();
    assert!(prepared.commit(&store).is_err());
    assert_eq!(
        store.meta_store().payment_epoch().unwrap(),
        fixture.blocks[0].hash
    );
    assert_eq!(
        bincode::serialize(&store.account_store().get_account(&address(0)).unwrap()).unwrap(),
        before
    );
    assert_eq!(
        store.block_store().get_block(&hash).unwrap(),
        bincode::serialize(block).unwrap()
    );
    assert!(store
        .meta_store()
        .get_meta(crate::confirmed_replay::installation::INSTALLED_KEY)
        .is_err());
}

#[test]
fn roster_epoch_rejects_competing_creator_and_ordinary_admissions() {
    let mut fixture = Fixture::new();
    fixture.params.bootstrap_exit_threshold = 3;
    fixture.blocks[0] = crate::genesis_state::block(NetworkId::Dev, &fixture.params);
    fixture.push(0, BlockType::Endorse, 100, |block| {
        block.brn_balance = 1;
        block.link = destination(1);
    });
    fixture.push(1, BlockType::Open, 101, |_| {});
    let (_dir, store, _) = fixture.store();
    let first = candidate(&store, &fixture.params, 110);
    accept(&store, &fixture.params, &first, &[0]);
    let epoch = first.manifest().checkpoint.hash();
    fixture.push(1, BlockType::Endorse, 111, |block| {
        block.payment_epoch = epoch;
        block.brn_balance += 1;
        block.link = destination(2);
    });
    fixture.push(0, BlockType::Endorse, 112, |block| {
        block.brn_balance += 1;
        block.link = destination(2);
    });
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let error = match ConfirmedReplay::replay_with_events(
        &fixture.history(115),
        NetworkId::Dev,
        &fixture.params,
        &events,
    ) {
        Ok(_) => panic!("ambiguous admission was replayed"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("competing bootstrap"), "{error}");
}

#[test]
fn roster_checkpoint_recovers_a_decided_journal_without_signing_or_installing_again() {
    use crate::checkpoint_service::{Clock, Runtime, Settings};
    use std::{sync::Arc, time::Instant};
    let fixture = fixture();
    let (dir, store, _) = fixture.store();
    let prepared = candidate(&store, &fixture.params, 110);
    let parent = prepared.parent().clone();
    let store = Arc::new(store);
    let key = Arc::new(burst_crypto::keypair_from_seed(&[0; 32]).private);
    let mut driver = CheckpointDriver::new_candidate(
        store.clone(),
        fixture.params.clone(),
        key.clone(),
        ReplayedCandidate::Roster(Box::new(prepared)),
    )
    .unwrap();
    driver
        .propose(Some(SigningClock {
            now: 120,
            error_bound: 0,
        }))
        .unwrap();
    assert!(driver.state().unwrap().unwrap().decision().is_some());
    assert_eq!(
        CertifiedCheckpoint::stored_tip(&store, NetworkId::Dev, &fixture.params).unwrap(),
        0
    );
    let journal = bincode::serialize(&driver.state().unwrap().unwrap()).unwrap();
    let before =
        bincode::serialize(&store.account_store().get_account(&address(1)).unwrap()).unwrap();
    drop(driver);
    drop(store);
    let store = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
    let mut runtime = Runtime::new(Settings {
        store: store.clone(),
        network: NetworkId::Dev,
        initial_params: fixture.params.clone(),
        key: Some(key),
    })
    .unwrap();
    let no_clock: Clock = Arc::new(|| None);
    runtime.poll(&[], Instant::now(), &no_clock).unwrap();
    runtime.poll(&[], Instant::now(), &no_clock).unwrap();
    assert_eq!(
        CertifiedCheckpoint::stored_tip(&store, NetworkId::Dev, &fixture.params).unwrap(),
        1
    );
    assert_eq!(
        store.meta_store().payment_epoch().unwrap(),
        fixture.blocks[0].hash
    );
    assert_eq!(
        bincode::serialize(&store.account_store().get_account(&address(1)).unwrap()).unwrap(),
        before
    );
    assert_eq!(
        bincode::serialize(
            &crate::checkpoint_agreement::read_state(&store, &parent, &address(0))
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        journal
    );
    PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    runtime
        .poll(
            &[],
            Instant::now() + std::time::Duration::from_secs(2),
            &no_clock,
        )
        .unwrap();
    assert_eq!(
        CertifiedCheckpoint::stored_tip(&store, NetworkId::Dev, &fixture.params).unwrap(),
        1
    );
}
