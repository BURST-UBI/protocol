use super::*;
use crate::checkpoint_replay::SigningClock;

#[test]
fn compact_drivers_agree_and_resume_decisions_without_full_prefixes() {
    use crate::checkpoint_driver::{CheckpointDriver, Message};
    use std::{
        collections::{HashSet, VecDeque},
        sync::Arc,
    };
    let (fixture, _source_dir, source, manifest, _) = setup();
    let snapshot = authenticate(&source, &fixture.params, manifest.checkpoint.parent);
    let base = snapshot.into_continuation();
    let coverage = || {
        let snapshot = authenticate(&source, &fixture.params, manifest.checkpoint.parent);
        let retained = compact::retained(&snapshot, &fixture, &source, &[]);
        let (_dir, suffix) = suffix_store(&[]);
        let tail = base
            .read_tail(&suffix, CUTOFF, &manifest.heads, retained)
            .unwrap();
        base.selection_vote_coverage(tail, &manifest, &[]).unwrap()
    };
    let mut directories = Vec::new();
    let mut drivers = Vec::new();
    for seed in 0..9 {
        let (directory, local) = cemented::sparse(&base, &source);
        local
            .meta_store()
            .put_meta(
                &format!(
                    "checkpoint_chain/v1/dev/{}/head",
                    base.anchor().genesis_hash()
                ),
                &base.anchor().height().to_be_bytes(),
            )
            .unwrap();
        let mut driver = CheckpointDriver::new_compact_selection(
            Arc::new(local),
            fixture.params.clone(),
            Arc::new(burst_crypto::keypair_from_seed(&[seed; 32]).private),
            base.anchor().clone(),
            coverage(),
            Vec::new(),
        )
        .unwrap();
        assert!(driver.resume().unwrap().is_empty());
        drivers.push(driver);
        directories.push(directory);
    }
    let proposer = (0..9)
        .find(|seed| address(*seed) == *base.anchor().proposer(0))
        .unwrap();
    let clock = Some(SigningClock {
        now: CUTOFF,
        error_bound: 0,
    });
    let mut queue: VecDeque<_> = drivers[proposer as usize].propose(clock).unwrap().into();
    let mut seen = HashSet::new();
    let mut decision = None;
    while let Some(message) = queue.pop_front() {
        if !seen.insert(bincode::serialize(&message).unwrap()) {
            continue;
        }
        assert!(seen.len() < 200, "agreement did not converge");
        if let Message::Decision { certificate, .. } = &message {
            base.anchor().verify_certificate(certificate).unwrap();
            decision = Some(certificate.clone());
        }
        for driver in &mut drivers {
            queue.extend(driver.receive(message.clone(), clock).unwrap());
        }
    }
    let decision = decision.expect("human quorum must decide");
    base.anchor().verify_certificate(&decision).unwrap();
    let mut saved_decisions = Vec::new();
    for driver in &drivers {
        let saved = driver.state().unwrap().unwrap().decision().unwrap().clone();
        assert_eq!(saved.checkpoint, decision.checkpoint);
        saved_decisions.push(saved);
        assert!(driver.persist_decision().unwrap());
        assert!(driver.persist_decision().unwrap());
    }
    drop(drivers);
    for (seed, directory) in directories.iter().enumerate() {
        let local = Arc::new(LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
        assert_eq!(
            local.meta_store().payment_epoch().unwrap(),
            base.anchor().hash()
        );
        assert_eq!(
            CertifiedCheckpoint::stored_tip(&local, NetworkId::Dev, &fixture.params).unwrap(),
            manifest.checkpoint.height
        );
        let mut driver = CheckpointDriver::new_compact_selection(
            Arc::clone(&local),
            fixture.params.clone(),
            Arc::new(burst_crypto::keypair_from_seed(&[seed as u8; 32]).private),
            base.anchor().clone(),
            coverage(),
            Vec::new(),
        )
        .unwrap();
        let resumed = driver.resume().unwrap();
        assert!(resumed.iter().any(|message| matches!(message,
            Message::Decision { certificate, .. } if certificate == &saved_decisions[seed])));
        assert!(driver.persist_decision().unwrap());
        let snapshot = authenticate(&source, &fixture.params, manifest.checkpoint.parent);
        let recovered = snapshot
            .recover_stored_successors(&local, |base, plan, _| {
                if plan.witness_hashes().is_empty() {
                    return Ok(Vec::new());
                }
                witness_transfer::exchange(
                    crate::checkpoint_sync::witness::Roots::for_plan(base, plan)?,
                    plan.witness_hashes().to_vec(),
                    &source,
                    &local,
                )
            })
            .unwrap();
        assert_eq!(
            recovered.continuation.anchor().hash(),
            manifest.checkpoint.hash()
        );
    }
}

#[test]
fn compact_signing_requires_every_authenticated_selection_receipt() {
    let (fixture, _source_dir, source, manifest, certificate) = setup();
    let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
    let witnesses = compact::retained(&snapshot, &fixture, &source, &[]);
    let base = snapshot.into_continuation();
    let (_suffix_dir, suffix) = suffix_store(&[]);
    let tail = base
        .read_tail(&suffix, CUTOFF + 1, &manifest.heads, witnesses)
        .unwrap();
    let error = base
        .selection_vote_coverage(tail, &manifest, &[])
        .err()
        .expect("missing receipt must fail");
    assert!(error
        .to_string()
        .contains("every authenticated selection receipt"));
}

#[test]
fn compact_signing_commits_one_fence_and_journal_and_resumes_after_reopen() {
    let (fixture, _source_dir, source, manifest, _) = setup();
    let snapshot = authenticate(&source, &fixture.params, manifest.checkpoint.parent);
    let witnesses = compact::retained(&snapshot, &fixture, &source, &[]);
    let base = snapshot.into_continuation();
    let (_suffix_dir, suffix) = suffix_store(&[]);
    let tail = base
        .read_tail(&suffix, CUTOFF, &manifest.heads, witnesses)
        .unwrap();
    let candidate = base.selection_vote_coverage(tail, &manifest, &[]).unwrap();
    let anchor = base.anchor();
    let seed = (0..9)
        .find(|seed| address(*seed) == *anchor.proposer(0))
        .unwrap();
    let key = burst_crypto::keypair_from_seed(&[seed; 32]);
    let representative = address(seed);
    let tip_key = format!("checkpoint_chain/v1/dev/{}/head", anchor.genesis_hash());
    for case in 0..7 {
        let (directory, local) = cemented::sparse(&base, &source);
        local
            .meta_store()
            .put_meta(&tip_key, &anchor.height().to_be_bytes())
            .unwrap();
        let mut clock = Some(SigningClock {
            now: CUTOFF,
            error_bound: 0,
        });
        match case {
            0 => (),
            1 => {
                local
                    .meta_store()
                    .reserve_consensus_vote_at(
                        &anchor.hash(),
                        &representative,
                        &address(99),
                        &BlockHash::ZERO,
                        &BlockHash::new([99; 32]),
                        true,
                        0,
                        Some(CUTOFF),
                    )
                    .unwrap();
            }
            2 => local
                .meta_store()
                .enqueue_confirmation(&BlockHash::new([99; 32]))
                .unwrap(),
            3 => clock = None,
            4 => {
                clock = Some(SigningClock {
                    now: candidate.replay().request().beacon_time().unwrap(),
                    error_bound: 0,
                })
            }
            5 => local
                .meta_store()
                .put_meta(&tip_key, &(anchor.height() + 1).to_be_bytes())
                .unwrap(),
            _ => local
                .meta_store()
                .put_meta(
                    crate::confirmed_replay::installation::INSTALLED_KEY,
                    b"changed",
                )
                .unwrap(),
        }
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        assert!(
            checkpoint_agreement::read_retained_manifests(&local, anchor, &representative,)
                .unwrap()
                .is_empty()
        );
        let result = checkpoint_agreement::apply_compact_selections(
            &local,
            anchor,
            &key.private,
            Command::Propose(manifest.checkpoint.clone()),
            &[&candidate],
            clock,
        );
        if case != 0 {
            assert!(result.is_err(), "case {case}");
            assert_eq!(
                local
                    .tx_begin_write()
                    .unwrap()
                    .checkpoint_metadata(b"")
                    .unwrap(),
                before,
                "case {case}"
            );
            continue;
        }
        let outgoing = result.unwrap();
        assert_eq!(outgoing.len(), 1);
        let checkpoint_agreement::Outgoing::Proposal(proposal) = &outgoing[0] else {
            panic!("missing signed proposal")
        };
        proposal.verify(anchor).unwrap();
        assert_eq!(proposal.body.checkpoint, manifest.checkpoint);
        let fence = format!("consensus_fence/v2/{}/{representative}", anchor.hash());
        assert_eq!(
            local.meta_store().get_meta(&fence).unwrap(),
            anchor.hash().as_bytes()
        );
        assert!(local
            .meta_store()
            .reserve_consensus_vote_at(
                &anchor.hash(),
                &representative,
                &address(99),
                &BlockHash::ZERO,
                &BlockHash::new([99; 32]),
                true,
                0,
                Some(CUTOFF),
            )
            .is_err());
        drop(local);
        let reopened = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
        // Recover inputs from the durable journal, not the caller's old
        // in-memory candidate. Rebuild coverage from authenticated evidence.
        assert!(checkpoint_agreement::read_candidates(
            &reopened,
            anchor,
            &representative,
            &fixture.params,
        )
        .is_err());
        let before_recovery = reopened
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let retained =
            checkpoint_agreement::read_retained_manifests(&reopened, anchor, &representative)
                .unwrap();
        let [checkpoint_agreement::ReplayManifest::Selection(saved)] = retained.as_slice() else {
            panic!("missing retained selection manifest")
        };
        assert_eq!(saved, &manifest);
        let snapshot = authenticate(&source, &fixture.params, saved.checkpoint.parent);
        let witnesses = compact::retained(&snapshot, &fixture, &source, &[]);
        let recovered_base = snapshot.into_continuation();
        let recovered_tail = recovered_base
            .read_tail(&suffix, CUTOFF, &saved.heads, witnesses)
            .unwrap();
        let candidate = recovered_base
            .selection_vote_coverage(recovered_tail, saved, &[])
            .unwrap();
        assert_eq!(
            reopened
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before_recovery,
        );
        let resumed = checkpoint_agreement::apply_compact_selections(
            &reopened,
            anchor,
            &key.private,
            Command::Resume,
            &[&candidate],
            None,
        )
        .unwrap();
        assert_eq!(resumed, outgoing);
        assert_eq!(
            reopened.meta_store().get_meta(&fence).unwrap(),
            anchor.hash().as_bytes()
        );
        let prevoted = checkpoint_agreement::apply_compact_selections(
            &reopened,
            anchor,
            &key.private,
            Command::Proposal(proposal),
            &[&candidate],
            Some(SigningClock {
                now: CUTOFF,
                error_bound: 0,
            }),
        )
        .unwrap();
        assert!(
            matches!(prevoted.as_slice(), [checkpoint_agreement::Outgoing::Vote(vote)]
            if vote.ballot.phase == burst_consensus::checkpoint_round::Phase::Prevote)
        );
        drop(reopened);
        let reopened = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
        // Late prepare evidence binds the original value and remains usable
        // after reopening, even when fresh selection signatures are too late.
        use burst_consensus::checkpoint_round::{Ballot, Phase, Quorum};
        let ballot = Ballot {
            network: anchor.network(),
            genesis: anchor.genesis_hash(),
            parent: anchor.hash(),
            height: manifest.checkpoint.height,
            round: 0,
            phase: Phase::Prevote,
            value: Some(manifest.checkpoint.hash()),
        };
        let proof = Quorum {
            signatures: (0..7)
                .map(|seed| {
                    let key = burst_crypto::keypair_from_seed(&[seed; 32]);
                    burst_consensus::checkpoint::CheckpointSignature {
                        representative: address(seed),
                        signature: burst_crypto::sign_message(
                            &ballot.signing_payload(),
                            &key.private,
                        ),
                    }
                })
                .collect(),
            ballot,
        };
        let prepared = checkpoint_agreement::apply_compact_selections(
            &reopened,
            anchor,
            &key.private,
            Command::Prepared(proposal, &proof),
            &[&candidate],
            Some(SigningClock {
                now: candidate.replay().request().beacon_time().unwrap(),
                error_bound: 0,
            }),
        )
        .unwrap();
        let [checkpoint_agreement::Outgoing::Vote(vote)] = prepared.as_slice() else {
            panic!("missing precommit")
        };
        vote.verify(anchor).unwrap();
        assert_eq!(vote.ballot.phase, Phase::Precommit);
        assert_eq!(vote.ballot.value, Some(manifest.checkpoint.hash()));
        drop(reopened);
        let reopened = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
        let resumed = checkpoint_agreement::apply_compact_selections(
            &reopened,
            anchor,
            &key.private,
            Command::Resume,
            &[&candidate],
            None,
        )
        .unwrap();
        assert!(resumed.contains(&prepared[0]));

        let journal_key = format!(
            "checkpoint_round/v1/dev/{}/{representative}/{}",
            anchor.genesis_hash(),
            anchor.height() + 1,
        );
        let mut damaged = reopened.meta_store().get_meta(&journal_key).unwrap();
        *damaged.last_mut().unwrap() ^= 1;
        reopened
            .meta_store()
            .put_meta(&journal_key, &damaged)
            .unwrap();
        let before = reopened
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        assert!(
            checkpoint_agreement::read_retained_manifests(&reopened, anchor, &representative,)
                .is_err()
        );
        // Already-replayed coverage cannot repair or bypass a changed journal.
        assert!(checkpoint_agreement::apply_compact_selections(
            &reopened,
            anchor,
            &key.private,
            Command::Resume,
            &[&candidate],
            None,
        )
        .is_err());
        assert_eq!(
            reopened
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before,
        );
    }
}

#[test]
fn compact_decision_observation_preserves_minority_promises_without_new_signatures() {
    let (fixture, _source_dir, source, manifest, certificate) = setup();
    let snapshot = authenticate(&source, &fixture.params, manifest.checkpoint.parent);
    let witnesses = compact::retained(&snapshot, &fixture, &source, &[]);
    let base = snapshot.into_continuation();
    let (_suffix_dir, suffix) = suffix_store(&[]);
    let tail = base
        .read_tail(&suffix, CUTOFF, &manifest.heads, witnesses)
        .unwrap();
    let candidate = base.selection_vote_coverage(tail, &manifest, &[]).unwrap();
    let (_local_dir, local) = cemented::sparse(&base, &source);
    let anchor = base.anchor();
    let key = burst_crypto::keypair_from_seed(&[0; 32]);
    local
        .meta_store()
        .reserve_consensus_vote_at(
            &anchor.hash(),
            &address(0),
            &address(99),
            &BlockHash::ZERO,
            &BlockHash::new([99; 32]),
            true,
            0,
            Some(CUTOFF),
        )
        .unwrap();
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"consensus_final/")
        .unwrap();
    let outgoing = checkpoint_agreement::apply_compact_selections(
        &local,
        anchor,
        &key.private,
        Command::Decide(certificate.clone()),
        &[&candidate],
        None,
    )
    .unwrap();
    assert_eq!(
        outgoing,
        vec![checkpoint_agreement::Outgoing::Decision(
            certificate.clone()
        )]
    );
    assert_eq!(
        checkpoint_agreement::read_state(&local, anchor, &address(0))
            .unwrap()
            .unwrap()
            .decision(),
        Some(&certificate)
    );
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"consensus_final/")
            .unwrap(),
        before
    );
    assert_eq!(local.meta_store().payment_epoch().unwrap(), anchor.hash());
}
