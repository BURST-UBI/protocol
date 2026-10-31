use super::*;
use crate::{
    checkpoint_driver::{CheckpointDriver, Message},
    checkpoint_replay::SigningClock,
};
use burst_consensus::checkpoint_round::{Ballot, Phase, Quorum, SignedBallot};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_worker_refreshes_an_expired_unsigned_candidate_on_its_timer() {
    use crate::checkpoint_service::{self, Session, Settings};
    use crate::confirmed_replay::tests::snapshot_history::installed_recovery::WitnessService;
    use std::time::Duration;

    // Exercise expiry, new confirmation, and a final promise for an unconfirmed
    // extension. Each needs fresh coverage before the worker may sign.
    for case in 0..3 {
        let stale_finality = case != 0;
        let (mut fixture, _source_dir, source, original, _) = setup();
        let base =
            authenticate(&source, &fixture.params, original.checkpoint.parent).into_continuation();
        let (_dir, local) = cemented::sparse(&base, &source);
        let mut heights = HashMap::new();
        for block in &fixture.blocks {
            let height = heights.entry(block.account.clone()).or_insert(0u64);
            *height += 1;
            if *height
                > base
                    .state()
                    .account(&block.account)
                    .map_or(0, |account| account.block_count)
            {
                cemented::append(&local, block, true);
            }
        }
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
        let snapshot = authenticate(&source, &fixture.params, original.checkpoint.parent);
        let proofs = compact::retained(&snapshot, &fixture, &source, &[]);
        let tail = base
            .read_tail(&local, CUTOFF, &original.heads, proofs)
            .unwrap();
        let coverage = base.prepare_next_selection(tail, &[]).unwrap().unwrap();
        let seed = (0..9)
            .find(|seed| {
                address(*seed) == *base.anchor().proposer(if stale_finality { 0 } else { 1 })
            })
            .unwrap();
        let key = Arc::new(burst_crypto::keypair_from_seed(&[seed; 32]).private);
        let local = Arc::new(local);
        let mut driver = CheckpointDriver::new_compact_selection(
            local.clone(),
            fixture.params.clone(),
            key.clone(),
            base.anchor().clone(),
            coverage,
            vec![],
        )
        .unwrap();
        driver.resume().unwrap();
        if !stale_finality {
            let mut ballot = Ballot::precommit(&original.checkpoint, 1);
            ballot.phase = Phase::Prevote;
            ballot.value = None;
            driver
                .receive(
                    Message::CatchUp {
                        round: 1,
                        evidence: (0..3)
                            .map(|seed| SignedBallot {
                                signer: burst_consensus::checkpoint::CheckpointSignature {
                                    representative: address(seed),
                                    signature: burst_crypto::sign_message(
                                        &ballot.signing_payload(),
                                        &burst_crypto::keypair_from_seed(&[seed; 32]).private,
                                    ),
                                },
                                ballot: ballot.clone(),
                            })
                            .collect(),
                    },
                    None,
                )
                .unwrap();
        }
        drop(driver);
        let recovered = snapshot
            .recover_stored_successors(&local, |_, _, _| unreachable!())
            .unwrap();
        let promised_replay = if case == 2 {
            Some(Arc::new(recovered.fork(local.env().path()).unwrap()))
        } else {
            None
        };
        let service = WitnessService::start(Arc::new(source), local.clone(), 0).await;
        let session = Session::from_compact_candidate(
            Settings {
                store: local.clone(),
                network: NetworkId::Dev,
                initial_params: fixture.params.clone(),
                key: Some(key),
            },
            service.registry.clone(),
            recovered,
            Some(ReplayManifest::Selection(original.clone())),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
        let advanced = if stale_finality {
            fixture.push(1, BlockType::ChangeRepresentative, CUTOFF + 2, |block| {
                block.payment_epoch = base.anchor().hash();
            });
            let block = fixture.blocks.last().unwrap();
            cemented::append(&local, block, case != 2);
            if case == 2 {
                local
                    .meta_store()
                    .reserve_consensus_vote_at(
                        &base.anchor().hash(),
                        &address(seed),
                        &block.account,
                        &block.previous,
                        &block.hash,
                        true,
                        0,
                        Some(block.timestamp.as_secs()),
                    )
                    .unwrap();
            }
            Some(block.hash)
        } else {
            None
        };
        let promises = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"consensus_final/")
            .unwrap();
        let (_sender, receiver) = tokio::sync::mpsc::channel(checkpoint_service::INBOX_CAPACITY);
        let stop = Arc::new(crate::shutdown::ShutdownController::new());
        let worker = tokio::spawn(checkpoint_service::run(
            session,
            service.registry.clone(),
            receiver,
            Arc::new(move || {
                Some(SigningClock {
                    now: CUTOFF + if stale_finality { 4 } else { 32 },
                    error_bound: 0,
                })
            }),
            stop.clone(),
        ));
        let result = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let state = checkpoint_agreement::read_state(&local, base.anchor(), &address(seed))
                    .unwrap()
                    .unwrap();
                if let Some(proposal) = state.proposal() {
                    break proposal.checkpoint.clone();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        stop.shutdown();
        worker.await.unwrap().unwrap();
        if let Some(recovered) = promised_replay {
            let mut changed = fixture.blocks.last().unwrap().clone();
            changed.signature.0[0] ^= 1;
            local
                .block_store()
                .put_block(&changed.hash, &bincode::serialize(&changed).unwrap())
                .unwrap();
            let result = recovered
                .discover_promised_candidate_from_peers(
                    local.clone(),
                    service.registry.clone(),
                    SigningClock {
                        now: CUTOFF + 4,
                        error_bound: 0,
                    },
                    address(seed),
                )
                .await;
            let Err(error) = result else {
                panic!("promised replay accepted an invalid signature");
            };
            assert!(
                error
                    .to_string()
                    .contains("invalid signed snapshot tail block"),
                "{error}"
            );
            let original = fixture.blocks.last().unwrap();
            local
                .block_store()
                .put_block(&original.hash, &bincode::serialize(original).unwrap())
                .unwrap();
        }
        service.stop().await;
        let checkpoint = result.expect("worker kept its expired candidate instead of refreshing");
        assert_eq!(
            checkpoint.cutoff,
            CUTOFF + if stale_finality { 4 } else { 31 }
        );
        if let Some(hash) = advanced {
            assert!(local.block_store().get_block(&hash).is_ok());
            let saved = checkpoint_agreement::read_state(&local, base.anchor(), &address(seed))
                .unwrap()
                .unwrap();
            assert!(saved.valid_proof().is_none());
            assert!(saved.decision().is_none());
            assert_eq!(
                local
                    .tx_begin_write()
                    .unwrap()
                    .checkpoint_metadata(b"consensus_final/")
                    .unwrap(),
                promises
            );
            if case == 2 {
                let block = fixture.blocks.last().unwrap();
                let account = local.account_store().get_account(&block.account).unwrap();
                assert_eq!(
                    account.confirmation_height + 1,
                    account.block_count,
                    "replaying a promised extension must not publish confirmation"
                );
            }
        }
        assert_ne!(checkpoint, original.checkpoint);
        assert_eq!(
            local.meta_store().payment_epoch().unwrap(),
            base.anchor().hash()
        );
        assert_eq!(
            CertifiedCheckpoint::stored_tip(&local, NetworkId::Dev, &fixture.params).unwrap(),
            base.anchor().height()
        );
    }
}

#[test]
fn compact_refresh_preserves_journal_and_late_prepare_priority_after_restart() {
    let (fixture, _source_dir, source, original, _) = setup();
    let base =
        authenticate(&source, &fixture.params, original.checkpoint.parent).into_continuation();
    let coverage = |cutoff| {
        let snapshot = authenticate(&source, &fixture.params, original.checkpoint.parent);
        let proofs = compact::retained(&snapshot, &fixture, &source, &[]);
        let (_dir, suffix) = suffix_store(&[]);
        let tail = base
            .read_tail(&suffix, cutoff, &original.heads, proofs)
            .unwrap();
        base.prepare_next_selection(tail, &[]).unwrap().unwrap()
    };
    let (dir, local) = cemented::sparse(&base, &source);
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
    let seed = (0..9)
        .find(|seed| address(*seed) == *base.anchor().proposer(1))
        .unwrap();
    let key = Arc::new(burst_crypto::keypair_from_seed(&[seed; 32]).private);
    let local = Arc::new(local);
    let make_driver = |store: Arc<LmdbStore>| {
        CheckpointDriver::new_compact_selection(
            store,
            fixture.params.clone(),
            key.clone(),
            base.anchor().clone(),
            coverage(CUTOFF),
            Vec::new(),
        )
        .unwrap()
    };
    let late = Some(SigningClock {
        now: CUTOFF + 32,
        error_bound: 0,
    });
    let sign = |seed: u8, ballot: Ballot| SignedBallot {
        signer: burst_consensus::checkpoint::CheckpointSignature {
            representative: address(seed),
            signature: burst_crypto::sign_message(
                &ballot.signing_payload(),
                &burst_crypto::keypair_from_seed(&[seed; 32]).private,
            ),
        },
        ballot,
    };
    let catch_up = |round| {
        let mut ballot = Ballot::precommit(&original.checkpoint, round);
        ballot.phase = Phase::Prevote;
        ballot.value = None;
        Message::CatchUp {
            round,
            evidence: (0..3).map(|seed| sign(seed, ballot.clone())).collect(),
        }
    };
    let mut driver = make_driver(local.clone());
    driver.resume().unwrap();
    assert!(!driver
        .replace_compact_selection(coverage(CUTOFF + 31), late)
        .unwrap());
    driver.receive(catch_up(1), None).unwrap();
    drop(driver);
    drop(local);
    let local = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
    let mut driver = make_driver(local.clone());
    assert_eq!(driver.round(), 1);
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    assert!(!driver
        .replace_compact_selection(coverage(CUTOFF + 31), None)
        .unwrap());
    assert!(!driver
        .replace_compact_selection(
            coverage(CUTOFF + 31),
            Some(SigningClock {
                now: CUTOFF + 32,
                error_bound: 40
            })
        )
        .unwrap());
    assert!(driver.propose(late).is_err());
    assert!(driver
        .replace_compact_selection(coverage(CUTOFF + 31), late)
        .unwrap());
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    let messages = driver.propose(late).unwrap();
    let (proposal, manifest) = messages
        .iter()
        .find_map(|message| match message {
            Message::Proposal { proposal, manifest } => Some((proposal.clone(), manifest.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(proposal.body.checkpoint.cutoff, CUTOFF + 31);
    assert_ne!(proposal.body.checkpoint, original.checkpoint);
    let before = driver.state().unwrap();
    assert!(driver.compact_candidate_refresh_signer().unwrap().is_none());
    assert!(!driver
        .replace_unsigned_compact_candidate(
            checkpoint_agreement::CompactReplayCandidate::Selection(Box::new(coverage(
                CUTOFF + 91
            ))),
        )
        .unwrap());
    assert!(!driver
        .replace_compact_selection(
            coverage(CUTOFF + 91),
            Some(SigningClock {
                now: CUTOFF + 92,
                error_bound: 0
            })
        )
        .unwrap());
    assert_eq!(driver.state().unwrap(), before);
    let mut ballot = Ballot::precommit(&proposal.body.checkpoint, 1);
    ballot.phase = Phase::Prevote;
    let proof = Quorum {
        signatures: (0..7)
            .map(|seed| sign(seed, ballot.clone()).signer)
            .collect(),
        ballot,
    };
    driver
        .receive(
            Message::Prepared {
                proposal,
                proof,
                manifest,
            },
            None,
        )
        .unwrap();
    driver.receive(catch_up(2), None).unwrap();
    assert!(driver.state().unwrap().unwrap().valid_proof().is_some());
    assert!(driver.compact_candidate_refresh_signer().unwrap().is_none());
    assert!(!driver
        .replace_unsigned_compact_candidate(
            checkpoint_agreement::CompactReplayCandidate::Selection(Box::new(coverage(
                CUTOFF + 91
            ))),
        )
        .unwrap());
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    assert!(!driver
        .replace_compact_selection(
            coverage(CUTOFF + 91),
            Some(SigningClock {
                now: CUTOFF + 92,
                error_bound: 0
            })
        )
        .unwrap());
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
}
