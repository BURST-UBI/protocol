use super::*;
use crate::{checkpoint_store::chain, verification_deadline};
use burst_store::{account::AccountStore, pending::PendingStore};
use burst_types::{ChallengeReason, WalletState};

mod appeals;
pub(super) mod native_appeal;

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the native wallet, Vite, WebKitWebDriver and NATIVE_FAKETIME_LIBRARY"]
async fn desktop_appeal_is_certified_then_restarts_and_catches_up() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    journey_with_desktop(ChallengeReason::Fraud, false, &[true], true).await;
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the native wallet, Vite, WebKitWebDriver and NATIVE_FAKETIME_LIBRARY"]
async fn desktop_inactivity_appeal_is_certified_then_restarts_and_catches_up() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    journey_with_desktop(ChallengeReason::Inactivity, false, &[true], true).await;
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the native wallet, Vite, WebKitWebDriver and NATIVE_FAKETIME_LIBRARY"]
async fn desktop_repeated_appeals_preserve_failed_stakes_and_recover() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    journey_with_desktop(ChallengeReason::Fraud, false, &[true, true, false], true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_pending_expiry_and_challenge_deadline_recover_for_fraud_and_inactivity() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    for reason in [ChallengeReason::Fraud, ChallengeReason::Inactivity] {
        journey(reason, false, &[]).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partitioned_challenge_expiry_waits_for_quorum_then_recovers() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    journey(ChallengeReason::Fraud, true, &[]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn certified_appeal_restores_after_expiry_then_restarts_and_catches_up() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    journey(ChallengeReason::Fraud, false, &[true]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn certified_repeated_appeals_preserve_failed_stakes_and_revoke_without_duplicate_bounty() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    journey(ChallengeReason::Fraud, false, &[true, true, false]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn certified_inactivity_appeal_restores_accrual_without_fraud_revocation() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    journey(ChallengeReason::Inactivity, false, &[true]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partitioned_appeal_keeps_identity_stakes_and_epoch_until_quorum_recovers() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    journey(ChallengeReason::Fraud, true, &[true]).await;
}

async fn journey(reason: ChallengeReason, partitioned: bool, appeals: &'static [bool]) {
    journey_with_desktop(reason, partitioned, appeals, false).await;
}

async fn journey_with_desktop(
    reason: ChallengeReason,
    partitioned: bool,
    appeals: &'static [bool],
    desktop: bool,
) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(AtomicU64::new(time(10).as_secs()));
    let relay = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_url = format!("http://{}", relay.local_addr().unwrap());
    let relay_clock = clock.clone();
    let relay_task = tokio::spawn(async move {
        let beacons: Vec<burst_vrf::DrandBeacon> =
            serde_json::from_str(include_str!("../../tests/fixtures/selection-beacons.json"))
                .unwrap();
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
            let round = std::str::from_utf8(&request)
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap()
                .parse::<u64>()
                .unwrap();
            let (status, body) = if (round == 102
                || (0..appeals.len()).any(|index| round == 20266 + 4 * index as u64))
                && relay_clock.load(Ordering::SeqCst) >= time(round - 1).as_secs()
            {
                (
                    "200 OK",
                    serde_json::to_string(beacons.iter().find(|b| b.round == round).unwrap())
                        .unwrap(),
                )
            } else {
                ("503 Service Unavailable", "{}".into())
            };
            let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    // Round 102 activates at time(101) + 1. Both merged origins expire at
    // precisely the panel deadline, not at a receiving node's wall clock.
    let deadline = time(101).as_secs() + 61;
    let params = ProtocolParams {
        brn_rate: BRN_UNIT,
        min_work_difficulty: 0,
        bootstrap_exit_threshold: 5,
        endorsement_threshold: 1,
        endorsement_burn_amount: 1,
        num_verifiers: 3,
        verification_threshold_bps: 9000,
        verifier_stake_amount: 10 * BRN_UNIT,
        challenge_duration_secs: 60,
        max_revotes: 0,
        min_verification_age_secs: 0,
        new_wallet_spending_limit: 0,
        new_wallet_tx_limit_per_day: 0,
        trst_expiry_secs: deadline - time(20).as_secs(),
        ..ProtocolParams::burst_defaults()
    };
    let ports: Vec<_> = (0..4).map(|_| port()).collect();
    let mut configs = Vec::new();
    let mut clients = Vec::new();
    let mut stores = Vec::new();
    let mut workers = Vec::new();
    let mut registries = Vec::new();
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
        stores.push(node.store.clone());
        registries.push(node.connection_registry.clone());
        workers.push((
            node.shutdown.clone(),
            tokio::spawn(async move {
                node.start().await.unwrap();
                node.stop().await.unwrap();
            }),
        ));
    }
    let observed_clock = clock.clone();
    let mut scenario = tokio::spawn(async move {
        let (addresses, expected, receipt) = run(
            &clients,
            &stores,
            &params,
            &clock,
            reason,
            deadline,
            partitioned.then_some(registries.as_slice()),
        )
        .await;
        if !appeals.is_empty() {
            let (expected, receipt) = appeals::run(
                &clients,
                &stores,
                &params,
                &clock,
                &addresses,
                receipt,
                appeals::ReviewPlan {
                    judgments: appeals,
                    partitions: partitioned.then_some(registries.as_slice()),
                    desktop,
                },
            )
            .await;
            (addresses, expected, receipt)
        } else {
            (addresses, expected, receipt)
        }
    });
    let result = tokio::time::timeout(
        Duration::from_secs(240 + 60 * appeals.len() as u64 + if desktop { 180 } else { 0 }),
        &mut scenario,
    )
    .await;
    if result.is_err() {
        scenario.abort();
        let _ = scenario.await;
    }
    governance_certified::stop(&mut workers).await;
    let (addresses, expected, receipt) =
        result.expect("challenge/expiry journey timed out").unwrap();
    post_challenge_recovery::run(
        configs,
        observed_clock,
        &relay_url,
        addresses,
        expected,
        Some(receipt),
    )
    .await;
    relay_task.abort();
    let _ = relay_task.await;
    eprintln!("PASS mixed pending challenge/expiry: {reason:?}");
}

#[test]
fn every_selection_beacon_fixture_has_an_authentic_proof() {
    let beacons: Vec<burst_vrf::DrandBeacon> =
        serde_json::from_str(include_str!("../../tests/fixtures/selection-beacons.json")).unwrap();
    for beacon in beacons {
        assert!(
            burst_vrf::DrandClient::verify_beacon(&beacon).unwrap(),
            "invalid round {}",
            beacon.round
        );
    }
}

async fn run(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    params: &ProtocolParams,
    clock: &Arc<AtomicU64>,
    reason: ChallengeReason,
    deadline: u64,
    registries: Option<&[partition::Registry]>,
) -> (
    Vec<WalletAddress>,
    serde_json::Value,
    crate::verification_commit::receipts::Receipt,
) {
    for client in clients {
        while !client
            .rpc_call("peers", json!({}))
            .await
            .is_ok_and(|v| v["count"].as_u64().unwrap_or(0) >= 3)
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    let keys: Vec<_> = (0..5)
        .map(|seed| burst_crypto::keypair_from_seed(&[seed; 32]))
        .collect();
    let addresses: Vec<_> = keys
        .iter()
        .map(|key| burst_crypto::derive_address(&key.public))
        .collect();
    for index in 1..5 {
        let endorsement = signed_block(
            &clients[0],
            &keys[0],
            Transaction::Endorse(
                build_endorse_tx(&addresses[0], &addresses[index], 1, time(10)).unwrap(),
            ),
            params,
        )
        .await;
        submit(clients, &endorsement).await;
        let burn = signed_block(
            &clients[0],
            &keys[0],
            Transaction::Burn(
                build_burn_tx(&addresses[0], &addresses[index], BRN_UNIT, time(10)).unwrap(),
            ),
            params,
        )
        .await;
        submit(clients, &burn).await;
        let opening = signed_block(
            &clients[0],
            &keys[index],
            Transaction::Receive(
                build_receive_tx(
                    &addresses[index],
                    burn.hash.into_tx_hash(),
                    BRN_UNIT,
                    time(10),
                )
                .unwrap(),
            ),
            params,
        )
        .await;
        submit(clients, &opening).await;
        if index < 4 {
            let mut opted = opening.clone();
            opted.block_type = BlockType::VerifierOptIn;
            opted.previous = opening.hash;
            opted.link = BlockHash::ZERO;
            opted.origin = TxHash::ZERO;
            opted.transaction =
                burst_crypto::hash_transaction(format!("expiry-opt-in-{index}").as_bytes());
            opted.hash = opted.compute_hash();
            submit(clients, &sign_state_block(opted, &keys[index].private)).await;
        }
    }
    clock.store(time(20).as_secs(), Ordering::SeqCst);
    wait_human_authority(stores, 5).await;
    let mut inputs = Vec::new();
    for originator in [0, 4] {
        let burn = signed_block(
            &clients[0],
            &keys[originator],
            Transaction::Burn(
                build_burn_tx(&addresses[originator], &addresses[4], BRN_UNIT, time(20)).unwrap(),
            ),
            params,
        )
        .await;
        submit(clients, &burn).await;
        let received = signed_block(
            &clients[0],
            &keys[4],
            Transaction::Receive(
                build_receive_tx(&addresses[4], burn.hash.into_tx_hash(), BRN_UNIT, time(20))
                    .unwrap(),
            ),
            params,
        )
        .await;
        submit(clients, &received).await;
        inputs.push(received.hash.into_tx_hash());
    }
    let merge = signed_block(
        &clients[0],
        &keys[4],
        Transaction::Merge(build_merge_tx(&addresses[4], inputs, time(20)).unwrap()),
        params,
    )
    .await;
    submit(clients, &merge).await;
    let portfolio = clients[0]
        .rpc_call("trst_portfolio", json!({"account":addresses[4].as_str()}))
        .await
        .unwrap();
    let merged = portfolio["batches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|batch| batch["id"] == merge.hash.to_string())
        .unwrap();
    let origin = burst_wallet_core::signed_client::parse_hash(merged["origin"].as_str().unwrap())
        .unwrap()
        .into_tx_hash();
    let send = signed_block(
        &clients[0],
        &keys[4],
        Transaction::Send(
            build_send_tx(
                &addresses[4],
                &addresses[0],
                2 * BRN_UNIT,
                merge.hash.into_tx_hash(),
                origin,
                time(20),
            )
            .unwrap(),
        ),
        params,
    )
    .await;
    submit(clients, &send).await;
    assert_eq!(send.trst_balance, BRN_UNIT);
    eprintln!("{reason:?}: mixed-origin payment is confirmed and pending");
    clock.store(time(100).as_secs(), Ordering::SeqCst);
    let challenge = signed_block(
        &clients[0],
        &keys[0],
        Transaction::Challenge(
            build_challenge_tx(
                &addresses[0],
                &addresses[4],
                params.challenge_stake_amount,
                reason,
                time(100),
            )
            .unwrap(),
        ),
        params,
    )
    .await;
    submit(clients, &challenge).await;
    let (context, beacon_time) = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let events = chain::load(&stores[0], NetworkId::Dev, params).unwrap();
            if let Some(selection) = events
                .iter()
                .filter_map(chain::CertifiedEvent::selection)
                .find(|selection| selection.replayed().request().trigger() == challenge.hash)
            {
                let request = selection.replayed().request();
                assert_eq!(request.round(), 102);
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
    .unwrap_or_else(|error| {
        for (index, store) in stores.iter().enumerate() {
            let events = chain::load(store, NetworkId::Dev, params).unwrap();
            let anchor = chain::next_anchor(&events, NetworkId::Dev, params).unwrap();
            let state =
                crate::checkpoint_agreement::read_state(store, &anchor, &addresses[index]).unwrap();
            eprintln!(
                "selection timeout: node={index} height={} parent={} installed={:?} state={:?}",
                anchor.height() + 1,
                anchor.hash(),
                store.meta_store().payment_epoch(),
                state.as_ref().map(|state| (
                    state.round(),
                    state.step(),
                    state.prevote(),
                    state.precommit(),
                    state.decision().is_some(),
                )),
            );
        }
        panic!("challenge selection missing: {error}");
    });
    clock.store(beacon_time + 2, Ordering::SeqCst);
    let activation = pending_challenge::deadline::wait_for_activation(
        stores,
        params,
        &addresses[4],
        clock,
        context,
    )
    .await;
    assert_eq!(activation.deadline, deadline);
    let now = Timestamp::new(clock.load(Ordering::SeqCst));
    let vote = signed_block(
        &clients[0],
        &keys[1],
        Transaction::VerificationVote(
            build_verification_vote_tx(
                &addresses[1],
                &addresses[4],
                context,
                2,
                params.verifier_stake_amount,
                now,
            )
            .unwrap(),
        ),
        params,
    )
    .await;
    submit(clients, &vote).await;
    let late_receive = signed_block(
        &clients[0],
        &keys[0],
        Transaction::Receive(
            build_receive_tx(
                &addresses[0],
                send.hash.into_tx_hash(),
                2 * BRN_UNIT,
                Timestamp::new(deadline - 1),
            )
            .unwrap(),
        ),
        params,
    )
    .await;
    let before_head = clients[0].signing_state(&addresses[0]).await.unwrap().head;
    for store in stores {
        let pending = store
            .pending_store()
            .get_pending(&addresses[0], &send.hash.into_tx_hash())
            .unwrap();
        assert_eq!(pending.amount, 2 * BRN_UNIT);
        assert!(!pending.return_credit);
        assert!(store
            .account_store()
            .get_account(&addresses[4])
            .unwrap()
            .state
            .is_verified());
    }
    let partition = match registries {
        Some(registries) => Some(partition::Partition::split(registries, 2).await),
        None => None,
    };
    let before_epochs: Vec<_> = stores
        .iter()
        .map(|store| store.meta_store().payment_epoch().unwrap())
        .collect();
    clock.store(deadline + 2, Ordering::SeqCst);
    if let Some(partition) = partition {
        let until = tokio::time::Instant::now() + Duration::from_secs(6);
        while tokio::time::Instant::now() < until {
            for (store, epoch) in stores.iter().zip(&before_epochs) {
                assert_eq!(
                    &store.meta_store().payment_epoch().unwrap(),
                    epoch,
                    "a minority installed a new payment epoch during the partition"
                );
                assert!(matches!(
                    store
                        .meta_store()
                        .get_meta(&verification_deadline::key(context)),
                    Err(burst_store::StoreError::NotFound(_))
                ));
                assert!(store
                    .account_store()
                    .get_account(&addresses[4])
                    .unwrap()
                    .state
                    .is_verified());
                assert_eq!(
                    store
                        .pending_store()
                        .get_pending(&addresses[0], &send.hash.into_tx_hash())
                        .unwrap()
                        .amount,
                    2 * BRN_UNIT
                );
                assert_eq!(
                    store
                        .account_store()
                        .get_account(&addresses[1])
                        .unwrap()
                        .total_brn_staked,
                    params.verifier_stake_amount
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        partition.heal().await;
    }
    let expected_state = if reason == ChallengeReason::Fraud {
        WalletState::Revoked
    } else {
        WalletState::Deactivated
    };
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if stores.iter().all(|store| {
                store
                    .meta_store()
                    .get_meta(&verification_deadline::key(context))
                    .is_ok()
                    && store
                        .account_store()
                        .get_account(&addresses[4])
                        .is_ok_and(|a| a.state == expected_state)
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("challenge deadline and expiry did not install");
    let mut completion = None;
    for store in stores {
        let bytes = store
            .meta_store()
            .get_meta(&verification_deadline::key(context))
            .unwrap();
        let value = verification_deadline::decode(&bytes).unwrap();
        assert_eq!(value.completed_at, deadline + 1);
        assert_eq!(value.absent.len(), 2);
        if let Some(expected) = &completion {
            assert_eq!(&bytes, expected);
        } else {
            completion = Some(bytes);
        }
        assert!(store
            .pending_store()
            .get_pending(&addresses[0], &send.hash.into_tx_hash())
            .is_err());
        let returns = store
            .pending_store()
            .get_pending_for_account(&addresses[4])
            .unwrap();
        assert_eq!(returns.len(), 1);
        assert!(returns[0].return_credit);
        assert_eq!(returns[0].amount, 2 * BRN_UNIT);
        let account = store.account_store().get_account(&addresses[4]).unwrap();
        // Lifecycle effects cannot rewrite the owner's last signed balance.
        // The return-credit receipt accounts for the two returned units.
        assert_eq!(account.trst_balance, send.trst_balance);
        assert_eq!(
            account.revoked_trst,
            if reason == ChallengeReason::Fraud {
                BRN_UNIT
            } else {
                0
            }
        );
        assert_eq!(
            account.expired_trst,
            if reason == ChallengeReason::Fraud {
                2 * BRN_UNIT
            } else {
                3 * BRN_UNIT
            }
        );
        for address in &addresses {
            assert_eq!(
                store
                    .account_store()
                    .get_account(address)
                    .unwrap()
                    .total_brn_staked,
                0
            );
        }
    }
    for client in clients {
        assert!(
            !client
                .process(&serde_json::to_string(&late_receive).unwrap())
                .await
                .unwrap()
                .accepted
        );
        assert_eq!(
            client.signing_state(&addresses[0]).await.unwrap().head,
            before_head
        );
    }
    let forged_fresh = signed_block(
        &clients[0],
        &keys[0],
        Transaction::Receive(
            build_receive_tx(
                &addresses[0],
                send.hash.into_tx_hash(),
                2 * BRN_UNIT,
                Timestamp::new(deadline + 2),
            )
            .unwrap(),
        ),
        params,
    )
    .await;
    assert!(
        !clients[0]
            .process(&serde_json::to_string(&forged_fresh).unwrap())
            .await
            .unwrap()
            .accepted
    );
    let expected = post_challenge_recovery::capture(&clients[0], &addresses)
        .await
        .unwrap();
    assert_eq!(expected["accounts"][4]["portfolio"]["total_active"], "0");
    eprintln!("{reason:?}: agreed deadline, mixed return, and late-receive rejection pass");
    for client in clients {
        assert_eq!(
            post_challenge_recovery::capture(client, &addresses)
                .await
                .unwrap(),
            expected
        );
    }
    use crate::verification_commit::receipts;
    let mut agreed_receipt = None;
    for store in stores {
        let reference = store
            .meta_store()
            .get_meta(&receipts::latest_key(&addresses[4]))
            .unwrap();
        let source = BlockHash::new(reference.as_slice().try_into().unwrap());
        let receipt =
            receipts::decode(&store.meta_store().get_meta(&receipts::key(source)).unwrap())
                .unwrap();
        assert_eq!(receipt.target, addresses[4]);
        assert_eq!(receipt.completed_at.as_secs(), deadline + 1);
        assert_eq!(
            receipt.verdict,
            receipts::Verdict::Challenge {
                admission: challenge.hash,
                reason,
                result: receipts::ChallengeVerdict::Upheld,
            }
        );
        if reason == ChallengeReason::Fraud {
            assert_eq!(receipt.rewards.len(), 1);
            assert_eq!(receipt.rewards[0].recipient, addresses[0]);
            assert_eq!(receipt.rewards[0].amount, BRN_UNIT / 100);
        } else {
            assert!(receipt.rewards.is_empty());
        }
        if let Some(expected) = &agreed_receipt {
            assert_eq!(&receipt, expected);
        } else {
            agreed_receipt = Some(receipt);
        }
    }
    (addresses, expected, agreed_receipt.unwrap())
}
