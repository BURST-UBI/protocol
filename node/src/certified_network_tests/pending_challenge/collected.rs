use super::*;

pub(super) async fn run(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    params: &ProtocolParams,
    addresses: &[WalletAddress],
    context: BlockHash,
    at: Timestamp,
) {
    for seed in [2, 3] {
        let key = burst_crypto::keypair_from_seed(&[seed; 32]);
        let tx = Transaction::VerificationVote(
            build_verification_vote_tx(
                &addresses[seed as usize],
                &addresses[4],
                context,
                2,
                params.verifier_stake_amount,
                at,
            )
            .unwrap(),
        );
        submit(clients, &signed_block(&clients[0], &key, tx, params).await).await;
    }
    // The vote set is complete, but no agreed revocation boundary exists yet.
    // A local last-packet event must not revoke or release stake.
    let key = burst_crypto::keypair_from_seed(&[4; 32]);
    let tx = Transaction::Burn(build_burn_tx(&addresses[4], &addresses[0], BRN_UNIT, at).unwrap());
    submit(clients, &signed_block(&clients[0], &key, tx, params).await).await;
    let mut projection = None;
    for (client, store) in clients.iter().zip(stores) {
        let status = client
            .rpc_call(
                "verification_status",
                json!({"account":addresses[4].as_str()}),
            )
            .await
            .unwrap();
        assert_eq!(status["status"], "verified");
        assert_eq!(status["challenge_active"], true);
        assert_eq!(status["verifier_votes"].as_array().unwrap().len(), 3);
        for (index, address) in addresses.iter().enumerate().take(4) {
            let components = client
                .rpc_call("brn_components", json!({"account":address.as_str()}))
                .await
                .unwrap();
            assert_eq!(
                components["total_staked"],
                if index == 0 {
                    params.challenge_stake_amount
                } else {
                    params.verifier_stake_amount
                }
                .to_string()
            );
        }
        let heads =
            crate::confirmed_history::ConfirmedHistory::heads_at(store, at.as_secs()).unwrap();
        let history = crate::confirmed_history::ConfirmedHistory::read(
            store,
            NetworkId::Dev,
            crate::genesis_state::block(NetworkId::Dev, params).hash,
            at.as_secs(),
            &heads,
            &[],
        )
        .unwrap();
        let certificates =
            crate::checkpoint_store::chain::load(store, NetworkId::Dev, params).unwrap();
        let replay = crate::confirmed_replay::ConfirmedReplay::replay_with_events(
            &history,
            NetworkId::Dev,
            params,
            &certificates,
        )
        .unwrap();
        let target = replay.account(&addresses[4]).unwrap();
        assert!(target.state.is_verified());
        assert_eq!(target.total_brn_burned, 3 * BRN_UNIT);
        let current = bincode::serialize(
            &addresses
                .iter()
                .map(|address| replay.account(address))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        if let Some(expected) = &projection {
            assert_eq!(expected, &current);
        } else {
            projection = Some(current);
        }
    }
}
