use super::*;
use crate::checkpoint_agreement::{self, Command, Outgoing, ReplayInput, ReplayedCandidate};
use crate::checkpoint_replay::SigningClock;
use crate::durable_votes::generate_vote;
use burst_consensus::VoteGenerator;
use burst_store::meta::MetaStore;

#[test]
fn outcome_vote_fence_rejects_stale_cut_and_survives_signer_recovery() {
    let (mut fixture, chain) = completed(ChallengeReason::Fraud);
    let earlier = PreparedChallengeOutcome::build(
        &fixture.history(CUTOFF + 60),
        &fixture.params,
        &chain,
        &address(20),
    )
    .unwrap();
    merged_payments(&mut fixture);
    let tail = fixture.blocks.last().unwrap().hash;
    let (dir, store, heads) = restore_chain(&fixture, &chain);
    let current = PreparedChallengeOutcome::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 70,
        &heads,
        &address(20),
    )
    .unwrap();
    let anchor = current.parent().clone();
    let seed = (0..=8)
        .chain([20])
        .find(|seed| address(*seed) == *anchor.proposer(0))
        .unwrap();
    let key = burst_crypto::keypair_from_seed(&[seed; 32]);
    let mut generator = VoteGenerator::new(address(seed), key.private.0);
    let final_vote = generate_vote(&store, &mut generator, tail, true).unwrap();
    assert_eq!(final_vote.sequence, 1);
    let clock = Some(SigningClock {
        now: CUTOFF + 75,
        error_bound: 0,
    });
    assert!(checkpoint_agreement::apply_candidates(
        &store,
        &anchor,
        &key.private,
        Command::Propose(earlier.manifest().checkpoint.clone()),
        &[ReplayInput::ChallengeOutcome(&earlier)],
        clock,
    )
    .is_err());
    assert!(
        checkpoint_agreement::read_state(&store, &anchor, &address(seed))
            .unwrap()
            .is_none()
    );
    // A rejected cut must not leave a fence behind or consume signing sequence.
    assert_eq!(
        generate_vote(&store, &mut generator, tail, true)
            .unwrap()
            .sequence,
        2
    );
    let proposal = checkpoint_agreement::apply_candidates(
        &store,
        &anchor,
        &key.private,
        Command::Propose(current.manifest().checkpoint.clone()),
        &[ReplayInput::ChallengeOutcome(&current)],
        clock,
    )
    .unwrap();
    assert!(matches!(&proposal[..], [Outgoing::Proposal(_)]));
    for final_vote in [false, true] {
        assert!(generate_vote(&store, &mut generator, tail, final_vote).is_err());
    }
    assert_eq!(generator.sequence(), 2);
    assert_eq!(
        CertifiedCheckpoint::stored_tip(&store, NetworkId::Dev, &fixture.params).unwrap(),
        2
    );
    drop(store);

    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let mut generator = VoteGenerator::new(address(seed), key.private.0);
    assert!(generate_vote(&store, &mut generator, tail, true).is_err());
    assert_eq!(generator.sequence(), 0);
    let recovered =
        checkpoint_agreement::read_candidates(&store, &anchor, &address(seed), &fixture.params)
            .unwrap();
    let inputs: Vec<_> = recovered.iter().map(ReplayedCandidate::input).collect();
    assert_eq!(
        checkpoint_agreement::apply_candidates(
            &store,
            &anchor,
            &key.private,
            Command::Resume,
            &inputs,
            None,
        )
        .unwrap(),
        proposal
    );
    assert!(generate_vote(&store, &mut generator, tail, false).is_err());
    assert_eq!(
        store
            .meta_store()
            .get_meta(&format!("consensus_sequence/v1/{}", address(seed)))
            .unwrap(),
        2u64.to_be_bytes()
    );
    // Exercise the storage boundary only; live economic installation remains gated.
    let next = current.manifest().checkpoint.hash();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch
        .advance_payment_epoch(
            &current.state().payment_epoch(),
            &anchor.hash(),
            &next,
            current.history_hashes()
        )
        .unwrap()
        .is_empty());
    batch.commit().unwrap();
    assert!(checkpoint_agreement::apply_candidates(
        &store,
        &anchor,
        &key.private,
        Command::Resume,
        &inputs,
        None,
    )
    .is_err());
    assert!(matches!(
        store
            .meta_store()
            .get_meta(&format!("consensus_fence/v2/{next}/{}", address(seed))),
        Err(burst_store::StoreError::NotFound(_))
    ));
    assert!(generate_vote(&store, &mut generator, tail, true).is_err());
}

#[test]
fn rejected_outcome_clock_does_not_seal_ordinary_voting() {
    let (fixture, chain) = completed(ChallengeReason::Inactivity);
    let (_dir, store, heads) = restore_chain(&fixture, &chain);
    let current = PreparedChallengeOutcome::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 70,
        &heads,
        &address(20),
    )
    .unwrap();
    let anchor = current.parent();
    let seed = (0..=8)
        .chain([20])
        .find(|seed| address(*seed) == *anchor.proposer(0))
        .unwrap();
    let key = burst_crypto::keypair_from_seed(&[seed; 32]);
    assert!(checkpoint_agreement::apply_candidates(
        &store,
        anchor,
        &key.private,
        Command::Propose(current.manifest().checkpoint.clone()),
        &[ReplayInput::ChallengeOutcome(&current)],
        None,
    )
    .is_err());
    assert!(
        checkpoint_agreement::read_state(&store, anchor, &address(seed))
            .unwrap()
            .is_none()
    );
    let mut generator = VoteGenerator::new(address(seed), key.private.0);
    assert_eq!(
        generate_vote(
            &store,
            &mut generator,
            fixture.blocks.last().unwrap().hash,
            true
        )
        .unwrap()
        .sequence,
        1
    );
}
