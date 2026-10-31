use super::governance_content::{launch, wait_phase};
use super::*;
use burst_transactions::governance::GovernanceVote;

mod mixed;

async fn confirmed(clients: &[NodeClient], hash: &str) {
    for client in clients {
        client
            .wait_confirmed(hash, Duration::from_secs(25))
            .await
            .unwrap();
    }
}

async fn transact(clients: &[NodeClient], signing: &tokio::sync::Mutex<()>, signer: &KeyPair, tx: Transaction) -> String {
    let _signing = signing.lock().await;
    let submitted = clients[0]
        .submit_signed(signer, &tx, TxHash::ZERO)
        .await
        .unwrap_or_else(|error| panic!("signed {tx:?} failed: {error}"));
    confirmed(clients, &submitted.hash).await;
    submitted.hash
}

async fn wait_roster_epoch(clients: &[NodeClient], previous: &str) {
    let mut observed = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            observed.clear();
            for client in clients {
                let params = client.rpc_call("params_hash", json!({})).await.unwrap();
                observed.push(params["payment_epoch"].as_str().unwrap().to_owned());
            }
            if observed[0] != previous && observed.iter().all(|epoch| epoch == &observed[0]) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "verified human did not enter a shared installed roster: {observed:?}"
    );
}

async fn check(clients: &[NodeClient], hash: &str) {
    wait_phase(clients, hash, "Activated").await;
    for client in clients {
        let proposal = client
            .rpc_call("governance_proposal_info", json!({"hash":hash}))
            .await
            .unwrap();
        assert_eq!(
            proposal["votes_yea"], 2,
            "delegated vote missing: {proposal}"
        );
        assert_eq!(
            proposal["votes_nay"], 1,
            "direct override missing: {proposal}"
        );
        assert_eq!(proposal["votes_abstain"], 0);
        assert_eq!(proposal["phase_projected"], false);
        let constitution = client.rpc_call("consti", json!({})).await.unwrap();
        assert_eq!(constitution["version"], 1);
        assert_eq!(
            constitution["articles"].as_array().unwrap().last().unwrap()["text"],
            "Each verified human has one vote."
        );
        let owner = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[0; 32]).public);
        let head = client.signing_state(&owner).await.unwrap().head;
        client
            .wait_confirmed(&head.to_string(), Duration::from_secs(25))
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn independent_humans_keep_delegated_tally_after_restart_and_fresh_catchup() {
    governance_journey(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_multi_human_ballot_replays_at_its_cut_without_later_revocation() {
    governance_journey(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit capped real-clock mixed governance qualification"]
async fn sustained_real_clock_payments_cross_ballot_counting_activation_and_recovery() {
    governance_journey(true, true).await;
}

async fn governance_journey(check_catchup: bool, mixed: bool) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_|
            tracing_subscriber::EnvFilter::new("burst_node::node=debug,burst_node::bootstrap=debug,burst_node::checkpoint_service=debug,burst_node::checkpoint_sync=debug")))
        .with_test_writer()
        .try_init();
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let first = port();
    let configs: Vec<_> = (0..4)
        .map(|index| {
            let key_file = directory.path().join(format!("representative-{index}.key"));
            std::fs::write(&key_file, hex::encode([index as u8; 32])).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
            }
            NodeConfig {
                network: NetworkId::Dev,
                data_dir: directory.path().join(format!("ballot-{index}")),
                port: if index == 0 { first } else { port() },
                rpc_port: port(),
                enable_upnp: false,
                enable_faucet: false,
                representative_key_file: Some(key_file),
                bootstrap_peers: if index == 0 {
                    vec![]
                } else {
                    vec![format!("127.0.0.1:{first}")]
                },
                params: ProtocolParams {
                    min_work_difficulty: 0,
                    governance_proposal_duration_secs: 10,
                    governance_exploration_duration_secs: 30,
                    governance_cooldown_duration_secs: 1,
                    governance_propagation_buffer_secs: 1,
                    governance_proposal_endorsements: 1,
                    governance_proposal_cost: 5,
                    // These short phases and lower approval threshold belong only to this fixture.
                    consti_supermajority_bps: 6000,
                    consti_quorum_bps: 10000,
                    ..ProtocolParams::burst_defaults()
                },
                ..NodeConfig::default()
            }
        })
        .collect();
    let mut nodes = vec![
        launch(configs[0].clone()).await,
        launch(configs[1].clone()).await,
        launch(configs[2].clone()).await,
    ];
    let clients: Vec<_> = configs
        .iter()
        .map(|config| NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap())
        .collect();
    let initial = clients[..3].to_vec();
    let counting_offset = configs[0].params.governance_proposal_duration_secs
        + configs[0].params.governance_exploration_duration_secs
        + configs[0].params.governance_propagation_buffer_secs;
    let traffic_params = configs[0].params.clone();
    let mut journey = tokio::spawn(async move {
        let signing = Arc::new(tokio::sync::Mutex::new(()));
        for client in &initial {
            loop {
                if client
                    .rpc_call("peers", json!({}))
                    .await
                    .is_ok_and(|value| value["count"].as_u64().unwrap_or(0) > 0)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let keys: Vec<_> = (0..3)
            .map(|seed| burst_crypto::keypair_from_seed(&[seed; 32]))
            .collect();
        let addresses: Vec<_> = keys
            .iter()
            .map(|key| burst_crypto::derive_address(&key.public))
            .collect();
        for index in 1..3 {
            let previous = initial[0].rpc_call("params_hash", json!({})).await.unwrap();
            transact(
                &initial,
                &signing,
                &keys[0],
                Transaction::Endorse(
                    build_endorse_tx(&addresses[0], &addresses[index], 1, Timestamp::now())
                        .unwrap(),
                ),
            )
            .await;
            // The new human changes certified voting authority. Fund that
            // account only after all participants have installed the new epoch.
            wait_roster_epoch(&initial, previous["payment_epoch"].as_str().unwrap()).await;
            let source = transact(
                &initial,
                &signing,
                &keys[0],
                Transaction::Burn(
                    build_burn_tx(&addresses[0], &addresses[index], 1, Timestamp::now()).unwrap(),
                ),
            )
            .await;
            transact(
                &initial,
                &signing,
                &keys[index],
                Transaction::Receive(
                    build_receive_tx(
                        &addresses[index],
                        burst_wallet_core::signed_client::parse_hash(&source)
                            .unwrap()
                            .into_tx_hash(),
                        1,
                        Timestamp::now(),
                    )
                    .unwrap(),
                ),
            )
            .await;
            transact(
                &initial,
                &signing,
                &keys[index],
                Transaction::Delegate(
                    build_delegate_tx(&addresses[index], &addresses[0], Timestamp::now()).unwrap(),
                ),
            )
            .await;
        }
        let traffic = mixed.then(|| mixed::Traffic::start(&initial, traffic_params, signing.clone()));
        if let Some(traffic) = &traffic { traffic.progress(0).await; traffic.phase(1); }
        let proposal_guard = signing.lock().await;
        let proposal = initial[0]
            .propose_content_signed(
                &keys[0],
                &burst_governance::ProposalContent::ConstitutionalAmendment {
                    title: "Voting".into(),
                    text: "Each verified human has one vote.".into(),
                },
            )
            .await
            .unwrap();
        drop(proposal_guard);
        let id = burst_wallet_core::signed_client::parse_hash(&proposal.hash)
            .unwrap()
            .into_tx_hash();
        let details = initial[0]
            .rpc_call("governance_proposal_info", json!({"hash":proposal.hash}))
            .await
            .unwrap();
        let counting_cut = details["created_at"].as_u64().unwrap() + counting_offset;
        let endorsement_guard = signing.lock().await;
        let endorsement = initial[0]
            .endorse_proposal_signed(&keys[0], id, 5)
            .await
            .unwrap();
        confirmed(&initial, &endorsement.hash).await;
        drop(endorsement_guard);
        wait_phase(&initial, &proposal.hash, "Exploration").await;
        for client in &initial {
            let view = client
                .rpc_call("governance_proposal_info", json!({"hash":proposal.hash}))
                .await
                .unwrap();
            assert_eq!(
                view["phase_projected"], true,
                "opening the voting window is a projection, not a ledger write"
            );
        }
        if let Some(traffic) = &traffic { traffic.progress(1).await; traffic.phase(2); }
        for (index, vote) in [(0, GovernanceVote::Yea), (2, GovernanceVote::Nay)] {
            transact(
                &initial,
                &signing,
                &keys[index],
                Transaction::GovernanceVote(
                    build_governance_vote_tx(&addresses[index], id, vote, Timestamp::now())
                        .unwrap(),
                ),
            )
            .await;
        }
        if let Some(traffic) = &traffic { traffic.progress(2).await; }
        check(&initial, &proposal.hash).await;
        if let Some(traffic) = traffic { traffic.phase(3); traffic.finish().await; }
        // Revocation remains available, but cannot rewrite this closed ballot.
        transact(
            &initial,
            &signing,
            &keys[1],
            Transaction::RevokeDelegation(
                build_revoke_delegation_tx(&addresses[1], Timestamp::now()).unwrap(),
            ),
        )
        .await;
        check(&initial, &proposal.hash).await;
        (proposal.hash, counting_cut)
    });
    let result = tokio::time::timeout(Duration::from_secs(120), &mut journey).await;
    if !matches!(result, Ok(Ok(_))) {
        journey.abort();
        for (stop, _) in &nodes {
            stop.shutdown();
        }
        for (_, task) in nodes {
            let _ = tokio::time::timeout(Duration::from_secs(20), task).await;
        }
        let retained = directory.keep();
        eprintln!("failed ballot fixture retained at {}", retained.display());
        panic!("multi-human ballot journey failed: {result:?}");
    }
    let (hash, counting_cut) = result.unwrap().unwrap();
    let late = if check_catchup {
        eprintln!("three representatives activated and retained the closed tally; starting fresh-node catch-up");
        nodes.push(launch(configs[3].clone()).await);
        let late_client = clients[3].clone();
        let late_hash = hash.clone();
        let mut late_task = tokio::spawn(async move {
            check(&[late_client], &late_hash).await;
        });
        let result = tokio::time::timeout(Duration::from_secs(70), &mut late_task).await;
        if result.is_err() {
            late_task.abort();
        }
        Some(result)
    } else {
        None
    };
    for (stop, _) in &nodes {
        stop.shutdown();
    }
    for (_, task) in nodes {
        tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .unwrap()
            .unwrap();
    }
    for config in &configs[..3] {
        assert_counting_cut(config, &hash, counting_cut);
    }
    if let Some(late) = late {
        late.unwrap().unwrap();
    }
    let reopened = launch(configs[0].clone()).await;
    let recovery = tokio::time::timeout(Duration::from_secs(30), check(&clients[..1], &hash)).await;
    reopened.0.shutdown();
    tokio::time::timeout(Duration::from_secs(20), reopened.1)
        .await
        .unwrap()
        .unwrap();
    recovery.unwrap();
}

fn assert_counting_cut(config: &NodeConfig, hash: &str, cutoff: u64) {
    use crate::confirmed_history::ConfirmedHistory;
    use crate::confirmed_replay::ConfirmedReplay;
    let store = burst_store_lmdb::LmdbStore::open(&config.data_dir, 32, 256 << 20).unwrap();
    let mut events =
        crate::checkpoint_store::chain::load(&store, NetworkId::Dev, &config.params).unwrap();
    events.retain(|event| event.certificate().checkpoint.cutoff < cutoff);
    let parent_heads = events.last().map_or(
        &[][..],
        crate::checkpoint_store::chain::CertifiedEvent::heads,
    );
    let heads = ConfirmedHistory::heads_at(&store, cutoff).unwrap();
    let history = ConfirmedHistory::read(
        &store,
        NetworkId::Dev,
        crate::genesis_state::block(NetworkId::Dev, &config.params).hash,
        cutoff,
        &heads,
        parent_heads,
    )
    .unwrap();
    let id = burst_wallet_core::signed_client::parse_hash(hash)
        .unwrap()
        .into_tx_hash();
    let prepare = |history: &ConfirmedHistory| {
        ConfirmedReplay::replay_governance_candidate(
            history,
            NetworkId::Dev,
            &config.params,
            &events,
        )
        .unwrap()
        .ballot_at(history, id)
        .unwrap()
    };
    let prepared = prepare(&history);
    let proposal = prepared.next_proposal();
    assert_eq!(proposal.phase, burst_governance::GovernancePhase::Cooldown);
    assert_eq!(
        (
            proposal.exploration_votes_yea,
            proposal.exploration_votes_nay,
            proposal.exploration_votes_abstain,
            proposal.total_eligible_voters
        ),
        (2, 1, 0, 3)
    );
    let mut blocks = history.replay_blocks().to_vec();
    blocks.reverse();
    let detached = ConfirmedHistory::from_parts(
        NetworkId::Dev,
        history.genesis(),
        cutoff,
        &heads,
        parent_heads,
        blocks,
        history.proposal_witnesses().to_vec(),
    )
    .unwrap();
    assert_eq!(
        prepare(&detached).commitment().unwrap(),
        prepared.commitment().unwrap()
    );
    assert!(
        ConfirmedReplay::replay(&history, NetworkId::Dev, &config.params).is_err(),
        "ballot preparation must not enable ordinary checkpoint governance replay"
    );
}
