use super::*;
use crate::{
    checkpoint_activation::PreparedActivation,
    checkpoint_agreement::ReplayManifest,
    checkpoint_deadline::PreparedDeadline,
    checkpoint_store::{chain, historical::HistoricalAdmissions},
    confirmed_replay::installation::PreparedInstallation,
    verification_activation, verification_deadline,
};

const START: u64 = CUTOFF + 61;
const END: u64 = START + 5;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_challenge_archive_continues_through_certified_deadline() {
    assert_live_deadline(u64::MAX, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pruned_live_challenge_deadline_preserves_pending_returns_after_restart() {
    assert_live_deadline(31, true).await;
}

async fn assert_live_deadline(expiry: u64, retire: bool) {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        let (mut fixture, directory, store, selection, _) =
            setup_with_expiry(reason, 0, 9000, expiry);
        let epoch = store.meta_store().payment_epoch().unwrap();
        vote_choice_in_epoch(
            &mut fixture,
            &selection,
            panel(&selection)[0],
            START + 1,
            2,
            epoch,
        );
        fixture.append_confirmed_prefix(&store, START + 1);
        finish(&fixture, &store);
        if retire {
            let state = state_at(&fixture, &store, END + 1);
            assert_eq!(state.pending.len(), 2);
            assert!(state.pending.values().all(|pending| pending.return_credit));
            let target = state.account(&address(20)).unwrap();
            let expected = if reason == ChallengeReason::Fraud {
                (1, 1)
            } else {
                (2, 0)
            };
            assert_eq!((target.expired_trst, target.revoked_trst), expected);
            assert_eq!(target.trst_balance, 0);
            crate::confirmed_replay::tests::snapshot_history::installed_recovery::installation::live::bootstrap::progression::assert_successor_after_retirement(fixture, directory, store, 3).await;
        } else {
            crate::confirmed_replay::tests::snapshot_history::installed_recovery::installation::live::bootstrap::progression::assert_successor(fixture, directory, store, 3).await;
        }
    }
}

#[test]
fn challenge_activation_rejects_zero_and_overflowing_review_windows() {
    for duration in [0, u64::MAX] {
        let (fixture, first, _) = pending_with(ChallengeReason::Fraud, |params| {
            params.challenge_duration_secs = duration;
        });
        let (_dir, store, _) = certify_challenge(&fixture, &first);
        assert!(
            PreparedActivation::prepare_next(&store, NetworkId::Dev, &fixture.params, START)
                .is_err()
        );
        assert_eq!(
            CertifiedCheckpoint::stored_tip(&store, NetworkId::Dev, &fixture.params).unwrap(),
            2
        );
    }
}

pub(super) fn setup(
    reason: ChallengeReason,
    max_revotes: u32,
    threshold: u32,
) -> (
    Fixture,
    tempfile::TempDir,
    LmdbStore,
    CertifiedCheckpoint,
    BlockHash,
) {
    setup_with_expiry(reason, max_revotes, threshold, u64::MAX)
}

fn setup_with_expiry(
    reason: ChallengeReason,
    max_revotes: u32,
    threshold: u32,
    expiry: u64,
) -> (
    Fixture,
    tempfile::TempDir,
    LmdbStore,
    CertifiedCheckpoint,
    BlockHash,
) {
    setup_with_parameters(reason, max_revotes, threshold, expiry, |_| {})
}

fn setup_with_parameters(
    reason: ChallengeReason,
    max_revotes: u32,
    threshold: u32,
    expiry: u64,
    configure: impl FnOnce(&mut ProtocolParams),
) -> (
    Fixture,
    tempfile::TempDir,
    LmdbStore,
    CertifiedCheckpoint,
    BlockHash,
) {
    let (fixture, first, origin) = pending_with(reason, |params| {
        params.verification_timeout_secs = 2;
        params.challenge_duration_secs = 5;
        params.trst_expiry_secs = expiry;
        params.max_revotes = max_revotes;
        params.verification_threshold_bps = threshold;
        configure(params);
    });
    let (_source_dir, source, selection) = certify_challenge(&fixture, &first);
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    crate::payment_epoch::initialize(&mut batch, fixture.blocks[0].hash).unwrap();
    batch.commit().unwrap();
    for height in 1..=2 {
        let transfer =
            CertifiedCheckpoint::export_transfer(&source, NetworkId::Dev, &fixture.params, height)
                .unwrap()
                .unwrap();
        CertifiedCheckpoint::import_transfer(
            &store,
            NetworkId::Dev,
            &fixture.params,
            height,
            &transfer,
            &HistoricalAdmissions::default(),
        )
        .unwrap();
        PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap()
            .commit(&store)
            .unwrap();
    }
    let candidate =
        PreparedActivation::prepare_next(&store, NetworkId::Dev, &fixture.params, START)
            .unwrap()
            .unwrap();
    assert_eq!(
        candidate.manifest().activation.selection,
        selection.certificate().checkpoint.hash()
    );
    let manifest = candidate.manifest().clone();
    chain::accept_manifest(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 6]),
        &ReplayManifest::Activation(manifest),
    )
    .unwrap();
    PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    (fixture, dir, store, selection, origin)
}

#[test]
fn governance_count_and_challenge_deadline_share_one_cut_and_use_the_settled_electorate() {
    use crate::checkpoint_governance::PreparedGovernance;
    use burst_governance::{GovernableParam, GovernancePhase, ProposalContent};
    let (mut fixture, _dir, store, selection, _) =
        setup_with_parameters(ChallengeReason::Fraud, 0, 9000, u64::MAX, |params| {
            params.governance_proposal_duration_secs = 1;
            params.governance_exploration_duration_secs = 3;
            params.governance_propagation_buffer_secs = 1;
            params.governance_cooldown_duration_secs = 10;
            params.governance_proposal_endorsements = 0;
            params.governance_quorum_bps = 0;
            params.governance_supermajority_bps = 5000;
        });
    let epoch = store.meta_store().payment_epoch().unwrap();
    let content = ProposalContent::ParameterChange {
        param: GovernableParam::BrnRate,
        new_value: 2,
    };
    let proposal = fixture.push(0, BlockType::GovernanceProposal, START, |block| {
        block.payment_epoch = epoch;
        block.link = BlockHash::new(content.encode_compact().unwrap());
    });
    for seed in [0, 20] {
        fixture.push(seed, BlockType::GovernanceVote, START + 1, |block| {
            block.payment_epoch = epoch;
            block.link = proposal;
            block.transaction = TxHash::ZERO;
        });
    }
    vote_choice_in_epoch(
        &mut fixture,
        &selection,
        panel(&selection)[0],
        START + 1,
        2,
        epoch,
    );
    fixture.append_confirmed_prefix(&store, START + 1);
    assert!(
        PreparedGovernance::prepare_next(&store, NetworkId::Dev, &fixture.params, END - 1)
            .unwrap()
            .is_none()
    );
    let candidate = PreparedGovernance::prepare_next(&store, NetworkId::Dev, &fixture.params, END)
        .unwrap()
        .unwrap();
    let manifest = candidate.manifest().clone();
    let mut corrupt = manifest.clone();
    corrupt.checkpoint.selection_root = BlockHash::ZERO;
    assert!(PreparedGovernance::replay_manifest(&store, &fixture.params, &corrupt).is_err());
    chain::accept_manifest(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 6]),
        &ReplayManifest::Governance(manifest),
    )
    .unwrap();
    PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    let state = state_at(&fixture, &store, END + 1);
    assert_eq!(
        state.account(&address(20)).unwrap().state,
        burst_types::WalletState::Revoked
    );
    let ballot = state
        .governance
        .as_ref()
        .unwrap()
        .governance
        .get_proposal(&proposal.into_tx_hash())
        .unwrap();
    assert_eq!(ballot.phase, GovernancePhase::Cooldown);
    assert_eq!(
        ballot.exploration_votes_yea, 1,
        "the revoked human's ballot must not count"
    );
    assert_eq!(ballot.total_eligible_voters, 9);
    assert!(
        PreparedDeadline::prepare_next(&store, NetworkId::Dev, &fixture.params, END + 1)
            .unwrap()
            .is_none()
    );

    let fresh_dir = tempfile::tempdir().unwrap();
    let fresh = LmdbStore::open(fresh_dir.path(), 32, 32 << 20).unwrap();
    let mut batch = fresh.tx_begin_write().unwrap();
    crate::payment_epoch::initialize(&mut batch, fixture.blocks[0].hash).unwrap();
    batch.commit().unwrap();
    for height in 1..=4 {
        let bytes =
            CertifiedCheckpoint::export_transfer(&store, NetworkId::Dev, &fixture.params, height)
                .unwrap()
                .unwrap();
        CertifiedCheckpoint::import_transfer(
            &fresh,
            NetworkId::Dev,
            &fixture.params,
            height,
            &bytes,
            &HistoricalAdmissions::default(),
        )
        .unwrap();
        PreparedInstallation::next(&fresh, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap()
            .commit(&fresh)
            .unwrap();
    }
    assert_eq!(
        fresh
            .account_store()
            .get_account(&address(20))
            .unwrap()
            .state,
        burst_types::WalletState::Revoked
    );
    assert_eq!(
        fresh.meta_store().payment_epoch().unwrap(),
        store.meta_store().payment_epoch().unwrap()
    );
}

fn finish(fixture: &Fixture, store: &LmdbStore) -> BlockHash {
    assert!(
        PreparedDeadline::prepare_next(store, NetworkId::Dev, &fixture.params, END - 1)
            .unwrap()
            .is_none()
    );
    let candidate = PreparedDeadline::prepare_next(store, NetworkId::Dev, &fixture.params, END)
        .unwrap()
        .unwrap();
    let manifest = candidate.manifest().clone();
    for change in 0..3 {
        let mut forged = manifest.clone();
        match change {
            0 => forged.checkpoint.cutoff -= 1,
            1 => forged.checkpoint.selection_root = BlockHash::ZERO,
            _ => forged.checkpoint.next_roster = BlockHash::ZERO,
        }
        assert!(PreparedDeadline::replay_manifest(store, &fixture.params, &forged).is_err());
    }
    assert!(candidate
        .certify(certify(&manifest.checkpoint, &[0]))
        .is_err());
    let before = store.meta_store().payment_epoch().unwrap();
    chain::accept_manifest(
        store,
        NetworkId::Dev,
        &fixture.params,
        certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 6]),
        &ReplayManifest::Deadline(manifest.clone()),
    )
    .unwrap();
    assert_eq!(store.meta_store().payment_epoch().unwrap(), before);
    PreparedInstallation::next(store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(store)
        .unwrap();
    manifest.checkpoint.hash()
}

fn state_at(fixture: &Fixture, store: &LmdbStore, cutoff: u64) -> ConfirmedReplay {
    let events = chain::load(store, NetworkId::Dev, &fixture.params).unwrap();
    ConfirmedReplay::replay_with_events(
        &fixture.history(cutoff),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap()
}

#[test]
fn challenge_deadline_and_pending_expiry_preserve_fraud_and_inactivity_accounting() {
    for (reason, expected, expired, revoked) in [
        (
            ChallengeReason::Fraud,
            burst_types::WalletState::Revoked,
            1,
            1,
        ),
        (
            ChallengeReason::Inactivity,
            burst_types::WalletState::Deactivated,
            2,
            0,
        ),
    ] {
        // Both pending payments originate at CUTOFF + 35 and expire at END.
        let (mut fixture, _dir, store, selection, _) = setup_with_expiry(reason, 0, 9000, 31);
        let epoch = store.meta_store().payment_epoch().unwrap();
        vote_choice_in_epoch(
            &mut fixture,
            &selection,
            panel(&selection)[0],
            START + 1,
            2,
            epoch,
        );
        fixture.append_confirmed_prefix(&store, START + 1);
        finish(&fixture, &store);
        let state = state_at(&fixture, &store, END + 1);
        assert_eq!(state.pending.len(), 2);
        assert!(state.pending.values().all(|pending| pending.return_credit));
        let target = state.account(&address(20)).unwrap();
        assert_eq!(target.state, expected);
        assert_eq!(
            (
                target.trst_balance,
                target.expired_trst,
                target.revoked_trst
            ),
            (0, expired, revoked)
        );
        assert_eq!(
            state.portfolio(&address(20)).unwrap().cached_transferable,
            0
        );
        let stored = store.account_store().get_account(&address(20)).unwrap();
        assert_eq!(
            (stored.expired_trst, stored.revoked_trst),
            (expired, revoked)
        );
    }
}

#[test]
fn challenge_deadline_settles_partial_and_silent_panels_at_the_certified_cut() {
    for (reason, threshold, choice, expected) in [
        (
            ChallengeReason::Fraud,
            5000,
            Some(1),
            burst_types::WalletState::Verified,
        ),
        (
            ChallengeReason::Fraud,
            9000,
            Some(2),
            burst_types::WalletState::Revoked,
        ),
        (
            ChallengeReason::Inactivity,
            9000,
            Some(2),
            burst_types::WalletState::Deactivated,
        ),
        (
            ChallengeReason::Fraud,
            9000,
            None,
            burst_types::WalletState::Revoked,
        ),
    ] {
        let (mut fixture, _dir, store, selection, origin) = setup(reason, 0, threshold);
        let members = panel(&selection);
        if let Some(choice) = choice {
            let epoch = store.meta_store().payment_epoch().unwrap();
            vote_choice_in_epoch(
                &mut fixture,
                &selection,
                members[0],
                START + 1,
                choice,
                epoch,
            );
            fixture.append_confirmed_prefix(&store, START + 1);
        }
        let before = state_at(&fixture, &store, END - 1);
        assert!(before.account(&address(20)).unwrap().state.is_verified());
        assert!(before.verification.has_active_challenge(&address(20)));
        assert!(
            before.account(&address(8)).unwrap().total_brn_staked
                >= fixture.params.challenge_stake_amount
        );
        finish(&fixture, &store);
        let after = state_at(&fixture, &store, END + 1);
        assert_eq!(after.account(&address(20)).unwrap().state, expected);
        assert!(!after.verification.has_active_challenge(&address(20)));
        assert_eq!(after.account(&address(8)).unwrap().total_brn_staked, 0);
        assert_eq!(
            after.account(&address(8)).unwrap().total_brn_burned,
            before.account(&address(8)).unwrap().total_brn_burned
                + if expected == burst_types::WalletState::Verified {
                    fixture.params.challenge_stake_amount
                } else {
                    0
                }
        );
        for seed in &members {
            assert_eq!(after.account(&address(*seed)).unwrap().total_brn_staked, 0);
        }
        let context = selection
            .replayed()
            .request()
            .vote_context(&selection.certificate().checkpoint)
            .unwrap();
        let record = verification_deadline::decode(
            &store
                .meta_store()
                .get_meta(&verification_deadline::key(context))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(record.lifecycle, origin);
        assert_eq!(record.completed_at, END);
        assert_eq!(record.absent.len(), if choice.is_some() { 1 } else { 2 });
        assert!(
            PreparedDeadline::prepare_next(&store, NetworkId::Dev, &fixture.params, END + 1)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn challenge_silent_revote_survives_transfer_restart_and_later_signed_resolution() {
    let (mut fixture, _dir, store, first, origin) = setup(ChallengeReason::Fraud, 1, 9000);
    let epoch = finish(&fixture, &store);
    let mut replay = state_at(&fixture, &store, END + 1);
    assert!(replay.account(&address(20)).unwrap().state.is_verified());
    assert_eq!(
        replay
            .verification
            .get_state(&address(20))
            .unwrap()
            .revote_count,
        1
    );
    assert_eq!(
        replay
            .verification
            .active_challenge_context(&address(20))
            .unwrap(),
        Some(origin)
    );
    let request = replay.prepare_selection(&address(20)).unwrap();
    assert!(!fixture
        .blocks
        .iter()
        .any(|block| block.hash == request.trigger()));
    let context = first
        .replayed()
        .request()
        .vote_context(&first.certificate().checkpoint)
        .unwrap();
    let bytes = store
        .meta_store()
        .get_meta(&verification_deadline::key(context))
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let fresh = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let mut batch = fresh.tx_begin_write().unwrap();
    crate::payment_epoch::initialize(&mut batch, fixture.blocks[0].hash).unwrap();
    batch.commit().unwrap();
    for height in 1..=4 {
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
    drop(fresh);
    let fresh = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    assert_eq!(fresh.meta_store().payment_epoch().unwrap(), epoch);
    assert_eq!(
        fresh
            .meta_store()
            .get_meta(&verification_deadline::key(context))
            .unwrap(),
        bytes
    );
    let cutoff = CUTOFF + (1005 - 102) * 30;
    let next = CertifiedCheckpoint::prepare_next(&fresh, NetworkId::Dev, &fixture.params, cutoff)
        .unwrap()
        .unwrap();
    assert_eq!(next.request().attempt(), 1);
    assert_eq!(next.request().round(), 1005);
    let mut second = CertifiedCheckpoint::accept(
        &fresh,
        NetworkId::Dev,
        &fixture.params,
        certify(next.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]),
        next.manifest().heads,
        address(20),
    )
    .unwrap();
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    second
        .attach_beacon(
            &fresh,
            &fixture.params,
            burst_vrf::DrandClient::beacon_to_random_output(&beacons[1]).unwrap(),
        )
        .unwrap();
    PreparedInstallation::next(&fresh, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&fresh)
        .unwrap();
    let previous = panel(&first);
    let selected = panel(&second);
    assert!(selected.iter().all(|seed| !previous.contains(seed)));
    for (index, seed) in selected.iter().enumerate() {
        vote(&mut fixture, &second, *seed, cutoff + 31 + index as u64);
    }
    let resolved = state_at(&fixture, &fresh, cutoff + 32);
    assert!(resolved.account(&address(20)).unwrap().state.is_verified());
    assert!(!resolved.verification.has_active_challenge(&address(20)));
    assert_eq!(resolved.account(&address(8)).unwrap().total_brn_staked, 0);
    for seed in previous {
        assert_eq!(
            resolved.account(&address(seed)).unwrap().total_brn_staked,
            0
        );
    }
}

#[test]
fn activated_challenge_rejects_late_votes_and_uncertified_deadline_crossing() {
    let (mut fixture, _dir, store, selection, _) = setup(ChallengeReason::Fraud, 1, 9000);
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let error = ConfirmedReplay::replay_with_events(
        &fixture.history(END),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .err()
    .unwrap();
    assert!(error
        .to_string()
        .contains("deadline requires agreed completion"));
    let epoch = store.meta_store().payment_epoch().unwrap();
    let context = selection
        .replayed()
        .request()
        .vote_context(&selection.certificate().checkpoint)
        .unwrap();
    let activation = verification_activation::decode(
        &store
            .meta_store()
            .get_meta(&verification_activation::key(context))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(activation.deadline, END);
    vote_choice_in_epoch(
        &mut fixture,
        &selection,
        panel(&selection)[0],
        END,
        1,
        epoch,
    );
    let error = ConfirmedReplay::replay_deadline_candidate(
        &fixture.history(END),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("deadline has passed"), "{error}");
    assert_eq!(store.meta_store().payment_epoch().unwrap(), epoch);
    assert!(store
        .meta_store()
        .get_meta(&verification_deadline::key(context))
        .is_err());
}
