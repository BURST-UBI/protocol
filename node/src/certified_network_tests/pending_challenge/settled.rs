use super::*;
use burst_store::{account::AccountStore, meta::MetaStore};

pub(super) async fn run(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    params: &ProtocolParams,
    addresses: &[WalletAddress],
    clock: &Arc<AtomicU64>,
    voted_at: Timestamp,
    mode: ChallengeMode,
) {
    let honest = burst_crypto::keypair_from_seed(&[2; 32]);
    let tx =
        Transaction::Burn(build_burn_tx(&addresses[2], &addresses[0], BRN_UNIT, voted_at).unwrap());
    let old_payment = signed_block(&clients[0], &honest, tx, params).await;

    // All votes and the final pre-settlement payment are cemented first. A
    // later agreed cut, not arrival of the last vote, authorizes revocation.
    let settled_at = Timestamp::new(voted_at.as_secs() + 2);
    clock.store(settled_at.as_secs(), Ordering::SeqCst);
    let mut agreed_epoch = None;
    let mut reward_source = None;
    for (client, store) in clients.iter().zip(stores) {
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let account = store.account_store().get_account(&addresses[4]).unwrap();
                if account.state == mode.settled_state() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("upheld challenge was not installed at its certified boundary");
        let authority = crate::payment_authority::PaymentAuthority::read(store)
            .unwrap()
            .unwrap();
        assert_eq!(authority.roster.total_humans(), 4);
        assert_eq!(authority.roster.weight(&addresses[4]), 0);
        assert_ne!(authority.epoch, old_payment.payment_epoch);
        if let Some(expected) = agreed_epoch {
            assert_eq!(authority.epoch, expected);
        } else {
            agreed_epoch = Some(authority.epoch);
        }
        let events = crate::checkpoint_store::chain::load(store, NetworkId::Dev, params).unwrap();
        let outcome = events.last().unwrap();
        assert!(matches!(
            outcome,
            crate::checkpoint_store::chain::CertifiedEvent::Outcome(_)
        ));
        assert_eq!(outcome.certificate().checkpoint.hash(), authority.epoch);
        let history = crate::confirmed_history::ConfirmedHistory::read_certified(
            store,
            NetworkId::Dev,
            outcome.certificate().checkpoint.genesis,
            outcome.certificate().checkpoint.cutoff,
            outcome.heads(),
            &[],
        )
        .unwrap();
        let replay = crate::confirmed_replay::ConfirmedReplay::replay_with_events(
            &history,
            NetworkId::Dev,
            params,
            &events,
        )
        .unwrap();
        let source = history
            .replay_blocks()
            .iter()
            .filter(|block| {
                block.block_type == BlockType::VerificationVote && block.timestamp == voted_at
            })
            .max_by_key(|block| *block.hash.as_bytes())
            .unwrap()
            .hash;
        if let Some(expected) = reward_source {
            assert_eq!(source, expected);
        } else {
            reward_source = Some(source);
        }
        let live = {
            let batch = store.tx_begin_write().unwrap();
            crate::payment_transition::restore(&batch, params).unwrap()
        };
        for address in addresses {
            assert_eq!(
                bincode::serialize(&store.account_store().get_account(address).unwrap()).unwrap(),
                bincode::serialize(&replay.account(address).unwrap()).unwrap(),
            );
            assert_eq!(
                bincode::serialize(&live.trst.get_portfolio(address)).unwrap(),
                bincode::serialize(&replay.portfolio(address)).unwrap(),
            );
        }
        let status = client
            .rpc_call(
                "verification_status",
                json!({"account":addresses[4].as_str()}),
            )
            .await
            .unwrap();
        assert_eq!(status["status"], mode.settled_status());
        assert_eq!(store.meta_store().payment_epoch().unwrap(), authority.epoch);
    }

    let rejected = clients[0]
        .process(&serde_json::to_string(&old_payment).unwrap())
        .await
        .unwrap();
    assert!(
        !rejected.accepted,
        "pre-settlement payment crossed the closed epoch"
    );
    let revoked = burst_crypto::keypair_from_seed(&[4; 32]);
    let tx = Transaction::Burn(
        build_burn_tx(&addresses[4], &addresses[0], BRN_UNIT, settled_at).unwrap(),
    );
    let forbidden = signed_block(&clients[0], &revoked, tx, params).await;
    assert_eq!(Some(forbidden.payment_epoch), agreed_epoch);
    let rejected = clients[0]
        .process(&serde_json::to_string(&forbidden).unwrap())
        .await
        .unwrap();
    assert!(
        !rejected.accepted,
        "challenged wallet retained payment rights"
    );
    assert!(rejected
        .detail
        .as_deref()
        .unwrap()
        .contains("verified wallet"));
    let tx = Transaction::Burn(
        build_burn_tx(&addresses[2], &addresses[0], BRN_UNIT, settled_at).unwrap(),
    );
    let payment = signed_block(&clients[0], &honest, tx, params).await;
    assert_eq!(Some(payment.payment_epoch), agreed_epoch);
    submit(clients, &payment).await;

    let source = reward_source.unwrap();
    for seed in [0u8, 2, 3] {
        let address = &addresses[seed as usize];
        let key = burst_crypto::keypair_from_seed(&[seed; 32]);
        let pending = clients[0]
            .rpc_call("account_pending", json!({"account":address.as_str()}))
            .await
            .unwrap();
        let reward = pending["pending"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["hash"] == source.to_string());
        let no_bounty = mode == ChallengeMode::ReverifiedInactivity && seed == 0;
        if no_bounty {
            assert!(reward.is_none(), "inactivity cannot mint a fraud bounty");
        } else {
            let reward = reward.expect("upheld challenge reward missing")["amount"]
                .as_str()
                .unwrap()
                .parse::<u128>()
                .unwrap();
            assert!(reward > 0);
            let tx = Transaction::Receive(
                build_receive_tx(address, source.into_tx_hash(), reward, settled_at).unwrap(),
            );
            let claim = signed_block(&clients[0], &key, tx, params).await;
            // The honest payment above already acknowledged verifier 2's refund.
            assert_eq!(claim.release_sources.is_empty(), seed == 2);
            submit(clients, &claim).await;
        }

        let available = {
            let batch = stores[0].tx_begin_write().unwrap();
            let state = crate::payment_transition::restore(&batch, params).unwrap();
            let wallet = state.brn.get_wallet(address).unwrap();
            assert_eq!(wallet.total_staked, 0);
            let balance = state.brn.compute_balance(wallet, settled_at);
            let spendable = wallet
                .spendable_balance_checked(&state.brn.rate_history, settled_at)
                .unwrap();
            assert_eq!(
                balance - spendable,
                if no_bounty {
                    params.challenge_stake_amount
                } else {
                    0
                }
            );
            balance
        };
        let tx = Transaction::Burn(
            build_burn_tx(address, &addresses[1], available, settled_at).unwrap(),
        );
        let spend = signed_block(&clients[0], &key, tx, params).await;
        assert_eq!(spend.release_sources.is_empty(), !no_bounty);
        assert_eq!(Some(spend.payment_epoch), agreed_epoch);
        submit(clients, &spend).await;
    }
}
