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
    assert_minority_decision_observation(false);
    assert_minority_decision_observation(true);
}

fn assert_minority_decision_observation(accepted_ahead: bool) {
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
    if accepted_ahead {
        local
            .meta_store()
            .put_meta(
                &format!("checkpoint_chain/v1/dev/{}/head", anchor.genesis_hash()),
                &anchor.height().to_be_bytes(),
            )
            .unwrap();
        checkpoint_agreement::apply_compact_selections(
            &local,
            anchor,
            &key.private,
            Command::Resume,
            &[&candidate],
            None,
        )
        .unwrap();
        crate::checkpoint_store::compact::accept_selection(
            &local,
            anchor,
            &candidate,
            &certificate,
        )
        .unwrap();
        assert_eq!(local.meta_store().payment_epoch().unwrap(), anchor.hash());
        assert!(
            checkpoint_agreement::read_state(&local, anchor, &address(0))
                .unwrap()
                .unwrap()
                .decision()
                .is_none()
        );
        let metadata = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let error = checkpoint_agreement::apply_compact_selections(
            &local,
            anchor,
            &key.private,
            Command::Resume,
            &[&candidate],
            None,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("compact signing parent is not the accepted tip"));
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            metadata,
        );
    }
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
    if accepted_ahead {
        assert_eq!(
            checkpoint_agreement::apply_compact_selections(
                &local,
                anchor,
                &key.private,
                Command::Resume,
                &[&candidate],
                None,
            )
            .unwrap(),
            outgoing,
        );
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"consensus_final/")
                .unwrap(),
            before,
        );
    }
}

#[test]
fn compact_split_final_promises_recover_with_replayed_extension_in_later_round() {
    use crate::checkpoint_agreement::CompactReplayCandidate;
    use crate::checkpoint_driver::{CheckpointDriver, Message};
    use burst_consensus::checkpoint_round::Step;
    use std::{
        collections::{HashSet, VecDeque},
        sync::Arc,
    };

    let (mut fixture, _source_dir, source, original, _) = setup();
    let base =
        authenticate(&source, &fixture.params, original.checkpoint.parent).into_continuation();
    let initial_blocks = fixture.blocks.clone();
    fixture.push(1, BlockType::ChangeRepresentative, CUTOFF + 1, |block| {
        block.payment_epoch = base.anchor().hash();
    });
    let promised = fixture.blocks.last().unwrap().clone();
    let initial_proposer = (0..9)
        .find(|seed| address(*seed) == *base.anchor().proposer(0))
        .unwrap();
    let fenced_signers: HashSet<_> = std::iter::once(initial_proposer)
        .chain((0..9).filter(|seed| *seed != initial_proposer))
        .take(4)
        .collect();
    let mut directories = Vec::new();
    let mut stores = Vec::new();
    let mut drivers = Vec::new();
    let coverage = |store: &LmdbStore, extended: bool| {
        let snapshot = authenticate(&source, &fixture.params, original.checkpoint.parent);
        let proofs = compact::retained(&snapshot, &fixture, &source, &[]);
        let mut heads = original.heads.clone();
        if extended {
            let head = heads
                .iter_mut()
                .find(|head| head.account == promised.account)
                .unwrap();
            assert_eq!(head.hash, promised.previous);
            head.height += 1;
            head.hash = promised.hash;
        }
        let tail = base
            .read_tail(store, CUTOFF + u64::from(extended), &heads, proofs)
            .unwrap();
        base.prepare_next_selection(tail, &[]).unwrap().unwrap()
    };
    for seed in 0..9 {
        let (dir, store) = cemented::sparse(&base, &source);
        let mut heights = HashMap::new();
        for block in &initial_blocks {
            let height = heights.entry(block.account.clone()).or_insert(0u64);
            *height += 1;
            if *height
                > base
                    .state()
                    .account(&block.account)
                    .map_or(0, |account| account.block_count)
            {
                cemented::append(&store, block, true);
            }
        }
        store
            .meta_store()
            .put_meta(
                &format!(
                    "checkpoint_chain/v1/dev/{}/head",
                    base.anchor().genesis_hash()
                ),
                &base.anchor().height().to_be_bytes(),
            )
            .unwrap();
        cemented::append(&store, &promised, false);
        if !fenced_signers.contains(&seed) {
            store
                .meta_store()
                .reserve_consensus_vote_at(
                    &base.anchor().hash(),
                    &address(seed),
                    &promised.account,
                    &promised.previous,
                    &promised.hash,
                    true,
                    0,
                    Some(promised.timestamp.as_secs()),
                )
                .unwrap();
        }
        let candidate = coverage(&store, false);
        let store = Arc::new(store);
        let mut driver = CheckpointDriver::new_compact_selection(
            store.clone(),
            fixture.params.clone(),
            Arc::new(burst_crypto::keypair_from_seed(&[seed; 32]).private),
            base.anchor().clone(),
            candidate,
            Vec::new(),
        )
        .unwrap();
        driver.resume().unwrap();
        directories.push(dir);
        stores.push(store);
        drivers.push(driver);
    }
    let clock = Some(SigningClock {
        now: CUTOFF + 2,
        error_bound: 0,
    });
    let proposal = drivers[initial_proposer as usize]
        .propose(clock)
        .unwrap()
        .into_iter()
        .find(|message| matches!(message, Message::Proposal { .. }))
        .unwrap();
    for (seed, driver) in drivers.iter_mut().enumerate() {
        let result = driver.receive(proposal.clone(), clock);
        if fenced_signers.contains(&(seed as u8)) {
            result.unwrap();
        } else {
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("omits a durable final vote"));
        }
        assert!(driver.state().unwrap().unwrap().decision().is_none());
    }
    let promises_before: Vec<_> = stores
        .iter()
        .map(|store| {
            store
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"consensus_final/")
                .unwrap()
        })
        .collect();
    // Finish the failed round through actual signed timeout participation.
    // Inventing future nil catch-up votes would equivocate when those same
    // signers later vote for the extended cut in that round.
    let mut votes = Vec::new();
    for driver in &mut drivers {
        votes.extend(
            driver
                .resume()
                .unwrap()
                .into_iter()
                .filter(|message| matches!(message, Message::Vote(_))),
        );
        votes.extend(driver.timeout(Step::Propose).unwrap());
    }
    for vote in votes {
        for driver in &mut drivers {
            driver.receive(vote.clone(), None).unwrap();
        }
    }
    let mut votes = Vec::new();
    for driver in &mut drivers {
        votes.extend(driver.timeout(Step::Prevote).unwrap());
    }
    for vote in votes {
        for driver in &mut drivers {
            driver.receive(vote.clone(), None).unwrap();
        }
    }
    for (index, driver) in drivers.iter_mut().enumerate() {
        driver.timeout(Step::Precommit).unwrap();
        assert_eq!(driver.round(), 1);
        assert!(driver
            .replace_unsigned_compact_candidate(CompactReplayCandidate::Selection(Box::new(
                coverage(&stores[index], true)
            )),)
            .unwrap());
    }
    let proposer = (0..9)
        .find(|seed| address(*seed) == *base.anchor().proposer(1))
        .unwrap();
    let mut queue: VecDeque<_> = drivers[proposer as usize].propose(clock).unwrap().into();
    let mut seen = HashSet::new();
    while let Some(message) = queue.pop_front() {
        if !seen.insert(bincode::serialize(&message).unwrap()) {
            continue;
        }
        assert!(seen.len() < 200, "extended cut did not converge");
        for driver in &mut drivers {
            queue.extend(driver.receive(message.clone(), clock).unwrap());
        }
    }
    for (index, driver) in drivers.iter().enumerate() {
        let state = driver.state().unwrap().unwrap();
        let decision = state
            .decision()
            .expect("replayed promised extension must decide");
        base.anchor().verify_certificate(decision).unwrap();
        assert_eq!(decision.checkpoint.cutoff, CUTOFF + 1);
        assert_eq!(
            stores[index]
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"consensus_final/")
                .unwrap(),
            promises_before[index]
        );
        let account = stores[index]
            .account_store()
            .get_account(&promised.account)
            .unwrap();
        assert_eq!(
            account.confirmation_height + 1,
            account.block_count,
            "a checkpoint decision alone must not publish payment confirmation"
        );
    }
}
