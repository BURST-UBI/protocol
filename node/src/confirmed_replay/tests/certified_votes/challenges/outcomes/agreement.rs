use super::*;
use crate::checkpoint_agreement::{
    self, apply_candidates, read_candidates, Command, Outgoing, ReplayInput, ReplayedCandidate,
};
use crate::checkpoint_replay::SigningClock;
use burst_consensus::checkpoint_round::{Phase, Proposal, ProposalBody, Quorum, SignedBallot};

fn key(seed: u8) -> burst_types::KeyPair {
    burst_crypto::keypair_from_seed(&[seed; 32])
}

fn vote(output: Vec<Outgoing>) -> SignedBallot {
    assert_eq!(output.len(), 1);
    let Outgoing::Vote(vote) = output.into_iter().next().unwrap() else {
        panic!("expected signed ballot")
    };
    vote
}

fn clock() -> Option<SigningClock> {
    Some(SigningClock {
        now: CUTOFF + 75,
        error_bound: 0,
    })
}

#[test]
fn replayed_outcome_gets_quorum_through_durable_signers_and_recovers_after_restart() {
    let (mut fixture, chain) = completed(ChallengeReason::Fraud);
    merged_payments(&mut fixture);
    let history = fixture.history(CUTOFF + 70);
    let prepared =
        PreparedChallengeOutcome::build(&history, &fixture.params, &chain, &address(20)).unwrap();
    let anchor = prepared.parent().clone();
    let proposer = (0..=8)
        .chain([20])
        .find(|seed| address(*seed) == *anchor.proposer(0))
        .unwrap();
    let seeds: Vec<_> = std::iter::once(proposer)
        .chain((0..=8).chain([20]).filter(|seed| *seed != proposer))
        .take(7)
        .collect();
    let mut stores: Vec<_> = seeds
        .iter()
        .map(|_| restore_chain(&fixture, &chain))
        .collect();
    let Outgoing::Proposal(proposal) = apply_candidates(
        &stores[0].1,
        &anchor,
        &key(proposer).private,
        Command::Propose(prepared.manifest().checkpoint.clone()),
        &[ReplayInput::ChallengeOutcome(&prepared)],
        clock(),
    )
    .unwrap()
    .remove(0) else {
        panic!("expected replay-authorized proposal")
    };
    proposal.verify(&anchor).unwrap();
    let mut prevotes = Vec::new();
    for (index, seed) in seeds.iter().enumerate() {
        let local = PreparedChallengeOutcome::prepare(
            &stores[index].1,
            NetworkId::Dev,
            &fixture.params,
            CUTOFF + 70,
            &stores[index].2,
            &address(20),
        )
        .unwrap();
        assert_eq!(local.manifest(), prepared.manifest());
        prevotes.push(vote(
            apply_candidates(
                &stores[index].1,
                &anchor,
                &key(*seed).private,
                Command::Proposal(&proposal),
                &[ReplayInput::ChallengeOutcome(&local)],
                clock(),
            )
            .unwrap(),
        ));
    }
    let mut quorum = Quorum {
        ballot: prevotes[0].ballot.clone(),
        signatures: prevotes[..6]
            .iter()
            .map(|vote| vote.signer.clone())
            .collect(),
    };
    assert!(
        quorum.verify(&anchor).is_err(),
        "six of ten humans cannot prepare an outcome"
    );
    quorum.signatures.push(prevotes[6].signer.clone());
    quorum.verify(&anchor).unwrap();
    assert_eq!(quorum.ballot.phase, Phase::Prevote);
    let mut commits = Vec::new();
    for (index, seed) in seeds.iter().enumerate() {
        let (dir, store, heads) = stores.remove(0);
        drop(store);
        let reopened = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let recovered =
            read_candidates(&reopened, &anchor, &address(*seed), &fixture.params).unwrap();
        assert_eq!(recovered.len(), 1);
        assert!(matches!(
            &recovered[0],
            ReplayedCandidate::ChallengeOutcome(_)
        ));
        assert!(checkpoint_agreement::read_replays(
            &reopened,
            &anchor,
            &address(*seed),
            &fixture.params
        )
        .is_err());
        let replayed = recovered
            .iter()
            .map(ReplayedCandidate::input)
            .collect::<Vec<_>>();
        let resumed = apply_candidates(
            &reopened,
            &anchor,
            &key(*seed).private,
            Command::Resume,
            &replayed,
            None,
        )
        .unwrap();
        assert!(resumed
            .iter()
            .any(|message| matches!(message, Outgoing::Vote(vote) if vote == &prevotes[index])));
        commits.push(vote(
            apply_candidates(
                &reopened,
                &anchor,
                &key(*seed).private,
                Command::Prepared(&proposal, &quorum),
                &replayed,
                None,
            )
            .unwrap(),
        ));
        stores.push((dir, reopened, heads));
    }
    let certificate = CheckpointCertificate {
        checkpoint: prepared.manifest().checkpoint.clone(),
        round: 0,
        signatures: commits
            .iter()
            .map(|vote| {
                assert_eq!(vote.ballot.phase, Phase::Precommit);
                vote.signer.clone()
            })
            .collect(),
    };
    let certified = prepared.certify(certificate.clone()).unwrap();
    assert_eq!(
        certified.state().account(&address(20)).unwrap().state,
        WalletState::Revoked
    );
    for ((dir, store, _), seed) in stores.into_iter().zip(seeds) {
        let recovered = read_candidates(&store, &anchor, &address(seed), &fixture.params).unwrap();
        let inputs = recovered
            .iter()
            .map(ReplayedCandidate::input)
            .collect::<Vec<_>>();
        assert_eq!(
            apply_candidates(
                &store,
                &anchor,
                &key(seed).private,
                Command::Decide(certificate.clone()),
                &inputs,
                None
            )
            .unwrap(),
            vec![Outgoing::Decision(certificate.clone())]
        );
        drop(store);
        let reopened = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let recovered =
            read_candidates(&reopened, &anchor, &address(seed), &fixture.params).unwrap();
        let inputs = recovered
            .iter()
            .map(ReplayedCandidate::input)
            .collect::<Vec<_>>();
        assert_eq!(
            apply_candidates(
                &reopened,
                &anchor,
                &key(seed).private,
                Command::Resume,
                &inputs,
                None
            )
            .unwrap(),
            vec![Outgoing::Decision(certificate.clone())]
        );
    }
}

#[test]
fn outcome_and_selection_share_one_signer_record_and_cannot_double_prevote() {
    let (mut fixture, chain) = completed(ChallengeReason::Fraud);
    for seed in [1, 2] {
        fixture.push(seed, BlockType::Endorse, CUTOFF + 61, |block| {
            block.link = destination(21);
            block.brn_spent += 1;
        });
    }
    let (_dir, store, heads) = restore_chain(&fixture, &chain);
    let outcome = PreparedChallengeOutcome::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 70,
        &heads,
        &address(20),
    )
    .unwrap();
    let selection = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 70,
        &heads,
        &address(21),
    )
    .unwrap();
    let anchor = outcome.parent();
    let seed = (0..=8)
        .chain([20])
        .find(|seed| address(*seed) == *anchor.proposer(0))
        .unwrap();
    let key = key(seed);
    let inputs = [
        ReplayInput::ChallengeOutcome(&outcome),
        ReplayInput::Selection(&selection),
    ];
    let Outgoing::Proposal(proposal) = apply_candidates(
        &store,
        anchor,
        &key.private,
        Command::Propose(outcome.manifest().checkpoint.clone()),
        &inputs,
        clock(),
    )
    .unwrap()
    .remove(0) else {
        panic!("expected proposal")
    };
    let signed = vote(
        apply_candidates(
            &store,
            anchor,
            &key.private,
            Command::Proposal(&proposal),
            &inputs,
            clock(),
        )
        .unwrap(),
    );
    let before = checkpoint_agreement::read_state(&store, anchor, &address(seed)).unwrap();
    assert!(
        checkpoint_agreement::apply(
            &store,
            anchor,
            &key.private,
            Command::Resume,
            &[&selection],
            None
        )
        .is_err(),
        "the selection-only API must not forget an outcome reservation"
    );
    let body = ProposalBody {
        checkpoint: selection.checkpoint().clone(),
        round: 0,
        valid_round: None,
    };
    let conflict = Proposal {
        signer: CheckpointSignature {
            representative: address(seed),
            signature: burst_crypto::sign_message(&body.signing_payload(), &key.private),
        },
        body,
        justification: None,
    };
    conflict.verify(anchor).unwrap();
    assert!(apply_candidates(
        &store,
        anchor,
        &key.private,
        Command::Proposal(&conflict),
        &inputs,
        clock()
    )
    .is_err());
    assert_eq!(
        checkpoint_agreement::read_state(&store, anchor, &address(seed)).unwrap(),
        before
    );
    let resumed =
        apply_candidates(&store, anchor, &key.private, Command::Resume, &inputs, None).unwrap();
    assert!(resumed
        .iter()
        .any(|message| matches!(message, Outgoing::Vote(vote) if vote == &signed)));
}

#[test]
fn fresh_outcome_signatures_require_a_cut_in_the_trusted_past() {
    let (fixture, chain) = completed(ChallengeReason::Inactivity);
    let (_dir, store, heads) = restore_chain(&fixture, &chain);
    let candidate = PreparedChallengeOutcome::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 70,
        &heads,
        &address(20),
    )
    .unwrap();
    let anchor = candidate.parent();
    let seed = (0..=8)
        .chain([20])
        .find(|seed| address(*seed) == *anchor.proposer(0))
        .unwrap();
    for clock in [
        None,
        Some(SigningClock {
            now: CUTOFF + 69,
            error_bound: 0,
        }),
        Some(SigningClock {
            now: CUTOFF + 71,
            error_bound: 2,
        }),
        Some(SigningClock {
            now: 0,
            error_bound: u64::MAX,
        }),
    ] {
        assert!(apply_candidates(
            &store,
            anchor,
            &key(seed).private,
            Command::Propose(candidate.manifest().checkpoint.clone()),
            &[ReplayInput::ChallengeOutcome(&candidate)],
            clock
        )
        .is_err());
        assert!(
            checkpoint_agreement::read_state(&store, anchor, &address(seed))
                .unwrap()
                .is_none()
        );
    }
    assert!(apply_candidates(
        &store,
        anchor,
        &key(seed).private,
        Command::Propose(candidate.manifest().checkpoint.clone()),
        &[ReplayInput::ChallengeOutcome(&candidate)],
        clock()
    )
    .is_ok());
}
