use super::*;
use crate::checkpoint_store::{chain as events, historical::HistoricalAdmissions};
use crate::verification_commit::receipts;

fn latest(state: &ConfirmedReplay) -> receipts::Receipt {
    let source = BlockHash::new(
        state.lifecycle_metadata[&receipts::latest_key(&address(20))]
            .as_slice()
            .try_into()
            .unwrap(),
    );
    receipts::decode(&state.lifecycle_metadata[&receipts::key(source)]).unwrap()
}

fn pending_appeal(
    reason: ChallengeReason,
) -> (Fixture, tempfile::TempDir, LmdbStore, CertifiedCheckpoint) {
    let (mut fixture, first, _) = pending_with(reason, |params| params.trst_expiry_secs = u64::MAX);
    let (_dir, _store, second) = certify_challenge(&fixture, &first);
    for voter in panel(&second) {
        vote_choice(&mut fixture, &second, voter, CUTOFF + 60, 2);
    }
    merged_payments(&mut fixture);
    let (directory, store, heads) = restore_chain(&fixture, &[first, second]);
    let outcome = PreparedChallengeOutcome::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 70,
        &heads,
        &address(20),
    )
    .unwrap();
    let certificate = certify(&outcome.manifest().checkpoint, &[0, 1, 2, 3, 4, 5, 6]);
    let outcome = outcome
        .certify(certificate)
        .unwrap()
        .persist(&store, NetworkId::Dev, &fixture.params)
        .unwrap();
    let root = latest(outcome.state());
    let epoch = outcome.certificate().checkpoint.hash();
    let beacon_time = burst_vrf::DrandClient::new()
        .chain_info()
        .unwrap()
        .time_of_round(1005)
        .unwrap();
    let stake = fixture.params.challenge_stake_amount;
    let admission = fixture.push(8, BlockType::Challenge, beacon_time - 1, |block| {
        block.payment_epoch = epoch;
        block.link = destination(20);
        block.origin = TxHash::new(reason.to_origin());
        block.transaction = TxHash::ZERO;
        block.target_head = root.source;
        block.brn_balance += stake;
    });
    fixture.append_confirmed_prefix(&store, beacon_time - 1);
    let selection =
        CertifiedCheckpoint::prepare_next(&store, NetworkId::Dev, &fixture.params, beacon_time - 1)
            .unwrap()
            .unwrap();
    assert_eq!(
        selection.lifecycle(),
        SelectionLifecycle::Challenge(admission)
    );
    let mut selection = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certify(selection.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]),
        selection.manifest().heads,
        address(20),
    )
    .unwrap();
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/selection-beacons.json"
    )))
    .unwrap();
    selection
        .attach_beacon(
            &store,
            &fixture.params,
            burst_vrf::DrandClient::beacon_to_random_output(
                beacons.iter().find(|beacon| beacon.round == 1005).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    (fixture, directory, store, selection)
}

fn corrected(reason: ChallengeReason) -> (Fixture, tempfile::TempDir, LmdbStore) {
    corrected_from(reason, false)
}

fn corrected_from(
    reason: ChallengeReason,
    activate: bool,
) -> (Fixture, tempfile::TempDir, LmdbStore) {
    let (mut fixture, directory, store, selection) = pending_appeal(reason);
    let beacon_time = selection.replayed().request().beacon_time().unwrap();
    let prefix = events::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let events::CertifiedEvent::Outcome(outcome) = &prefix[prefix.len() - 2] else {
        panic!("missing reviewed outcome")
    };
    let root = latest(outcome.state());
    let previous_state = outcome.state().account(&address(20)).unwrap().state;
    let vote_epoch = if activate {
        let activation = crate::checkpoint_activation::PreparedActivation::prepare_next(
            &store,
            NetworkId::Dev,
            &fixture.params,
            beacon_time,
        )
        .unwrap()
        .unwrap();
        let manifest = activation.manifest().clone();
        let epoch = manifest.checkpoint.hash();
        events::accept_manifest(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 6]),
            &crate::checkpoint_agreement::ReplayManifest::Activation(manifest),
        )
        .unwrap();
        epoch
    } else {
        selection.certificate().checkpoint.hash()
    };
    for voter in panel(&selection) {
        vote_choice_in_epoch(
            &mut fixture,
            &selection,
            voter,
            beacon_time + 1,
            1,
            vote_epoch,
        );
    }
    let prefix = events::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let uncommitted = ConfirmedReplay::replay_with_events(
        &fixture.history(beacon_time + 1),
        NetworkId::Dev,
        &fixture.params,
        &prefix,
    )
    .unwrap();
    assert_eq!(
        uncommitted.account(&address(20)).unwrap().state,
        previous_state
    );
    assert_eq!(uncommitted.human_roster().unwrap().weight(&address(20)), 0);
    assert_eq!(latest(&uncommitted), root);
    fixture.append_confirmed_prefix(&store, beacon_time + 1);
    let heads = fixture.history(beacon_time + 1).heads().to_vec();
    let prepare = || {
        PreparedChallengeOutcome::prepare(
            &store,
            NetworkId::Dev,
            &fixture.params,
            beacon_time + 2,
            &heads,
            &address(20),
        )
        .unwrap()
    };
    let candidate = prepare();
    let checkpoint = &candidate.manifest().checkpoint;
    // The restored target cannot vote itself back into the previous electorate.
    assert!(prepare()
        .certify(certify(checkpoint, &[0, 1, 2, 3, 4, 5, 20]))
        .is_err());
    let certificate = certify(checkpoint, &[0, 1, 2, 3, 4, 5, 6]);
    let corrected = candidate
        .certify(certificate)
        .unwrap()
        .persist(&store, NetworkId::Dev, &fixture.params)
        .unwrap();
    assert_eq!(corrected.next_roster().weight(&address(20)), 1);
    assert_eq!(
        corrected.state().account(&address(20)).unwrap().state,
        WalletState::Verified
    );
    let final_receipt = latest(corrected.state());
    assert!(
        matches!(final_receipt.verdict, receipts::Verdict::Appeal { reviewed, .. } if reviewed == root.source)
    );
    for reward in &root.rewards {
        assert!(corrected
            .state()
            .trst
            .merger_graph
            .is_origin_revoked(&reward.origin));
    }
    for holder in [1, 8] {
        let active: u128 = corrected
            .state()
            .portfolio(&address(holder))
            .unwrap()
            .tokens
            .iter()
            .filter(|token| token.state == burst_types::TrstState::Active)
            .map(|token| token.amount)
            .sum();
        assert_eq!(active, 10_001);
    }
    let expected_brn: Vec<_> = (0..=8)
        .chain([20])
        .map(|seed| bincode::serialize(&corrected.state().brn.get_wallet(&address(seed))).unwrap())
        .collect();
    let expected_trst = corrected.state().trst.save_wallets();
    crate::confirmed_replay::tests::snapshot_history::boundaries::assert_chain(&fixture, &store);
    let height = corrected.certificate().checkpoint.height;
    let transfers: Vec<_> = (1..=height)
        .map(|height| {
            CertifiedCheckpoint::export_transfer(&store, NetworkId::Dev, &fixture.params, height)
                .unwrap()
                .unwrap()
        })
        .collect();
    drop(store);
    let reopened = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
    let recovered = events::load(&reopened, NetworkId::Dev, &fixture.params).unwrap();
    let events::CertifiedEvent::Outcome(last) = recovered.last().unwrap() else {
        panic!("missing appeal outcome")
    };
    assert_eq!(latest(last.state()), final_receipt);
    assert_eq!(last.state().trst.save_wallets(), expected_trst);
    let initial = Fixture {
        params: fixture.params.clone(),
        blocks: vec![fixture.blocks[0].clone()],
    };
    let (_fresh_dir, fresh, _) = initial.store();
    let admissions = HistoricalAdmissions::default();
    for (index, transfer) in transfers.iter().enumerate() {
        CertifiedCheckpoint::import_transfer(
            &fresh,
            NetworkId::Dev,
            &fixture.params,
            index as u64 + 1,
            transfer,
            &admissions,
        )
        .unwrap();
    }
    let recovered = events::load(&fresh, NetworkId::Dev, &fixture.params).unwrap();
    let events::CertifiedEvent::Outcome(last) = recovered.last().unwrap() else {
        panic!("missing imported appeal")
    };
    assert_eq!(latest(last.state()), final_receipt);
    let recovered_brn: Vec<_> = (0..=8)
        .chain([20])
        .map(|seed| bincode::serialize(&last.state().brn.get_wallet(&address(seed))).unwrap())
        .collect();
    assert_eq!(recovered_brn, expected_brn);
    assert_eq!(last.state().trst.save_wallets(), expected_trst);
    (fixture, directory, reopened)
}

#[test]
fn certified_appeal_restores_identity_and_provenance_only_after_old_roster_quorum_and_transfers() {
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        corrected(reason);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_appeal_archive_preserves_restoration_and_reward_invalidation() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        let (fixture, directory, store) = corrected(reason);
        crate::confirmed_replay::tests::snapshot_history::installed_recovery::installation::live::bootstrap::assert_lifecycle_archive(fixture, directory, store).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_appeal_archive_preserves_active_review_and_original_judgment() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        let (fixture, directory, store, selection) = pending_appeal(reason);
        let activation = crate::checkpoint_activation::PreparedActivation::prepare_next(
            &store,
            NetworkId::Dev,
            &fixture.params,
            selection.replayed().request().beacon_time().unwrap() + 1,
        )
        .unwrap()
        .unwrap();
        let manifest = activation.manifest().clone();
        assert_eq!(
            manifest.activation.selection,
            selection.certificate().checkpoint.hash()
        );
        events::accept_manifest(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 6]),
            &crate::checkpoint_agreement::ReplayManifest::Activation(manifest),
        )
        .unwrap();
        crate::confirmed_replay::tests::snapshot_history::installed_recovery::installation::live::bootstrap::assert_lifecycle_archive(fixture, directory, store).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_appeal_archive_continues_through_certified_outcome() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        let (fixture, directory, store) = corrected_from(reason, true);
        crate::confirmed_replay::tests::snapshot_history::installed_recovery::installation::live::bootstrap::progression::assert_successor(fixture, directory, store, 5).await;
    }
}
