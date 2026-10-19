//! Replay payments, bootstrap verification and pending endorsements on a producer,
//! observer and fresh joiner.
//! One representative signs; this exercises replay/catch-up, not BFT safety.
use burst_node::{
    confirmed_history::{ConfirmedHistory, HistoryHead},
    confirmed_replay::ConfirmedReplay,
    BurstNode, NodeConfig, ShutdownController,
};
use burst_store::{account::AccountStore, block::BlockStore};
use burst_store_lmdb::LmdbStore;
use burst_types::{NetworkId, ProtocolParams, Timestamp, TxHash, WalletAddress, WalletState};
use burst_wallet_core::{
    signed_client::parse_hash,
    transaction_builder::{build_change_rep_tx, build_endorse_tx, build_merge_tx},
    NodeClient,
};
use serde_json::json;
use std::{sync::Arc, time::Duration};

struct RunningNode {
    client: NodeClient,
    store: Arc<LmdbStore>,
    shutdown: Arc<ShutdownController>,
    task: tokio::task::JoinHandle<()>,
}

fn port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn launch(config: NodeConfig) -> RunningNode {
    let client = NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap();
    let mut node = BurstNode::new(config).await.unwrap();
    let store = node.store.clone();
    let shutdown = node.shutdown.clone();
    let task = tokio::spawn(async move {
        node.start().await.unwrap();
        node.stop().await.unwrap();
    });
    RunningNode {
        client,
        store,
        shutdown,
        task,
    }
}

async fn confirmed(nodes: &[RunningNode], hash: &str) {
    confirmed_with_timeout(nodes, hash, Duration::from_secs(20)).await;
}

async fn confirmed_with_timeout(nodes: &[RunningNode], hash: &str, timeout: Duration) {
    for (index, node) in nodes.iter().enumerate() {
        node.client
            .wait_confirmed(hash, timeout)
            .await
            .unwrap_or_else(|error| panic!("node {index} did not confirm {hash}: {error}"));
    }
}

fn replay_cut(
    nodes: &[RunningNode],
    creator: &WalletAddress,
    params: &ProtocolParams,
) -> Vec<ConfirmedReplay> {
    let cutoff = Timestamp::now().as_secs();
    let mut commitment = None;
    nodes
        .iter()
        .map(|node| {
            let genesis = node
                .store
                .block_store()
                .block_at_height(creator, 1)
                .unwrap()
                .unwrap();
            let accounts = node.store.account_store().iter_accounts().unwrap();
            let heads: Vec<_> = accounts
                .iter()
                .filter(|account| !account.head.is_zero())
                .map(|account| {
                    assert_eq!(account.block_count, account.confirmation_height);
                    HistoryHead {
                        account: account.address.clone(),
                        height: account.confirmation_height,
                        hash: account.head,
                    }
                })
                .collect();
            let history =
                ConfirmedHistory::read(&node.store, NetworkId::Dev, genesis, cutoff, &heads, &[])
                    .unwrap();
            if let Some(expected) = commitment {
                assert_eq!(history.commitment(), expected);
            }
            commitment = Some(history.commitment());
            let replay = ConfirmedReplay::replay(&history, NetworkId::Dev, params).unwrap();
            let brn = burst_brn::BrnEngine::load_from_store(&node.store.brn_store()).unwrap();
            for account in accounts {
                assert_eq!(
                    bincode::serialize(&account).unwrap(),
                    bincode::serialize(replay.account(&account.address).unwrap()).unwrap()
                );
                if let Some(wallet) = brn.get_wallet(&account.address) {
                    assert_eq!(
                        replay.brn_balance(&account.address).unwrap(),
                        brn.compute_balance_checked(wallet, Timestamp::new(cutoff))
                            .unwrap()
                    );
                }
            }
            replay
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn payments_and_bootstrap_replay_identically_after_wire_delivery_and_fresh_catchup() {
    payment_replay_journey(false).await;
}

#[cfg(feature = "experimental-testnet")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn certified_selection_catches_up_after_payment_replay() {
    assert!(burst_verification::checkpoint_selection_enabled(
        NetworkId::Dev
    ));
    payment_replay_journey(true).await;
}

async fn payment_replay_journey(certified_catchup: bool) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "burst_node::checkpoint_sync=debug,burst_node::checkpoint_service=debug".into()
            }),
        )
        .with_test_writer()
        .try_init();
    let directory = tempfile::tempdir().unwrap();
    let p2p = [port(), port(), port()];
    let mut configs = Vec::new();
    for index in 0..3 {
        let mut config = NodeConfig {
            network: NetworkId::Dev,
            data_dir: directory.path().join(format!("node{index}")),
            port: p2p[index],
            rpc_port: port(),
            enable_upnp: false,
            enable_representative: index == 0,
            bootstrap_peers: vec![format!("127.0.0.1:{}", p2p[if index == 0 { 1 } else { 0 }])],
            ..NodeConfig::default()
        };
        config.params.min_work_difficulty = 0;
        config.params.endorsement_threshold = 1;
        config.params.endorsement_burn_amount = 2;
        config.params.num_verifiers = 1;
        // Creator plus recipient end bootstrap before normal verifier selection.
        config.params.bootstrap_exit_threshold = 2;
        configs.push(config);
    }
    let params = configs[0].params.clone();
    let mut nodes = vec![
        launch(configs[0].clone()).await,
        launch(configs[1].clone()).await,
    ];
    let result = tokio::time::timeout(Duration::from_secs(300), async {
        for node in &nodes {
            loop {
                if node
                    .client
                    .rpc_call("peers", json!({}))
                    .await
                    .is_ok_and(|reply| reply["count"].as_u64().unwrap_or(0) >= 1)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        let creator_keys = burst_crypto::keypair_from_seed(&[0; 32]);
        let recipient_keys = burst_crypto::keypair_from_seed(&[44; 32]);
        let creator = burst_crypto::derive_address(&creator_keys.public);
        let recipient = burst_crypto::derive_address(&recipient_keys.public);
        let mut inputs = Vec::new();
        for amount in [20, 30] {
            let burn = nodes[0]
                .client
                .pay_signed(&creator_keys, &creator, amount)
                .await
                .unwrap();
            assert!(burn.accepted);
            confirmed(&nodes, &burn.hash).await;
            let receive = nodes[0]
                .client
                .receive_signed(&creator_keys, &burn.hash)
                .await
                .unwrap();
            assert!(receive.accepted);
            confirmed(&nodes, &receive.hash).await;
            inputs.push(parse_hash(&receive.hash).unwrap().into_tx_hash());
        }
        let merge = nodes[0]
            .client
            .submit_signed(
                &creator_keys,
                &burst_transactions::Transaction::Merge(
                    build_merge_tx(&creator, inputs, Timestamp::now()).unwrap(),
                ),
                TxHash::ZERO,
            )
            .await
            .unwrap();
        assert!(merge.accepted);
        confirmed(&nodes, &merge.hash).await;
        let origin = parse_hash(&merge.hash).unwrap().into_tx_hash();
        let send = nodes[0]
            .client
            .transfer_signed(&creator_keys, &recipient, 15, origin)
            .await
            .unwrap();
        assert!(send.accepted);
        confirmed(&nodes, &send.hash).await;
        let receive = nodes[1]
            .client
            .receive_signed(&recipient_keys, &send.hash)
            .await
            .unwrap();
        assert!(receive.accepted);
        confirmed(&nodes, &receive.hash).await;
        let pending = nodes[0]
            .client
            .transfer_signed(&creator_keys, &recipient, 5, origin)
            .await
            .unwrap();
        assert!(pending.accepted);
        confirmed(&nodes, &pending.hash).await;

        for replay in replay_cut(&nodes, &creator, &params) {
            assert_eq!(replay.account(&creator).unwrap().trst_balance, 30);
            assert_eq!(replay.account(&recipient).unwrap().trst_balance, 15);
            assert_eq!(
                replay.account(&recipient).unwrap().state,
                WalletState::Unverified
            );
            assert_eq!(
                replay
                    .pending(
                        &recipient,
                        parse_hash(&pending.hash).unwrap().into_tx_hash()
                    )
                    .unwrap()
                    .amount,
                5
            );
            assert_eq!(
                replay
                    .merger_graph()
                    .get_merge(&origin)
                    .unwrap()
                    .total_amount,
                50
            );
        }

        // The newly verified person delegates to the sole online signer before
        // joining the electorate. No second representative key is cloned here.
        let delegate = nodes[1]
            .client
            .submit_signed(
                &recipient_keys,
                &burst_transactions::Transaction::ChangeRepresentative(
                    build_change_rep_tx(&recipient, &creator, Timestamp::now()).unwrap(),
                ),
                TxHash::ZERO,
            )
            .await
            .unwrap();
        confirmed(&nodes, &delegate.hash).await;
        let endorse = nodes[0]
            .client
            .rpc_call("genesis_endorse", json!({"account":recipient.as_str()}))
            .await
            .unwrap();
        let endorsement_hash = endorse["block_hash"].as_str().unwrap();
        confirmed(&nodes, endorsement_hash).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let burn = nodes[1]
            .client
            .pay_signed(&recipient_keys, &creator, 3)
            .await
            .unwrap();
        confirmed(&nodes, &burn.hash).await;
        let receipt = nodes[0]
            .client
            .receive_signed(&creator_keys, &burn.hash)
            .await
            .unwrap();
        confirmed(&nodes, &receipt.hash).await;
        let opt_in = nodes[1]
            .client
            .set_verifier_opt_in_signed(&recipient_keys, true)
            .await
            .unwrap();
        confirmed(&nodes, &opt_in.hash).await;
        for replay in replay_cut(&nodes, &creator, &params) {
            assert!(
                replay.eligible_verifiers().unwrap().is_empty(),
                "fresh opt-in does not waive verification age or BRN eligibility"
            );
            let roster = replay.human_roster().unwrap();
            assert_eq!(roster.total_humans(), 2);
            assert_eq!(roster.weight(&creator), 2);
            assert_eq!(roster.weight(&recipient), 0);
            assert!(replay
                .account(&recipient)
                .unwrap()
                .verifier_opted_in_at
                .is_some());
            assert_eq!(
                replay.account(&recipient).unwrap().state,
                WalletState::Verified
            );
        }
        let opt_out = nodes[1]
            .client
            .set_verifier_opt_in_signed(&recipient_keys, false)
            .await
            .unwrap();
        confirmed(&nodes, &opt_out.hash).await;

        let applicant =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[45; 32]).public);
        let before = nodes[1]
            .client
            .rpc_call("account_info", json!({"account": recipient.as_str()}))
            .await
            .unwrap();
        let underfunded = nodes[1]
            .client
            .submit_signed(
                &recipient_keys,
                &burst_transactions::Transaction::Endorse(
                    build_endorse_tx(&recipient, &applicant, 1, Timestamp::now()).unwrap(),
                ),
                TxHash::ZERO,
            )
            .await
            .unwrap_err();
        assert!(
            underfunded
                .to_string()
                .contains("insufficient endorsement burn"),
            "{underfunded}"
        );
        for node in &nodes {
            let after = node
                .client
                .rpc_call("account_info", json!({"account": recipient.as_str()}))
                .await
                .unwrap();
            for field in ["head", "total_brn_burned", "total_brn_staked"] {
                assert_eq!(
                    after[field], before[field],
                    "rejected endorsement changed {field}"
                );
            }
        }
        let normal_endorsement = nodes[1]
            .client
            .submit_signed(
                &recipient_keys,
                &burst_transactions::Transaction::Endorse(
                    build_endorse_tx(&recipient, &applicant, 2, Timestamp::now()).unwrap(),
                ),
                TxHash::ZERO,
            )
            .await
            .unwrap();
        assert!(normal_endorsement.accepted);
        confirmed(&nodes, &normal_endorsement.hash).await;

        nodes.push(launch(configs[2].clone()).await);
        // Loopback peers share the five-handshakes-per-minute IP limit. Fresh
        // catch-up must allow that window plus frontier discovery and block
        // pulls; already connected payment confirmations keep their 20s bound.
        confirmed_with_timeout(&nodes, &normal_endorsement.hash, Duration::from_secs(120)).await;
        confirmed(&nodes, &receipt.hash).await;
        for mut replay in replay_cut(&nodes, &creator, &params) {
            assert_eq!(replay.account(&creator).unwrap().trst_balance, 33);
            assert!(replay.eligible_verifiers().unwrap().is_empty());
            assert!(
                replay.prepare_selection(&applicant).is_err(),
                "insufficient confirmed eligibility must not select a smaller or provisional panel"
            );
            assert_eq!(replay.human_roster().unwrap().weight(&creator), 2);
            let account = replay.account(&recipient).unwrap();
            assert_eq!(account.state, WalletState::Verified);
            assert_eq!(account.total_brn_burned, 5);
            assert_eq!(account.trst_balance, 15);
            assert!(account.verifier_opted_in_at.is_none());
            assert_eq!(
                replay.verification(&recipient).unwrap().started_at,
                account.verified_at.unwrap()
            );
            let verification = replay.verification(&applicant).unwrap();
            assert_eq!(verification.endorsements.len(), 1);
            assert_eq!(verification.endorsements[0].endorser, recipient);
            assert_eq!(verification.endorsements[0].burn_amount, 2);
            assert!(verification.selected_verifiers.is_empty());
            assert!(replay.account(&applicant).is_none());
            assert_eq!(replay.brn_balance(&applicant).unwrap(), 0);
            assert_eq!(
                replay
                    .pending(
                        &recipient,
                        parse_hash(&pending.hash).unwrap().into_tx_hash()
                    )
                    .unwrap()
                    .amount,
                5
            );
        }
        for node in &nodes {
            let status = node
                .client
                .rpc_call(
                    "verification_status",
                    json!({"account": applicant.as_str()}),
                )
                .await
                .unwrap();
            assert_eq!(status["status"], "endorsing");
            assert_eq!(status["endorser_count"], 1);
            assert_eq!(
                status["selection_available"],
                cfg!(feature = "experimental-testnet")
            );
            assert!(status["selection_context"].is_null());
        }

        // Default artifacts exercise payment/bootstrap catch-up above. Certified
        // record catch-up requires the explicit experimental testnet build;
        // integration tests do not compile the node library with cfg(test).
        if !certified_catchup {
            return;
        }

        // The creator has sufficient age and BRN and a completed payment with
        // another verified human. Opting in supplies one eligible non-endorser.
        // A one-person panel is an explicit test-network parameter, not a
        // runtime reduction of the configured committee size.
        let creator_opt_in = nodes[0]
            .client
            .set_verifier_opt_in_signed(&creator_keys, true)
            .await
            .unwrap();
        confirmed(&nodes, &creator_opt_in.hash).await;
        use burst_consensus::checkpoint::CheckpointCertificate;
        use burst_consensus::checkpoint_round::Quorum;
        use burst_node::checkpoint_agreement::{self, Command, Outgoing};
        use burst_node::checkpoint_replay::{ReplayedCheckpoint, SigningClock};
        use burst_node::checkpoint_store::CertifiedCheckpoint;
        let heads: Vec<_> = nodes[0]
            .store
            .account_store()
            .iter_accounts()
            .unwrap()
            .into_iter()
            .filter(|account| !account.head.is_zero())
            .map(|account| HistoryHead {
                account: account.address,
                height: account.confirmation_height,
                hash: account.head,
            })
            .collect();
        let cutoff = Timestamp::now().as_secs();
        let replay = CertifiedCheckpoint::prepare(
            &nodes[0].store,
            NetworkId::Dev,
            &params,
            cutoff,
            &heads,
            &applicant,
        )
        .unwrap();
        let anchor = ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &params).unwrap();
        let clock = Some(SigningClock {
            now: cutoff,
            error_bound: 0,
        });
        // This is a replay/import fixture, not a live agreement test. Construct
        // its certificate with an isolated durable signer; the running source's
        // round manager may already have advanced while payments were delivered.
        let signer_node = BurstNode::new(NodeConfig {
            data_dir: directory.path().join("certificate-signer"),
            ..configs[0].clone()
        })
        .await
        .unwrap();
        let signer = &signer_node.store;
        let proposed = checkpoint_agreement::apply(
            signer,
            &anchor,
            &creator_keys.private,
            Command::Propose(replay.checkpoint().clone()),
            &[&replay],
            clock,
        )
        .unwrap();
        let Outgoing::Proposal(proposal) = &proposed[0] else {
            panic!("expected checkpoint proposal")
        };
        let votes = checkpoint_agreement::apply(
            signer,
            &anchor,
            &creator_keys.private,
            Command::Proposal(proposal),
            &[&replay],
            clock,
        )
        .unwrap();
        let Outgoing::Vote(prevote) = &votes[0] else {
            panic!("expected checkpoint prevote")
        };
        let proof = Quorum {
            ballot: prevote.ballot.clone(),
            signatures: vec![prevote.signer.clone()],
        };
        let commits = checkpoint_agreement::apply(
            signer,
            &anchor,
            &creator_keys.private,
            Command::Prepared(proposal, &proof),
            &[&replay],
            None,
        )
        .unwrap();
        let Outgoing::Vote(precommit) = &commits[0] else {
            panic!("expected checkpoint precommit")
        };
        let certificate = CheckpointCertificate {
            checkpoint: replay.checkpoint().clone(),
            round: 0,
            signatures: vec![precommit.signer.clone()],
        };
        checkpoint_agreement::apply(
            signer,
            &anchor,
            &creator_keys.private,
            Command::Decide(certificate.clone()),
            &[&replay],
            None,
        )
        .unwrap();
        CertifiedCheckpoint::accept(
            &nodes[0].store,
            NetworkId::Dev,
            &params,
            certificate.clone(),
            heads,
            applicant.clone(),
        )
        .unwrap();
        eprintln!("source accepted selection certificate; waiting for connected observers");
        // No direct import on observers: the running peer service transfers,
        // authenticates and replays the record using their own confirmed blocks.
        for node in &nodes[1..] {
            loop {
                if let Some(imported) =
                    CertifiedCheckpoint::load(&node.store, NetworkId::Dev, &params).unwrap()
                {
                    assert_eq!(imported.certificate(), &certificate);
                    assert_eq!(imported.replayed().request(), replay.request());
                    eprintln!("connected observer imported selection certificate");
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let mut fresh = configs[2].clone();
        fresh.data_dir = directory.path().join("checkpoint-fresh-joiner");
        fresh.port = port();
        fresh.rpc_port = port();
        nodes.push(launch(fresh).await);
        eprintln!("fresh checkpoint observer launched");
        loop {
            let node = nodes.last().unwrap();
            if let Some(imported) =
                CertifiedCheckpoint::load(&node.store, NetworkId::Dev, &params).unwrap()
            {
                assert_eq!(imported.certificate(), &certificate);
                assert_eq!(imported.replayed().request(), replay.request());
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    for node in &nodes {
        node.shutdown.shutdown();
    }
    for node in nodes {
        tokio::time::timeout(Duration::from_secs(20), node.task)
            .await
            .unwrap()
            .unwrap();
    }
    result.expect("confirmed payment replay network test timed out");
}
