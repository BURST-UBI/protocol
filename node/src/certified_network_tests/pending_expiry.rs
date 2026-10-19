use super::*;
use crate::payment_authority::PaymentAuthority;
use burst_store::{account::AccountStore, pending::PendingStore};

async fn returned(
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    sender: &WalletAddress,
) -> BlockHash {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let mut epochs = Vec::new();
            for store in stores {
                let account = match store.account_store().get_account(sender) {
                    Ok(account) => account,
                    Err(burst_store::StoreError::NotFound(_)) => continue,
                    Err(error) => panic!("cannot read expiry account: {error}"),
                };
                if account.expired_trst == BRN_UNIT {
                    assert_eq!(account.trst_balance, 0);
                    assert_eq!(account.revoked_trst, 0);
                    assert_eq!(store.pending_store().pending_count().unwrap(), 1);
                    assert!(
                        store
                            .pending_store()
                            .get_pending_for_account(sender)
                            .unwrap()[0]
                            .return_credit
                    );
                    assert!(store
                        .pending_store()
                        .get_pending_from_account(sender)
                        .unwrap()
                        .is_empty());
                    epochs.push(PaymentAuthority::read(store).unwrap().unwrap().epoch);
                }
            }
            if epochs.len() == stores.len() && epochs.iter().all(|epoch| *epoch == epochs[0]) {
                return epochs[0];
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("certified pending return did not install on every node")
}

async fn assert_pending_ownership(
    clients: &[NodeClient],
    sender: &WalletAddress,
    receiver: &WalletAddress,
    pending: u128,
    expired: u128,
    received: u128,
) {
    for client in clients {
        let owned = client
            .rpc_call("trst_portfolio", json!({"account":sender.as_str()}))
            .await
            .unwrap();
        assert_eq!(owned["total_pending"], pending.to_string());
        assert_eq!(owned["pending_count"], u64::from(pending > 0));
        assert_eq!(owned["total_active"], "0");
        assert_eq!(owned["total_expired"], expired.to_string());
        assert_eq!(owned["total_revoked"], "0");
        let unclaimed = client
            .rpc_call("trst_portfolio", json!({"account":receiver.as_str()}))
            .await
            .unwrap();
        assert_eq!(unclaimed["total_active"], received.to_string());
        for field in ["total_pending", "total_expired", "total_revoked"] {
            assert_eq!(
                unclaimed[field], "0",
                "pending ownership leaked to receiver: {field}"
            );
        }
    }
}

async fn received(
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    sender: &WalletAddress,
    receiver: &WalletAddress,
    head: BlockHash,
) -> BlockHash {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let mut epochs = Vec::new();
            for store in stores {
                let account = match store.account_store().get_account(receiver) {
                    Ok(account) => account,
                    Err(burst_store::StoreError::NotFound(_)) => continue,
                    Err(error) => panic!("cannot read received account: {error}"),
                };
                if account.head != head {
                    continue;
                }
                assert_eq!(account.trst_balance, BRN_UNIT);
                assert_eq!(
                    store
                        .account_store()
                        .get_account(sender)
                        .unwrap()
                        .trst_balance,
                    0
                );
                assert_eq!(store.pending_store().pending_count().unwrap(), 0);
                assert!(store
                    .pending_store()
                    .get_pending_from_account(sender)
                    .unwrap()
                    .is_empty());
                epochs.push(PaymentAuthority::read(store).unwrap().unwrap().epoch);
            }
            if epochs.len() == stores.len() && epochs.iter().all(|epoch| *epoch == epochs[0]) {
                return epochs[0];
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("received ownership did not reach every node")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pending_expiry_returns_once_and_recovers_after_restart_and_fresh_catchup() {
    pending_journey(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pending_receipt_moves_ownership_once_and_recovers_after_restart_and_fresh_catchup() {
    pending_journey(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed native wallet, WebKitWebDriver and libfaketime"]
async fn desktop_expired_merge_preserves_reputation_across_node_restart() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(AtomicU64::new(time(10).as_secs()));
    let params = ProtocolParams {
        brn_rate: BRN_UNIT,
        min_work_difficulty: 0,
        trst_expiry_secs: 5,
        new_wallet_spending_limit: 0,
        new_wallet_tx_limit_per_day: 0,
        ..ProtocolParams::burst_defaults()
    };
    let ports = [port(), port()];
    let mut configs = Vec::new();
    let mut clients = Vec::new();
    let mut workers = Vec::new();
    for index in 0..2 {
        let config = NodeConfig {
            network: NetworkId::Dev,
            data_dir: directory.path().join(format!("node-{index}")),
            port: ports[index],
            rpc_port: port(),
            enable_representative: index == 0,
            enable_upnp: false,
            nts_sources: vec![],
            params: params.clone(),
            bootstrap_peers: vec![format!("127.0.0.1:{}", ports[1 - index])],
            ..NodeConfig::default()
        };
        clients.push(NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap());
        configs.push(config.clone());
        let mut node = fixture_node(config, &clock, "http://127.0.0.1:1").await;
        workers.push((
            node.shutdown.clone(),
            tokio::spawn(async move {
                node.start().await.unwrap();
                node.stop().await.unwrap();
            }),
        ));
    }
    let result = tokio::time::timeout(Duration::from_secs(230), async {
        for client in &clients {
            while !client
                .rpc_call("peers", json!({}))
                .await
                .is_ok_and(|peers| peers["count"].as_u64().unwrap_or(0) >= 1)
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let key = burst_crypto::keypair_from_seed(&[0; 32]);
        let owner = burst_crypto::derive_address(&key.public);
        for amount in [7, 11] {
            let burn = signed_block(
                &clients[0],
                &key,
                Transaction::Burn(build_burn_tx(&owner, &owner, amount, time(10)).unwrap()),
                &params,
            )
            .await;
            submit(&clients, &burn).await;
            let receive = signed_block(
                &clients[0],
                &key,
                Transaction::Receive(
                    build_receive_tx(&owner, burn.hash.into_tx_hash(), amount, time(10)).unwrap(),
                ),
                &params,
            )
            .await;
            submit(&clients, &receive).await;
        }
        clock.store(time(11).as_secs(), Ordering::SeqCst);
        let result_path = directory.path().join("native-result.json");
        let runner = std::env::var_os("NATIVE_WALLET_TEST_RUNNER")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../wallet/tests/native-session.mjs")
            });
        let output = tokio::process::Command::new("node")
            .arg(runner)
            .args(["--expired-merge-fixture", "--embedded"])
            .env("NATIVE_FIXTURE_RPC_URL", clients[0].node_url())
            .env("NATIVE_RESULT_PATH", &result_path)
            .env("NATIVE_RETURN_TIME", time(11).as_secs().to_string())
            .kill_on_drop(true)
            .output()
            .await
            .unwrap();
        eprintln!("{}", String::from_utf8_lossy(&output.stdout));
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(
            output.status.success(),
            "native expired-merge journey failed"
        );
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(result_path).unwrap()).unwrap();
        assert_eq!(report["address"], owner.as_str());
        for client in &clients {
            client
                .wait_confirmed(report["hash"].as_str().unwrap(), Duration::from_secs(25))
                .await
                .unwrap();
            assert_eq!(
                client
                    .rpc_call("trst_portfolio", json!({"account":owner.as_str()}))
                    .await
                    .unwrap(),
                report["portfolio"]
            );
        }
        report
    })
    .await;
    super::governance_certified::stop(&mut workers).await;
    let report = result.expect("expired-merge native journey timed out");
    clients.clear();
    for config in configs {
        clients.push(NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap());
        let mut node = fixture_node(config, &clock, "http://127.0.0.1:1").await;
        workers.push((
            node.shutdown.clone(),
            tokio::spawn(async move {
                node.start().await.unwrap();
                node.stop().await.unwrap();
            }),
        ));
    }
    let recovered = tokio::time::timeout(Duration::from_secs(45), async {
        for client in clients {
            loop {
                if client
                    .rpc_call("block_info", json!({"hash":report["hash"]}))
                    .await
                    .is_ok_and(|block| block["confirmed"] == true)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            assert_eq!(
                client
                    .rpc_call("trst_portfolio", json!({"account":report["address"]}))
                    .await
                    .unwrap(),
                report["portfolio"]
            );
            assert_eq!(
                client
                    .rpc_call(
                        "trst_origins",
                        json!({"origin":report["portfolio"]["batches"][0]["origin"]})
                    )
                    .await
                    .unwrap(),
                report["origins"]
            );
        }
    })
    .await;
    super::governance_certified::stop(&mut workers).await;
    recovered.expect("expired merge did not survive node restart");
}

async fn pending_journey(accept: bool) {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let clock = Arc::new(AtomicU64::new(time(10).as_secs()));
    let params = ProtocolParams {
        brn_rate: BRN_UNIT,
        min_work_difficulty: 0,
        trst_expiry_secs: if accept { u64::MAX } else { 5 },
        new_wallet_spending_limit: 0,
        new_wallet_tx_limit_per_day: 0,
        ..ProtocolParams::burst_defaults()
    };
    let ports = [port(), port()];
    let mut configs = Vec::new();
    let mut clients = Vec::new();
    let mut stores = Vec::new();
    let mut workers = Vec::new();
    for index in 0..2 {
        let config = NodeConfig {
            network: NetworkId::Dev,
            data_dir: directory.path().join(format!("node-{index}")),
            port: ports[index],
            rpc_port: port(),
            enable_representative: index == 0,
            enable_upnp: false,
            nts_sources: vec![],
            params: params.clone(),
            bootstrap_peers: vec![format!("127.0.0.1:{}", ports[1 - index])],
            ..NodeConfig::default()
        };
        clients.push(NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap());
        configs.push(config.clone());
        let mut node = fixture_node(config, &clock, "http://127.0.0.1:1").await;
        stores.push(node.store.clone());
        workers.push((
            node.shutdown.clone(),
            tokio::spawn(async move {
                node.start().await.unwrap();
                node.stop().await.unwrap();
            }),
        ));
    }
    let observed_stores = stores.clone();
    let observed_clock = clock.clone();
    let observed_params = params.clone();
    let mut journey = tokio::spawn(async move {
        for client in &clients {
            loop {
                if client
                    .rpc_call("peers", json!({}))
                    .await
                    .is_ok_and(|value| value["count"].as_u64().unwrap_or(0) >= 1)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let sender_key = burst_crypto::keypair_from_seed(&[0; 32]);
        let receiver_key = burst_crypto::keypair_from_seed(&[1; 32]);
        let sender = burst_crypto::derive_address(&sender_key.public);
        let receiver = burst_crypto::derive_address(&receiver_key.public);
        let burn = signed_block(
            &clients[0],
            &sender_key,
            Transaction::Burn(build_burn_tx(&sender, &receiver, BRN_UNIT, time(10)).unwrap()),
            &observed_params,
        )
        .await;
        submit(&clients, &burn).await;
        assert_pending_ownership(&clients, &sender, &receiver, BRN_UNIT, 0, 0).await;
        let late_receive = signed_block(
            &clients[0],
            &receiver_key,
            Transaction::Receive(
                build_receive_tx(&receiver, burn.hash.into_tx_hash(), BRN_UNIT, time(14)).unwrap(),
            ),
            &observed_params,
        )
        .await;
        for store in &observed_stores {
            assert_eq!(store.pending_store().pending_count().unwrap(), 1);
            let outgoing = store
                .pending_store()
                .get_pending_from_account(&sender)
                .unwrap();
            assert_eq!(outgoing.len(), 1);
            assert_eq!(
                (&outgoing[0].0, outgoing[0].1, outgoing[0].2.amount),
                (&receiver, burn.hash.into_tx_hash(), BRN_UNIT)
            );
            assert_eq!(
                store
                    .account_store()
                    .get_account(&sender)
                    .unwrap()
                    .expired_trst,
                0
            );
        }
        if accept {
            observed_clock.store(time(14).as_secs(), Ordering::SeqCst);
            submit(&clients, &late_receive).await;
            let epoch = received(&observed_stores, &sender, &receiver, late_receive.hash).await;
            assert_eq!(epoch, burn.payment_epoch);
            assert_pending_ownership(&clients, &sender, &receiver, 0, 0, BRN_UNIT).await;
            let duplicate_claim = signed_block(
                &clients[0],
                &receiver_key,
                Transaction::Receive(
                    build_receive_tx(&receiver, burn.hash.into_tx_hash(), BRN_UNIT, time(14))
                        .unwrap(),
                ),
                &observed_params,
            )
            .await;
            assert_ne!(duplicate_claim.hash, late_receive.hash);
            for client in &clients {
                assert!(
                    !client
                        .process(&serde_json::to_string(&duplicate_claim).unwrap())
                        .await
                        .unwrap()
                        .accepted
                );
            }
            assert_pending_ownership(&clients, &sender, &receiver, 0, 0, BRN_UNIT).await;
            return (sender, receiver, late_receive.hash, epoch);
        }
        observed_clock.store(time(20).as_secs(), Ordering::SeqCst);
        let epoch = returned(&observed_stores, &sender).await;
        assert_pending_ownership(&clients, &sender, &receiver, 0, BRN_UNIT, 0).await;
        assert_ne!(epoch, burn.payment_epoch);
        for client in &clients {
            assert!(
                !client
                    .process(&serde_json::to_string(&late_receive).unwrap())
                    .await
                    .unwrap()
                    .accepted
            );
        }
        (sender, receiver, burn.hash, epoch)
    });
    let result = tokio::time::timeout(Duration::from_secs(100), &mut journey).await;
    if result.is_err() {
        journey.abort();
    }
    for (shutdown, _) in &workers {
        shutdown.shutdown();
    }
    for (_, worker) in workers.drain(..) {
        tokio::time::timeout(Duration::from_secs(20), worker)
            .await
            .unwrap()
            .unwrap();
    }
    let (sender, receiver, final_block, epoch) = result
        .expect("pending ownership journey timed out")
        .unwrap();
    stores.clear();
    let mut observer = configs[1].clone();
    observer.data_dir = directory.path().join("fresh-observer");
    observer.port = port();
    observer.rpc_port = port();
    observer.bootstrap_peers = ports
        .iter()
        .map(|port| format!("127.0.0.1:{port}"))
        .collect();
    configs.push(observer);
    let mut clients = Vec::new();
    for config in configs {
        clients.push(NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap());
        let mut node = fixture_node(config, &clock, "http://127.0.0.1:1").await;
        stores.push(node.store.clone());
        workers.push((
            node.shutdown.clone(),
            tokio::spawn(async move {
                node.start().await.unwrap();
                node.stop().await.unwrap();
            }),
        ));
    }
    let result = tokio::time::timeout(Duration::from_secs(90), async {
        if accept {
            assert_eq!(
                received(&stores, &sender, &receiver, final_block).await,
                epoch
            );
            assert_pending_ownership(&clients, &sender, &receiver, 0, 0, BRN_UNIT).await;
        } else {
            assert_eq!(returned(&stores, &sender).await, epoch);
            assert_pending_ownership(&clients, &sender, &receiver, 0, BRN_UNIT, 0).await;
        }
        for client in clients {
            client
                .wait_confirmed(&final_block.to_string(), Duration::from_secs(20))
                .await
                .unwrap();
            assert_eq!(
                client.signing_state(&sender).await.unwrap().payment_epoch,
                epoch
            );
        }
    })
    .await;
    for (shutdown, _) in &workers {
        shutdown.shutdown();
    }
    for (_, worker) in workers {
        tokio::time::timeout(Duration::from_secs(20), worker)
            .await
            .unwrap()
            .unwrap();
    }
    result.expect("pending ownership restart or fresh catchup failed");
}
