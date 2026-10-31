use super::*;

pub(super) async fn capture(
    client: &NodeClient,
    addresses: &[WalletAddress],
) -> Result<serde_json::Value, String> {
    let mut verification = client
        .rpc_call(
            "verification_status",
            json!({"account":addresses[4].as_str()}),
        )
        .await
        .map_err(|error| error.to_string())?;
    if let Some(votes) = verification["verifier_votes"].as_array_mut() {
        votes.sort_by(|left, right| left["verifier"].as_str().cmp(&right["verifier"].as_str()));
    }
    let mut accounts = Vec::new();
    for address in addresses {
        let signing = client
            .inspect_signing_state(address)
            .await
            .map_err(|error| error.to_string())?;
        let components = client
            .rpc_call("brn_components", json!({"account":address.as_str()}))
            .await
            .map_err(|error| error.to_string())?;
        let mut portfolio = client
            .rpc_call("trst_portfolio", json!({"account":address.as_str()}))
            .await
            .map_err(|error| error.to_string())?;
        if let Some(batches) = portfolio["batches"].as_array_mut() {
            batches.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
        }
        let mut pending = client
            .rpc_call("account_pending", json!({"account":address.as_str()}))
            .await
            .map_err(|error| error.to_string())?;
        if let Some(entries) = pending["pending"].as_array_mut() {
            entries.sort_by(|left, right| left["hash"].as_str().cmp(&right["hash"].as_str()));
        }
        accounts.push(json!({"head":signing.head.to_string(),
            "payment_epoch":signing.payment_epoch.to_string(),
            "brn_counter":signing.brn_spent.to_string(), "trst_balance":signing.trst_balance.to_string(),
            "staked":components["total_staked"], "burned":components["total_burned"],
            "release_sources":components["release_sources"],
            "accrual_active":components["accrual_active"],
            "accrual_stopped_at":components["accrual_stopped_at"],
            "paused_intervals":components["paused_intervals"],
            "verified_at":components["verified_at"],
            "portfolio":portfolio, "pending":pending}));
    }
    Ok(json!({"verification":verification,"accounts":accounts}))
}

pub(super) async fn run(
    mut configs: Vec<NodeConfig>,
    clock: Arc<AtomicU64>,
    relay: &str,
    addresses: Vec<WalletAddress>,
    expected: serde_json::Value,
    expected_receipt: Option<crate::verification_commit::receipts::Receipt>,
) {
    let mut observer = configs.last().unwrap().clone();
    observer.data_dir = observer
        .data_dir
        .parent()
        .unwrap()
        .join("after-challenge-observer");
    observer.port = port();
    observer.rpc_port = port();
    observer.enable_representative = false;
    observer.representative_key_file = None;
    observer.bootstrap_peers = configs
        .iter()
        .map(|config| format!("127.0.0.1:{}", config.port))
        .collect();
    configs.push(observer);
    let mut workers = Vec::new();
    let mut clients = Vec::new();
    let mut stores = Vec::new();
    for config in configs {
        clients.push(NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap());
        let mut node = fixture_node(config, &clock, relay).await;
        stores.push(node.store.clone());
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
    let result = tokio::time::timeout(Duration::from_secs(180), async {
        for (index, client) in clients.iter().enumerate() {
            let mut last = None;
            let mut last_frontier = None;
            loop {
                // Avoid repeated full balance captures while signed heads are
                // still missing. The complete economic comparison remains below.
                let frontier = pending_frontier(client, &addresses, &expected).await;
                if !matches!(frontier, Ok(None)) {
                    if last_frontier.as_ref() != Some(&frontier) {
                        eprintln!("challenge recovery node {index} waiting: {frontier:?}");
                        last_frontier = Some(frontier);
                    }
                    assert!(!workers[index].1.is_finished(), "recovery node {index} stopped early");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                let observed = capture(client, &addresses).await;
                if observed
                    .as_ref()
                    .is_ok_and(|observed| *observed == expected)
                {
                    for account in expected["accounts"].as_array().unwrap() {
                        client.wait_confirmed(account["head"].as_str().unwrap(), Duration::from_secs(25))
                            .await.unwrap_or_else(|error| panic!("recovery node {index} did not cement its recovered head: {error}"));
                    }
                    if let Some(receipt) = &expected_receipt {
                        use crate::verification_commit::receipts;
                        let bytes = stores[index].meta_store().get_meta(&receipts::key(receipt.source)).unwrap();
                        assert_eq!(&receipts::decode(&bytes).unwrap(), receipt,
                            "recovered outcome receipt differs on node {index}");
                        assert_eq!(stores[index].meta_store().get_meta(&receipts::latest_key(&receipt.target)).unwrap(),
                            receipt.source.as_bytes());
                    }
                    eprintln!("challenge recovery node {index} matched");
                    break;
                }
                if last.as_ref() != Some(&observed) {
                    eprintln!("challenge recovery node {index}: {observed:?}");
                    last = Some(observed);
                }
                assert!(
                    !workers[index].1.is_finished(),
                    "recovery node {index} stopped early"
                );
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    })
    .await;
    if result.is_err() {
        eprintln!("expected challenge recovery: {expected}");
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
    result.expect("settled challenge was not recovered by restarted nodes and a fresh observer");
}

async fn pending_frontier(
    client: &NodeClient,
    addresses: &[WalletAddress],
    expected: &serde_json::Value,
) -> Result<Option<(usize, BlockHash, BlockHash)>, String> {
    for (index, address) in addresses.iter().enumerate() {
        let signing = client
            .inspect_signing_state(address)
            .await
            .map_err(|error| error.to_string())?;
        let account = &expected["accounts"][index];
        if account["head"] != signing.head.to_string()
            || account["payment_epoch"] != signing.payment_epoch.to_string()
        {
            return Ok(Some((index, signing.head, signing.payment_epoch)));
        }
    }
    Ok(None)
}
