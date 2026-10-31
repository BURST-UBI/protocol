use super::*;
use crate::verification_commit::receipts;
use burst_store::block::BlockStore;

pub(super) struct ReviewPlan<'a> {
    pub judgments: &'a [bool],
    pub partitions: Option<&'a [partition::Registry]>,
    pub desktop: bool,
}

pub(super) async fn run(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    params: &ProtocolParams,
    clock: &Arc<AtomicU64>,
    addresses: &[WalletAddress],
    root: receipts::Receipt,
    plan: ReviewPlan<'_>,
) -> (serde_json::Value, receipts::Receipt) {
    let (_, reason, _) = root.challenge_judgment().unwrap();
    let mut history = vec![root];
    let mut expected = None;
    for (attempt, &legitimate) in plan.judgments.iter().enumerate() {
        let (state, receipt) = run_round(
            clients,
            stores,
            params,
            clock,
            addresses,
            history.last().unwrap().clone(),
            ReviewRound {
                attempt,
                legitimate,
                reason,
                partitions: plan.partitions,
                desktop: plan.desktop,
            },
        )
        .await;
        for store in stores {
            let batch = store.tx_begin_write().unwrap();
            let economic = crate::payment_transition::restore(&batch, params).unwrap();
            for prior in &history {
                assert_eq!(
                    receipts::decode(
                        &store
                            .meta_store()
                            .get_meta(&receipts::key(prior.source))
                            .unwrap()
                    )
                    .unwrap(),
                    *prior,
                    "an appeal rewrote a historical receipt"
                );
                let prior_legitimate =
                    *prior.challenge_judgment().unwrap().2 == receipts::ChallengeVerdict::Rejected;
                for reward in &prior.rewards {
                    assert_eq!(
                        economic.trst.merger_graph.is_origin_revoked(&reward.origin),
                        prior_legitimate != legitimate,
                        "reward correction did not follow the full judgment history"
                    );
                }
            }
        }
        history.push(receipt);
        expected = Some(state);
    }
    (expected.unwrap(), history.pop().unwrap())
}

struct ReviewRound<'a> {
    attempt: usize,
    legitimate: bool,
    reason: ChallengeReason,
    partitions: Option<&'a [partition::Registry]>,
    desktop: bool,
}

async fn run_round(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    params: &ProtocolParams,
    clock: &Arc<AtomicU64>,
    addresses: &[WalletAddress],
    root: receipts::Receipt,
    round: ReviewRound<'_>,
) -> (serde_json::Value, receipts::Receipt) {
    let keys: Vec<_> = (0..5)
        .map(|seed| burst_crypto::keypair_from_seed(&[seed; 32]))
        .collect();
    for store in stores {
        assert!(store
            .meta_store()
            .get_meta(&receipts::certified_key(root.source))
            .is_ok());
    }
    let selection_cutoff = 20264 + 4 * round.attempt as u64;
    let admission_time = time(if round.attempt == 0 {
        104
    } else {
        selection_cutoff
    });
    clock.store(admission_time.as_secs(), Ordering::SeqCst);
    let before_state = stores[0]
        .account_store()
        .get_account(&addresses[4])
        .unwrap()
        .state;
    let sponsor_burned = {
        let batch = stores[0].tx_begin_write().unwrap();
        crate::payment_transition::restore(&batch, params)
            .unwrap()
            .brn
            .get_wallet(&addresses[0])
            .unwrap()
            .total_burned
    };
    let review = clients[0].appeal_review(&addresses[4]).await.unwrap();
    assert!(review.enabled);
    assert!(!review.challenge_active);
    assert_eq!(
        review.outcome.as_ref().unwrap().hash,
        root.source.to_string()
    );
    assert_eq!(
        review.required_stake,
        params.challenge_stake_amount.to_string()
    );
    assert_eq!(review.outcome.as_ref().unwrap().reason, round.reason);
    let succeeds = review.outcome.as_ref().unwrap().legitimate != round.legitimate;
    let result = if round.desktop {
        native_appeal::submit(&clients[0], &review, admission_time).await
    } else {
        clients[0]
            .appeal_signed(&keys[0], &review, admission_time)
            .await
            .unwrap()
    };
    assert!(result.accepted);
    for client in clients {
        client
            .wait_confirmed(&result.hash, Duration::from_secs(25))
            .await
            .unwrap();
        let current = client.appeal_review(&addresses[4]).await.unwrap();
        assert!(current.challenge_active);
        assert_eq!(current.outcome, review.outcome);
    }
    let hash = burst_wallet_core::signed_client::parse_hash(&result.hash).unwrap();
    let appeal: StateBlock =
        bincode::deserialize(&stores[0].block_store().get_block(&hash).unwrap()).unwrap();
    assert_eq!(appeal.target_head, root.source);
    assert_eq!(appeal.account, addresses[0]);
    assert_eq!(appeal.timestamp, admission_time);
    // The original deadline penalized the two absent voters. Admission is
    // allowed, but the appeal must not select them during that cooldown.
    if round.attempt == 0 {
        for store in stores {
            let events = chain::load(store, NetworkId::Dev, params).unwrap();
            let anchor = chain::next_anchor(&events, NetworkId::Dev, params).unwrap();
            let heads =
                crate::confirmed_history::ConfirmedHistory::heads_at(store, time(104).as_secs())
                    .unwrap();
            let history = crate::confirmed_history::ConfirmedHistory::read(
                store,
                NetworkId::Dev,
                anchor.genesis_hash(),
                time(104).as_secs(),
                &heads,
                events.last().unwrap().heads(),
            )
            .unwrap();
            let mut replay = crate::confirmed_replay::ConfirmedReplay::replay_with_events(
                &history,
                NetworkId::Dev,
                params,
                &events,
            )
            .unwrap();
            let snapshot = {
                let batch = store.tx_begin_write().unwrap();
                crate::verification_storage::load(&batch).unwrap().snapshot()
            };
            for address in &addresses[2..4] {
                let until = snapshot.penalized_verifiers[address];
                assert!(until > time(104).as_secs());
                assert!(until <= time(20264).as_secs());
            }
            assert!(replay
                .prepare_selection(&addresses[4])
                .unwrap_err()
                .to_string()
                .contains("invalid or non-canonical selection request"));
            // Only the fraud fixture mints a bounty that later needs expiry.
            // Inactivity must not create that extra economic boundary.
            assert_eq!(
                crate::checkpoint_deadline::PreparedDeadline::prepare_next(
                    store,
                    NetworkId::Dev,
                    params,
                    time(20264).as_secs(),
                )
                .expect("deadline replay after verifier cooldown")
                .is_some(),
                round.reason == ChallengeReason::Fraud
            );
        }
    }
    eprintln!(
        "appeal {}: admission replayed for {:?}; requested legitimate={}",
        round.attempt, round.reason, round.legitimate
    );
    clock.store(time(selection_cutoff).as_secs(), Ordering::SeqCst);
    let (context, beacon_time) = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let events = chain::load(&stores[0], NetworkId::Dev, params).unwrap();
            if let Some(selection) = events
                .iter()
                .filter_map(chain::CertifiedEvent::selection)
                .find(|selection| selection.replayed().request().trigger() == appeal.hash)
            {
                let request = selection.replayed().request();
                assert_eq!(request.round(), selection_cutoff + 2);
                break (
                    request
                        .vote_context(&selection.certificate().checkpoint)
                        .unwrap(),
                    request.beacon_time().unwrap(),
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("appeal selection missing");
    clock.store(beacon_time + 2, Ordering::SeqCst);
    let activation = pending_challenge::deadline::wait_for_activation(
        stores,
        params,
        &addresses[4],
        clock,
        context,
    )
    .await;
    let now = Timestamp::new(clock.load(Ordering::SeqCst));
    assert!(now.as_secs() < activation.deadline);
    for index in 1..4 {
        let vote = signed_block(
            &clients[0],
            &keys[index],
            Transaction::VerificationVote(
                build_verification_vote_tx(
                    &addresses[index],
                    &addresses[4],
                    context,
                    if round.legitimate { 1 } else { 2 },
                    params.verifier_stake_amount,
                    now,
                )
                .unwrap(),
            ),
            params,
        )
        .await;
        submit(clients, &vote).await;
        if index < 3 {
            for store in stores {
                assert_eq!(
                    store
                        .account_store()
                        .get_account(&addresses[4])
                        .unwrap()
                        .state,
                    before_state
                );
                assert_eq!(
                    store
                        .meta_store()
                        .get_meta(&receipts::latest_key(&addresses[4]))
                        .unwrap(),
                    root.source.as_bytes()
                );
            }
        }
    }
    let partition = match round.partitions {
        Some(registries) => Some(partition::Partition::split(registries, 2).await),
        None => None,
    };
    let before_epochs: Vec<_> = stores
        .iter()
        .map(|store| store.meta_store().payment_epoch().unwrap())
        .collect();
    clock.store(now.as_secs() + 2, Ordering::SeqCst);
    if let Some(partition) = partition {
        let until = tokio::time::Instant::now() + Duration::from_secs(6);
        while tokio::time::Instant::now() < until {
            for (store, epoch) in stores.iter().zip(&before_epochs) {
                assert_eq!(
                    &store.meta_store().payment_epoch().unwrap(),
                    epoch,
                    "a minority installed an appeal payment epoch"
                );
                assert_eq!(
                    store
                        .account_store()
                        .get_account(&addresses[4])
                        .unwrap()
                        .state,
                    before_state,
                    "a local panel reversed identity without certified settlement"
                );
                assert_eq!(
                    store
                        .meta_store()
                        .get_meta(&receipts::latest_key(&addresses[4]))
                        .unwrap(),
                    root.source.as_bytes(),
                    "a minority replaced the certified judgment"
                );
                assert_eq!(
                    store
                        .account_store()
                        .get_account(&addresses[0])
                        .unwrap()
                        .total_brn_staked,
                    params.challenge_stake_amount,
                    "a minority released the appeal sponsor's stake"
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        partition.heal().await;
    }
    let expected_state = if round.legitimate {
        WalletState::Verified
    } else if round.reason == ChallengeReason::Fraud {
        WalletState::Revoked
    } else {
        WalletState::Deactivated
    };
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if stores.iter().all(|store| {
                store
                    .account_store()
                    .get_account(&addresses[4])
                    .is_ok_and(|account| account.state == expected_state)
                    && store
                        .meta_store()
                        .get_meta(&receipts::latest_key(&addresses[4]))
                        .is_ok_and(|source| source != root.source.as_bytes())
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("appeal judgment did not settle");
    wait_human_authority(stores, if round.legitimate { 5 } else { 4 }).await;
    let mut final_receipt = None;
    for store in stores {
        let reference = store
            .meta_store()
            .get_meta(&receipts::latest_key(&addresses[4]))
            .unwrap();
        let source = BlockHash::new(reference.as_slice().try_into().unwrap());
        let receipt =
            receipts::decode(&store.meta_store().get_meta(&receipts::key(source)).unwrap())
                .unwrap();
        assert_eq!(
            receipt.verdict,
            receipts::Verdict::Appeal {
                admission: appeal.hash,
                reason: round.reason,
                result: if round.legitimate {
                    receipts::ChallengeVerdict::Rejected
                } else {
                    receipts::ChallengeVerdict::Upheld
                },
                reviewed: root.source,
            }
        );
        assert!(
            !receipt
                .rewards
                .iter()
                .any(|reward| reward.recipient == addresses[0]),
            "repeated review minted another fraud bounty"
        );
        // Verifier rewards divide dissenting verifier stakes. A failed appeal
        // burns the sponsor's stake; it does not add that burn to this pool.
        assert!(
            receipt.rewards.is_empty(),
            "unanimous review must not invent verifier rewards"
        );
        let batch = store.tx_begin_write().unwrap();
        let economic = crate::payment_transition::restore(&batch, params).unwrap();
        assert_eq!(
            economic.brn.get_wallet(&addresses[0]).unwrap().total_burned,
            sponsor_burned
                + if succeeds {
                    0
                } else {
                    params.challenge_stake_amount
                }
        );
        assert_eq!(
            economic.brn.get_wallet(&addresses[0]).unwrap().total_staked,
            0
        );
        assert_eq!(
            economic
                .brn
                .get_wallet(&addresses[4])
                .unwrap()
                .accrual_stopped_at
                .is_none(),
            round.legitimate
        );
        let account = store.account_store().get_account(&addresses[4]).unwrap();
        let mut head = account.head;
        for _ in 0..account.block_count {
            let block: StateBlock =
                bincode::deserialize(&store.block_store().get_block(&head).unwrap()).unwrap();
            if block.block_type == BlockType::Burn && block.timestamp >= time(20264) {
                assert_eq!(
                    economic
                        .trst
                        .merger_graph
                        .is_origin_revoked(&block.hash.into_tx_hash()),
                    !round.legitimate && round.reason == ChallengeReason::Fraud,
                    "new payments after restoration did not follow the later fraud judgment"
                );
            }
            head = block.previous;
        }
        if let Some(expected) = &final_receipt {
            assert_eq!(&receipt, expected);
        } else {
            final_receipt = Some(receipt);
        }
    }
    // Restoration cannot make the target's already-expired TRST transferable.
    let portfolio = clients[0]
        .rpc_call("trst_portfolio", json!({"account":addresses[4].as_str()}))
        .await
        .unwrap();
    assert_eq!(portfolio["total_active"], "0");
    let now = Timestamp::new(clock.load(Ordering::SeqCst));
    if round.legitimate {
        let payment = signed_block(
            &clients[0],
            &keys[4],
            Transaction::Burn(build_burn_tx(&addresses[4], &addresses[1], BRN_UNIT, now).unwrap()),
            params,
        )
        .await;
        submit(clients, &payment).await;
    } else {
        let before = clients[0].signing_state(&addresses[4]).await.unwrap().head;
        let payment = signed_block(
            &clients[0],
            &keys[4],
            Transaction::Burn(build_burn_tx(&addresses[4], &addresses[1], BRN_UNIT, now).unwrap()),
            params,
        )
        .await;
        let rejected = clients[0]
            .process(&serde_json::to_string(&payment).unwrap())
            .await
            .unwrap();
        assert!(!rejected.accepted, "revoked wallet spent: {rejected:?}");
        assert_eq!(
            rejected.detail.as_deref(),
            Some("spending requires a verified wallet")
        );
        assert_eq!(
            clients[0].signing_state(&addresses[4]).await.unwrap().head,
            before
        );
    }
    let expected = post_challenge_recovery::capture(&clients[0], addresses)
        .await
        .unwrap();
    for client in clients {
        assert_eq!(
            post_challenge_recovery::capture(client, addresses)
                .await
                .unwrap(),
            expected
        );
    }
    eprintln!(
        "appeal {}: {:?} judgment legitimate={}, sponsor success={}, expiry preserved",
        round.attempt, round.reason, round.legitimate, succeeds
    );
    (expected, final_receipt.unwrap())
}
