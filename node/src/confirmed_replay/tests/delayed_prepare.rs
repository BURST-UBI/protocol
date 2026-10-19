use super::*;
use crate::checkpoint_driver::{CheckpointDriver, Message};
use burst_consensus::checkpoint::CheckpointSignature;
use burst_consensus::checkpoint_round::{Ballot, Phase, Quorum, SignedBallot, Step};

#[test]
fn old_round_prepare_survives_deadline_catchup_and_restart_without_fresh_selection() {
    let DriverFixture {
        fixture,
        dirs,
        mut stores,
        mut drivers,
        manifests,
        leader,
        cutoff,
    } = checkpoint_drivers();
    let clock = Some(crate::checkpoint_replay::SigningClock {
        now: cutoff + 30,
        error_bound: 0,
    });
    let messages = drivers[leader].as_mut().unwrap().propose(clock).unwrap();
    let (proposal, manifest) = messages
        .into_iter()
        .find_map(|message| match message {
            Message::Proposal { proposal, manifest } => Some((proposal, manifest)),
            _ => None,
        })
        .unwrap();
    let sign = |seed: u8, ballot: Ballot| SignedBallot {
        signer: CheckpointSignature {
            representative: address(seed),
            signature: burst_crypto::sign_message(
                &ballot.signing_payload(),
                &burst_crypto::keypair_from_seed(&[seed; 32]).private,
            ),
        },
        ballot,
    };
    let mut ballot = Ballot::precommit(manifest.checkpoint(), 0);
    ballot.phase = Phase::Prevote;
    let proof = Quorum {
        ballot: ballot.clone(),
        signatures: (0..7)
            .map(|seed| sign(seed, ballot.clone()).signer)
            .collect(),
    };
    let index = (0..9)
        .find(|index| {
            &address(*index as u8) == drivers[leader].as_ref().unwrap().anchor().proposer(4)
        })
        .unwrap();
    let driver = drivers[index].as_mut().unwrap();
    let mut later = Ballot::precommit(manifest.checkpoint(), 4);
    later.phase = Phase::Prevote;
    later.value = None;
    driver
        .receive(
            Message::CatchUp {
                round: 4,
                evidence: (0..3).map(|seed| sign(seed, later.clone())).collect(),
            },
            None,
        )
        .unwrap();
    assert_eq!(driver.state().unwrap().unwrap().step(), Step::Propose);
    assert!(driver.state().unwrap().unwrap().valid_proof().is_none());
    let before = driver.state().unwrap();
    assert!(
        driver
            .propose(Some(crate::checkpoint_replay::SigningClock {
                now: cutoff + 3600,
                error_bound: 0,
            }))
            .is_err(),
        "round catch-up cannot authorize a fresh expired selection"
    );
    assert_eq!(driver.state().unwrap(), before);
    driver
        .refresh_candidate(Some(crate::checkpoint_replay::SigningClock {
            now: cutoff + 3601,
            error_bound: 0,
        }))
        .unwrap();
    assert_eq!(driver.state().unwrap(), before);
    let message = Message::Prepared {
        proposal: proposal.clone(),
        proof: proof.clone(),
        manifest: manifest.clone(),
    };
    let mut bad = proof.clone();
    bad.signatures.pop();
    assert!(driver
        .receive(
            Message::Prepared {
                proposal: proposal.clone(),
                proof: bad,
                manifest: manifest.clone()
            },
            None
        )
        .is_err());
    assert!(driver.state().unwrap().unwrap().valid_proof().is_none());
    let replies = driver.receive(message.clone(), None).unwrap();
    assert!(
        replies.is_empty(),
        "old prepare evidence must not sign a current-round vote"
    );
    let state = driver.state().unwrap().unwrap();
    assert_eq!(state.step(), Step::Propose);
    assert_eq!(state.valid_proof(), Some(&proof));
    driver.receive(message, None).unwrap();
    drop(drivers[index].take());
    drop(stores[index].take());
    let reopened = std::sync::Arc::new(LmdbStore::open(dirs[index].path(), 32, 32 << 20).unwrap());
    let replay = crate::checkpoint_store::CertifiedCheckpoint::replay_manifest(
        &reopened,
        &fixture.params,
        &manifests[index],
    )
    .unwrap();
    let mut restored = CheckpointDriver::new(
        reopened,
        fixture.params,
        burst_crypto::keypair_from_seed(&[index as u8; 32]).private,
        replay,
    )
    .unwrap();
    assert_eq!(restored.round(), 4);
    assert_eq!(
        restored.state().unwrap().unwrap().valid_proof(),
        Some(&proof)
    );
    restored
        .refresh_candidate(Some(crate::checkpoint_replay::SigningClock {
            now: cutoff + 3601,
            error_bound: 0,
        }))
        .unwrap();
    assert_eq!(
        restored.state().unwrap().unwrap().valid_proof(),
        Some(&proof)
    );
    let replies = restored.propose(None).unwrap();
    assert!(replies.iter().any(
        |message| matches!(message, Message::Proposal { proposal, .. }
        if proposal.body.checkpoint == *manifest.checkpoint() && proposal.body.round == 4
            && proposal.body.valid_round == Some(0))
    ));
    let mut recovered = Ballot::precommit(manifest.checkpoint(), 4);
    recovered.phase = Phase::Prevote;
    for seed in 0..7 {
        restored
            .receive(Message::Vote(sign(seed, recovered.clone())), None)
            .unwrap();
    }
    recovered.phase = Phase::Precommit;
    for seed in 0..7 {
        restored
            .receive(Message::Vote(sign(seed, recovered.clone())), None)
            .unwrap();
    }
    assert_eq!(
        restored
            .state()
            .unwrap()
            .unwrap()
            .decision()
            .unwrap()
            .checkpoint,
        *manifest.checkpoint()
    );
}
