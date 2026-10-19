use super::*;
use burst_governance::proposal::AmendmentOp;

fn amendment(text: &str) -> burst_governance::ProposalContent {
    burst_governance::ProposalContent::ConstitutionalDiff {
        title: "共同の憲法".into(),
        operations: vec![
            AmendmentOp::ModifyArticle {
                article_number: 1,
                new_text: "人".into(),
            },
            AmendmentOp::RepealArticle { article_number: 2 },
            AmendmentOp::AddArticle {
                title: "🌍".into(),
                text: text.into(),
            },
        ],
    }
}

pub(super) async fn wait_phase(clients: &[NodeClient], hash: &str, phase: &str) {
    let mut observations = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let mut ready = true;
            observations.clear();
            for client in clients {
                let observed = client
                    .rpc_call("governance_proposal_info", json!({"hash":hash}))
                    .await;
                ready &= observed.as_ref().is_ok_and(|value| value["phase"] == phase);
                observations.push(format!("{observed:?}"));
            }
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    assert!(
        result.is_ok(),
        "proposal did not reach {phase}: {observations:?}"
    );
}

pub(super) async fn launch(
    config: NodeConfig,
) -> (
    Arc<crate::shutdown::ShutdownController>,
    tokio::task::JoinHandle<()>,
) {
    let mut node = BurstNode::new(config).await.unwrap();
    let stop = node.shutdown.clone();
    (
        stop,
        tokio::spawn(async move {
            node.start().await.unwrap();
            node.stop().await.unwrap();
        }),
    )
}

async fn check(client: &NodeClient, hash: &str, text: &str) {
    client
        .wait_confirmed(hash, Duration::from_secs(30))
        .await
        .unwrap();
    let proposal = client
        .rpc_call("governance_proposal_info", json!({"hash":hash}))
        .await
        .unwrap();
    let burst_governance::ProposalContent::ConstitutionalDiff { operations, .. } = amendment(text)
    else {
        unreachable!()
    };
    assert_eq!(
        proposal["amendment_operations"],
        serde_json::to_value(operations).unwrap()
    );
    assert!(proposal.get("amendment_text").is_none());
    assert_eq!(proposal["phase"], "Activated");
    let owner = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[0; 32]).public);
    let head = client.signing_state(&owner).await.unwrap().head;
    client
        .wait_confirmed(&head.to_string(), Duration::from_secs(30))
        .await
        .unwrap();
    let constitution = client.rpc_call("consti", json!({})).await.unwrap();
    assert_eq!(constitution["version"], 1);
    let articles = constitution["articles"].as_array().unwrap();
    assert_eq!(articles.len(), 3);
    assert_eq!(articles[0]["id"], "1");
    assert_eq!(articles[0]["text"], "人");
    assert_eq!(articles[1]["id"], "3");
    assert_eq!(articles[2]["id"], "4");
    assert_eq!(articles[2]["text"], text);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signed_constitution_diff_activates_over_tcp_and_survives_reopen_and_fresh_bootstrap() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let first = port();
    let mut configs = Vec::new();
    for index in 0..3 {
        configs.push(NodeConfig {
            network: NetworkId::Dev,
            data_dir: directory.path().join(format!("content-{index}")),
            port: if index == 0 { first } else { port() },
            rpc_port: port(),
            enable_upnp: false,
            enable_faucet: false,
            bootstrap_peers: if index == 0 {
                vec![]
            } else {
                vec![format!("127.0.0.1:{first}")]
            },
            params: ProtocolParams {
                min_work_difficulty: 0,
                governance_proposal_duration_secs: 5,
                governance_exploration_duration_secs: 20,
                governance_cooldown_duration_secs: 1,
                governance_propagation_buffer_secs: 1,
                governance_proposal_endorsements: 1,
                governance_proposal_cost: 5,
                ..ProtocolParams::burst_defaults()
            },
            ..NodeConfig::default()
        });
    }
    let mut nodes = vec![
        launch(configs[0].clone()).await,
        launch(configs[1].clone()).await,
    ];
    let clients: Vec<_> = configs
        .iter()
        .map(|config| NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap())
        .collect();
    let initial_clients = clients[..2].to_vec();
    let mut journey = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let mut ready = true;
                for client in &initial_clients {
                    ready &= client
                        .rpc_call("peers", json!({}))
                        .await
                        .is_ok_and(|value| value["count"].as_u64().unwrap_or(0) > 0);
                }
                if ready {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
        let text = "🌍".repeat(burst_governance::content::MAX_AMENDMENT_CHARS - 1);
        let content = amendment(&text);
        let keys = burst_crypto::keypair_from_seed(&[0; 32]);
        let submitted = initial_clients[0]
            .propose_content_signed(&keys, &content)
            .await
            .unwrap();
        let id = burst_wallet_core::signed_client::parse_hash(&submitted.hash)
            .unwrap()
            .into_tx_hash();
        let support = initial_clients[0]
            .endorse_proposal_signed(&keys, id, 5)
            .await
            .unwrap();
        for client in &initial_clients {
            client
                .wait_confirmed(&support.hash, Duration::from_secs(30))
                .await
                .unwrap();
        }
        wait_phase(&initial_clients, &submitted.hash, "Exploration").await;
        let vote = build_governance_vote_tx(
            &burst_crypto::derive_address(&keys.public),
            id,
            burst_transactions::governance::GovernanceVote::Yea,
            Timestamp::now(),
        )
        .unwrap();
        let voted = initial_clients[0]
            .submit_signed(&keys, &Transaction::GovernanceVote(vote), TxHash::ZERO)
            .await
            .unwrap();
        for client in &initial_clients {
            client
                .wait_confirmed(&voted.hash, Duration::from_secs(30))
                .await
                .unwrap();
        }
        wait_phase(&initial_clients, &submitted.hash, "Activated").await;
        for client in &initial_clients {
            check(client, &submitted.hash, &text).await;
        }
        (submitted.hash, text)
    });
    let result = tokio::time::timeout(Duration::from_secs(90), &mut journey).await;
    if !matches!(result, Ok(Ok(_))) {
        journey.abort();
        for (stop, _) in &nodes {
            stop.shutdown();
        }
        for (_, task) in nodes {
            let _ = task.await;
        }
        panic!("full proposal live submission failed: {result:?}");
    }
    let (hash, text) = result.unwrap().unwrap();
    nodes.push(launch(configs[2].clone()).await);
    let late = tokio::time::timeout(Duration::from_secs(60), async {
        wait_phase(&clients[2..], &hash, "Activated").await;
        check(&clients[2], &hash, &text).await;
    })
    .await;
    for (stop, _) in &nodes {
        stop.shutdown();
    }
    for (_, task) in nodes {
        tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .unwrap()
            .unwrap();
    }
    late.unwrap();

    for config in &configs {
        let store = burst_store_lmdb::LmdbStore::open(&config.data_dir, 32, 256 << 20).unwrap();
        let cutoff = Timestamp::now().as_secs();
        let heads = crate::confirmed_history::ConfirmedHistory::heads_at(&store, cutoff).unwrap();
        let history = crate::confirmed_history::ConfirmedHistory::read(
            &store,
            NetworkId::Dev,
            crate::genesis_state::block(NetworkId::Dev, &config.params).hash,
            cutoff,
            &heads,
            &[],
        )
        .unwrap();
        let proposal = burst_wallet_core::signed_client::parse_hash(&hash).unwrap();
        let bytes = history
            .proposal_content(&proposal)
            .expect("cemented proposal body missing from history");
        assert_eq!(
            burst_governance::ProposalContent::decode_content(bytes).unwrap(),
            amendment(&text)
        );
        let detached = crate::confirmed_history::ConfirmedHistory::from_parts(
            NetworkId::Dev,
            history.genesis(),
            cutoff,
            &heads,
            &[],
            history.replay_blocks().to_vec(),
            history.proposal_witnesses().to_vec(),
        )
        .unwrap();
        assert_eq!(detached.commitment(), history.commitment());
        assert_eq!(detached.proposal_content(&proposal), Some(bytes));
        let replayed =
            crate::governance_commit::ledger::replay::genesis_transcript(&detached, &config.params);
        let governance = replayed
            .governance
            .get_proposal(&proposal.into_tx_hash())
            .unwrap();
        assert_eq!(governance.content, amendment(&text));
        assert_eq!(
            governance.phase,
            burst_governance::GovernancePhase::Activated,
            "cemented history differs at {:?}: {:?}",
            config.data_dir,
            heads
        );
        assert_eq!(governance.exploration_votes_yea, 1);
        assert_eq!(replayed.params.params_hash(), config.params.params_hash());
        let constitution = replayed.constitution.get_constitution();
        assert_eq!(constitution.version, 1);
        assert_eq!(constitution.get_article(1).unwrap().text, "人");
        assert!(constitution.get_article(2).is_none());
        assert_eq!(constitution.get_article(4).unwrap().text, text);
    }

    let reopened = launch(configs[0].clone()).await;
    let recovery =
        tokio::time::timeout(Duration::from_secs(30), check(&clients[0], &hash, &text)).await;
    reopened.0.shutdown();
    tokio::time::timeout(Duration::from_secs(20), reopened.1)
        .await
        .unwrap()
        .unwrap();
    recovery.unwrap();
}
