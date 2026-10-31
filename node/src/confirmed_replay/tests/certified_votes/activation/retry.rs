use super::*;
use crate::{
    checkpoint_agreement::{self, ReplayedCandidate},
    checkpoint_driver::{CheckpointDriver, Message},
    checkpoint_replay::SigningClock,
};
use burst_consensus::{
    checkpoint::{Checkpoint, CheckpointAnchor, CheckpointSignature},
    checkpoint_round::{Ballot, Phase, Proposal, ProposalBody, Quorum, SignedBallot, Step},
};
use std::sync::Arc;

fn signature(seed: u8, payload: &[u8]) -> CheckpointSignature {
    CheckpointSignature {
        representative: address(seed),
        signature: burst_crypto::sign_message(
            payload,
            &burst_crypto::keypair_from_seed(&[seed; 32]).private,
        ),
    }
}

fn proposer(anchor: &CheckpointAnchor, round: u64) -> u8 {
    (0..=8)
        .find(|seed| address(*seed) == *anchor.proposer(round))
        .unwrap()
}

fn proposal(checkpoint: &Checkpoint, anchor: &CheckpointAnchor) -> Proposal {
    let body = ProposalBody {
        checkpoint: checkpoint.clone(),
        round: 0,
        valid_round: None,
    };
    Proposal {
        signer: signature(proposer(anchor, 0), &body.signing_payload()),
        body,
        justification: None,
    }
}

fn clock(offset: u64) -> Option<SigningClock> {
    Some(SigningClock {
        now: CUTOFF + offset,
        error_bound: 0,
    })
}

fn proposed(messages: Vec<Message>) -> (Proposal, ReplayManifest) {
    messages
        .into_iter()
        .find_map(|message| match message {
            Message::Proposal { proposal, manifest } => Some((*proposal, manifest)),
            _ => None,
        })
        .expect("signed proposal")
}

#[test]
fn expired_unsigned_activation_retries_without_changing_the_committee() {
    let (fixture, _dir, store, selection, _) = setup();
    let initial =
        PreparedActivation::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 31)
            .unwrap()
            .unwrap();
    let original = initial.manifest().clone();
    let seed = proposer(initial.parent(), 0);
    let store = Arc::new(store);
    let mut driver = CheckpointDriver::new_candidate(
        store.clone(),
        fixture.params.clone(),
        burst_crypto::keypair_from_seed(&[seed; 32]).private,
        ReplayedCandidate::Activation(Box::new(initial)),
    )
    .unwrap();
    driver.resume().unwrap();
    let journal = bincode::serialize(&driver.state().unwrap().unwrap()).unwrap();
    driver.refresh_activation(None).unwrap();
    assert!(driver
        .refresh_activation(Some(SigningClock {
            now: CUTOFF + 200,
            error_bound: 60,
        }))
        .is_err());
    driver.refresh_activation(clock(101)).unwrap();
    assert_eq!(
        bincode::serialize(&driver.state().unwrap().unwrap()).unwrap(),
        journal
    );
    let (proposal, manifest) = proposed(driver.propose(clock(101)).unwrap());
    let ReplayManifest::Activation(retried) = manifest else {
        panic!("activation")
    };
    assert_eq!(retried.checkpoint.cutoff, CUTOFF + 100);
    assert_eq!(retried.activation, original.activation);
    assert_eq!(retried.checkpoint.parent, original.checkpoint.parent);
    assert_eq!(proposal.body.round, 0);
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
fn unsigned_activation_includes_an_early_final_vote_before_its_start_expires() {
    let (mut fixture, _dir, store, selection, _) = setup();
    let initial =
        PreparedActivation::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 31)
            .unwrap()
            .unwrap();
    let original = initial.manifest().clone();
    let seed = proposer(initial.parent(), 0);
    let store = Arc::new(store);
    let mut driver = CheckpointDriver::new_candidate(
        store.clone(),
        fixture.params.clone(),
        burst_crypto::keypair_from_seed(&[seed; 32]).private,
        ReplayedCandidate::Activation(Box::new(initial)),
    )
    .unwrap();
    driver.resume().unwrap();
    let journal = bincode::serialize(&driver.state().unwrap().unwrap()).unwrap();

    let voter = panel(&selection)[0];
    vote(&mut fixture, &selection, voter, CUTOFF + 32);
    fixture.append_confirmed_prefix(&store, CUTOFF + 32);
    let early_vote = fixture.blocks.last().unwrap();
    store
        .meta_store()
        .reserve_consensus_vote_at(
            &selection.certificate().checkpoint.hash(),
            &address(seed),
            &early_vote.account,
            &early_vote.previous,
            &early_vote.hash,
            true,
            0,
            Some(early_vote.timestamp.as_secs()),
        )
        .unwrap();
    let error = driver.propose(clock(34)).err().unwrap();
    assert!(
        error.to_string().contains("omits a durable final vote"),
        "{error}"
    );
    driver.refresh_activation(clock(34)).unwrap();
    assert_eq!(
        bincode::serialize(&driver.state().unwrap().unwrap()).unwrap(),
        journal
    );
    let (_, manifest) = proposed(driver.propose(clock(34)).unwrap());
    let ReplayManifest::Activation(retried) = manifest else {
        panic!("activation")
    };
    assert_eq!(retried.activation, original.activation);
    assert_eq!(retried.checkpoint.parent, original.checkpoint.parent);
    assert_eq!(retried.checkpoint.cutoff, CUTOFF + 33);
    assert!(retried
        .heads
        .iter()
        .any(|head| head.hash == early_vote.hash));

    let signed = bincode::serialize(&driver.state().unwrap().unwrap()).unwrap();
    driver.refresh_activation(clock(35)).unwrap();
    assert_eq!(
        bincode::serialize(&driver.state().unwrap().unwrap()).unwrap(),
        signed
    );
    assert_eq!(
        store.meta_store().payment_epoch().unwrap(),
        original.checkpoint.parent
    );
}

#[test]
fn expired_activation_moves_only_after_participation_advances_the_round() {
    let (fixture, _dir, store, _, _) = setup();
    let initial =
        PreparedActivation::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 31)
            .unwrap()
            .unwrap();
    let original = initial.manifest().clone();
    let anchor = initial.parent().clone();
    let seed = proposer(&anchor, 1);
    let mut driver = CheckpointDriver::new_candidate(
        Arc::new(store),
        fixture.params.clone(),
        burst_crypto::keypair_from_seed(&[seed; 32]).private,
        ReplayedCandidate::Activation(Box::new(initial)),
    )
    .unwrap();
    driver
        .receive(
            Message::Proposal {
                proposal: Box::new(proposal(&original.checkpoint, &anchor)),
                manifest: ReplayManifest::Activation(original.clone()),
            },
            clock(32),
        )
        .unwrap();
    let journal = bincode::serialize(&driver.state().unwrap().unwrap()).unwrap();
    driver.refresh_activation(clock(101)).unwrap();
    assert_eq!(
        bincode::serialize(&driver.state().unwrap().unwrap()).unwrap(),
        journal
    );
    assert!(
        driver.timeout(Step::Prevote).is_err(),
        "time alone cannot advance agreement"
    );
    for phase in [Phase::Prevote, Phase::Precommit] {
        let mut ballot = Ballot::precommit(&original.checkpoint, 0);
        ballot.phase = phase;
        ballot.value = None;
        for voter in (0..=8).filter(|voter| *voter != seed) {
            driver
                .receive(
                    Message::Vote(SignedBallot {
                        signer: signature(voter, &ballot.signing_payload()),
                        ballot: ballot.clone(),
                    }),
                    None,
                )
                .unwrap();
        }
        driver
            .timeout(match phase {
                Phase::Prevote => Step::Prevote,
                Phase::Precommit => Step::Precommit,
            })
            .unwrap();
    }
    assert_eq!(driver.round(), 1);
    driver.refresh_activation(clock(101)).unwrap();
    let (proposal, manifest) = proposed(driver.propose(clock(101)).unwrap());
    let ReplayManifest::Activation(retried) = manifest else {
        panic!("activation")
    };
    assert_eq!(retried.activation, original.activation);
    assert_eq!(retried.checkpoint.cutoff, CUTOFF + 100);
    assert_eq!(proposal.body.round, 1);
    assert!(proposal.justification.is_none());
}

#[test]
fn completed_normal_panel_replaces_unsigned_activation_with_roster_or_revote() {
    for choice in [1, 2] {
        let (mut fixture, _dir, store, selection, _) = setup_config(|params| {
            params.num_verifiers = 3;
        });
        let initial =
            PreparedActivation::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 31)
                .unwrap()
                .unwrap();
        let anchor = initial.parent().clone();
        let seed = proposer(&anchor, 0);
        let store = Arc::new(store);
        let mut driver = CheckpointDriver::new_candidate(
            store.clone(),
            fixture.params.clone(),
            burst_crypto::keypair_from_seed(&[seed; 32]).private,
            ReplayedCandidate::Activation(Box::new(initial)),
        )
        .unwrap();
        driver.resume().unwrap();
        let journal = bincode::serialize(&driver.state().unwrap().unwrap()).unwrap();
        let start = fixture.blocks.len();
        for voter in panel(&selection) {
            vote_choice(&mut fixture, &selection, voter, CUTOFF + 32, choice);
        }
        fixture.append_confirmed_prefix(&store, CUTOFF + 32);
        for vote in &fixture.blocks[start..] {
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
        }
        driver.refresh_activation(clock(34)).unwrap();
        assert_eq!(
            bincode::serialize(&driver.state().unwrap().unwrap()).unwrap(),
            journal
        );
        let (_, manifest) = proposed(driver.propose(clock(34)).unwrap());
        match choice {
            1 => {
                let ReplayManifest::Roster(manifest) = manifest else {
                    panic!("roster")
                };
                assert_ne!(
                    manifest.checkpoint.next_roster,
                    anchor.roster().commitment()
                );
                assert_eq!(manifest.checkpoint.cutoff, CUTOFF + 33);
            }
            _ => {
                let ReplayManifest::Selection(manifest) = manifest else {
                    panic!("revote selection")
                };
                let expected = CertifiedCheckpoint::prepare_next(
                    &store,
                    NetworkId::Dev,
                    &fixture.params,
                    CUTOFF + 34,
                )
                .unwrap()
                .unwrap();
                assert_eq!(manifest, expected.manifest());
                assert_eq!(expected.request().attempt(), 1);
                assert_eq!(expected.request().target(), &address(20));
            }
        }
        assert_eq!(store.meta_store().payment_epoch().unwrap(), anchor.hash());
    }
}

#[test]
fn prepared_activation_survives_expiry_restart_and_a_later_round() {
    let (fixture, dir, store, _, _) = setup();
    let initial =
        PreparedActivation::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 31)
            .unwrap()
            .unwrap();
    let original = initial.manifest().clone();
    let anchor = initial.parent().clone();
    let seed = proposer(&anchor, 1);
    let key = Arc::new(burst_crypto::keypair_from_seed(&[seed; 32]).private);
    let store = Arc::new(store);
    let mut driver = CheckpointDriver::new_candidate(
        store.clone(),
        fixture.params.clone(),
        key.clone(),
        ReplayedCandidate::Activation(Box::new(initial)),
    )
    .unwrap();
    let mut ballot = Ballot::precommit(&original.checkpoint, 0);
    ballot.phase = Phase::Prevote;
    let proof = Quorum {
        signatures: (0..7)
            .map(|voter| signature(voter, &ballot.signing_payload()))
            .collect(),
        ballot,
    };
    driver
        .receive(
            Message::Prepared {
                proposal: Box::new(proposal(&original.checkpoint, &anchor)),
                proof: proof.clone(),
                manifest: ReplayManifest::Activation(original.clone()),
            },
            None,
        )
        .unwrap();
    let mut ballot = Ballot::precommit(&original.checkpoint, 0);
    ballot.value = None;
    for voter in (0..=8).filter(|voter| *voter != seed) {
        driver
            .receive(
                Message::Vote(SignedBallot {
                    signer: signature(voter, &ballot.signing_payload()),
                    ballot: ballot.clone(),
                }),
                None,
            )
            .unwrap();
    }
    driver.timeout(Step::Precommit).unwrap();
    drop(driver);
    drop(store);
    let store = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
    let retained =
        checkpoint_agreement::read_candidates(&store, &anchor, &address(seed), &fixture.params)
            .unwrap();
    let mut driver = CheckpointDriver::new_candidate(
        store,
        fixture.params,
        key,
        retained.into_iter().next().unwrap(),
    )
    .unwrap();
    driver.refresh_activation(clock(200)).unwrap();
    let (proposal, manifest) = proposed(driver.propose(None).unwrap());
    assert_eq!(manifest, ReplayManifest::Activation(original));
    assert_eq!(proposal.body.round, 1);
    assert_eq!(proposal.justification, Some(proof));
}

#[test]
fn activation_retry_rejects_an_old_cutoff_or_a_new_parent() {
    let (fixture, _dir, store, _, _) = setup();
    let initial =
        PreparedActivation::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 31)
            .unwrap()
            .unwrap();
    for cutoff in [CUTOFF, CUTOFF + 31] {
        let error = initial
            .prepare_later(&store, &fixture.params, cutoff)
            .err()
            .unwrap();
        assert!(error.to_string().contains("advance its cutoff"), "{error}");
    }
    let epoch = activate(&fixture, &store);
    let error = initial
        .prepare_later(&store, &fixture.params, CUTOFF + 100)
        .err()
        .unwrap();
    assert!(error.to_string().contains("parent has advanced"), "{error}");
    assert_eq!(store.meta_store().payment_epoch().unwrap(), epoch);
    assert_eq!(
        chain::load(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activation_worker_refreshes_an_expired_start_without_deciding_alone() {
    use crate::{
        checkpoint_service::{self, Clock, Session},
        connection_registry::ConnectionRegistry,
        shutdown::ShutdownController,
    };
    use std::{
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        time::{Duration, Instant},
    };
    use tokio::sync::{mpsc, RwLock};

    let (fixture, _dir, store, selection, _) = setup();
    let initial =
        PreparedActivation::prepare_next(&store, NetworkId::Dev, &fixture.params, CUTOFF + 31)
            .unwrap()
            .unwrap();
    let anchor = initial.parent().clone();
    let seed = proposer(&anchor, 0);
    let store = Arc::new(store);
    let driver = CheckpointDriver::new_candidate(
        store.clone(),
        fixture.params.clone(),
        burst_crypto::keypair_from_seed(&[seed; 32]).private,
        ReplayedCandidate::Activation(Box::new(initial)),
    )
    .unwrap();
    let session = Session::new(driver, Instant::now(), Duration::from_secs(60)).unwrap();
    let (_sender, receiver) = mpsc::channel(4);
    let stop = Arc::new(ShutdownController::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let trusted = Arc::new(AtomicBool::new(false));
    let observed_calls = calls.clone();
    let observed_trust = trusted.clone();
    let clock: Clock = Arc::new(move || {
        let first = observed_calls.fetch_add(1, Ordering::SeqCst) == 0;
        if first || observed_trust.load(Ordering::SeqCst) {
            clock(101)
        } else {
            None
        }
    });
    let worker = tokio::spawn(checkpoint_service::run(
        session,
        Arc::new(RwLock::new(ConnectionRegistry::new())),
        receiver,
        clock,
        stop.clone(),
    ));
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        while calls.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let state = checkpoint_agreement::read_state(&store, &anchor, &address(seed))
            .unwrap()
            .unwrap();
        assert!(
            state.proposal().is_none(),
            "clock trust lost during replay must prevent signing"
        );
        trusted.store(true, Ordering::SeqCst);
        loop {
            let state = checkpoint_agreement::read_state(&store, &anchor, &address(seed))
                .unwrap()
                .unwrap();
            if let Some(proposal) = state.proposal() {
                assert_eq!(proposal.checkpoint.cutoff, CUTOFF + 100);
                assert!(state.decision().is_none());
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    stop.shutdown();
    tokio::time::timeout(Duration::from_secs(15), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    result.unwrap();
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
fn compact_unsigned_activation_refreshes_finality_without_replacing_signed_choices() {
    use crate::checkpoint_agreement::CompactReplayCandidate;
    use crate::confirmed_replay::tests::snapshot_history::{authenticate, compact::retained};
    let (mut fixture, _dir, store, selection, _) = setup();
    let initial = PreparedActivation::prepare_next(
        &store, NetworkId::Dev, &fixture.params, CUTOFF + 31,
    ).unwrap().unwrap().manifest().clone();
    let coverage = |fixture: &Fixture, manifest: &crate::checkpoint_activation::ActivationManifest| {
        let snapshot = authenticate(&store, &fixture.params, manifest.checkpoint.parent);
        let witnesses = retained(&snapshot, fixture, &store, &[]);
        let base = snapshot.into_continuation();
        let tail = base.read_tail(&store, manifest.checkpoint.cutoff, &manifest.heads, witnesses).unwrap();
        (base.anchor().clone(), CompactReplayCandidate::Activation(Box::new(
            base.activation_vote_coverage_for_manifest(&tail, manifest, &[&selection]).unwrap(),
        )))
    };
    let (anchor, initial_coverage) = coverage(&fixture, &initial);
    let seed = proposer(&anchor, 0);
    let store = Arc::new(store);
    let mut driver = CheckpointDriver::new_compact(
        store.clone(), fixture.params.clone(),
        Arc::new(burst_crypto::keypair_from_seed(&[seed; 32]).private),
        anchor, initial_coverage, Vec::new(),
    ).unwrap();
    driver.resume().unwrap();
    let journal = bincode::serialize(&driver.state().unwrap().unwrap()).unwrap();
    assert!(driver.compact_candidate_refresh_signer().unwrap().is_none());
    let voter = panel(&selection)[0];
    vote(&mut fixture, &selection, voter, CUTOFF + 32);
    fixture.append_confirmed_prefix(&store, CUTOFF + 32);
    let early = fixture.blocks.last().unwrap();
    store.meta_store().reserve_consensus_vote_at(
        &selection.certificate().checkpoint.hash(), &address(seed), &early.account,
        &early.previous, &early.hash, true, 0, Some(early.timestamp.as_secs()),
    ).unwrap();
    assert!(driver.propose(clock(34)).is_err());
    assert_eq!(driver.compact_candidate_refresh_signer().unwrap(), Some(address(seed)));
    let fresh = PreparedActivation::prepare_next(
        &store, NetworkId::Dev, &fixture.params, CUTOFF + 33,
    ).unwrap().unwrap().manifest().clone();
    let compact = |manifest: &crate::checkpoint_activation::ActivationManifest| {
        let snapshot = authenticate(&store, &fixture.params, manifest.checkpoint.parent);
        let witnesses = retained(&snapshot, &fixture, &store, &[]);
        let base = snapshot.into_continuation();
        let tail = base.read_tail(&store, manifest.checkpoint.cutoff, &manifest.heads, witnesses).unwrap();
        CompactReplayCandidate::Activation(Box::new(
            base.activation_vote_coverage_for_manifest(&tail, manifest, &[&selection]).unwrap(),
        ))
    };
    assert!(driver.replace_unsigned_compact_candidate(compact(&fresh)).unwrap());
    assert_eq!(bincode::serialize(&driver.state().unwrap().unwrap()).unwrap(), journal);
    let (_, manifest) = proposed(driver.propose(clock(34)).unwrap());
    assert_eq!(manifest, ReplayManifest::Activation(fresh.clone()));
    assert_eq!(fresh.activation, initial.activation);
    assert!(fresh.heads.iter().any(|head| head.hash == early.hash));
    let signed = bincode::serialize(&driver.state().unwrap().unwrap()).unwrap();
    assert!(driver.compact_candidate_refresh_signer().unwrap().is_none());
    let alternative = PreparedActivation::prepare_next(
        &store, NetworkId::Dev, &fixture.params, CUTOFF + 34,
    ).unwrap().unwrap().manifest().clone();
    assert!(!driver.replace_unsigned_compact_candidate(compact(&alternative)).unwrap());
    assert_eq!(bincode::serialize(&driver.state().unwrap().unwrap()).unwrap(), signed);
    assert!(store.meta_store().reserve_consensus_vote_at(
        &selection.certificate().checkpoint.hash(), &address(seed), &early.account,
        &early.hash, &BlockHash::new([91; 32]), false, 0, Some(CUTOFF + 34),
    ).is_err(), "refresh must not release the durable signing fence");
}
