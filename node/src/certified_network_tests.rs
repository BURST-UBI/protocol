use crate::{
    checkpoint_replay::SigningClock, verification_test_support::time, BurstNode, NodeConfig,
};
use burst_ledger::{BlockType, StateBlock};
use burst_store::meta::MetaStore;
use burst_transactions::Transaction;
use burst_types::{
    BlockHash, KeyPair, NetworkId, ProtocolParams, Timestamp, TxHash, WalletAddress, BRN_UNIT,
};
use burst_wallet_core::{transaction_builder::*, NodeClient};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

mod challenge_expiry;
mod deadline;
mod governance_ballot;
mod governance_certified;
mod governance_content;
mod native_delegation;
mod partition;
mod payment_load;
mod pending_challenge;
mod pending_expiry;
mod post_challenge_recovery;
mod roster;

pub(crate) async fn governance_queries_from_snapshot(baseline: &std::path::Path) {
    governance_certified::queries_from_snapshot(baseline).await;
}

fn port() -> u16 {
    // Nodes bind all IPv4 interfaces. A localhost-only probe can choose a port
    // already held by a VPN/interface-specific service.
    static ISSUED: std::sync::Mutex<std::collections::BTreeSet<u16>> =
        std::sync::Mutex::new(std::collections::BTreeSet::new());
    let mut issued = ISSUED.lock().unwrap();
    loop {
        let listener = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        if issued.insert(port) {
            return port;
        }
    }
}

async fn signed_block(
    client: &NodeClient,
    key: &KeyPair,
    tx: Transaction,
    params: &ProtocolParams,
) -> StateBlock {
    let address = burst_crypto::derive_address(&key.public);
    if std::env::var("NATIVE_UI_OFFLINE_PANEL").as_deref() == Ok("1")
        && key.public == burst_crypto::keypair_from_seed(&[1; 32]).public
    {
        if let Transaction::VerificationVote(vote) = &tx {
            return challenge_expiry::native_appeal::offline_panel(client, vote).await;
        }
    }
    if std::env::var("NATIVE_SDK_OFFLINE_IDENTITY").as_deref() == Ok("1") {
        assert!(burst_verification::checkpoint_selection_enabled(NetworkId::Dev),
            "offline panel qualification requires the testnet feature; default artifacts keep selection gated");
        use burst_wallet_core::offline::{
            load_action_with_content, OfflineAction, OfflineActionContext,
        };
        let action = match &tx {
            Transaction::Challenge(challenge) if challenge.appeal_of.is_none() => {
                Some(OfflineAction::Challenge {
                    target: challenge.target.clone(),
                    stake: challenge.stake_amount,
                    reason: challenge.reason,
                })
            }
            Transaction::VerificationVote(vote) => Some(OfflineAction::VerificationVote {
                target: vote.target_wallet.clone(),
                context: vote.selection_context,
                vote: vote.vote,
                stake: vote.stake_amount,
            }),
            _ => None,
        };
        if let Some(action) = action {
            let before = client.inspect_signing_state(&address).await.unwrap().head;
            let prepared = client
                .prepare_offline_action(&address, action)
                .await
                .unwrap();
            let encoded = serde_json::to_string(&prepared).unwrap();
            let restored: OfflineActionContext = serde_json::from_str(&encoded).unwrap();
            let signed = restored.sign(key, tx.timestamp()).unwrap();
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("action.json");
            signed.save(&path).unwrap();
            let (loaded, content) = load_action_with_content(&path).unwrap();
            assert!(content.is_none());
            assert_eq!(
                serde_json::to_string(&loaded).unwrap(),
                serde_json::to_string(signed.block()).unwrap()
            );
            assert_eq!(
                client.inspect_signing_state(&address).await.unwrap().head,
                before
            );
            eprintln!(
                "PASS SDK offline identity prepared/signed/journaled {:?}",
                loaded.block_type
            );
            return loaded;
        }
    }
    let state_started = std::time::Instant::now();
    let state = client.signing_state(&address).await.unwrap();
    let state_elapsed = state_started.elapsed();
    let signing_started = std::time::Instant::now();
    let origin = match &tx {
        Transaction::Send(send) => send.origin,
        _ => TxHash::ZERO,
    };
    let mut block = build_state_block(&state, &tx, origin, params.params_hash()).unwrap();
    if let Transaction::Endorse(endorsement) = &tx {
        block.target_head = client
            .signing_state(&endorsement.target)
            .await
            .unwrap()
            .head;
    }
    if state.head.is_zero() && matches!(tx, Transaction::Receive(_)) {
        block.block_type = BlockType::Open;
    }
    block.hash = block.compute_hash();
    let signed = sign_state_block(block, &key.private);
    if std::env::var_os("BURST_FINALITY_STAGE_LOG").is_some() {
        println!(
            "payment_signing_stages hash={} account={} head={} state_us={} construct_sign_us={}",
            signed.hash,
            address,
            state.head,
            state_elapsed.as_micros(),
            signing_started.elapsed().as_micros()
        );
    }
    signed
}

async fn submit(clients: &[NodeClient], block: &StateBlock) {
    let started = std::time::Instant::now();
    let response = clients[0]
        .process(&serde_json::to_string(block).unwrap())
        .await
        .unwrap();
    assert!(
        response.accepted,
        "rejected {:?}: {response:?}",
        block.block_type
    );
    let accepted = started.elapsed();
    for (index, client) in clients.iter().enumerate() {
        if let Err(error) = client
            .wait_confirmed(&block.hash.to_string(), Duration::from_secs(25))
            .await
        {
            for (peer, client) in clients.iter().enumerate() {
                eprintln!(
                    "peer {peer} block {:?}",
                    client
                        .rpc_call("block_info", json!({"hash":block.hash.to_string()}))
                        .await
                );
            }
            panic!(
                "{:?} {} did not confirm on peer {index}: {error}",
                block.block_type, block.hash
            );
        }
        if std::env::var_os("BURST_FINALITY_STAGE_LOG").is_some() {
            println!("confirmed_observation operation={:?} hash={} peer={} accepted_ms={:.3} published_to_observed_ms={:.3}",
                block.block_type, block.hash, index, accepted.as_secs_f64() * 1000.0,
                started.elapsed().as_secs_f64() * 1000.0);
        }
    }
}

async fn wait_human_authority(stores: &[Arc<burst_store_lmdb::LmdbStore>], humans: u64) {
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let authorities: Vec<_> = stores
                .iter()
                .filter_map(|store| {
                    crate::payment_authority::PaymentAuthority::read(store).unwrap()
                })
                .collect();
            if authorities.len() == stores.len()
                && authorities.iter().all(|authority| {
                    authority.roster.total_humans() == humans
                        && authority.epoch == authorities[0].epoch
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    if result.is_err() {
        for (index, store) in stores.iter().enumerate() {
            let authority = crate::payment_authority::PaymentAuthority::read(store).map(|value| {
                value.map(|value| (value.epoch, value.cutoff, value.roster.total_humans()))
            });
            let verification = store
                .meta_store()
                .get_meta(crate::endorsement_commit::SNAPSHOT_KEY)
                .map(|bytes| {
                    burst_verification::OrchestratorSnapshot::decode(&bytes).map(|snapshot| {
                        let mut states: Vec<_> = snapshot
                            .states
                            .into_iter()
                            .map(|(wallet, state)| {
                                (
                                    wallet.to_string(),
                                    state.phase,
                                    state.votes.len(),
                                    state.selected_verifiers.len(),
                                )
                            })
                            .collect();
                        states.sort_by(|a, b| a.0.cmp(&b.0));
                        states
                    })
                });
            eprintln!("node {index} expected {humans} humans; authority={authority:?}; verification={verification:?}");
        }
    }
    result.expect("verified human authority did not reach every node");
}

async fn fixture_node(config: NodeConfig, clock: &Arc<AtomicU64>, relay_url: &str) -> BurstNode {
    let mut node = BurstNode::new(config).await.unwrap();
    node.block_processor.lock().await.test_time = Some(clock.clone());
    let signing_time = clock.clone();
    node.verification_fixture_clock = Some(Arc::new(move || {
        Some(SigningClock {
            now: signing_time.load(Ordering::SeqCst),
            error_bound: 0,
        })
    }));
    let info = burst_vrf::DrandClient::mainnet()
        .unwrap()
        .chain_info()
        .unwrap()
        .clone();
    node.vrf_client = Arc::new(
        burst_vrf::DrandClient::with_chain(relay_url, &info.chain_hash)
            .with_verifier(burst_vrf::DrandVerifier::mainnet().unwrap())
            .with_chain_info(info),
    );
    node
}

async fn assert_settlement(client: &NodeClient, addresses: &[WalletAddress]) {
    for (index, address) in addresses.iter().enumerate().take(4).skip(1) {
        let components = client
            .rpc_call("brn_components", json!({"account":address.as_str()}))
            .await
            .unwrap();
        assert_eq!(components["total_staked"], "0");
        assert_eq!(
            components["total_burned"],
            if index == 3 {
                (10 * BRN_UNIT).to_string()
            } else {
                "0".into()
            }
        );
        let pending = client
            .rpc_call("account_pending", json!({"account":address.as_str()}))
            .await
            .unwrap();
        let rewards = pending["pending"].as_array().unwrap();
        if index == 3 {
            assert!(rewards.is_empty());
        } else {
            assert_eq!(rewards.len(), 1);
            assert_eq!(rewards[0]["amount"], (5 * BRN_UNIT).to_string());
        }
    }
}

// Historic BLS proofs plus controlled time exercise the running services, not
// real-time beacon secrecy, production NTS, or Byzantine checkpoint liveness.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn certified_committee_settles_signed_votes_across_independent_nodes() {
    certified_journey(ChallengeMode::Rejected).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn upheld_challenge_installs_agreed_settlement_across_restart_and_fresh_catchup() {
    certified_journey(ChallengeMode::Upheld).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoked_wallet_reverifies_restores_pending_trst_and_recovers_on_fresh_nodes() {
    certified_journey(ChallengeMode::Reverified).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inactive_wallet_preserves_trst_and_reverifies_across_restart_and_fresh_catchup() {
    certified_journey(ChallengeMode::ReverifiedInactivity).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn challenge_deadline_installs_agreed_settlement_across_restart_and_fresh_catchup() {
    certified_journey(ChallengeMode::Deadline).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn early_challenge_vote_survives_activation_deadline_and_fresh_catchup() {
    certified_journey(ChallengeMode::EarlyVoteDeadline).await;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChallengeMode {
    Rejected,
    Upheld,
    Reverified,
    ReverifiedInactivity,
    Deadline,
    EarlyVoteDeadline,
}

impl ChallengeMode {
    fn reverifies(self) -> bool {
        matches!(self, Self::Reverified | Self::ReverifiedInactivity)
    }

    fn settled_state(self) -> burst_types::WalletState {
        if self == Self::ReverifiedInactivity {
            burst_types::WalletState::Deactivated
        } else {
            burst_types::WalletState::Revoked
        }
    }

    fn settled_status(self) -> &'static str {
        if self == Self::ReverifiedInactivity {
            "deactivated"
        } else {
            "revoked"
        }
    }

    fn has_deadline(self) -> bool {
        matches!(self, Self::Deadline | Self::EarlyVoteDeadline)
    }
}

async fn certified_journey(mode: ChallengeMode) {
    certified_journey_with_load(mode, None).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit capped authenticated payment-load diagnostic; not release qualification"]
async fn certified_independent_account_payment_load() {
    let seconds = std::env::var("BURST_PAYMENT_LOAD_SECONDS")
        .expect("set load duration")
        .parse::<u64>()
        .expect("integer duration");
    assert!((2..=30).contains(&seconds));
    certified_journey_with_load(
        ChallengeMode::Rejected,
        Some(PaymentExercise::Load(Duration::from_secs(seconds))),
    )
    .await;
}

#[derive(Clone, Copy)]
enum PaymentExercise {
    Load(Duration),
    FragmentedSend,
    FragmentedLoad(Duration),
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn certified_fragmented_send_round_trip() {
    certified_journey_with_load(
        ChallengeMode::Rejected,
        Some(PaymentExercise::FragmentedSend),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit capped fragmented-wallet payment diagnostic; not release qualification"]
async fn certified_fragmented_account_payment_load() {
    let seconds = std::env::var("BURST_PAYMENT_LOAD_SECONDS")
        .expect("set load duration")
        .parse::<u64>()
        .expect("integer duration");
    assert!((2..=30).contains(&seconds));
    certified_journey_with_load(
        ChallengeMode::Rejected,
        Some(PaymentExercise::FragmentedLoad(Duration::from_secs(
            seconds,
        ))),
    )
    .await;
}

async fn certified_journey_with_load(mode: ChallengeMode, load: Option<PaymentExercise>) {
    let load_accounts = if load.is_some() {
        std::env::var("BURST_PAYMENT_LOAD_ACCOUNTS").map_or(3, |value| {
            value.parse::<usize>().expect("integer account count")
        })
    } else {
        3
    };
    assert!((3..=32).contains(&load_accounts));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(AtomicU64::new(time(10).as_secs()));
    let released = Arc::new(AtomicBool::new(false));
    let relay = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_url = format!("http://{}", relay.local_addr().unwrap());
    let relay_release = released.clone();
    let relay_task = tokio::spawn(async move {
        let beacons: Vec<burst_vrf::DrandBeacon> =
            serde_json::from_str(include_str!("../tests/fixtures/selection-beacons.json")).unwrap();
        loop {
            let (mut socket, _) = relay.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 1024];
                let length = socket.read(&mut buffer).await.unwrap();
                if length == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..length]);
                if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    break;
                }
                assert!(request.len() <= 4096);
            }
            let path = std::str::from_utf8(&request)
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap();
            let round = path.rsplit('/').next().unwrap().parse::<u64>().unwrap();
            let beacon = beacons.iter().find(|beacon| beacon.round == round);
            let (status, body) = if relay_release.load(Ordering::SeqCst) {
                (
                    "200 OK",
                    serde_json::to_string(
                        beacon.unwrap_or_else(|| panic!("missing round {round}")),
                    )
                    .unwrap(),
                )
            } else {
                ("503 Service Unavailable", "{}".to_string())
            };
            let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    let params = ProtocolParams {
        brn_rate: BRN_UNIT,
        min_work_difficulty: 0,
        bootstrap_exit_threshold: (load_accounts + 1) as u32,
        endorsement_threshold: 1,
        endorsement_burn_amount: 1,
        num_verifiers: 3,
        verification_threshold_bps: 6000,
        verifier_stake_amount: 10 * BRN_UNIT,
        verification_timeout_secs: if mode.has_deadline() {
            60
        } else {
            ProtocolParams::burst_defaults().verification_timeout_secs
        },
        challenge_duration_secs: if mode.has_deadline() {
            60
        } else {
            ProtocolParams::burst_defaults().challenge_duration_secs
        },
        max_revotes: if mode.has_deadline() {
            0
        } else {
            ProtocolParams::burst_defaults().max_revotes
        },
        min_verification_age_secs: 0,
        new_wallet_spending_limit: 0,
        trst_expiry_secs: u64::MAX,
        ..ProtocolParams::burst_defaults()
    };
    let ports: Vec<_> = (0..4).map(|_| port()).collect();
    let mut clients = Vec::new();
    let mut workers = Vec::new();
    let mut orchestrators = Vec::new();
    let mut stores = Vec::new();
    let mut election_views = Vec::new();
    let mut configs = Vec::new();
    for index in 0..4 {
        let key_file = directory.path().join(format!("representative-{index}.key"));
        std::fs::write(&key_file, hex::encode([index as u8; 32])).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let config = NodeConfig {
            network: NetworkId::Dev,
            data_dir: directory.path().join(format!("node-{index}")),
            port: ports[index],
            rpc_port: port(),
            enable_upnp: false,
            nts_sources: vec![],
            representative_key_file: Some(key_file),
            params: params.clone(),
            bootstrap_peers: ports
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, port)| format!("127.0.0.1:{port}"))
                .collect(),
            ..NodeConfig::default()
        };
        clients.push(NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap());
        configs.push(config.clone());
        let mut node = fixture_node(config, &clock, &relay_url).await;
        orchestrators.push(node.verification_orchestrator.clone());
        stores.push(node.store.clone());
        election_views.push(node.active_elections.clone());
        let shutdown = node.shutdown.clone();
        workers.push((
            shutdown,
            tokio::spawn(async move {
                node.start_with_services(burst_daemon::start_public_services)
                    .await
                    .unwrap();
                node.stop().await.unwrap();
            }),
        ));
    }
    let restart_clock = clock.clone();
    let load_failure_path = directory.path().join("payment-load-failure.json");
    let mut journey = tokio::spawn(async move {
        for client in &clients {
            loop {
                if client
                    .rpc_call("peers", json!({}))
                    .await
                    .is_ok_and(|value| value["count"].as_u64().unwrap_or(0) >= 3)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let key_count = if load_accounts > 3 {
            load_accounts + 1
        } else {
            5
        };
        let funding = if load_accounts > 3 {
            BRN_UNIT / 32
        } else {
            BRN_UNIT
        };
        let keys: Vec<_> = (0..key_count)
            .map(|index| burst_crypto::keypair_from_seed(&[index as u8; 32]))
            .collect();
        let addresses: Vec<_> = keys
            .iter()
            .map(|key| burst_crypto::derive_address(&key.public))
            .collect();
        for index in 1..key_count {
            if index <= load_accounts {
                let tx = Transaction::Endorse(
                    build_endorse_tx(&addresses[0], &addresses[index], 1, time(10)).unwrap(),
                );
                submit(
                    &clients,
                    &signed_block(&clients[0], &keys[0], tx, &params).await,
                )
                .await;
            }
            let tx = Transaction::Burn(
                build_burn_tx(&addresses[0], &addresses[index], funding, time(10)).unwrap(),
            );
            let burn = signed_block(&clients[0], &keys[0], tx, &params).await;
            submit(&clients, &burn).await;
            let tx = Transaction::Receive(
                build_receive_tx(
                    &addresses[index],
                    burn.hash.into_tx_hash(),
                    funding,
                    time(10),
                )
                .unwrap(),
            );
            let mut opening = signed_block(&clients[0], &keys[index], tx, &params).await;
            if index >= 4 && index <= load_accounts {
                // Additional humans delegate to live representatives; their own
                // keys still authorize every payment on their account chains.
                opening.representative = addresses[index % 4].clone();
                opening.hash = opening.compute_hash();
                opening = sign_state_block(opening, &keys[index].private);
            }
            submit(&clients, &opening).await;
            if index < 4 {
                let mut opt_in = opening.clone();
                opt_in.block_type = BlockType::VerifierOptIn;
                opt_in.previous = opening.hash;
                opt_in.link = BlockHash::ZERO;
                opt_in.origin = TxHash::ZERO;
                opt_in.transaction =
                    burst_crypto::hash_transaction(format!("fixture-opt-in-{index}").as_bytes());
                opt_in.hash = opt_in.compute_hash();
                submit(&clients, &sign_state_block(opt_in, &keys[index].private)).await;
            }
        }
        clock.store(time(20).as_secs(), Ordering::SeqCst);
        wait_human_authority(&stores, (load_accounts + 1) as u64).await;
        if let Some(exercise) = load {
            match exercise {
                PaymentExercise::Load(duration) => {
                    payment_load::run(
                        &clients,
                        &stores,
                        &election_views,
                        &keys[..load_accounts + 1],
                        &params,
                        time(20),
                        duration,
                        &load_failure_path,
                        payment_load::Workload::BurnReceive,
                    )
                    .await
                }
                PaymentExercise::FragmentedLoad(duration) => {
                    payment_load::run(
                        &clients,
                        &stores,
                        &election_views,
                        &keys[..load_accounts + 1],
                        &params,
                        time(20),
                        duration,
                        &load_failure_path,
                        payment_load::Workload::FragmentedSend,
                    )
                    .await
                }
                PaymentExercise::FragmentedSend => {
                    payment_load::fragmented_send_round_trip(
                        &clients,
                        &stores,
                        &keys,
                        &params,
                        time(20),
                    )
                    .await
                }
            }
            return None;
        }
        let tx = Transaction::Endorse(
            build_endorse_tx(&addresses[0], &addresses[4], 1, time(100)).unwrap(),
        );
        clock.store(time(100).as_secs(), Ordering::SeqCst);
        submit(
            &clients,
            &signed_block(&clients[0], &keys[0], tx, &params).await,
        )
        .await;
        let request = loop {
            let chain =
                crate::checkpoint_store::chain::load(&stores[0], NetworkId::Dev, &params).unwrap();
            if let Some(checkpoint) = chain
                .iter()
                .find_map(crate::checkpoint_store::chain::CertifiedEvent::selection)
            {
                break checkpoint.replayed().request().clone();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert_eq!(request.round(), 102);
        for orchestrator in &orchestrators {
            assert!(orchestrator
                .lock()
                .await
                .selection_context(&addresses[4])
                .is_none());
        }
        clock.store(request.beacon_time().unwrap(), Ordering::SeqCst);
        released.store(true, Ordering::SeqCst);
        let mut contexts = Vec::new();
        for orchestrator in &orchestrators {
            loop {
                let state = orchestrator.lock().await;
                if let Some(context) = state.selection_context(&addresses[4]) {
                    let mut selected = state
                        .get_state(&addresses[4])
                        .unwrap()
                        .selected_verifiers
                        .clone();
                    selected.sort_by(|left, right| left.as_str().cmp(right.as_str()));
                    let mut expected = addresses[1..4].to_vec();
                    expected.sort_by(|left, right| left.as_str().cmp(right.as_str()));
                    assert_eq!(*selected, expected);
                    contexts.push(context);
                    break;
                }
                drop(state);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        assert!(contexts.iter().all(|context| *context == contexts[0]));
        clock.store(request.beacon_time().unwrap() + 2, Ordering::SeqCst);
        let activation_key = crate::verification_activation::key(contexts[0]);
        let activation_bytes = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let installed: Vec<_> = stores
                    .iter()
                    .filter_map(|store| store.meta_store().get_meta(&activation_key).ok())
                    .collect();
                if installed.len() == stores.len()
                    && installed.iter().all(|bytes| *bytes == installed[0])
                {
                    let activation = crate::verification_activation::decode(&installed[0]).unwrap();
                    assert_eq!(activation.started_at, request.beacon_time().unwrap() + 1);
                    assert_eq!(
                        activation.deadline,
                        activation.started_at + params.verification_timeout_secs
                    );
                    for store in &stores {
                        let events =
                            crate::checkpoint_store::chain::load(store, NetworkId::Dev, &params)
                                .unwrap();
                        let last = events.last().unwrap();
                        assert!(matches!(
                            last,
                            crate::checkpoint_store::chain::CertifiedEvent::Activation(_)
                        ));
                        assert_eq!(
                            store.meta_store().payment_epoch().unwrap(),
                            last.certificate().checkpoint.hash()
                        );
                    }
                    break installed[0].clone();
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("committee activation did not install on every node");
        let now = Timestamp::new(clock.load(Ordering::SeqCst));
        let tx = Transaction::VerificationVote(
            build_verification_vote_tx(
                &addresses[1],
                &addresses[4],
                BlockHash::new([99; 32]),
                1,
                10 * BRN_UNIT,
                now,
            )
            .unwrap(),
        );
        let invalid = signed_block(&clients[0], &keys[1], tx, &params).await;
        let before = clients[0].signing_state(&addresses[1]).await.unwrap().head;
        let rejected = clients[0]
            .process(&serde_json::to_string(&invalid).unwrap())
            .await
            .unwrap();
        assert!(!rejected.accepted);
        assert!(rejected
            .detail
            .as_deref()
            .unwrap()
            .contains("wrong selection context"));
        assert_eq!(
            clients[0].signing_state(&addresses[1]).await.unwrap().head,
            before
        );
        for index in 1..4 {
            let vote = if index == 3 { 2 } else { 1 };
            let tx = Transaction::VerificationVote(
                build_verification_vote_tx(
                    &addresses[index],
                    &addresses[4],
                    contexts[0],
                    vote,
                    10 * BRN_UNIT,
                    now,
                )
                .unwrap(),
            );
            submit(
                &clients,
                &signed_block(&clients[0], &keys[index], tx, &params).await,
            )
            .await;
        }
        for client in &clients {
            let status = client
                .rpc_call(
                    "verification_status",
                    json!({"account":addresses[4].as_str()}),
                )
                .await
                .unwrap();
            assert_eq!(status["status"], "verified");
            assert_eq!(status["verified_at"], now.as_secs());
            // A test fixture must not advertise production availability.
            assert_eq!(status["selection_available"], false);
            assert_settlement(client, &addresses).await;
        }
        let genesis = crate::genesis_state::block(NetworkId::Dev, &params).hash;
        for store in &stores {
            let heads =
                crate::confirmed_history::ConfirmedHistory::heads_at(store, now.as_secs()).unwrap();
            let history = crate::confirmed_history::ConfirmedHistory::read(
                store,
                NetworkId::Dev,
                genesis,
                now.as_secs(),
                &heads,
                &[],
            )
            .unwrap();
            let certificates =
                crate::checkpoint_store::chain::load(store, NetworkId::Dev, &params).unwrap();
            let replay = crate::confirmed_replay::ConfirmedReplay::replay_with_events(
                &history,
                NetworkId::Dev,
                &params,
                &certificates,
            )
            .unwrap();
            assert!(replay.account(&addresses[4]).unwrap().state.is_verified());
            for (index, address) in addresses.iter().enumerate().take(4).skip(1) {
                let account = replay.account(address).unwrap();
                assert_eq!(account.total_brn_staked, 0);
                assert_eq!(
                    account.total_brn_burned,
                    if index == 3 { 10 * BRN_UNIT } else { 0 }
                );
            }
        }
        clock.store(now.as_secs() + 3, Ordering::SeqCst);
        wait_human_authority(&stores, 5).await;
        let mut heads = Vec::new();
        for address in &addresses {
            heads.push(clients[0].signing_state(address).await.unwrap().head);
        }
        Some((
            clients,
            addresses,
            heads,
            now,
            contexts[0],
            activation_bytes,
        ))
    });
    let outcome = tokio::time::timeout(Duration::from_secs(240), &mut journey).await;
    if outcome.is_err() {
        journey.abort();
        let _ = journey.await;
    }
    for (shutdown, _) in &workers {
        shutdown.shutdown();
    }
    let worker_failures = payment_load::drain_workers(
        workers.drain(..).map(|(_, worker)| worker),
        Duration::from_secs(20),
    )
    .await;
    if load.is_some() {
        relay_task.abort();
        let _ = relay_task.await;
        if !matches!(&outcome, Ok(Ok(None))) || !worker_failures.is_empty() {
            let retained = directory.keep();
            eprintln!("payment_load_retained_fixture {}", retained.display());
        }
        assert!(
            worker_failures.is_empty(),
            "payment load workers failed: {worker_failures:?}"
        );
        assert!(outcome
            .expect("payment load fixture timed out")
            .unwrap()
            .is_none());
        return;
    }
    assert!(
        worker_failures.is_empty(),
        "certified network workers failed: {worker_failures:?}"
    );
    let completed = outcome.expect("certified verification network journey timed out");
    let (mut clients, addresses, heads, verified_at, context, activation_bytes) = completed
        .unwrap()
        .expect("lifecycle journey must return recovery inputs");
    let mut observer = configs[0].clone();
    observer.data_dir = directory.path().join("fresh-observer");
    observer.port = port();
    observer.rpc_port = port();
    observer.enable_representative = false;
    observer.representative_key_file = None;
    observer.bootstrap_peers = ports
        .iter()
        .map(|port| format!("127.0.0.1:{port}"))
        .collect();
    clients.push(NodeClient::new(format!("http://127.0.0.1:{}", observer.rpc_port)).unwrap());
    configs.push(observer);
    let final_configs = configs.clone();
    let final_clock = Arc::clone(&restart_clock);
    let params = configs[0].params.clone();
    let mut recovered_stores = Vec::new();
    for config in configs {
        let mut node = fixture_node(config, &restart_clock, &relay_url).await;
        recovered_stores.push(node.store.clone());
        let shutdown = node.shutdown.clone();
        workers.push((
            shutdown,
            tokio::spawn(async move {
                node.start_with_services(burst_daemon::start_public_services)
                    .await
                    .unwrap();
                node.stop().await.unwrap();
            }),
        ));
    }
    let mut recovery = tokio::spawn(async move {
        for client in &clients {
            loop {
                let status = client
                    .rpc_call(
                        "verification_status",
                        json!({"account":addresses[4].as_str()}),
                    )
                    .await;
                if status
                    .as_ref()
                    .is_ok_and(|status| status["status"] == "verified")
                {
                    let status = status.unwrap();
                    assert_eq!(status["verified_at"], verified_at.as_secs());
                    assert_eq!(status["selection_context"], context.to_string());
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            for (address, head) in addresses.iter().zip(&heads) {
                client
                    .wait_confirmed(&head.to_string(), Duration::from_secs(30))
                    .await
                    .unwrap();
                assert_eq!(client.signing_state(address).await.unwrap().head, *head);
            }
            assert_settlement(client, &addresses).await;
        }
        for store in &recovered_stores {
            assert_eq!(
                store
                    .meta_store()
                    .get_meta(&crate::verification_activation::key(context))
                    .unwrap(),
                activation_bytes
            );
        }
        let spend_at = Timestamp::new(verified_at.as_secs() + 60);
        restart_clock.store(spend_at.as_secs(), Ordering::SeqCst);
        let winner = burst_crypto::keypair_from_seed(&[1; 32]);
        let available = {
            let batch = recovered_stores[0].tx_begin_write().unwrap();
            let state = crate::payment_transition::restore(&batch, &params).unwrap();
            let wallet = state.brn.get_wallet(&addresses[1]).unwrap();
            let available = state.brn.compute_balance(wallet, spend_at);
            assert_eq!(
                available
                    - wallet
                        .spendable_balance_checked(&state.brn.rate_history, spend_at)
                        .unwrap(),
                10 * BRN_UNIT
            );
            available
        };
        let tx = Transaction::Burn(
            build_burn_tx(&addresses[1], &addresses[0], available, spend_at).unwrap(),
        );
        let spend = signed_block(&clients[0], &winner, tx, &params).await;
        let mut expected_sources = heads[1..4].to_vec();
        expected_sources.sort_unstable();
        assert_eq!(spend.release_sources, expected_sources);
        let mut unproven = spend.clone();
        unproven.release_sources.clear();
        unproven.hash = unproven.compute_hash();
        unproven.signature = burst_crypto::sign_message(unproven.hash.as_bytes(), &winner.private);
        let rejected = clients[0]
            .process(&serde_json::to_string(&unproven).unwrap())
            .await
            .unwrap();
        assert!(
            !rejected.accepted,
            "returned stake was spendable without its signed outcome"
        );
        submit(&clients, &spend).await;
        let rewards = clients[0]
            .rpc_call("account_pending", json!({"account":addresses[1].as_str()}))
            .await
            .unwrap();
        let source = burst_wallet_core::signed_client::parse_hash(
            rewards["pending"][0]["hash"].as_str().unwrap(),
        )
        .unwrap();
        let tx = Transaction::Receive(
            build_receive_tx(&addresses[1], source.into_tx_hash(), 5 * BRN_UNIT, spend_at).unwrap(),
        );
        submit(
            &clients,
            &signed_block(&clients[0], &winner, tx, &params).await,
        )
        .await;
        let target = burst_crypto::keypair_from_seed(&[4; 32]);
        let tx = Transaction::Burn(
            build_burn_tx(&addresses[4], &addresses[0], BRN_UNIT, spend_at).unwrap(),
        );
        submit(
            &clients,
            &signed_block(&clients[0], &target, tx, &params).await,
        )
        .await;
        for client in &clients {
            assert_eq!(
                client
                    .signing_state(&addresses[1])
                    .await
                    .unwrap()
                    .trst_balance,
                6 * BRN_UNIT
            );
            let pending = client
                .rpc_call("account_pending", json!({"account":addresses[1].as_str()}))
                .await
                .unwrap();
            assert!(pending["pending"].as_array().unwrap().is_empty());
            let components = client
                .rpc_call("brn_components", json!({"account":addresses[4].as_str()}))
                .await
                .unwrap();
            assert_eq!(components["total_burned"], BRN_UNIT.to_string());
            let components = client
                .rpc_call("brn_components", json!({"account":addresses[1].as_str()}))
                .await
                .unwrap();
            assert_eq!(components["total_burned"], available.to_string());
            assert!(components["release_sources"].as_array().unwrap().is_empty());
        }
        for store in &recovered_stores {
            let heads =
                crate::confirmed_history::ConfirmedHistory::heads_at(store, spend_at.as_secs())
                    .unwrap();
            let genesis = crate::genesis_state::block(NetworkId::Dev, &params).hash;
            let history = crate::confirmed_history::ConfirmedHistory::read(
                store,
                NetworkId::Dev,
                genesis,
                spend_at.as_secs(),
                &heads,
                &[],
            )
            .unwrap();
            let certificates =
                crate::checkpoint_store::chain::load(store, NetworkId::Dev, &params).unwrap();
            let replay = crate::confirmed_replay::ConfirmedReplay::replay_with_events(
                &history,
                NetworkId::Dev,
                &params,
                &certificates,
            )
            .unwrap();
            assert_eq!(
                replay.account(&addresses[4]).unwrap().total_brn_burned,
                BRN_UNIT
            );
            assert_eq!(
                replay.account(&addresses[1]).unwrap().trst_balance,
                6 * BRN_UNIT
            );
            assert_eq!(
                replay.account(&addresses[1]).unwrap().total_brn_burned,
                available
            );
        }
        pending_challenge::run(
            &clients,
            &recovered_stores,
            &params,
            &addresses,
            &restart_clock,
            mode,
        )
        .await;
        let expected = post_challenge_recovery::capture(&clients[0], &addresses)
            .await
            .unwrap();
        (addresses, expected)
    });
    let recovery_budget = if mode.reverifies() { 300 } else { 180 };
    let outcome = tokio::time::timeout(Duration::from_secs(recovery_budget), &mut recovery).await;
    if outcome.is_err() {
        recovery.abort();
        let _ = recovery.await;
    }
    for (shutdown, _) in &workers {
        shutdown.shutdown();
    }
    for (_, worker) in workers {
        tokio::time::timeout(Duration::from_secs(20), worker)
            .await
            .unwrap()
            .unwrap();
    }
    let (addresses, expected) = match outcome {
        Ok(Ok(result)) => result,
        error => {
            relay_task.abort();
            let _ = relay_task.await;
            panic!("certified verification restart or observer catch-up failed: {error:?}");
        }
    };
    post_challenge_recovery::run(
        final_configs,
        final_clock,
        &relay_url,
        addresses,
        expected,
        None,
    )
    .await;
    relay_task.abort();
    let _ = relay_task.await;
}
