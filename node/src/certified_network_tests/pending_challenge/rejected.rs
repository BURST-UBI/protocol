use super::*;

pub(super) async fn run(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    params: &ProtocolParams,
    addresses: &[WalletAddress],
    context: BlockHash,
    initial_sources: [BlockHash; 2],
    at: Timestamp,
) {
    let before = clients[0]
        .rpc_call("brn_components", json!({"account":addresses[0].as_str()}))
        .await
        .unwrap();
    let challenger_burned: u128 = before["total_burned"].as_str().unwrap().parse().unwrap();
    let mut sources = initial_sources.to_vec();
    for seed in [2, 3] {
        let key = burst_crypto::keypair_from_seed(&[seed; 32]);
        let tx = Transaction::VerificationVote(
            build_verification_vote_tx(
                &addresses[seed as usize],
                &addresses[4],
                context,
                if seed == 3 { 2 } else { 1 },
                params.verifier_stake_amount,
                at,
            )
            .unwrap(),
        );
        let block = signed_block(&clients[0], &key, tx, params).await;
        submit(clients, &block).await;
        sources.push(block.hash);
    }
    let outcome = *sources[1..].iter().max().unwrap();
    sources.sort_unstable();
    for client in clients {
        let status = client
            .rpc_call(
                "verification_status",
                json!({"account":addresses[4].as_str()}),
            )
            .await
            .unwrap();
        assert_eq!(status["status"], "verified");
        assert_eq!(status["challenge_active"], false);
        assert_eq!(status["verifier_votes"].as_array().unwrap().len(), 3);
        for (seed, address) in addresses.iter().enumerate().take(4) {
            let components = client
                .rpc_call("brn_components", json!({"account":address.as_str()}))
                .await
                .unwrap();
            assert_eq!(components["total_staked"], "0");
            if seed == 0 {
                assert_eq!(
                    components["total_burned"],
                    (challenger_burned + params.challenge_stake_amount).to_string()
                );
                assert!(components["release_sources"].as_array().unwrap().is_empty());
            } else if seed == 3 {
                assert_eq!(
                    components["total_burned"],
                    (2 * params.verifier_stake_amount).to_string()
                );
            }
        }
    }
    let winner = burst_crypto::keypair_from_seed(&[1; 32]);
    let tx = Transaction::Burn(build_burn_tx(&addresses[1], &addresses[4], BRN_UNIT, at).unwrap());
    let spend = signed_block(&clients[0], &winner, tx, params).await;
    assert_eq!(spend.release_sources, sources);
    let mut incomplete = spend.clone();
    incomplete
        .release_sources
        .retain(|hash| *hash != initial_sources[0]);
    incomplete.hash = incomplete.compute_hash();
    let incomplete = sign_state_block(incomplete, &winner.private);
    let rejected = clients[0]
        .process(&serde_json::to_string(&incomplete).unwrap())
        .await
        .unwrap();
    assert!(
        !rejected.accepted,
        "an incomplete challenge release proof was accepted"
    );
    submit(clients, &spend).await;
    let tx = Transaction::Receive(
        build_receive_tx(
            &addresses[1],
            outcome.into_tx_hash(),
            params.verifier_stake_amount / 2,
            at,
        )
        .unwrap(),
    );
    submit(
        clients,
        &signed_block(&clients[0], &winner, tx, params).await,
    )
    .await;
    let target = burst_crypto::keypair_from_seed(&[4; 32]);
    let tx = Transaction::Burn(build_burn_tx(&addresses[4], &addresses[0], BRN_UNIT, at).unwrap());
    submit(
        clients,
        &signed_block(&clients[0], &target, tx, params).await,
    )
    .await;

    let mut expected = None;
    for store in stores {
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
        assert!(replay.account(&addresses[4]).unwrap().state.is_verified());
        assert_eq!(
            replay.account(&addresses[4]).unwrap().total_brn_burned,
            3 * BRN_UNIT
        );
        assert_eq!(
            replay.account(&addresses[1]).unwrap().trst_balance,
            11 * BRN_UNIT
        );
        assert_eq!(
            replay.verification(&addresses[4]).unwrap().phase,
            burst_verification::state::VerificationPhase::Verified
        );
        let projection = bincode::serialize(
            &addresses
                .iter()
                .map(|address| replay.account(address))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        if let Some(expected) = &expected {
            assert_eq!(&projection, expected);
        } else {
            expected = Some(projection);
        }
    }
}
