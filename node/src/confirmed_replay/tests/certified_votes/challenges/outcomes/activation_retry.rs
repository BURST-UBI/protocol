use super::*;
use crate::{
    checkpoint_activation::PreparedActivation,
    checkpoint_agreement::{ReplayManifest, ReplayedCandidate},
    checkpoint_driver::{CheckpointDriver, Message},
    checkpoint_replay::SigningClock,
    checkpoint_store::historical::HistoricalAdmissions,
    confirmed_replay::installation::PreparedInstallation,
};
use std::sync::Arc;

#[test]
fn unsigned_activation_yields_to_a_completed_challenge_without_resetting_signed_choices() {
    let (mut fixture, first, _) = pending_reason(ChallengeReason::Fraud);
    let (_source_dir, source, selection) = certify_challenge(&fixture, &first);
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
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
    let initial =
        PreparedActivation::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 61)
            .unwrap()
            .unwrap();
    let anchor = initial.parent().clone();
    let seed = (0..=8)
        .chain([20])
        .find(|seed| address(*seed) == *anchor.proposer(0))
        .unwrap();
    let mut driver = CheckpointDriver::new_candidate(
        store.clone(),
        fixture.params.clone(),
        burst_crypto::keypair_from_seed(&[seed; 32]).private,
        ReplayedCandidate::Activation(Box::new(initial)),
    )
    .unwrap();
    driver.resume().unwrap();
    let journal = bincode::serialize(&driver.state().unwrap().unwrap()).unwrap();
    for voter in panel(&selection) {
        vote_choice(&mut fixture, &selection, voter, CUTOFF + 62, 2);
    }
    fixture.append_confirmed_prefix(&store, CUTOFF + 62);
    let vote = fixture.blocks.last().unwrap();
    store
        .meta_store()
        .reserve_consensus_vote_at(
            &anchor.hash(),
            &address(seed),
            &vote.account,
            &vote.previous,
            &vote.hash,
            true,
            0,
            Some(vote.timestamp.as_secs()),
        )
        .unwrap();
    let clock = Some(SigningClock {
        now: CUTOFF + 64,
        error_bound: 0,
    });
    driver.refresh_activation(clock).unwrap();
    assert_eq!(
        bincode::serialize(&driver.state().unwrap().unwrap()).unwrap(),
        journal
    );
    let messages = driver.propose(clock).unwrap();
    let manifest = messages
        .into_iter()
        .find_map(|message| match message {
            Message::Proposal { manifest, .. } => Some(manifest),
            _ => None,
        })
        .unwrap();
    let ReplayManifest::ChallengeOutcome(manifest) = manifest else {
        panic!("completed challenge must propose its outcome, not restart its window")
    };
    let expected = PreparedChallengeOutcome::prepare_next(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 63,
    )
    .unwrap()
    .unwrap();
    assert_eq!(&manifest, expected.manifest());
    assert_eq!(
        expected.state().account(&address(20)).unwrap().state,
        WalletState::Revoked
    );
    let signed = bincode::serialize(&driver.state().unwrap().unwrap()).unwrap();
    driver
        .refresh_activation(Some(SigningClock {
            now: CUTOFF + 65,
            error_bound: 0,
        }))
        .unwrap();
    assert_eq!(
        bincode::serialize(&driver.state().unwrap().unwrap()).unwrap(),
        signed
    );
    assert_eq!(store.meta_store().payment_epoch().unwrap(), anchor.hash());
}
