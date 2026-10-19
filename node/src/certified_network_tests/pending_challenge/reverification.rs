use super::*;
use crate::checkpoint_store::chain;

mod inactivity;

pub(super) async fn run(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    params: &ProtocolParams,
    addresses: &[WalletAddress],
    clock: &Arc<AtomicU64>,
    mode: ChallengeMode,
) {
    let target = &addresses[4];
    let old = clients[0]
        .rpc_call("verification_status", json!({"account":target.as_str()}))
        .await
        .unwrap();
    assert_eq!(old["status"], mode.settled_status());
    let admitted_at = time(1100);
    let paused: Vec<_> = stores
        .iter()
        .map(|store| {
            let batch = store.tx_begin_write().unwrap();
            let state = crate::payment_transition::restore(&batch, params).unwrap();
            let wallet = state.brn.get_wallet(target).unwrap();
            assert!(!wallet.accrual_active);
            let stopped = wallet.accrual_stopped_at.unwrap();
            let balance = state.brn.compute_balance_checked(wallet, stopped).unwrap();
            assert_eq!(
                state
                    .brn
                    .compute_balance_checked(wallet, admitted_at)
                    .unwrap(),
                balance
            );
            (wallet.clone(), balance)
        })
        .collect();
    clock.store(admitted_at.as_secs(), Ordering::SeqCst);
    if mode == ChallengeMode::ReverifiedInactivity {
        inactivity::transfer_originated_trst(clients, params, addresses, admitted_at).await;
    }
    let genesis = burst_crypto::keypair_from_seed(&[0; 32]);
    let tx = Transaction::Endorse(
        build_endorse_tx(
            &addresses[0],
            target,
            params.endorsement_burn_amount,
            admitted_at,
        )
        .unwrap(),
    );
    let endorsement = signed_block(&clients[0], &genesis, tx, params).await;
    submit(clients, &endorsement).await;

    let (context, beacon_time) = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let events = chain::load(&stores[0], NetworkId::Dev, params).unwrap();
            if let Some(selection) = events
                .iter()
                .filter_map(chain::CertifiedEvent::selection)
                .find(|selection| selection.replayed().request().trigger() == endorsement.hash)
            {
                let request = selection.replayed().request();
                assert_eq!(request.attempt(), 0);
                assert_eq!(request.round(), 1102);
                assert_eq!(
                    selection.replayed().lifecycle(),
                    crate::checkpoint_replay::SelectionLifecycle::Verification(endorsement.hash)
                );
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
    .expect("fresh endorsements did not produce a new selection");
    assert_ne!(old["selection_context"], context.to_string());
    let voted_at = Timestamp::new(beacon_time + 2);
    clock.store(voted_at.as_secs(), Ordering::SeqCst);
    let mut activation = None;
    for (client, store) in clients.iter().zip(stores) {
        let proof = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if let Ok(bytes) = store
                    .meta_store()
                    .get_meta(&crate::verification_activation::key(context))
                {
                    break crate::verification_activation::decode(&bytes).unwrap();
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("fresh verification panel was not activated");
        assert_eq!(proof.target, *target);
        assert_eq!(
            proof.deadline,
            proof.started_at + params.verification_timeout_secs
        );
        if let Some(expected) = &activation {
            assert_eq!(&proof, expected);
        } else {
            activation = Some(proof);
        }
        let status = client
            .rpc_call("verification_status", json!({"account":target.as_str()}))
            .await
            .unwrap();
        assert_eq!(status["status"], mode.settled_status());
        assert_eq!(status["phase"], "voting");
        assert_eq!(status["endorser_count"], 1);
        assert_eq!(status["selection_context"], context.to_string());
        assert_eq!(status["verifier_votes"].as_array().unwrap().len(), 0);
        assert_eq!(status["challenge_active"], false);
    }
    for seed in 1..=3 {
        let verifier = burst_crypto::keypair_from_seed(&[seed; 32]);
        let tx = Transaction::VerificationVote(
            build_verification_vote_tx(
                &addresses[seed as usize],
                target,
                context,
                1,
                params.verifier_stake_amount,
                voted_at,
            )
            .unwrap(),
        );
        let vote = signed_block(&clients[0], &verifier, tx, params).await;
        submit(clients, &vote).await;
        for client in clients {
            let status = client
                .rpc_call("verification_status", json!({"account":target.as_str()}))
                .await
                .unwrap();
            assert_eq!(
                status["status"],
                if seed < 3 {
                    mode.settled_status()
                } else {
                    "verified"
                }
            );
            assert_eq!(
                status["verifier_votes"].as_array().unwrap().len(),
                seed as usize
            );
        }
    }

    let now = Timestamp::new(voted_at.as_secs() + 3);
    clock.store(now.as_secs(), Ordering::SeqCst);
    wait_human_authority(stores, 5).await;
    for (store, (before, balance)) in stores.iter().zip(&paused) {
        let batch = store.tx_begin_write().unwrap();
        let state = crate::payment_transition::restore(&batch, params).unwrap();
        let wallet = state.brn.get_wallet(target).unwrap();
        assert!(wallet.accrual_active);
        assert_eq!(wallet.accrual_stopped_at, None);
        assert_eq!(wallet.verified_at, before.verified_at);
        assert_eq!(wallet.total_burned, before.total_burned);
        assert_eq!(wallet.total_staked, before.total_staked);
        assert_eq!(
            wallet.paused_intervals.last(),
            Some(&(before.accrual_stopped_at.unwrap(), voted_at))
        );
        assert_eq!(
            state.brn.compute_balance_checked(wallet, voted_at).unwrap(),
            *balance,
            "re-verification must neither erase earned BRN nor accrue during the pause"
        );
    }
    let pending = clients[0]
        .rpc_call("account_pending", json!({"account":addresses[0].as_str()}))
        .await
        .unwrap();
    let restored = pending["pending"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["source"] == target.as_str())
        .unwrap();
    assert_eq!(restored["amount"], BRN_UNIT.to_string());
    let source = TxHash::new(
        hex::decode(restored["hash"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap(),
    );
    let tx = Transaction::Receive(build_receive_tx(&addresses[0], source, BRN_UNIT, now).unwrap());
    let claim = signed_block(&clients[0], &genesis, tx, params).await;
    submit(clients, &claim).await;
    for client in clients {
        let portfolio = client
            .rpc_call("trst_portfolio", json!({"account":addresses[0].as_str()}))
            .await
            .unwrap();
        assert!(portfolio["batches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|batch| batch["originator"] == target.as_str()
                && batch["amount"] == BRN_UNIT.to_string()
                && batch["status"] == "active"));
    }
    let key = burst_crypto::keypair_from_seed(&[4; 32]);
    let tx = Transaction::Burn(build_burn_tx(target, &addresses[1], BRN_UNIT, now).unwrap());
    let payment = signed_block(&clients[0], &key, tx, params).await;
    submit(clients, &payment).await;
    let receiver = burst_crypto::keypair_from_seed(&[1; 32]);
    let tx = Transaction::Receive(
        build_receive_tx(&addresses[1], payment.hash.into_tx_hash(), BRN_UNIT, now).unwrap(),
    );
    submit(
        clients,
        &signed_block(&clients[0], &receiver, tx, params).await,
    )
    .await;
}
