use super::*;
use crate::checkpoint_store::historical::HistoricalAdmissions;

fn records(store: &LmdbStore, params: &ProtocolParams) -> Vec<Vec<u8>> {
    (1..=CertifiedCheckpoint::stored_tip(store, NetworkId::Dev, params).unwrap())
        .map(|height| {
            CertifiedCheckpoint::export_record(store, NetworkId::Dev, params, height)
                .unwrap()
                .unwrap()
        })
        .collect()
}

fn import(store: &LmdbStore, params: &ProtocolParams, records: &[Vec<u8>]) {
    for (index, record) in records.iter().enumerate() {
        if record.first() == Some(&2) {
            CertifiedChallengeOutcome::import_record(
                store,
                NetworkId::Dev,
                params,
                index as u64 + 1,
                record,
            )
            .unwrap();
            continue;
        }
        CertifiedCheckpoint::import_record(store, NetworkId::Dev, params, index as u64 + 1, record)
            .unwrap();
    }
}

fn spend(fixture: &mut Fixture, epoch: BlockHash, origin: TxHash, amount: u128) {
    let send = fixture.push(8, BlockType::Send, CUTOFF + 72, |block| {
        block.payment_epoch = epoch;
        block.origin = origin;
        block.link = destination(2);
        block.trst_balance -= amount;
    });
    fixture.push(2, BlockType::Receive, CUTOFF + 73, |block| {
        block.payment_epoch = epoch;
        block.link = send;
        block.trst_balance += amount;
    });
}

#[test]
fn settled_epoch_spends_clean_remainder_and_inactivity_preserves_full_value() {
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        let (mut fixture, selections) = completed(reason);
        merged_payments(&mut fixture);
        let (_dir, store, heads) = restore_chain(&fixture, &selections);
        let outcome = persist_outcome(&fixture, &store, &heads);
        let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
        let epoch = outcome.certificate().checkpoint.hash();
        let token = outcome
            .state()
            .portfolio(&address(8))
            .unwrap()
            .tokens
            .iter()
            .find(|token| token.state == burst_types::TrstState::Active)
            .unwrap();
        let amount = if reason == ChallengeReason::Fraud {
            5_000
        } else {
            10_000
        };
        assert_eq!(token.amount, amount);
        let origin = token.origin;
        let prefix_len = fixture.blocks.len();
        spend(&mut fixture, epoch, origin, amount);
        let history = fixture.history(CUTOFF + 74);
        let state =
            ConfirmedReplay::replay_with_events(&history, NetworkId::Dev, &fixture.params, &events)
                .unwrap();
        assert_eq!(
            state.account(&address(8)).unwrap().trst_balance,
            10_001 - amount
        );
        assert_eq!(
            state
                .portfolio(&address(8))
                .unwrap()
                .tokens
                .iter()
                .filter(|token| token.state == burst_types::TrstState::Active)
                .map(|token| token.amount)
                .sum::<u128>(),
            0
        );
        assert!(state
            .portfolio(&address(2))
            .unwrap()
            .tokens
            .iter()
            .any(|token| token.origin == origin
                && token.amount == amount
                && token.state == burst_types::TrstState::Active));
        assert_eq!(state.human_roster().unwrap().weight(&address(20)), 0);

        let mut blocks = history.replay_blocks().to_vec();
        blocks.reverse();
        let reordered = ConfirmedHistory::from_blocks(
            NetworkId::Dev,
            history.genesis(),
            history.cutoff(),
            history.heads(),
            &[],
            blocks,
        )
        .unwrap();
        let repeat = ConfirmedReplay::replay_with_events(
            &reordered,
            NetworkId::Dev,
            &fixture.params,
            &events,
        )
        .unwrap();
        for seed in [2, 8, 20] {
            assert_eq!(
                bincode::serialize(&state.account(&address(seed))).unwrap(),
                bincode::serialize(&repeat.account(&address(seed))).unwrap()
            );
            assert_eq!(
                bincode::serialize(&state.portfolio(&address(seed))).unwrap(),
                bincode::serialize(&repeat.portfolio(&address(seed))).unwrap()
            );
        }

        if reason == ChallengeReason::Fraud {
            fixture.blocks.truncate(prefix_len);
            spend(&mut fixture, epoch, origin, 5_001);
            assert!(
                ConfirmedReplay::replay_with_events(
                    &fixture.history(CUTOFF + 74),
                    NetworkId::Dev,
                    &fixture.params,
                    &events
                )
                .is_err(),
                "a signed new-epoch payment must not spend the revoked share"
            );
        }
    }
}

#[test]
fn epoch_tail_rejects_resigned_stale_context_and_backdating() {
    let (mut fixture, selections) = completed(ChallengeReason::Fraud);
    let (_dir, store, heads) = restore_chain(&fixture, &selections);
    let outcome = persist_outcome(&fixture, &store, &heads);
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let epoch = outcome.certificate().checkpoint.hash();
    let prefix_len = fixture.blocks.len();
    for context in [
        fixture.blocks[0].hash,
        selections[1].certificate().checkpoint.hash(),
        BlockHash::new([98; 32]),
        BlockHash::ZERO,
    ] {
        fixture.blocks.truncate(prefix_len);
        fixture.push(1, BlockType::ChangeRepresentative, CUTOFF + 72, |block| {
            block.payment_epoch = context;
        });
        assert!(ConfirmedReplay::replay_with_events(
            &fixture.history(CUTOFF + 74),
            NetworkId::Dev,
            &fixture.params,
            &events
        )
        .is_err());
    }
    fixture.blocks.truncate(prefix_len);
    fixture.push(1, BlockType::ChangeRepresentative, CUTOFF + 69, |block| {
        block.payment_epoch = epoch;
    });
    let error = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 74),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .err()
    .unwrap();
    assert!(
        error.to_string().contains("predates the certified cut"),
        "{error}"
    );
    fixture.blocks.truncate(prefix_len);
    fixture.push(1, BlockType::ChangeRepresentative, CUTOFF + 72, |block| {
        block.payment_epoch = epoch;
    });
    ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 74),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    fixture.blocks.last_mut().unwrap().signature = Signature([0; 64]);
    assert!(ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 74),
        NetworkId::Dev,
        &fixture.params,
        &events
    )
    .is_err());
}

#[test]
fn epoch_tail_survives_next_certificate_restart_and_historical_transfer() {
    let (mut fixture, selections) = completed(ChallengeReason::Fraud);
    merged_payments(&mut fixture);
    pending_successor(&mut fixture);
    let (_dir, store, heads) = restore_chain(&fixture, &selections);
    let outcome = persist_outcome(&fixture, &store, &heads);
    let records = records(&store, &fixture.params);
    let epoch = outcome.certificate().checkpoint.hash();
    let origin = outcome
        .state()
        .portfolio(&address(8))
        .unwrap()
        .tokens
        .iter()
        .find(|token| token.state == burst_types::TrstState::Active)
        .unwrap()
        .origin;
    spend(&mut fixture, epoch, origin, 5_000);
    let (dir, source, heads) = fixture.store();
    import(&source, &fixture.params, &records);
    let next = CertifiedCheckpoint::prepare(
        &source,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 74,
        &heads,
        &address(21),
    )
    .unwrap();
    CertifiedCheckpoint::accept(
        &source,
        NetworkId::Dev,
        &fixture.params,
        certify(next.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]),
        heads,
        address(21),
    )
    .unwrap();
    let transfer =
        CertifiedCheckpoint::export_transfer(&source, NetworkId::Dev, &fixture.params, 4)
            .unwrap()
            .unwrap();
    drop(source);
    let source = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let events = chain::load(&source, NetworkId::Dev, &fixture.params).unwrap();
    assert_eq!(events.len(), 4);
    let expected = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 74),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();

    let (_dir, receiver, _) = fixture.store();
    let installed_before = receiver.meta_store().payment_epoch().unwrap();
    import(&receiver, &fixture.params, &records);
    let admissions = HistoricalAdmissions::default();
    CertifiedCheckpoint::import_transfer(
        &receiver,
        NetworkId::Dev,
        &fixture.params,
        4,
        &transfer,
        &admissions,
    )
    .unwrap();
    assert!(admissions.contains(&fixture.blocks.last().unwrap().hash));
    let events = chain::load(&receiver, NetworkId::Dev, &fixture.params).unwrap();
    let actual = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 74),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    for seed in [2, 8, 20] {
        assert_eq!(
            bincode::serialize(&expected.account(&address(seed))).unwrap(),
            bincode::serialize(&actual.account(&address(seed))).unwrap()
        );
        assert_eq!(
            bincode::serialize(&expected.portfolio(&address(seed))).unwrap(),
            bincode::serialize(&actual.portfolio(&address(seed))).unwrap()
        );
    }
    assert_eq!(
        receiver.meta_store().payment_epoch().unwrap(),
        installed_before,
        "history transfer must not activate the live epoch"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn outcome_rewards_and_stake_proofs_are_claimed_once_in_the_new_epoch() {
    let (mut fixture, selections) = completed(ChallengeReason::Fraud);
    merged_payments(&mut fixture);
    retirement_history(&mut fixture);
    let (_dir, store, heads) = restore_chain(&fixture, &selections);
    let outcome = persist_outcome(&fixture, &store, &heads);
    let epoch = outcome.certificate().checkpoint.hash();
    let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
    let wallet = outcome.state().brn.get_wallet(&address(8)).unwrap();
    let sources = wallet.release_sources(1024);
    assert!(!sources.is_empty());
    assert!(wallet.has_more_releases(&[]));
    assert!(!wallet.has_more_releases(&sources));
    let source = *wallet.pending_releases.keys().next().unwrap();
    let reward = outcome
        .state()
        .pending(&address(8), source.into_tx_hash())
        .unwrap()
        .amount;
    assert!(reward > 0);
    fixture.push(8, BlockType::Receive, CUTOFF + 72, |block| {
        block.payment_epoch = epoch;
        block.link = source;
        block.trst_balance += reward;
        block.release_sources = sources.clone();
    });
    let state = ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 74),
        NetworkId::Dev,
        &fixture.params,
        &events,
    )
    .unwrap();
    assert!(state.pending(&address(8), source.into_tx_hash()).is_none());
    assert!(state
        .brn
        .get_wallet(&address(8))
        .unwrap()
        .pending_releases
        .is_empty());
    assert!(!state.brn.get_wallet(&address(8)).unwrap().has_more_releases(&[]));
    let accepted = fixture.blocks.last().unwrap().clone();
    let prefix_len = fixture.blocks.len();
    fixture.push(8, BlockType::ChangeRepresentative, CUTOFF + 73, |block| {
        block.payment_epoch = epoch;
        block.release_sources = sources;
    });
    assert!(ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 74),
        NetworkId::Dev,
        &fixture.params,
        &events
    )
    .is_err());
    let repeated_release = fixture.blocks.last().unwrap().clone();
    fixture.blocks.truncate(prefix_len);
    fixture.push(8, BlockType::Receive, CUTOFF + 73, |block| {
        block.payment_epoch = epoch;
        block.link = source;
        block.trst_balance += reward;
    });
    assert!(ConfirmedReplay::replay_with_events(
        &fixture.history(CUTOFF + 74),
        NetworkId::Dev,
        &fixture.params,
        &events
    )
    .is_err());
    let repeated_reward = fixture.blocks.last().unwrap().clone();
    crate::confirmed_replay::tests::snapshot_history::installed_recovery::body_retirement::assert_pruned_claims(
        &fixture, std::sync::Arc::new(store), epoch, &[accepted],
        &[repeated_release, repeated_reward], &state,
    ).await;
}

#[test]
fn upheld_target_cannot_resume_burning_by_signing_the_new_epoch() {
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        let (mut fixture, selections) = completed(reason);
        let (_dir, store, heads) = restore_chain(&fixture, &selections);
        let outcome = persist_outcome(&fixture, &store, &heads);
        let events = chain::load(&store, NetworkId::Dev, &fixture.params).unwrap();
        fixture.push(20, BlockType::Burn, CUTOFF + 72, |block| {
            block.payment_epoch = outcome.certificate().checkpoint.hash();
            block.link = destination(2);
            block.brn_spent += 1;
        });
        let error = ConfirmedReplay::replay_with_events(
            &fixture.history(CUTOFF + 74),
            NetworkId::Dev,
            &fixture.params,
            &events,
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("verified wallet"), "{error}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn in_flight_revocation_survives_receive_rejection_and_restart() {
    for reject in [false, true] {
        let (mut fixture, selections) = completed(ChallengeReason::Fraud);
        merged_payments(&mut fixture);
        let receive = fixture.blocks.pop().unwrap();
        assert_eq!(receive.block_type, BlockType::Receive);
        let send = receive.link;
        retirement_history(&mut fixture);
        let (_dir, store, heads) = restore_chain(&fixture, &selections);
        let outcome = persist_outcome(&fixture, &store, &heads);
        let pending = outcome
            .state()
            .pending(&address(1), send.into_tx_hash())
            .unwrap();
        assert_eq!(
            pending.provenance[0]
                .revoked_amounts
                .values()
                .filter(|chunk| !chunk.restored)
                .map(|chunk| chunk.amount)
                .sum::<u128>(),
            5_000
        );
        let epoch = outcome.certificate().checkpoint.hash();
        let records = records(&store, &fixture.params);
        let claim_start = fixture.blocks.len();
        let (owner, claim) = if reject {
            let rejected = fixture.push(1, BlockType::RejectReceive, CUTOFF + 71, |block| {
                block.payment_epoch = epoch;
                block.link = send;
            });
            (8, rejected)
        } else {
            (1, send)
        };
        let received = fixture.push(owner, BlockType::Receive, CUTOFF + 72, |block| {
            block.payment_epoch = epoch;
            block.link = claim;
            block.trst_balance += 10_000;
        });
        let (dir, receiver, _) = fixture.store();
        import(&receiver, &fixture.params, &records);
        drop(receiver);
        let receiver = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let events = chain::load(&receiver, NetworkId::Dev, &fixture.params).unwrap();
        let state = ConfirmedReplay::replay_with_events(
            &fixture.history(CUTOFF + 74),
            NetworkId::Dev,
            &fixture.params,
            &events,
        )
        .unwrap();
        let portfolio = state.portfolio(&address(owner)).unwrap();
        assert!(portfolio
            .tokens
            .iter()
            .any(|token| token.id == received.into_tx_hash()
                && token.amount == 5_000
                && token.state == burst_types::TrstState::Active));
        assert!(portfolio
            .tokens
            .iter()
            .any(|token| token.link == received.into_tx_hash()
                && token.amount == 5_000
                && token.state == burst_types::TrstState::Revoked));
        assert!(state
            .pending(&address(owner), claim.into_tx_hash())
            .is_none());
        crate::confirmed_replay::tests::snapshot_history::installed_recovery::body_retirement::assert_pruned_claims(
            &fixture, std::sync::Arc::new(store), epoch,
            &fixture.blocks[claim_start..], &[], &state,
        ).await;
    }
}

#[test]
fn split_panel_resumes_across_epoch_then_requires_the_second_outcome_epoch() {
    let (mut fixture, first, _) = pending_reason(ChallengeReason::Fraud);
    let (_dir, _store, second) = certify_challenge(&fixture, &first);
    challenge(&mut fixture, 0, ChallengeReason::Fraud, CUTOFF + 35);
    let block = fixture.blocks.last_mut().unwrap();
    block.payment_epoch = second.certificate().checkpoint.hash();
    sign(8, block);
    let mut selections = vec![first, second];
    let (_dir, store, heads) = restore_chain(&fixture, &selections);
    let request = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 36,
        &heads,
        &address(0),
    )
    .unwrap();
    let mut third = CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certify(request.checkpoint(), &[0, 1, 2, 3, 4, 5, 6]),
        heads,
        address(0),
    )
    .unwrap();
    third
        .attach_beacon(
            &store,
            &fixture.params,
            selections[1].beacon().unwrap().clone(),
        )
        .unwrap();
    selections.push(third);
    for seed in panel(&selections[1]) {
        vote_choice_in_epoch(
            &mut fixture,
            &selections[1],
            seed,
            CUTOFF + 60,
            2,
            selections[2].certificate().checkpoint.hash(),
        );
    }
    let voters = panel(&selections[2]);
    assert_eq!(voters.len(), 2);
    vote_choice(&mut fixture, &selections[2], voters[0], CUTOFF + 61, 2);
    merged_payments(&mut fixture);
    let (_dir, store, heads) = restore_chain(&fixture, &selections);
    let first_outcome = persist_outcome(&fixture, &store, &heads);
    let epoch = first_outcome.certificate().checkpoint.hash();
    let clean = first_outcome
        .state()
        .portfolio(&address(8))
        .unwrap()
        .tokens
        .iter()
        .find(|token| token.state == burst_types::TrstState::Active)
        .unwrap()
        .id;
    let previous_records = records(&store, &fixture.params);
    vote_choice(&mut fixture, &selections[2], voters[1], CUTOFF + 71, 2);
    let vote = fixture.blocks.last_mut().unwrap();
    vote.payment_epoch = epoch;
    sign(voters[1], vote);
    let burn = fixture.push(2, BlockType::Burn, CUTOFF + 71, |block| {
        block.payment_epoch = epoch;
        block.link = destination(8);
        block.brn_spent += 10_000;
    });
    let received = fixture.push(8, BlockType::Receive, CUTOFF + 72, |block| {
        block.payment_epoch = epoch;
        block.link = burn;
        block.trst_balance += 10_000;
    });
    let remerged = fixture.push(8, BlockType::Merge, CUTOFF + 73, |block| {
        block.payment_epoch = epoch;
        block.merge_sources = vec![clean, received.into_tx_hash()];
    });
    let (_dir, receiver, heads) = fixture.store();
    import(&receiver, &fixture.params, &previous_records);
    let second_outcome = PreparedChallengeOutcome::prepare(
        &receiver,
        NetworkId::Dev,
        &fixture.params,
        CUTOFF + 75,
        &heads,
        &address(0),
    )
    .unwrap();
    let certificate = certify(
        &second_outcome.manifest().checkpoint,
        &[0, 1, 2, 3, 4, 5, 6],
    );
    let second_outcome = second_outcome
        .certify(certificate)
        .unwrap()
        .persist(&receiver, NetworkId::Dev, &fixture.params)
        .unwrap();
    let second_epoch = second_outcome.certificate().checkpoint.hash();
    let events = chain::load(&receiver, NetworkId::Dev, &fixture.params).unwrap();
    assert_eq!(events.len(), 5);
    let prefix_len = fixture.blocks.len();
    for (context, valid) in [(epoch, false), (second_epoch, true)] {
        fixture.blocks.truncate(prefix_len);
        fixture.push(1, BlockType::ChangeRepresentative, CUTOFF + 76, |block| {
            block.payment_epoch = context;
        });
        let result = ConfirmedReplay::replay_with_events(
            &fixture.history(CUTOFF + 77),
            NetworkId::Dev,
            &fixture.params,
            &events,
        );
        if valid {
            let state = result.unwrap();
            assert_eq!(state.human_roster().unwrap().total_humans(), 8);
            let tokens = &state.portfolio(&address(8)).unwrap().tokens;
            assert!(tokens
                .iter()
                .any(|token| token.id == remerged.into_tx_hash()
                    && token.amount == 10_000
                    && token.state == burst_types::TrstState::Active));
            assert!(tokens
                .iter()
                .any(|token| token.link == remerged.into_tx_hash()
                    && token.amount == 5_000
                    && token.state == burst_types::TrstState::Revoked));
            for seed in [0, 20] {
                assert_eq!(
                    state.account(&address(seed)).unwrap().state,
                    WalletState::Revoked
                );
            }
        } else {
            assert!(result.is_err());
        }
    }
}


fn retirement_history(fixture: &mut Fixture) {
    for offset in 0..4 {
        fixture.push(0, BlockType::ChangeRepresentative, CUTOFF + 66 + offset, |_| {});
    }
}
