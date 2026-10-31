use super::*;
use crate::checkpoint_agreement::{self, Command, Outgoing, ReplayedCandidate};
use crate::checkpoint_replay::SigningClock;
use crate::durable_votes::generate_vote;
use burst_consensus::VoteGenerator;
use burst_store::meta::MetaStore;

#[test]
fn selection_freezes_endorsements_before_its_beacon_or_first_vote() {
    let mut fixture = Fixture::selection();
    let record = certificate(&fixture, false);
    let epoch = record.certificate().checkpoint.hash();
    let state = replay(&fixture, std::slice::from_ref(&record)).unwrap();
    assert_eq!(
        state.verification.selection_request(&address(20)),
        Some(record.replayed().request())
    );
    assert!(state.verification.selection_context(&address(20)).is_none());
    fixture.push(3, BlockType::Endorse, CUTOFF + 1, |block| {
        block.payment_epoch = epoch;
        block.link = destination(20);
        block.brn_spent += 1;
    });
    assert!(replay(&fixture, &[record])
        .err()
        .unwrap()
        .to_string()
        .contains("frozen selection"));
}

#[test]
fn selection_closes_the_payment_context_even_without_a_verifier_vote() {
    let mut fixture = Fixture::selection();
    let record = certificate(&fixture, true);
    let epoch = record.certificate().checkpoint.hash();
    let records = [record];
    assert_eq!(replay(&fixture, &records).unwrap().payment_epoch(), epoch);
    let count = fixture.blocks.len();
    for context in [
        fixture.blocks[0].hash,
        BlockHash::ZERO,
        BlockHash::new([91; 32]),
        epoch,
    ] {
        fixture.blocks.truncate(count);
        fixture.push(0, BlockType::Burn, CUTOFF + 31, |block| {
            block.payment_epoch = context;
            block.link = destination(21);
            block.brn_spent += 1;
        });
        let result = replay(&fixture, &records);
        if context == epoch {
            assert_eq!(result.unwrap().payment_epoch(), epoch);
        } else {
            assert!(result.err().unwrap().to_string().contains("payment epoch"));
        }
    }
    fixture.blocks.truncate(count);
    fixture.push(0, BlockType::Burn, CUTOFF - 1, |block| {
        block.payment_epoch = epoch;
        block.link = destination(21);
        block.brn_spent += 1;
    });
    assert!(replay(&fixture, &records)
        .err()
        .unwrap()
        .to_string()
        .contains("predates"));
}

#[test]
fn selection_signer_fences_payment_votes_and_cannot_reseal_a_new_epoch_on_restart() {
    let mut fixture = Fixture::selection();
    let old_heads = fixture.history(CUTOFF).heads().to_vec();
    let tail = fixture.push(0, BlockType::Burn, CUTOFF + 1, |block| {
        block.link = destination(21);
        block.brn_spent += 1;
    });
    let (dir, store, heads) = fixture.store();
    let stale = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF,
        &old_heads,
        &address(20),
    )
    .unwrap();
    let current = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 2,
        &heads,
        &address(20),
    )
    .unwrap();
    let anchor = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap();
    let key = burst_crypto::keypair_from_seed(&[0; 32]);
    let mut generator = VoteGenerator::new(address(0), key.private.0);
    let original = generate_vote(&store, &mut generator, tail, true).unwrap();
    assert_eq!(original.sequence, 1);
    let clock = Some(SigningClock {
        now: CUTOFF + 5,
        error_bound: 0,
    });
    assert!(checkpoint_agreement::apply(
        &store,
        &anchor,
        &key.private,
        Command::Propose(stale.checkpoint().clone()),
        &[&stale],
        clock
    )
    .is_err());
    assert!(
        checkpoint_agreement::read_state(&store, &anchor, &address(0))
            .unwrap()
            .is_none()
    );
    let retransmitted = generate_vote(&store, &mut generator, tail, true).unwrap();
    assert_eq!(retransmitted.to_bytes(), original.to_bytes());
    assert_eq!(generator.sequence(), original.sequence);
    let proposal = checkpoint_agreement::apply(
        &store,
        &anchor,
        &key.private,
        Command::Propose(current.checkpoint().clone()),
        &[&current],
        clock,
    )
    .unwrap();
    assert!(matches!(&proposal[..], [Outgoing::Proposal(_)]));
    for final_vote in [false, true] {
        assert!(generate_vote(&store, &mut generator, tail, final_vote).is_err());
    }
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let recovered =
        checkpoint_agreement::read_candidates(&store, &anchor, &address(0), &fixture.params)
            .unwrap();
    let inputs: Vec<_> = recovered.iter().map(ReplayedCandidate::input).collect();
    assert_eq!(
        checkpoint_agreement::apply_candidates(
            &store,
            &anchor,
            &key.private,
            Command::Resume,
            &inputs,
            None
        )
        .unwrap(),
        proposal
    );
    // Storage boundary only: this fixture does not install a live economy.
    let next = current.checkpoint().hash();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(batch
        .advance_payment_epoch(
            &anchor.hash(),
            &anchor.hash(),
            &next,
            current.history_hashes().unwrap()
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
        None
    )
    .is_err());
    assert!(matches!(
        store
            .meta_store()
            .get_meta(&format!("consensus_fence/v2/{next}/{}", address(0))),
        Err(burst_store::StoreError::NotFound(_))
    ));
}

#[test]
fn runtime_waits_for_selection_epoch_then_rechecks_it_without_a_new_certificate() {
    use crate::checkpoint_service::{Clock, Runtime, Settings};
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    let mut fixture = Fixture::selection();
    for seed in [3, 4] {
        fixture.push(seed, BlockType::Endorse, CUTOFF - 1, |block| {
            block.link = destination(21);
            block.brn_spent += 1;
        });
    }
    let (_dir, store, heads) = fixture.store();
    let prepared = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF,
        &heads,
        &address(20),
    )
    .unwrap();
    let record = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certify(prepared.checkpoint(), &[0]),
        heads,
        address(20),
    )
    .unwrap();
    let anchor = record
        .parent()
        .advance(record.certificate(), record.replayed().roster().clone())
        .unwrap();
    let seed = (0..=8)
        .find(|seed| address(*seed) == *anchor.proposer(0))
        .unwrap();
    let store = Arc::new(store);
    let mut runtime = Runtime::new(Settings {
        store: store.clone(),
        network: NetworkId::Dev,
        initial_params: fixture.params.clone(),
        key: Some(Arc::new(
            burst_crypto::keypair_from_seed(&[seed; 32]).private,
        )),
    })
    .unwrap();
    let clock: Clock = Arc::new(|| {
        Some(SigningClock {
            now: CUTOFF + 35,
            error_bound: 0,
        })
    });
    let now = Instant::now();
    runtime.poll(&[], now, &clock).unwrap();
    assert!(
        checkpoint_agreement::read_state(&store, &anchor, &address(seed))
            .unwrap()
            .is_none()
    );
    // Only the storage precondition is supplied here. Full live installation
    // also has to publish the replayed economy and electorate.
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .advance_payment_epoch(
            &record.parent().hash(),
            &record.parent().hash(),
            &anchor.hash(),
            record.replayed().history_hashes().unwrap(),
        )
        .unwrap();
    batch.commit().unwrap();
    runtime
        .poll(&[], now + Duration::from_secs(2), &clock)
        .unwrap();
    assert!(
        checkpoint_agreement::read_state(&store, &anchor, &address(seed))
            .unwrap()
            .is_some()
    );
}

#[test]
fn minority_payment_voter_can_learn_and_recover_the_certified_selection() {
    let mut fixture = Fixture::selection();
    for seed in [3, 4] {
        fixture.push(seed, BlockType::Endorse, CUTOFF - 1, |block| {
            block.link = destination(21);
            block.brn_spent += 1;
        });
    }
    let first = certificate(&fixture, true);
    let old_heads = first.heads().to_vec();
    let epoch = first.certificate().checkpoint.hash();
    let (dir, store, _) = fixture.store();
    let tail = fixture.push(0, BlockType::Burn, CUTOFF + 1, |block| {
        block.payment_epoch = epoch;
        block.link = destination(22);
        block.brn_spent += 1;
    });
    CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        first.certificate().clone(),
        old_heads.clone(),
        address(20),
    )
    .unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .advance_payment_epoch(
            &first.parent().hash(),
            &first.parent().hash(),
            &epoch,
            first.replayed().history_hashes().unwrap(),
        )
        .unwrap();
    let mut account = batch.get_account(&address(0)).unwrap();
    account.block_count += 1;
    account.head = tail;
    // The excluded tail has a local final vote, not a cemented quorum.
    batch
        .put_block_with_account(
            &tail,
            &bincode::serialize(fixture.blocks.last().unwrap()).unwrap(),
            &account.address,
            account.block_count,
        )
        .unwrap();
    batch.put_account_info(&account, false).unwrap();
    batch.commit().unwrap();
    let next = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 2,
        &old_heads,
        &address(21),
    )
    .unwrap();
    let anchor = first
        .parent()
        .advance(first.certificate(), first.replayed().roster().clone())
        .unwrap();
    let key = burst_crypto::keypair_from_seed(&[8; 32]);
    let mut generator = VoteGenerator::new(address(8), key.private.0);
    generate_vote(&store, &mut generator, tail, true).unwrap();
    let decision = certify(next.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]);
    let output = checkpoint_agreement::apply(
        &store,
        &anchor,
        &key.private,
        Command::Decide(decision.clone()),
        &[&next],
        None,
    )
    .unwrap();
    assert_eq!(output, vec![Outgoing::Decision(decision.clone())]);
    assert!(generate_vote(&store, &mut generator, tail, true).is_err());
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let recovered =
        checkpoint_agreement::read_candidates(&store, &anchor, &address(8), &fixture.params)
            .unwrap();
    let inputs: Vec<_> = recovered.iter().map(ReplayedCandidate::input).collect();
    assert!(generate_vote(&store, &mut generator, tail, false).is_err());
    assert_eq!(
        checkpoint_agreement::apply_candidates(
            &store,
            &anchor,
            &key.private,
            Command::Resume,
            &inputs,
            None
        )
        .unwrap(),
        output
    );
    let mut batch = store.tx_begin_write().unwrap();
    assert_eq!(
        batch
            .advance_payment_epoch(
                &epoch,
                &anchor.hash(),
                &decision.checkpoint.hash(),
                next.history_hashes().unwrap()
            )
            .unwrap(),
        vec![tail]
    );
    batch.commit().unwrap();
    // A decided journal only relays the existing certificate after handoff.
    assert_eq!(
        checkpoint_agreement::apply_candidates(
            &store,
            &anchor,
            &key.private,
            Command::Resume,
            &inputs,
            None
        )
        .unwrap(),
        output
    );
    assert!(generate_vote(&store, &mut generator, tail, true).is_err());
}
