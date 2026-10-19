use super::*;
use crate::{
    checkpoint_agreement::{self, ReplayManifest, ReplayedCandidate},
    checkpoint_driver::{CheckpointDriver, Message},
    checkpoint_replay::SigningClock,
    checkpoint_service::{Clock, Runtime, Settings},
};
use std::{sync::Arc, time::Instant};

#[test]
fn outcome_runtime_recovers_decided_journal_before_chain_record_without_clock() {
    let (fixture, selections) = completed(ChallengeReason::Fraud);
    let (dir, store, heads) = restore_chain(&fixture, &selections);
    let prepared = PreparedChallengeOutcome::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 70,
        &heads,
        &address(20),
    )
    .unwrap();
    let parent = prepared.parent().clone();
    let seed = (0..=8)
        .chain([20])
        .find(|seed| address(*seed) == *parent.proposer(0))
        .unwrap();
    let manifest = prepared.manifest().clone();
    let certificate = certify(&manifest.checkpoint, &[0, 1, 2, 3, 4, 5, 20]);
    let before =
        bincode::serialize(&store.account_store().get_account(&address(20)).unwrap()).unwrap();
    let store = Arc::new(store);
    let key = Arc::new(burst_crypto::keypair_from_seed(&[seed; 32]).private);
    let mut driver = CheckpointDriver::new_candidate(
        store.clone(),
        fixture.params.clone(),
        key.clone(),
        ReplayedCandidate::ChallengeOutcome(Box::new(prepared)),
    )
    .unwrap();
    driver
        .propose(Some(SigningClock {
            now: CUTOFF + 75,
            error_bound: 0,
        }))
        .unwrap();
    driver
        .receive(
            Message::Decision {
                certificate: certificate.clone(),
                manifest: ReplayManifest::ChallengeOutcome(manifest),
            },
            None,
        )
        .unwrap();
    assert_eq!(
        CertifiedCheckpoint::stored_tip(&store, NetworkId::Dev, &fixture.params).unwrap(),
        2
    );
    let journal = bincode::serialize(&driver.state().unwrap().unwrap()).unwrap();
    drop(driver);
    assert_eq!(Arc::strong_count(&store), 1);
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
    let chain =
        crate::checkpoint_store::chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    assert_eq!(chain.len(), 3);
    assert_eq!(chain.last().unwrap().certificate(), &certificate);
    assert_eq!(
        bincode::serialize(
            &checkpoint_agreement::read_state(&store, &parent, &address(seed))
                .unwrap()
                .unwrap()
        )
        .unwrap(),
        journal
    );
    assert_eq!(
        bincode::serialize(&store.account_store().get_account(&address(20)).unwrap()).unwrap(),
        before
    );
    let mut voter = burst_consensus::VoteGenerator::new(
        address(seed),
        burst_crypto::keypair_from_seed(&[seed; 32]).private.0,
    );
    assert!(crate::durable_votes::generate_vote(
        &store,
        &mut voter,
        fixture.blocks.last().unwrap().hash,
        true
    )
    .is_err());
}

#[test]
fn outcome_discovery_requires_all_votes_and_excludes_rejected_panels() {
    for choice in [1, 2] {
        let (mut fixture, first, _) = pending_reason(ChallengeReason::Fraud);
        let (_dir, _store, second) = certify_challenge(&fixture, &first);
        let voters = panel(&second);
        for seed in &voters[..voters.len() - 1] {
            vote_choice(&mut fixture, &second, *seed, CUTOFF + 60, choice);
        }
        let selections = vec![first, second];
        let (_dir, store, _) = restore_chain(&fixture, &selections);
        assert!(PreparedChallengeOutcome::prepare_next(
            &store,
            NetworkId::Dev,
            &fixture.params,
            CUTOFF + 70
        )
        .unwrap()
        .is_none());
        vote_choice(
            &mut fixture,
            &selections[1],
            *voters.last().unwrap(),
            CUTOFF + 60,
            choice,
        );
        let (_dir, store, _) = restore_chain(&fixture, &selections);
        let candidate = PreparedChallengeOutcome::prepare_next(
            &store,
            NetworkId::Dev,
            &fixture.params,
            CUTOFF + 70,
        )
        .unwrap();
        assert_eq!(candidate.is_some(), choice == 2);
        if let Some(candidate) = candidate {
            assert_eq!(candidate.manifest().target, address(20));
            assert_eq!(candidate.manifest().checkpoint.height, 3);
        }
    }
}
