use super::*;
use crate::{
    checkpoint_agreement::ReplayManifest,
    checkpoint_governance::PreparedGovernance,
    checkpoint_roster::PreparedRoster,
    checkpoint_store::{chain, historical::HistoricalAdmissions, CertifiedCheckpoint},
    confirmed_replay::installation::PreparedInstallation,
};
use burst_governance::{GovernableParam, GovernanceEngine, GovernancePhase, ProposalContent};
use burst_store::BrnStore;

const CUT: u64 = 141;

mod lifecycle;
mod promises;
mod timing;

fn fixture() -> (Fixture, Vec<TxHash>) {
    fixture_with_cooldown(ProtocolParams::default().governance_cooldown_duration_secs)
}

pub(super) fn fixture_with_cooldown(cooldown: u64) -> (Fixture, Vec<TxHash>) {
    fixture_with_timing(cooldown, 1)
}

fn fixture_with_timing(cooldown: u64, buffer: u64) -> (Fixture, Vec<TxHash>) {
    fixture_with_settings(
        cooldown,
        buffer,
        |_| {},
        [2, 3]
            .into_iter()
            .map(|value| ProposalContent::ParameterChange {
                param: GovernableParam::BrnRate,
                new_value: value,
            })
            .collect(),
    )
}

fn fixture_with_settings(
    cooldown: u64,
    buffer: u64,
    configure: impl FnOnce(&mut ProtocolParams),
    contents: Vec<ProposalContent>,
) -> (Fixture, Vec<TxHash>) {
    let mut fixture = Fixture::new();
    fixture.params.governance_proposal_cost = 1;
    fixture.params.governance_proposal_duration_secs = 10;
    fixture.params.governance_exploration_duration_secs = 20;
    fixture.params.governance_propagation_buffer_secs = buffer;
    fixture.params.governance_proposal_endorsements = 1;
    fixture.params.governance_supermajority_bps = 6000;
    fixture.params.governance_quorum_bps = 10000;
    fixture.params.governance_cooldown_duration_secs = cooldown;
    configure(&mut fixture.params);
    fixture.blocks[0] = crate::genesis_state::block(NetworkId::Dev, &fixture.params);
    for seed in [1, 2] {
        fixture.push(0, BlockType::Endorse, 90 + u64::from(seed), |block| {
            block.brn_balance += 1;
            block.link = destination(seed);
        });
        fixture.push(seed, BlockType::Open, 94, |_| {});
    }
    fixture.push(1, BlockType::Delegate, 100, |block| {
        block.link = destination(0)
    });
    let mut proposals = Vec::new();
    for content in contents {
        let proposal = fixture.push(0, BlockType::GovernanceProposal, 110, |block| {
            block.link = BlockHash::new(content.encode_compact().unwrap());
        });
        proposals.push(proposal.into_tx_hash());
    }
    for proposal in &proposals {
        fixture.push(0, BlockType::GovernanceEndorse, 111, |block| {
            block.link = BlockHash::new(*proposal.as_bytes());
            block.brn_balance += 1;
        });
    }
    for proposal in &proposals {
        for (seed, choice) in [(0, 0), (2, 1)] {
            fixture.push(seed, BlockType::GovernanceVote, 122, |block| {
                block.link = BlockHash::new(*proposal.as_bytes());
                let mut vote = [0; 32];
                vote[0] = choice;
                block.transaction = TxHash::new(vote);
            });
        }
    }
    (fixture, proposals)
}

#[test]
fn zero_cooldown_and_buffer_activate_in_the_counting_certificate() {
    let (fixture, proposals) = fixture_with_timing(0, 0);
    let (_dir, store, _) = fixture.store();
    let prepared = prepare(&fixture, CUT - 1).unwrap();
    assert_eq!(prepared.manifest().proposals.len(), proposals.len());
    assert_ne!(
        prepared.manifest().checkpoint.params_hash,
        fixture.params.params_hash()
    );
    chain::accept_manifest(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certified_votes::certify(&prepared.manifest().checkpoint, &[0]),
        &ReplayManifest::Governance(prepared.manifest().clone()),
    )
    .unwrap();
    let installed = PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    for id in proposals {
        let proposal = installed.governance.governance.get_proposal(&id).unwrap();
        assert_eq!(proposal.phase, GovernancePhase::Activated);
        assert_eq!(proposal.activation_at.unwrap().as_secs(), CUT - 1);
    }
}

fn prepare(fixture: &Fixture, cutoff: u64) -> Result<PreparedGovernance, burst_store::StoreError> {
    let history = fixture.history(cutoff);
    let parent = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap();
    let replay = ConfirmedReplay::replay_governance_candidate(
        &history,
        NetworkId::Dev,
        &fixture.params,
        &[],
    )
    .map_err(|error| burst_store::StoreError::Corruption(error.to_string()))?;
    PreparedGovernance::from_replay(&history, &parent, &fixture.params, replay)
}

fn assert_ballots(governance: &GovernanceEngine, proposals: &[TxHash]) {
    for id in proposals {
        let proposal = governance.get_proposal(id).unwrap();
        assert_eq!(proposal.phase, GovernancePhase::Cooldown);
        assert_eq!(
            (
                proposal.exploration_votes_yea,
                proposal.exploration_votes_nay,
                proposal.exploration_votes_abstain,
                proposal.total_eligible_voters
            ),
            (2, 1, 0, 3)
        );
    }
}

#[test]
fn ordinary_replay_accepts_open_governance_but_cannot_skip_its_counting_boundary() {
    let (fixture, _) = fixture_with_cooldown(10);
    assert!(fixture.replay(CUT - 1).is_ok());
    let error = match fixture.replay(CUT) {
        Ok(_) => panic!("ordinary replay skipped a due governance count"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("requires a certified governance counting cut"),
        "{error}"
    );
}

#[test]
fn automatic_governance_discovery_rebuilds_the_cut_before_later_delegation() {
    let (mut fixture, _) = fixture_with_cooldown(10);
    let expected = prepare(&fixture, CUT).unwrap().manifest().clone();
    fixture.push(1, BlockType::RevokeDelegation, CUT + 3, |_| {});
    let (_dir, store, _) = fixture.store();
    assert!(
        PreparedGovernance::prepare_next(&store, NetworkId::Dev, &fixture.params, CUT - 1)
            .unwrap()
            .is_none()
    );
    let discovered =
        PreparedGovernance::prepare_next(&store, NetworkId::Dev, &fixture.params, CUT + 10)
            .unwrap()
            .unwrap();
    assert_eq!(discovered.manifest(), &expected);

    // Discovery cannot erase a later final promise to make its older cut signable.
    let tail = fixture.blocks.last().unwrap();
    store
        .meta_store()
        .reserve_consensus_vote(
            &tail.payment_epoch,
            &address(0),
            &tail.account,
            &tail.previous,
            &tail.hash,
            true,
            0,
        )
        .unwrap();
    let mut driver = crate::checkpoint_driver::CheckpointDriver::new_candidate(
        std::sync::Arc::new(store),
        fixture.params.clone(),
        burst_crypto::keypair_from_seed(&[0; 32]).private,
        crate::checkpoint_agreement::ReplayedCandidate::Governance(Box::new(discovered)),
    )
    .unwrap();
    assert!(driver
        .propose(Some(crate::checkpoint_replay::SigningClock {
            now: CUT + 11,
            error_bound: 0,
        }))
        .is_err());
}

#[test]
fn runtime_discovers_and_certifies_governance_without_a_supplied_manifest() {
    use crate::checkpoint_service::{Clock, Runtime, Settings};
    use std::{sync::Arc, time::Instant};
    let (fixture, proposals) = fixture_with_cooldown(10);
    let (_dir, store, _) = fixture.store();
    let store = Arc::new(store);
    let mut runtime = Runtime::new(Settings {
        store: store.clone(),
        network: NetworkId::Dev,
        initial_params: fixture.params.clone(),
        key: Some(Arc::new(burst_crypto::keypair_from_seed(&[0; 32]).private)),
    })
    .unwrap();
    let clock: Clock = Arc::new(|| {
        Some(crate::checkpoint_replay::SigningClock {
            now: CUT + 2,
            error_bound: 0,
        })
    });
    for _ in 0..3 {
        runtime.poll(&[], Instant::now(), &clock).unwrap();
    }
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], chain::CertifiedEvent::Governance(_)));
    assert_eq!(events[0].certificate().checkpoint.cutoff, CUT);
    assert_eq!(
        store.meta_store().payment_epoch().unwrap(),
        fixture.blocks[0].hash
    );
    let installed = PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    assert_ballots(&installed.governance.governance, &proposals);

    let activation_at = CUT + 11;
    assert!(PreparedGovernance::prepare_next(
        &store,
        NetworkId::Dev,
        &fixture.params,
        activation_at - 1
    )
    .unwrap()
    .is_none());
    let activation = PreparedGovernance::prepare_next(
        &store,
        NetworkId::Dev,
        &fixture.params,
        activation_at + 20,
    )
    .unwrap()
    .unwrap();
    assert_eq!(activation.manifest().checkpoint.cutoff, activation_at);
    assert_eq!(activation.parent().roster().total_humans(), 3);
    chain::accept_manifest(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certified_votes::certify(&activation.manifest().checkpoint, &[0, 1, 2]),
        &ReplayManifest::Governance(activation.manifest().clone()),
    )
    .unwrap();
    let installed = PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    for id in proposals {
        assert_eq!(
            installed
                .governance
                .governance
                .get_proposal(&id)
                .unwrap()
                .phase,
            GovernancePhase::Activated
        );
    }
    assert!(PreparedGovernance::prepare_next(
        &store,
        NetworkId::Dev,
        &fixture.params,
        activation_at + 30
    )
    .unwrap()
    .is_none());
}

#[test]
fn governance_certificate_binds_all_due_ballots_and_previous_roster() {
    let (fixture, proposals) = fixture();
    let (_dir, store, _) = fixture.store();
    assert!(prepare(&fixture, CUT - 1).is_err());
    assert!(prepare(&fixture, CUT + 1).is_err());
    let prepared = prepare(&fixture, CUT).unwrap();
    assert_eq!(prepared.parent().roster().total_humans(), 1);
    let mut sorted = proposals;
    sorted.sort_by_key(|id| *id.as_bytes());
    assert_eq!(prepared.manifest().proposals, sorted);
    for mutation in 0..4 {
        let mut manifest = prepared.manifest().clone();
        match mutation {
            0 => {
                manifest.proposals.pop();
            }
            1 => manifest.proposals.reverse(),
            2 => manifest.checkpoint.selection_root = BlockHash::new([7; 32]),
            _ => manifest.checkpoint.cutoff += 1,
        }
        assert!(PreparedGovernance::replay_manifest(&store, &fixture.params, &manifest).is_err());
    }
    assert!(
        PreparedGovernance::replay_manifest(&store, &fixture.params, prepared.manifest())
            .unwrap()
            .certify(certified_votes::certify(
                &prepared.manifest().checkpoint,
                &[1, 2]
            ))
            .is_err()
    );
    let certificate = certified_votes::certify(&prepared.manifest().checkpoint, &[0]);
    prepared.certify(certificate).unwrap();
}

#[test]
fn governance_certificate_installs_restores_and_transfers_frozen_ballots() {
    let (mut fixture, proposals) = fixture();
    let (dir, store, _) = fixture.store();
    let prepared = prepare(&fixture, CUT).unwrap();
    let epoch = prepared.manifest().checkpoint.hash();
    chain::accept_manifest(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certified_votes::certify(&prepared.manifest().checkpoint, &[0]),
        &ReplayManifest::Governance(prepared.manifest().clone()),
    )
    .unwrap();
    // Corrupt local projections cannot override the certified signed history.
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_brn_meta(b"governance_engine", b"broken").unwrap();
    batch
        .put_meta("governance_endorsement/stale", b"stale")
        .unwrap();
    batch.commit().unwrap();
    let installed = PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    assert_eq!(installed.anchor.hash(), epoch);
    assert_eq!(installed.anchor.roster().total_humans(), 3);
    assert_ballots(&installed.governance.governance, &proposals);
    assert!(store
        .meta_store()
        .get_meta("governance_endorsement/stale")
        .is_err());
    for id in &proposals {
        assert!(store
            .meta_store()
            .get_meta(&format!("governance_endorsement/{id}/{}", address(0)))
            .is_ok());
    }
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let restored: GovernanceEngine = bincode::deserialize(
        &store
            .brn_store()
            .get_meta(b"governance_engine")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_ballots(&restored, &proposals);
    assert!(
        PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .is_none()
    );

    fixture.push(1, BlockType::RevokeDelegation, CUT + 1, |block| {
        block.payment_epoch = epoch
    });
    fixture.push(2, BlockType::ChangeRepresentative, CUT + 2, |block| {
        block.representative = address(0)
    });
    fixture.append_confirmed_prefix(&store, CUT + 3);
    let next = PreparedRoster::prepare_next(&store, NetworkId::Dev, &fixture.params, CUT + 4)
        .unwrap()
        .unwrap();
    chain::accept_manifest(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certified_votes::certify(&next.manifest().checkpoint, &[0, 1, 2]),
        &ReplayManifest::Roster(next.manifest().clone()),
    )
    .unwrap();
    let installed = PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    assert_ballots(&installed.governance.governance, &proposals);
    assert_eq!(installed.governance.delegation.voting_power(&address(0)), 1);

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
        let imported = PreparedInstallation::next(&fresh, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap()
            .commit(&fresh)
            .unwrap();
        assert_ballots(&imported.governance.governance, &proposals);
    }
    assert_eq!(
        fresh.meta_store().payment_epoch().unwrap(),
        store.meta_store().payment_epoch().unwrap()
    );
    let fresh_governance: GovernanceEngine = bincode::deserialize(
        &fresh
            .brn_store()
            .get_meta(b"governance_engine")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let source_governance: GovernanceEngine = bincode::deserialize(
        &store
            .brn_store()
            .get_meta(b"governance_engine")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    for id in &proposals {
        assert_eq!(
            bincode::serialize(fresh_governance.get_proposal(id).unwrap()).unwrap(),
            bincode::serialize(source_governance.get_proposal(id).unwrap()).unwrap()
        );
        assert_eq!(
            fresh_governance.get_exploration_votes(id),
            source_governance.get_exploration_votes(id)
        );
        assert_eq!(
            fresh_governance.get_endorsements(id),
            source_governance.get_endorsements(id)
        );
    }

    let next_epoch = next.manifest().checkpoint.hash();
    let later = fixture.push(0, BlockType::GovernanceProposal, 150, |block| {
        block.payment_epoch = next_epoch;
        block.link = BlockHash::new(
            ProposalContent::ParameterChange {
                param: GovernableParam::BrnRate,
                new_value: 4,
            }
            .encode_compact()
            .unwrap(),
        );
    });
    fixture.push(0, BlockType::GovernanceEndorse, 151, |block| {
        block.link = later;
        block.brn_balance += 1;
    });
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    assert!(ConfirmedReplay::replay_with_events(
        &fixture.history(180),
        NetworkId::Dev,
        &fixture.params,
        &events
    )
    .is_ok());
    let error = match ConfirmedReplay::replay_with_events(
        &fixture.history(181),
        NetworkId::Dev,
        &fixture.params,
        &events,
    ) {
        Ok(_) => panic!("ordinary replay skipped an uncertified counting cut"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("requires a certified governance counting cut"),
        "{error}"
    );
}

#[test]
fn governance_decision_recovers_from_journal_without_installing_early() {
    use crate::{
        checkpoint_agreement::{read_candidates, ReplayedCandidate},
        checkpoint_driver::CheckpointDriver,
        checkpoint_replay::SigningClock,
    };
    let (fixture, _) = fixture();
    let (dir, store, _) = fixture.store();
    let prepared = prepare(&fixture, CUT).unwrap();
    let parent = prepared.parent().clone();
    let store = std::sync::Arc::new(store);
    let key = burst_crypto::keypair_from_seed(&[0; 32]).private;
    let mut driver = CheckpointDriver::new_candidate(
        store.clone(),
        fixture.params.clone(),
        key,
        ReplayedCandidate::Governance(Box::new(prepared)),
    )
    .unwrap();
    driver
        .propose(Some(SigningClock {
            now: CUT + 1,
            error_bound: 0,
        }))
        .unwrap();
    assert!(driver.state().unwrap().unwrap().decision().is_some());
    assert_eq!(
        CertifiedCheckpoint::stored_tip(&store, NetworkId::Dev, &fixture.params).unwrap(),
        0
    );
    drop(driver);
    drop(store);
    let store = std::sync::Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
    let retained = read_candidates(&store, &parent, &address(0), &fixture.params).unwrap();
    assert_eq!(retained.len(), 1);
    let candidate = retained.into_iter().next().unwrap();
    assert!(matches!(candidate, ReplayedCandidate::Governance(_)));
    let restored = CheckpointDriver::new_candidate(
        store.clone(),
        fixture.params.clone(),
        burst_crypto::keypair_from_seed(&[0; 32]).private,
        candidate,
    )
    .unwrap();
    assert!(restored.persist_decision().unwrap());
    assert_eq!(
        CertifiedCheckpoint::stored_tip(&store, NetworkId::Dev, &fixture.params).unwrap(),
        1
    );
    assert_eq!(store.meta_store().payment_epoch().unwrap(), parent.hash());
    PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
}

#[test]
fn governance_activations_order_competing_changes_and_replay_governed_payments() {
    let (mut fixture, proposals) = fixture_with_cooldown(10);
    let (_dir, store, _) = fixture.store();
    let ballot = prepare(&fixture, CUT).unwrap();
    chain::accept_manifest(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certified_votes::certify(&ballot.manifest().checkpoint, &[0]),
        &ReplayManifest::Governance(ballot.manifest().clone()),
    )
    .unwrap();
    PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    let at = CUT + 11;
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let parent = chain::next_anchor(&events, NetworkId::Dev, &fixture.params).unwrap();
    for time in [at - 1, at, at + 1] {
        let history = fixture.history(time);
        let replay = ConfirmedReplay::replay_governance_candidate(
            &history,
            NetworkId::Dev,
            &fixture.params,
            &events,
        )
        .unwrap();
        let candidate = PreparedGovernance::from_replay(&history, &parent, &fixture.params, replay);
        assert_eq!(candidate.is_ok(), time == at);
    }
    let history = fixture.history(at);
    let replay = ConfirmedReplay::replay_governance_candidate(
        &history,
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    let activation =
        PreparedGovernance::from_replay(&history, &parent, &fixture.params, replay).unwrap();
    let mut sorted = proposals.clone();
    sorted.sort_by_key(|id| *id.as_bytes());
    assert_eq!(activation.manifest().proposals, sorted);
    let mut governed = fixture.params.clone();
    governed.brn_rate = if sorted.last() == proposals.first() {
        2
    } else {
        3
    };
    assert_eq!(
        activation.manifest().checkpoint.params_hash,
        governed.params_hash()
    );
    chain::accept_manifest(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certified_votes::certify(&activation.manifest().checkpoint, &[0, 1, 2]),
        &ReplayManifest::Governance(activation.manifest().clone()),
    )
    .unwrap();
    let installed = PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    assert_eq!(installed.brn.rate_history.current_rate(), governed.brn_rate);
    assert_eq!(
        installed
            .brn
            .rate_history
            .segments
            .last()
            .unwrap()
            .start
            .as_secs(),
        at
    );
    for id in &proposals {
        assert_eq!(
            installed
                .governance
                .governance
                .get_proposal(id)
                .unwrap()
                .phase,
            GovernancePhase::Activated
        );
    }
    let epoch = installed.anchor.hash();
    fixture.push(0, BlockType::Burn, at + 1, |block| {
        block.payment_epoch = epoch;
        block.params_hash = governed.params_hash();
        block.brn_balance += 1;
        block.link = destination(1);
    });
    fixture.push(2, BlockType::ChangeRepresentative, at + 2, |block| {
        block.params_hash = governed.params_hash();
        block.representative = address(0);
    });
    fixture.append_confirmed_prefix(&store, at + 3);
    let next = PreparedRoster::prepare_next(&store, NetworkId::Dev, &fixture.params, at + 4)
        .unwrap()
        .unwrap();
    assert_eq!(
        next.manifest().checkpoint.params_hash,
        governed.params_hash()
    );
    chain::accept_manifest(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certified_votes::certify(&next.manifest().checkpoint, &[0, 1, 2]),
        &ReplayManifest::Roster(next.manifest().clone()),
    )
    .unwrap();
    PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();

    let fresh_dir = tempfile::tempdir().unwrap();
    let fresh = LmdbStore::open(fresh_dir.path(), 32, 32 << 20).unwrap();
    let mut batch = fresh.tx_begin_write().unwrap();
    crate::payment_epoch::initialize(&mut batch, fixture.blocks[0].hash).unwrap();
    batch.commit().unwrap();
    for height in 1..=3 {
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
    let restored: ProtocolParams = bincode::deserialize(
        &fresh
            .brn_store()
            .get_meta(b"protocol_params")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(restored.params_hash(), governed.params_hash());
    assert_eq!(
        fresh.meta_store().payment_epoch().unwrap(),
        store.meta_store().payment_epoch().unwrap()
    );
    let tail = fixture.blocks.last_mut().unwrap();
    tail.params_hash = fixture.params.params_hash();
    sign(2, tail);
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    assert!(ConfirmedReplay::replay_with_events(
        &fixture.history(at + 4),
        NetworkId::Dev,
        &fixture.params,
        &events[..2]
    )
    .is_err());
}

#[test]
fn certified_constitution_diff_uses_authenticated_body_and_rejects_invalid_effects() {
    use burst_governance::{content::EncodedContent, proposal::AmendmentOp};
    for valid in [true, false] {
        let (mut fixture, _) = fixture_with_cooldown(10);
        fixture.blocks.truncate(1);
        let operations = vec![
            AmendmentOp::AddArticle {
                title: "Voting".into(),
                text: "Each verified human has one vote.".into(),
            },
            AmendmentOp::ModifyArticle {
                article_number: if valid { 1 } else { 999 },
                new_text: "Equal human voting rights.".into(),
            },
        ];
        let body = EncodedContent::new(&ProposalContent::ConstitutionalDiff {
            title: "Voting rights".into(),
            operations,
        })
        .unwrap();
        let (link, origin, transaction) = body.manifest().block_fields().unwrap();
        let proposal = fixture.push(0, BlockType::GovernanceProposal, 110, |block| {
            block.link = link;
            block.origin = origin;
            block.transaction = transaction;
        });
        fixture.push(0, BlockType::GovernanceEndorse, 111, |block| {
            block.link = proposal;
            block.brn_balance += 1;
        });
        fixture.push(0, BlockType::GovernanceVote, 122, |block| {
            block.link = proposal
        });
        let (_dir, store, heads) = fixture.store();
        let mut batch = store.tx_begin_write().unwrap();
        batch
            .put_meta(&crate::governance_content::key(proposal), body.bytes())
            .unwrap();
        batch.commit().unwrap();
        let history = |cutoff| {
            ConfirmedHistory::read_certified(
                &store,
                NetworkId::Dev,
                fixture.blocks[0].hash,
                cutoff,
                &heads,
                &[],
            )
            .unwrap()
        };
        let cut = history(CUT);
        let parent = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            NetworkId::Dev,
            &fixture.params,
        )
        .unwrap();
        let replay = ConfirmedReplay::replay_governance_candidate(
            &cut,
            NetworkId::Dev,
            &fixture.params,
            &[],
        );
        if !valid {
            assert!(
                replay.is_err(),
                "unknown article must fail before endorsements are admitted"
            );
            continue;
        }
        let replay = replay.unwrap();
        let ballot =
            PreparedGovernance::from_replay(&cut, &parent, &fixture.params, replay).unwrap();
        chain::accept_manifest(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certified_votes::certify(&ballot.manifest().checkpoint, &[0]),
            &ReplayManifest::Governance(ballot.manifest().clone()),
        )
        .unwrap();
        PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap()
            .commit(&store)
            .unwrap();
        let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
        let parent = chain::next_anchor(&events, NetworkId::Dev, &fixture.params).unwrap();
        let cut = history(CUT + 11);
        let replay = ConfirmedReplay::replay_governance_candidate(
            &cut,
            NetworkId::Dev,
            &fixture.params,
            &events,
        )
        .unwrap();
        let activation = PreparedGovernance::from_replay(&cut, &parent, &fixture.params, replay);
        let activation = activation.unwrap();
        chain::accept_manifest(
            &store,
            NetworkId::Dev,
            &fixture.params,
            certified_votes::certify(&activation.manifest().checkpoint, &[0]),
            &ReplayManifest::Governance(activation.manifest().clone()),
        )
        .unwrap();
        let installed = PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap()
            .commit(&store)
            .unwrap();
        assert_eq!(
            installed.governance.constitution.get_constitution().version,
            1
        );
        let amendment = installed
            .governance
            .constitution
            .get_amendment(&proposal.into_tx_hash())
            .unwrap();
        assert_eq!(amendment.votes_yea, 1);
        assert_eq!(amendment.created_at.as_secs(), CUT + 11);
        let fresh_dir = tempfile::tempdir().unwrap();
        let fresh = LmdbStore::open(fresh_dir.path(), 32, 32 << 20).unwrap();
        let mut batch = fresh.tx_begin_write().unwrap();
        crate::payment_epoch::initialize(&mut batch, fixture.blocks[0].hash).unwrap();
        batch.commit().unwrap();
        for height in 1..=2 {
            let transfer = CertifiedCheckpoint::export_transfer(
                &store,
                NetworkId::Dev,
                &fixture.params,
                height,
            )
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
        let restored: burst_consti::ConstiEngine =
            bincode::deserialize(&fresh.meta_store().get_meta("consti_engine").unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(restored.get_constitution()).unwrap(),
            serde_json::to_value(installed.governance.constitution.get_constitution()).unwrap()
        );
        assert_eq!(
            serde_json::to_value(restored.get_amendment(&proposal.into_tx_hash()).unwrap())
                .unwrap(),
            serde_json::to_value(amendment).unwrap()
        );
    }
}
