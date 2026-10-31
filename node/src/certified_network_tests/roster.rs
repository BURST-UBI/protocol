use super::*;
use crate::{checkpoint_store::chain, payment_authority::PaymentAuthority};

async fn wait_for_installation(
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    height: u64,
    params: &ProtocolParams,
) -> BlockHash {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let mut epochs = Vec::new();
            for store in stores {
                let events = chain::load(store, NetworkId::Dev, params).unwrap();
                if let Some(event) = events.get(height as usize - 1) {
                    let authority = PaymentAuthority::read(store).unwrap().unwrap();
                    if authority.epoch == event.certificate().checkpoint.hash() {
                        assert!(matches!(event, chain::CertifiedEvent::Roster(_)));
                        epochs.push(authority.epoch);
                    }
                }
            }
            if epochs.len() == stores.len() && epochs.iter().all(|epoch| *epoch == epochs[0]) {
                return epochs[0];
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("roster authority did not install on every node")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn roster_updates_without_a_committee_and_recovers_on_a_fresh_node() {
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
    let params = ProtocolParams {
        brn_rate: BRN_UNIT,
        min_work_difficulty: 0,
        bootstrap_exit_threshold: 3,
        endorsement_burn_amount: 1,
        new_wallet_spending_limit: 0,
        trst_expiry_secs: u64::MAX,
        ..ProtocolParams::burst_defaults()
    };
    let ports = [port(), port()];
    let mut clients = Vec::new();
    let mut stores = Vec::new();
    let mut configs = Vec::new();
    let mut workers = Vec::new();
    for index in 0..2 {
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
            bootstrap_peers: vec![format!("127.0.0.1:{}", ports[1 - index])],
            ..NodeConfig::default()
        };
        clients.push(NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap());
        configs.push(config.clone());
        // No committee is requested, so a beacon relay must not be needed.
        let mut node = fixture_node(config, &clock, "http://127.0.0.1:1").await;
        stores.push(node.store.clone());
        workers.push((
            node.shutdown.clone(),
            tokio::spawn(async move {
                node.start_with_services(burst_daemon::start_public_services)
                    .await
                    .unwrap();
                node.stop().await.unwrap();
            }),
        ));
    }
    let first_clock = clock.clone();
    let first_params = params.clone();
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
        let keys: Vec<_> = (0..2)
            .map(|seed| burst_crypto::keypair_from_seed(&[seed; 32]))
            .collect();
        let addresses: Vec<_> = keys
            .iter()
            .map(|key| burst_crypto::derive_address(&key.public))
            .collect();
        let params = first_params;
        let tx = Transaction::Endorse(
            build_endorse_tx(&addresses[0], &addresses[1], 1, time(10)).unwrap(),
        );
        submit(
            &clients,
            &signed_block(&clients[0], &keys[0], tx, &params).await,
        )
        .await;
        let tx = Transaction::Burn(
            build_burn_tx(&addresses[0], &addresses[1], BRN_UNIT, time(10)).unwrap(),
        );
        let burn = signed_block(&clients[0], &keys[0], tx, &params).await;
        submit(&clients, &burn).await;
        let tx = Transaction::Receive(
            build_receive_tx(&addresses[1], burn.hash.into_tx_hash(), BRN_UNIT, time(10)).unwrap(),
        );
        submit(
            &clients,
            &signed_block(&clients[0], &keys[1], tx, &params).await,
        )
        .await;
        let tx =
            Transaction::Burn(build_burn_tx(&addresses[0], &addresses[1], 1, time(20)).unwrap());
        let old_payment = signed_block(&clients[0], &keys[0], tx, &params).await;
        first_clock.store(time(20).as_secs(), Ordering::SeqCst);
        let epoch = wait_for_installation(&stores, 1, &params).await;
        for (store, client) in stores.iter().zip(&clients) {
            let authority = PaymentAuthority::read(store).unwrap().unwrap();
            assert_eq!(authority.roster.total_humans(), 2);
            assert_eq!(authority.roster.weight(&addresses[1]), 1);
            assert!(
                !client
                    .process(&serde_json::to_string(&old_payment).unwrap())
                    .await
                    .unwrap()
                    .accepted
            );
        }
        let tx = Transaction::ChangeRepresentative(
            build_change_rep_tx(&addresses[1], &addresses[0], time(20)).unwrap(),
        );
        let change = signed_block(&clients[0], &keys[1], tx, &params).await;
        assert_eq!(change.payment_epoch, epoch);
        submit(&clients, &change).await;
        first_clock.store(time(30).as_secs(), Ordering::SeqCst);
        let next_epoch = wait_for_installation(&stores, 2, &params).await;
        assert_ne!(epoch, next_epoch);
        for store in &stores {
            let authority = PaymentAuthority::read(store).unwrap().unwrap();
            assert_eq!(authority.roster.weight(&addresses[0]), 2);
            assert_eq!(authority.roster.weight(&addresses[1]), 0);
        }
        let tx = Transaction::Burn(
            build_burn_tx(&addresses[1], &addresses[0], BRN_UNIT, time(30)).unwrap(),
        );
        let payment = signed_block(&clients[0], &keys[1], tx, &params).await;
        assert_eq!(payment.payment_epoch, next_epoch);
        submit(&clients, &payment).await;
        (addresses, payment, next_epoch)
    });
    let result = tokio::time::timeout(Duration::from_secs(180), &mut journey).await;
    if result.is_err() {
        journey.abort();
        let _ = journey.await;
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
    let (addresses, payment, epoch) = result.expect("live roster journey timed out").unwrap();
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
    configs.push(observer);
    let mut clients = Vec::new();
    let mut stores = Vec::new();
    for config in configs {
        clients.push(NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap());
        let mut node = fixture_node(config, &clock, "http://127.0.0.1:1").await;
        stores.push(node.store.clone());
        workers.push((
            node.shutdown.clone(),
            tokio::spawn(async move {
                node.start_with_services(burst_daemon::start_public_services)
                    .await
                    .unwrap();
                node.stop().await.unwrap();
            }),
        ));
    }
    let result = tokio::time::timeout(Duration::from_secs(120), async {
        assert_eq!(wait_for_installation(&stores, 2, &params).await, epoch);
        for client in &clients {
            client
                .wait_confirmed(&payment.hash.to_string(), Duration::from_secs(40))
                .await
                .unwrap();
            let state = client.signing_state(&addresses[1]).await.unwrap();
            assert_eq!(state.payment_epoch, epoch);
            assert_eq!(state.head, payment.hash);
            assert_eq!(state.brn_spent, payment.brn_spent);
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
    result.expect("roster recovery failed");
}
