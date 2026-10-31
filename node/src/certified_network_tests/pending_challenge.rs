use super::*;

mod collected;
pub(super) mod deadline;
mod rejected;
mod reverification;
mod settled;

pub(super) async fn run(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    params: &ProtocolParams,
    addresses: &[WalletAddress],
    clock: &Arc<AtomicU64>,
    mode: ChallengeMode,
) {
    // The relay has a real round-1005 proof. Advancing controlled fixture time
    // also lets the verifier who just spent their BRN become eligible again.
    let admitted_at = time(1003);
    clock.store(admitted_at.as_secs(), Ordering::SeqCst);
    let challenger = burst_crypto::keypair_from_seed(&[0; 32]);
    let tx = Transaction::Challenge(
        build_challenge_tx(
            &addresses[0],
            &addresses[4],
            params.challenge_stake_amount,
            if mode == ChallengeMode::ReverifiedInactivity {
                burst_types::ChallengeReason::Inactivity
            } else {
                burst_types::ChallengeReason::Fraud
            },
            admitted_at,
        )
        .unwrap(),
    );
    let admission = signed_block(&clients[0], &challenger, tx, params).await;
    submit(clients, &admission).await;

    let selection_started = std::time::Instant::now();
    let mut expected_context = None;
    for (store, client) in stores.iter().zip(clients) {
        let context = tokio::time::timeout(Duration::from_secs(40), async {
            loop {
                let chain =
                    crate::checkpoint_store::chain::load(store, NetworkId::Dev, params).unwrap();
                if let Some(checkpoint) = chain
                    .iter()
                    .filter_map(crate::checkpoint_store::chain::CertifiedEvent::selection)
                    .find(|checkpoint| checkpoint.replayed().request().trigger() == admission.hash)
                {
                    let request = checkpoint.replayed().request();
                    assert_eq!(request.attempt(), 0);
                    assert_eq!(request.round(), 1005);
                    assert_eq!(
                        checkpoint.replayed().lifecycle(),
                        crate::checkpoint_replay::SelectionLifecycle::Challenge(admission.hash)
                    );
                    break request
                        .vote_context(&checkpoint.certificate().checkpoint)
                        .unwrap();
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap_or_else(|error| {
            for (index, store) in stores.iter().enumerate() {
                let chain =
                    crate::checkpoint_store::chain::load(store, NetworkId::Dev, params).unwrap();
                let anchor =
                    crate::checkpoint_store::chain::next_anchor(&chain, NetworkId::Dev, params)
                        .unwrap();
                eprintln!(
                    "challenge wait node {index}: height {}, cutoff {}, epoch {:?}",
                    anchor.height(),
                    anchor.cutoff(),
                    store.meta_store().payment_epoch()
                );
                for address in addresses {
                    if let Some(state) =
                        crate::checkpoint_agreement::read_state(store, &anchor, address).unwrap()
                    {
                        eprintln!(
                            "  signer {address}: round {}, step {:?}, proposal {:?}, prepared {:?}",
                            state.round(),
                            state.step(),
                            state.proposal(),
                            state.valid_proof()
                        );
                    }
                }
                match crate::checkpoint_store::CertifiedCheckpoint::prepare_next(
                    store,
                    NetworkId::Dev,
                    params,
                    admitted_at.as_secs(),
                ) {
                    Ok(Some(candidate)) => eprintln!(
                        "  candidate {:?}, target {}, beacon {}",
                        candidate.checkpoint(),
                        candidate.request().target(),
                        candidate.request().round()
                    ),
                    Ok(None) => eprintln!("  no selection candidate"),
                    Err(error) => eprintln!("  selection replay failed: {error}"),
                }
            }
            panic!("pending challenge certificate did not arrive: {error}")
        });
        if let Some(expected) = expected_context {
            assert_eq!(context, expected);
        } else {
            expected_context = Some(context);
        }
        eprintln!(
            "challenge selection certificate observed after {:?}",
            selection_started.elapsed()
        );
        tokio::time::timeout(Duration::from_secs(40), async {
            loop {
                let status = client
                    .rpc_call(
                        "verification_status",
                        json!({"account":addresses[4].as_str()}),
                    )
                    .await
                    .unwrap();
                if status["selection_context"] == context.to_string() {
                    assert_eq!(status["status"], "verified");
                    assert_eq!(status["phase"], "voting");
                    assert_eq!(status["challenge_active"], true);
                    assert_eq!(status["selection_available"], false);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("pending challenge committee was not installed");
        let components = client
            .rpc_call("brn_components", json!({"account":addresses[0].as_str()}))
            .await
            .unwrap();
        assert_eq!(
            components["total_staked"],
            params.challenge_stake_amount.to_string()
        );
    }

    let target = burst_crypto::keypair_from_seed(&[4; 32]);
    let tx = Transaction::Burn(
        build_burn_tx(&addresses[4], &addresses[0], BRN_UNIT, admitted_at).unwrap(),
    );
    submit(
        clients,
        &signed_block(&clients[0], &target, tx, params).await,
    )
    .await;

    let voted_at = if mode == ChallengeMode::EarlyVoteDeadline {
        let events =
            crate::checkpoint_store::chain::load(&stores[0], NetworkId::Dev, params).unwrap();
        let selection = events
            .iter()
            .filter_map(crate::checkpoint_store::chain::CertifiedEvent::selection)
            .find(|selection| selection.replayed().request().trigger() == admission.hash)
            .unwrap();
        // At the beacon second no activation cut can yet be in the trusted past.
        Timestamp::new(selection.replayed().request().beacon_time().unwrap())
    } else {
        time(1005)
    };
    clock.store(voted_at.as_secs(), Ordering::SeqCst);
    if mode == ChallengeMode::Deadline {
        // Frozen fixture time cannot certify a past cut containing a vote from
        // this same second. Install the window before signing into its epoch.
        deadline::wait_for_activation(
            stores,
            params,
            &addresses[4],
            clock,
            expected_context.unwrap(),
        )
        .await;
    }
    let verifier = burst_crypto::keypair_from_seed(&[1; 32]);
    let tx = Transaction::VerificationVote(
        build_verification_vote_tx(
            &addresses[1],
            &addresses[4],
            expected_context.unwrap(),
            1,
            params.verifier_stake_amount,
            voted_at,
        )
        .unwrap(),
    );
    let first_vote = signed_block(&clients[0], &verifier, tx, params).await;
    submit(clients, &first_vote).await;
    if mode == ChallengeMode::EarlyVoteDeadline {
        for store in stores {
            assert!(matches!(
                store
                    .meta_store()
                    .get_meta(&crate::verification_activation::key(
                        expected_context.unwrap()
                    )),
                Err(burst_store::StoreError::NotFound(_))
            ));
        }
    }
    for client in clients {
        let status = client
            .rpc_call(
                "verification_status",
                json!({"account":addresses[4].as_str()}),
            )
            .await
            .unwrap();
        assert_eq!(status["status"], "verified");
        assert_eq!(status["challenge_active"], true);
        assert_eq!(status["verifier_votes"].as_array().unwrap().len(), 1);
        let components = client
            .rpc_call("brn_components", json!({"account":addresses[1].as_str()}))
            .await
            .unwrap();
        assert_eq!(
            components["total_staked"],
            params.verifier_stake_amount.to_string()
        );
    }
    let mut expected = None;
    for store in stores {
        let heads = crate::confirmed_history::ConfirmedHistory::heads_at(store, voted_at.as_secs())
            .unwrap();
        let history = crate::confirmed_history::ConfirmedHistory::read(
            store,
            NetworkId::Dev,
            crate::genesis_state::block(NetworkId::Dev, params).hash,
            voted_at.as_secs(),
            &heads,
            &[],
        )
        .unwrap();
        let certificates =
            crate::checkpoint_store::chain::load(store, NetworkId::Dev, params).unwrap();
        assert_eq!(
            certificates
                .iter()
                .filter_map(crate::checkpoint_store::chain::CertifiedEvent::selection)
                .count(),
            2
        );
        let replay = crate::confirmed_replay::ConfirmedReplay::replay_with_events(
            &history,
            NetworkId::Dev,
            params,
            &certificates,
        )
        .unwrap();
        let process = replay.verification(&addresses[4]).unwrap();
        assert_eq!(process.votes.len(), 1);
        assert_eq!(process.votes[0].verifier, addresses[1]);
        let mut selected = process.selected_verifiers.clone();
        selected.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        let mut original = addresses[1..4].to_vec();
        original.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        assert_eq!(
            *selected, original,
            "original verifiers remain eligible for a challenge"
        );
        let target = replay.account(&addresses[4]).unwrap();
        assert!(target.state.is_verified());
        assert_eq!(target.total_brn_burned, 2 * BRN_UNIT);
        assert_eq!(
            replay.account(&addresses[0]).unwrap().total_brn_staked,
            params.challenge_stake_amount
        );
        assert_eq!(
            replay.account(&addresses[1]).unwrap().total_brn_staked,
            params.verifier_stake_amount
        );
        let projection = bincode::serialize(&(
            target,
            replay.account(&addresses[0]),
            replay.account(&addresses[1]),
            &process.votes,
        ))
        .unwrap();
        if let Some(expected) = &expected {
            assert_eq!(&projection, expected);
        } else {
            expected = Some(projection);
        }
    }
    if mode.has_deadline() {
        if mode == ChallengeMode::EarlyVoteDeadline {
            clock.store(voted_at.as_secs() + 2, Ordering::SeqCst);
        }
        deadline::run(
            clients,
            stores,
            params,
            addresses,
            clock,
            (expected_context.unwrap(), first_vote.hash),
        )
        .await;
        return;
    }
    if mode == ChallengeMode::Upheld || mode.reverifies() {
        collected::run(
            clients,
            stores,
            params,
            addresses,
            expected_context.unwrap(),
            voted_at,
        )
        .await;
        settled::run(clients, stores, params, addresses, clock, voted_at, mode).await;
        if mode.reverifies() {
            reverification::run(clients, stores, params, addresses, clock, mode).await;
        }
        return;
    }
    rejected::run(
        clients,
        stores,
        params,
        addresses,
        expected_context.unwrap(),
        [admission.hash, first_vote.hash],
        voted_at,
    )
    .await;
}
