use super::*;
use crate::checkpoint_governance::PreparedGovernance;
use burst_governance::{GovernableParam, ProposalContent};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_governance_service_agrees_and_recovers_over_tcp() {
    let (mut fixture, _dir, source, _, _) = setup_config(|params| {
        params.governance_proposal_duration_secs = 1;
        params.governance_exploration_duration_secs = 1;
        params.governance_cooldown_duration_secs = 0;
        params.governance_propagation_buffer_secs = 1;
        params.governance_proposal_endorsements = 0;
        params.governance_quorum_bps = 1000;
    });
    let epoch = source.meta_store().payment_epoch().unwrap();
    let proposal = fixture.push(0, BlockType::GovernanceProposal, CUTOFF + 1, |block| {
        block.payment_epoch = epoch;
        block.link = BlockHash::new(
            ProposalContent::ParameterChange {
                param: GovernableParam::BrnRate,
                new_value: 2,
            }
            .encode_compact()
            .unwrap(),
        );
    });
    fixture.push(0, BlockType::GovernanceVote, CUTOFF + 2, |block| {
        block.link = proposal;
        block.transaction = TxHash::ZERO;
    });
    fixture.append_confirmed_prefix(&source, CUTOFF + 3);
    let primary =
        PreparedGovernance::prepare_next(&source, NetworkId::Dev, &fixture.params, CUTOFF + 4)
            .unwrap()
            .unwrap();
    let manifest = primary.manifest().clone();
    let mut heads = manifest.heads.clone();
    let head = heads
        .iter_mut()
        .find(|head| head.account == address(0))
        .unwrap();
    head.hash = proposal;
    head.height -= 1;
    let events = chain::load(&source, NetworkId::Dev, &fixture.params).unwrap();
    let history = ConfirmedHistory::read(
        &source,
        NetworkId::Dev,
        primary.parent().genesis_hash(),
        manifest.checkpoint.cutoff,
        &heads,
        events.last().unwrap().heads(),
    )
    .unwrap();
    let state = ConfirmedReplay::replay_governance_candidate(
        &history,
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    let alternative =
        PreparedGovernance::from_replay(&history, primary.parent(), &fixture.params, state)
            .unwrap()
            .manifest()
            .clone();
    assert_ne!(manifest.checkpoint, alternative.checkpoint);
    crate::confirmed_replay::tests::snapshot_history::selection::compact_service::assert_service(
        &fixture,
        std::sync::Arc::new(source),
        ReplayManifest::Governance(manifest),
        ReplayManifest::Governance(alternative),
        CUTOFF + 7,
    )
    .await;
}

#[test]
fn a_previously_selected_panel_activates_under_new_rules_without_resampling() {
    assert_governed_activation(90);
}

#[test]
fn activation_signing_window_uses_a_shorter_governed_timeout() {
    assert_governed_activation(30);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn governed_verification_activation_bootstraps_live_and_restarts_payment_admission() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let (fixture, directory, store) = assert_governed_activation(90);
    crate::confirmed_replay::tests::snapshot_history::installed_recovery::installation::live::bootstrap::assert_lifecycle_archive(
        fixture, directory, store,
    )
    .await;
}

fn assert_governed_activation(timeout: u64) -> (Fixture, tempfile::TempDir, LmdbStore) {
    let (mut fixture, directory, store, selection, _) = setup_config(|params| {
        params.verification_timeout_secs = 60;
        params.governance_proposal_duration_secs = 1;
        params.governance_exploration_duration_secs = 1;
        params.governance_cooldown_duration_secs = 0;
        params.governance_propagation_buffer_secs = 1;
        params.governance_proposal_endorsements = 0;
        params.governance_quorum_bps = 1000;
    });
    let selection_epoch = store.meta_store().payment_epoch().unwrap();
    let proposal = fixture.push(0, BlockType::GovernanceProposal, CUTOFF + 1, |block| {
        block.payment_epoch = selection_epoch;
        block.link = BlockHash::new(
            ProposalContent::ParameterChange {
                param: GovernableParam::VerificationTimeoutSecs,
                new_value: u128::from(timeout),
            }
            .encode_compact()
            .unwrap(),
        );
    });
    fixture.push(0, BlockType::GovernanceVote, CUTOFF + 2, |block| {
        block.link = proposal;
        block.transaction = TxHash::ZERO;
    });
    fixture.append_confirmed_prefix(&store, CUTOFF + 3);
    for cutoff in [CUTOFF + 4, CUTOFF + 5] {
        let candidate =
            PreparedGovernance::prepare_next(&store, NetworkId::Dev, &fixture.params, cutoff)
                .unwrap()
                .unwrap();
        assert_eq!(candidate.manifest().checkpoint.cutoff, cutoff);
        let manifest = ReplayManifest::Governance(candidate.manifest().clone());
        chain::accept_manifest(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certify(manifest.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]),
            &manifest,
        )
        .unwrap();
        PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap()
            .commit(&store)
            .unwrap();
    }
    let candidate =
        PreparedActivation::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 31)
            .unwrap()
            .unwrap();
    let deadline = CUTOFF + 31 + timeout.min(60);
    assert!(candidate
        .validate_fresh_time(crate::checkpoint_replay::SigningClock {
            now: deadline - 1,
            error_bound: 0
        })
        .is_ok());
    for (now, error_bound) in [(deadline, 0), (deadline - 1, 1), (u64::MAX, 1)] {
        assert!(candidate
            .validate_fresh_time(crate::checkpoint_replay::SigningClock { now, error_bound })
            .is_err());
    }
    let epoch = activate(&fixture, &store);
    crate::confirmed_replay::tests::snapshot_history::boundaries::assert_chain(&fixture, &store);
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
    assert_eq!(activation.deadline, CUTOFF + 31 + timeout);
    assert_eq!(
        activation.rules_at_activation.verification_timeout_secs,
        timeout
    );
    assert_ne!(
        activation.params_hash,
        selection.replayed().request().params_hash()
    );
    let seed = panel(&selection)[0];
    vote_choice_in_epoch(&mut fixture, &selection, seed, CUTOFF + 32, 1, epoch);
    let vote = fixture.blocks.last_mut().unwrap();
    vote.params_hash = activation.params_hash;
    sign(seed, vote);
    fixture.append_confirmed_prefix(&store, CUTOFF + 32);
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let replay = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 33),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    assert_eq!(
        replay.verification.selection_context(&address(20)),
        Some(context)
    );
    assert_eq!(
        replay
            .verification
            .get_state(&address(20))
            .unwrap()
            .votes
            .len(),
        1
    );
    crate::payment_authority::initialize(&store, NetworkId::Dev, &fixture.params).unwrap();
    (fixture, directory, store)
}
