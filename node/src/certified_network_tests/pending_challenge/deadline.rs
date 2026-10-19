use super::*;
use crate::{verification_activation, verification_deadline};
use burst_store::account::AccountStore;

pub(in crate::certified_network_tests) async fn wait_for_activation(
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    params: &ProtocolParams,
    target: &WalletAddress,
    clock: &Arc<AtomicU64>,
    context: BlockHash,
) -> burst_verification::CommitteeActivation {
    let mut agreed = None;
    for store in stores {
        let activation = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if let Ok(bytes) = store
                    .meta_store()
                    .get_meta(&verification_activation::key(context))
                {
                    break verification_activation::decode(&bytes).unwrap();
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap_or_else(|error| {
            for (index, store) in stores.iter().enumerate() {
                let events =
                    crate::checkpoint_store::chain::load(store, NetworkId::Dev, params).unwrap();
                let anchor =
                    crate::checkpoint_store::chain::next_anchor(&events, NetworkId::Dev, params)
                        .unwrap();
                eprintln!(
                    "activation node {index}: height {}, cutoff {}, installed {:?}, certified {:?}, wanted {context:?}",
                    anchor.height(),
                    anchor.cutoff(),
                    store.meta_store().payment_epoch(),
                    anchor.hash()
                );
                for selection in events.iter().filter_map(crate::checkpoint_store::chain::CertifiedEvent::selection) {
                    let request = selection.replayed().request();
                    eprintln!("selection height={} round={} beacon={} context={:?}", selection.certificate().checkpoint.height, request.round(), selection.beacon().is_some(), request.vote_context(&selection.certificate().checkpoint));
                }
                match crate::checkpoint_activation::PreparedActivation::prepare_next(
                    store,
                    NetworkId::Dev,
                    params,
                    clock.load(Ordering::SeqCst) - 1,
                ) {
                    Ok(Some(candidate)) => {
                        eprintln!("activation candidate {:?}", candidate.manifest().checkpoint)
                    }
                    Ok(None) => eprintln!("no activation candidate"),
                    Err(error) => eprintln!("activation replay error: {error}"),
                }
            }
            panic!("challenge activation did not reach every node: {error}")
        });
        assert_eq!(&activation.target, target);
        assert_eq!(
            activation.deadline,
            activation.started_at + params.challenge_duration_secs
        );
        if let Some(expected) = &agreed {
            assert_eq!(&activation, expected);
        } else {
            agreed = Some(activation);
        }
    }
    agreed.unwrap()
}

pub(super) async fn run(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    params: &ProtocolParams,
    addresses: &[WalletAddress],
    clock: &Arc<AtomicU64>,
    proof: (BlockHash, BlockHash),
) {
    let (context, reward_source) = proof;
    let activation = wait_for_activation(stores, params, &addresses[4], clock, context).await;
    let before: Vec<_> = addresses
        .iter()
        .map(|address| stores[0].account_store().get_account(address).unwrap())
        .collect();
    assert!(before[4].state.is_verified());
    assert_eq!(before[0].total_brn_staked, params.challenge_stake_amount);
    assert_eq!(before[1].total_brn_staked, params.verifier_stake_amount);
    let honest = burst_crypto::keypair_from_seed(&[2; 32]);
    let tx = Transaction::Burn(
        build_burn_tx(
            &addresses[2],
            &addresses[0],
            BRN_UNIT,
            Timestamp::new(clock.load(Ordering::SeqCst)),
        )
        .unwrap(),
    );
    let old_payment = signed_block(&clients[0], &honest, tx, params).await;
    clock.store(activation.deadline + 2, Ordering::SeqCst);
    let mut completion = None;
    let mut epoch = None;
    for (client, store) in clients.iter().zip(stores) {
        let observed = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if let Ok(bytes) = store
                    .meta_store()
                    .get_meta(&verification_deadline::key(context))
                {
                    break verification_deadline::decode(&bytes).unwrap();
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("challenge deadline was not certified and installed");
        assert_eq!(observed.activation, activation);
        assert_eq!(observed.completed_at, activation.deadline + 1);
        assert_eq!(observed.absent.len(), 2);
        assert!(observed.absent.contains(&addresses[2]));
        assert!(observed.absent.contains(&addresses[3]));
        if let Some(expected) = &completion {
            assert_eq!(&observed, expected);
        } else {
            completion = Some(observed);
        }
        let authority = crate::payment_authority::PaymentAuthority::read(store)
            .unwrap()
            .unwrap();
        assert_eq!(authority.roster.total_humans(), 4);
        assert_eq!(authority.roster.weight(&addresses[4]), 0);
        assert_ne!(authority.epoch, old_payment.payment_epoch);
        if let Some(expected) = epoch {
            assert_eq!(authority.epoch, expected);
        } else {
            epoch = Some(authority.epoch);
        }
        let target = store.account_store().get_account(&addresses[4]).unwrap();
        assert_eq!(target.state, burst_types::WalletState::Revoked);
        for (index, address) in addresses.iter().enumerate() {
            let account = store.account_store().get_account(address).unwrap();
            assert_eq!(account.total_brn_staked, 0);
            assert_eq!(
                account.total_brn_burned,
                before[index].total_brn_burned
                    + if index == 1 {
                        params.verifier_stake_amount
                    } else {
                        0
                    }
            );
        }
        let status = client
            .rpc_call(
                "verification_status",
                json!({"account":addresses[4].as_str()}),
            )
            .await
            .unwrap();
        assert_eq!(status["status"], "revoked");
        assert_eq!(status["challenge_active"], false);
    }
    assert!(
        !clients[0]
            .process(&serde_json::to_string(&old_payment).unwrap())
            .await
            .unwrap()
            .accepted
    );
    let now = Timestamp::new(clock.load(Ordering::SeqCst));
    let target = burst_crypto::keypair_from_seed(&[4; 32]);
    let tx = Transaction::Burn(build_burn_tx(&addresses[4], &addresses[0], BRN_UNIT, now).unwrap());
    let forbidden = signed_block(&clients[0], &target, tx, params).await;
    assert_eq!(Some(forbidden.payment_epoch), epoch);
    assert!(
        !clients[0]
            .process(&serde_json::to_string(&forbidden).unwrap())
            .await
            .unwrap()
            .accepted
    );
    let challenger = burst_crypto::keypair_from_seed(&[0; 32]);
    let pending = clients[0]
        .rpc_call("account_pending", json!({"account":addresses[0].as_str()}))
        .await
        .unwrap();
    let reward = pending["pending"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["hash"] == reward_source.to_string())
        .expect("challenger bounty is missing")["amount"]
        .as_str()
        .unwrap()
        .parse::<u128>()
        .unwrap();
    assert!(reward > 0);
    let tx = Transaction::Receive(
        build_receive_tx(&addresses[0], reward_source.into_tx_hash(), reward, now).unwrap(),
    );
    submit(
        clients,
        &signed_block(&clients[0], &challenger, tx, params).await,
    )
    .await;
    let available = {
        let batch = stores[0].tx_begin_write().unwrap();
        let state = crate::payment_transition::restore(&batch, params).unwrap();
        let wallet = state.brn.get_wallet(&addresses[0]).unwrap();
        assert_eq!(wallet.total_staked, 0);
        let balance = state.brn.compute_balance(wallet, now);
        assert_eq!(
            balance,
            wallet
                .spendable_balance_checked(&state.brn.rate_history, now)
                .unwrap()
        );
        balance
    };
    let tx =
        Transaction::Burn(build_burn_tx(&addresses[0], &addresses[2], available, now).unwrap());
    let refunded = signed_block(&clients[0], &challenger, tx, params).await;
    assert!(refunded.release_sources.is_empty());
    assert_eq!(Some(refunded.payment_epoch), epoch);
    submit(clients, &refunded).await;
}
