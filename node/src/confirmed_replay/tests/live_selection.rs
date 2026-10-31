use super::*;
use crate::checkpoint_selection;
use crate::checkpoint_store::CertifiedCheckpoint;
use burst_verification::{bootstrap::BootstrapPhase, OrchestratorSnapshot};

const CUTOFF: u64 = 1_595_434_050;

fn setup() -> (
    tempfile::TempDir,
    LmdbStore,
    Fixture,
    CertifiedCheckpoint,
    VerificationOrchestrator,
) {
    let fixture = Fixture::selection();
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
    let checkpoint = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        super::certified_votes::certify(replay.checkpoint(), &[0]),
        heads,
        address(20),
    )
    .unwrap();
    {
        let batch = store.tx_begin_write().unwrap();
        assert!(!checkpoint.live_epoch_ready(&batch).unwrap());
    }
    let live = crate::confirmed_replay::installation::PreparedInstallation::next(
        &store,
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap()
    .unwrap()
    .commit(&store)
    .unwrap()
    .verification;
    (dir, store, fixture, checkpoint, live)
}

fn saved(store: &LmdbStore) -> Vec<u8> {
    crate::verification_storage::load(&store.tx_begin_write().unwrap())
        .unwrap()
        .snapshot()
        .encode()
        .unwrap()
}

#[test]
fn opening_payment_exclusion_requires_closed_epoch_and_absent_account_and_index() {
    use burst_store::meta::MetaStore;
    use burst_types::payment_recovery::ExclusionQuery;
    let (dir, store, fixture, _, _) = setup();
    let account = address(99);
    let old_epoch = crate::genesis_state::block(NetworkId::Dev, &fixture.params).hash;
    let query = ExclusionQuery {
        account: account.to_string(),
        previous: BlockHash::ZERO.to_string(),
        payment_epoch: old_epoch.to_string(),
    };
    assert!(
        crate::payment_exclusion::read(&store, query.clone())
            .unwrap()
            .excluded
    );
    for changed in [
        ExclusionQuery {
            account: address(0).to_string(),
            ..query.clone()
        },
        ExclusionQuery {
            payment_epoch: store.meta_store().payment_epoch().unwrap().to_string(),
            ..query.clone()
        },
        ExclusionQuery {
            payment_epoch: BlockHash::new([99; 32]).to_string(),
            ..query.clone()
        },
    ] {
        assert!(
            !crate::payment_exclusion::read(&store, changed)
                .unwrap()
                .excluded
        );
    }
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    assert!(
        crate::payment_exclusion::read(&store, query.clone())
            .unwrap()
            .excluded
    );
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .put_block_with_account(
            &BlockHash::new([98; 32]),
            b"orphaned opening index",
            &account,
            1,
        )
        .unwrap();
    batch.commit().unwrap();
    assert!(
        !crate::payment_exclusion::read(&store, query)
            .unwrap()
            .excluded
    );
}

#[test]
fn payment_exclusion_requires_installed_closure_and_unchanged_confirmed_frontier() {
    use burst_store::{account::AccountStore, meta::MetaStore};
    use burst_types::payment_recovery::ExclusionQuery;
    let (dir, store, fixture, _, _) = setup();
    let account = address(0);
    let info = store.account_store().get_account(&account).unwrap();
    let old_epoch = crate::genesis_state::block(NetworkId::Dev, &fixture.params).hash;
    let query = ExclusionQuery {
        account: account.to_string(),
        previous: info.head.to_string(),
        payment_epoch: old_epoch.to_string(),
    };
    let review = crate::payment_exclusion::read(&store, query.clone()).unwrap();
    assert!(review.excluded);
    assert_eq!(
        review.installed_epoch,
        store.meta_store().payment_epoch().unwrap().to_string()
    );
    for changed in [
        ExclusionQuery {
            payment_epoch: review.installed_epoch.clone(),
            ..query.clone()
        },
        ExclusionQuery {
            payment_epoch: BlockHash::new([99; 32]).to_string(),
            ..query.clone()
        },
        ExclusionQuery {
            previous: BlockHash::new([98; 32]).to_string(),
            ..query.clone()
        },
        ExclusionQuery {
            account: address(99).to_string(),
            ..query.clone()
        },
    ] {
        assert!(
            !crate::payment_exclusion::read(&store, changed)
                .unwrap()
                .excluded
        );
    }
    let mut provisional = info.clone();
    provisional.head = BlockHash::new([97; 32]);
    provisional.block_count += 1;
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .put_block_with_account(
            &provisional.head,
            b"unconfirmed fixture",
            &account,
            provisional.block_count,
        )
        .unwrap();
    batch.put_account_info(&provisional, false).unwrap();
    batch.commit().unwrap();
    let provisional_query = ExclusionQuery {
        previous: provisional.head.to_string(),
        ..query.clone()
    };
    assert!(
        !crate::payment_exclusion::read(&store, provisional_query)
            .unwrap()
            .excluded
    );
    store.account_store().put_account(&info).unwrap();
    drop(store);
    let reopened = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    assert!(
        crate::payment_exclusion::read(&reopened, query.clone())
            .unwrap()
            .excluded
    );
    reopened
        .meta_store()
        .put_meta(
            &burst_store::meta::closed_payment_epoch_key(&old_epoch),
            b"corrupt",
        )
        .unwrap();
    assert!(crate::payment_exclusion::read(&reopened, query).is_err());
}

fn restore(store: &LmdbStore) -> VerificationOrchestrator {
    VerificationOrchestrator::restore(OrchestratorSnapshot::decode(&saved(store)).unwrap())
}

fn attach(store: &LmdbStore, params: &ProtocolParams, checkpoint: &mut CertifiedCheckpoint) {
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    checkpoint
        .attach_beacon(
            store,
            params,
            burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap(),
        )
        .unwrap();
}

#[test]
fn live_selection_freeze_and_committee_survive_abort_and_restart() {
    let (dir, store, fixture, mut checkpoint, live) = setup();
    let original = saved(&store);
    let mut batch = store.tx_begin_write().unwrap();
    assert!(
        checkpoint_selection::stage(&mut batch, &checkpoint, &fixture.params, &live)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        live.selection_request(&address(20)),
        Some(checkpoint.replayed().request())
    );
    assert!(live.selection_context(&address(20)).is_none());
    drop(batch);
    assert_eq!(saved(&store), original);

    let mut frozen = live;
    assert!(frozen
        .process_endorsement_at(
            &address(20),
            &address(3),
            fixture.params.endorsement_burn_amount,
            &fixture.params,
            Timestamp::new(CUTOFF + 1)
        )
        .is_err());
    let mut batch = store.tx_begin_write().unwrap();
    assert!(
        checkpoint_selection::stage(&mut batch, &checkpoint, &fixture.params, &frozen)
            .unwrap()
            .is_none()
    );
    drop(batch);
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let frozen = restore(&store);
    assert_eq!(
        frozen.selection_request(&address(20)),
        Some(checkpoint.replayed().request())
    );
    assert!(frozen.selection_context(&address(20)).is_none());
    attach(&store, &fixture.params, &mut checkpoint);
    let before_selection = saved(&store);
    let mut batch = store.tx_begin_write().unwrap();
    let selected = checkpoint_selection::stage(&mut batch, &checkpoint, &fixture.params, &frozen)
        .unwrap()
        .unwrap();
    assert!(selected.selection_context(&address(20)).is_some());
    drop(batch);
    assert_eq!(saved(&store), before_selection);
    assert!(frozen.selection_context(&address(20)).is_none());

    let mut batch = store.tx_begin_write().unwrap();
    let selected = checkpoint_selection::stage(&mut batch, &checkpoint, &fixture.params, &frozen)
        .unwrap()
        .unwrap();
    batch.commit().unwrap();
    let committee = selected
        .get_state(&address(20))
        .unwrap()
        .selected_verifiers
        .clone();
    assert_eq!(committee.len(), 2);
    assert!(committee
        .iter()
        .all(|voter| ![address(1), address(2), address(20)].contains(voter)));
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let mut restored = restore(&store);
    assert_eq!(
        restored.selection_context(&address(20)),
        selected.selection_context(&address(20))
    );
    assert_eq!(
        restored.get_state(&address(20)).unwrap().selected_verifiers,
        committee
    );
    assert!(restored.drain_events().is_empty());
    let bytes = saved(&store);
    let mut batch = store.tx_begin_write().unwrap();
    assert!(
        checkpoint_selection::stage(&mut batch, &checkpoint, &fixture.params, &restored)
            .unwrap()
            .is_none()
    );
    drop(batch);
    assert_eq!(saved(&store), bytes);
}

#[test]
fn live_selection_uses_archived_records_and_checks_the_installed_descriptor() {
    use crate::confirmed_replay::installation::{CUTOFF_KEY, INSTALLED_KEY};
    let (_dir, store, fixture, mut checkpoint, live) = setup();
    attach(&store, &fixture.params, &mut checkpoint);
    let epoch = checkpoint.certificate().checkpoint.hash();
    let active = format!(
        "checkpoint_chain/v1/dev/{}/{}",
        checkpoint.parent().genesis_hash(),
        checkpoint.certificate().checkpoint.height
    );
    let archived = format!(
        "checkpoint_record_archive/v1/dev/{}/{}",
        checkpoint.parent().genesis_hash(),
        checkpoint.certificate().checkpoint.height
    );
    let original = store.meta_store().get_meta(&active).unwrap();
    let before = saved(&store);
    for case in 0..6 {
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_meta(&archived, &original).unwrap();
        batch.delete_meta(&active).unwrap();
        match case {
            1 => batch
                .put_meta(CUTOFF_KEY, &(CUTOFF + 1).to_be_bytes())
                .unwrap(),
            2 => batch.put_meta(INSTALLED_KEY, b"corrupt authority").unwrap(),
            3 => batch
                .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &[99; 32])
                .unwrap(),
            4 => batch
                .delete_meta(burst_store::meta::PAYMENT_VOTE_LIMIT_KEY)
                .unwrap(),
            5 => batch
                .put_meta(
                    &burst_store::meta::closed_payment_epoch_key(&epoch),
                    b"closed",
                )
                .unwrap(),
            _ => (),
        }
        let result = checkpoint_selection::stage(&mut batch, &checkpoint, &fixture.params, &live);
        if case == 0 {
            assert!(result
                .unwrap()
                .unwrap()
                .selection_context(&address(20))
                .is_some());
            assert_eq!(batch.get_meta(&archived).unwrap(), Some(original.clone()));
        } else {
            assert!(result.is_err(), "case {case}");
        }
        drop(batch);
        assert_eq!(saved(&store), before);
    }
}

#[test]
fn live_selection_rejects_changed_endorsements_without_overwriting_state() {
    let (_dir, store, fixture, checkpoint, live) = setup();
    let original = saved(&store);
    for mutation in 0..5 {
        let mut snapshot = live.snapshot();
        let state = snapshot.states.get_mut(&address(20)).unwrap();
        match mutation {
            0 => {
                state.endorsements.pop();
            }
            1 => state.endorsements[0].burn_amount += 1,
            2 => state.endorsements[0].timestamp = Timestamp::new(CUTOFF + 1),
            3 => state.endorsements[0].endorser = address(3),
            _ => {
                let duplicate = state.endorsements[0].clone();
                state.endorsements.push(duplicate);
            }
        }
        let changed = VerificationOrchestrator::restore(snapshot);
        let mut batch = store.tx_begin_write().unwrap();
        let error = checkpoint_selection::stage(&mut batch, &checkpoint, &fixture.params, &changed)
            .err()
            .unwrap();
        assert!(
            error.to_string().contains("selection history differs"),
            "{error}"
        );
        drop(batch);
        assert_eq!(saved(&store), original);
        assert_eq!(
            changed.selection_request(&address(20)),
            live.selection_request(&address(20))
        );
    }
    let mut snapshot = live.snapshot();
    snapshot
        .states
        .get_mut(&address(20))
        .unwrap()
        .endorsements
        .reverse();
    let reversed = VerificationOrchestrator::restore(snapshot);
    let mut batch = store.tx_begin_write().unwrap();
    assert!(
        checkpoint_selection::stage(&mut batch, &checkpoint, &fixture.params, &reversed)
            .unwrap()
            .is_none()
    );
}

#[test]
fn live_selection_requires_confirmed_history_bootstrap_exit_and_matching_parameters() {
    let (_dir, store, fixture, checkpoint, live) = setup();
    let original = saved(&store);
    for mutation in 0..3 {
        let mut batch = store.tx_begin_write().unwrap();
        match mutation {
            0 => batch
                .put_brn_meta(BootstrapPhase::KEY, &BootstrapPhase::Active.encoding())
                .unwrap(),
            1 => {
                let mut changed = fixture.params.clone();
                changed.brn_rate += 1;
                batch
                    .put_brn_meta(b"protocol_params", &bincode::serialize(&changed).unwrap())
                    .unwrap();
            }
            _ => {
                let head = checkpoint
                    .heads()
                    .iter()
                    .find(|head| head.account == address(1))
                    .unwrap();
                batch
                    .put_block_with_account(
                        &BlockHash::new([99; 32]),
                        b"corrupt index",
                        &head.account,
                        head.height,
                    )
                    .unwrap();
            }
        }
        assert!(
            checkpoint_selection::stage(&mut batch, &checkpoint, &fixture.params, &live).is_err(),
            "mutation {mutation} must be rejected"
        );
        drop(batch);
        assert_eq!(saved(&store), original);
    }
    let (_other_dir, other_store, _) = fixture.store();
    let mut batch = other_store.tx_begin_write().unwrap();
    let error = checkpoint_selection::stage(&mut batch, &checkpoint, &fixture.params, &live)
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("certificate is not persisted"),
        "{error}"
    );
}

#[tokio::test]
async fn live_selection_publishes_after_commit_and_recovers_a_previously_attached_beacon() {
    use std::sync::Arc;
    use tokio::sync::Mutex;
    let (_dir, store, fixture, mut checkpoint, live) = setup();
    attach(&store, &fixture.params, &mut checkpoint);
    let store = Arc::new(store);
    let live = Arc::new(Mutex::new(live));
    let checkpoint = checkpoint_selection::install(
        store.clone(),
        live.clone(),
        checkpoint,
        fixture.params.clone(),
    )
    .await
    .unwrap()
    .unwrap();
    let context = live.lock().await.selection_context(&address(20)).unwrap();
    assert_eq!(
        restore(&store).selection_context(&address(20)),
        Some(context)
    );
    let before = saved(&store);
    checkpoint_selection::install(store.clone(), live.clone(), checkpoint, fixture.params)
        .await
        .unwrap();
    assert_eq!(saved(&store), before);
    assert_eq!(
        live.lock().await.selection_context(&address(20)),
        Some(context)
    );
}

#[test]
fn live_selection_cannot_replace_a_challenge_revote_or_uncommitted_transition() {
    use burst_verification::state::VerificationPhase;
    let (_dir, store, fixture, checkpoint, live) = setup();
    let before = saved(&store);
    for phase in [
        VerificationPhase::Challenged,
        VerificationPhase::Voting,
        VerificationPhase::Verified,
        VerificationPhase::Failed,
        VerificationPhase::Unverified,
        VerificationPhase::Deactivated,
    ] {
        let mut snapshot = live.snapshot();
        snapshot.states.get_mut(&address(20)).unwrap().phase = phase;
        let changed = VerificationOrchestrator::restore(snapshot);
        let mut batch = store.tx_begin_write().unwrap();
        assert!(
            checkpoint_selection::stage(&mut batch, &checkpoint, &fixture.params, &changed)
                .is_err()
        );
        drop(batch);
        assert_eq!(saved(&store), before);
    }
    let mut snapshot = live.snapshot();
    snapshot.states.get_mut(&address(20)).unwrap().revote_count = 1;
    let changed = VerificationOrchestrator::restore(snapshot);
    let mut batch = store.tx_begin_write().unwrap();
    assert!(
        checkpoint_selection::stage(&mut batch, &checkpoint, &fixture.params, &changed).is_err()
    );
    drop(batch);

    let mut pending = live.clone();
    for seed in [1, 2] {
        pending
            .process_endorsement_at(
                &address(21),
                &address(seed),
                fixture.params.endorsement_burn_amount,
                &fixture.params,
                Timestamp::new(CUTOFF),
            )
            .unwrap();
    }
    let mut batch = store.tx_begin_write().unwrap();
    let error = checkpoint_selection::stage(&mut batch, &checkpoint, &fixture.params, &pending)
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("uncommitted lifecycle events"),
        "{error}"
    );
    drop(batch);
    assert!(!pending.drain_events().is_empty());
    assert_eq!(saved(&store), before);
}

#[test]
fn live_selection_checks_a_restored_committee_even_when_its_context_matches() {
    let (_dir, store, fixture, mut checkpoint, live) = setup();
    attach(&store, &fixture.params, &mut checkpoint);
    let mut batch = store.tx_begin_write().unwrap();
    let selected = checkpoint_selection::stage(&mut batch, &checkpoint, &fixture.params, &live)
        .unwrap()
        .unwrap();
    batch.commit().unwrap();
    let before = saved(&store);
    let mut snapshot = selected.snapshot();
    snapshot
        .states
        .get_mut(&address(20))
        .unwrap()
        .selected_verifiers[0] = address(20);
    let changed = VerificationOrchestrator::restore(snapshot);
    assert_eq!(
        changed.selection_context(&address(20)),
        selected.selection_context(&address(20))
    );
    let mut batch = store.tx_begin_write().unwrap();
    let error = checkpoint_selection::stage(&mut batch, &checkpoint, &fixture.params, &changed)
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("installed committee differs"),
        "{error}"
    );
    drop(batch);
    assert_eq!(saved(&store), before);
}

#[cfg(test)]
mod payment_exclusion_cache;
