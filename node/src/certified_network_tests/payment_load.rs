//! Load generation on a bounded signed multi-account network fixture.
use super::*;
use burst_store::account::AccountStore;
use std::time::Instant;
use tokio::sync::mpsc;

fn load_clients(client: &NodeClient, accounts: usize, independent: bool) -> Vec<NodeClient> {
    assert!((3..=32).contains(&accounts));
    (0..accounts).map(|index| {
        if independent {
            client.clone().with_local_address(std::net::Ipv4Addr::new(
                127, 0, 0, index as u8 + 2).into()).expect("bind independent load client")
        } else { client.clone() }
    }).collect()
}

#[tokio::test]
async fn independent_client_addresses_reach_transport_and_keep_rpc_errors() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = NodeClient::new(format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let mut clients = load_clients(&client, 3, true);
    clients.push(client);
    let server = tokio::spawn(async move {
        let mut peers = Vec::new();
        for index in 0..4 {
            let (mut stream, peer) = listener.accept().await.unwrap();
            peers.push(peer.ip());
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 1024];
                let n = stream.read(&mut bytes).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&bytes[..n]);
                assert!(request.len() <= 4096);
                if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&request[..end]).unwrap();
                    let length = headers.lines().find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
                    }).unwrap();
                    if request.len() >= end + 4 + length { break; }
                }
            }
            let (status, body) = if index == 2 {
                ("429 Too Many Requests", r#"{"error":"rate limited"}"#)
            } else { ("200 OK", r#"{"result":{"ok":true}}"#) };
            stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
        peers
    });
    let requests = async {
        for (index, client) in clients.iter().enumerate() {
            let result = client.rpc_call("probe", json!({})).await;
            if index == 2 {
                assert!(matches!(result, Err(burst_wallet_core::WalletError::Node(message))
                    if message.contains("429 Too Many Requests")));
            } else { assert_eq!(result.unwrap(), json!({"ok":true})); }
        }
        assert_eq!(server.await.unwrap(), [2, 3, 4, 1].into_iter().map(|id|
            std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, id))).collect::<Vec<_>>());
    };
    tokio::time::timeout(Duration::from_secs(5), requests).await.unwrap();
}

struct Published {
    block: StateBlock,
    started: Instant,
    accepted_ms: f64,
    observed: Vec<bool>,
}

async fn seed_fragmented_wallet(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    key: &KeyPair,
    params: &ProtocolParams,
    now: Timestamp,
) -> (TxHash, TxHash) {
    let fragments = 8u128;
    let owner = burst_crypto::derive_address(&key.public);
    let seed_started = Instant::now();
    let burn = signed_block(&clients[0], key, Transaction::Burn(
        build_burn_tx(&owner, &owner, fragments, now).unwrap()), params).await;
    submit(clients, &burn).await;
    let origin = burn.hash.into_tx_hash();
    let receive = signed_block(&clients[0], key, Transaction::Receive(
        build_receive_tx(&owner, origin, fragments, now).unwrap()), params).await;
    submit(clients, &receive).await;
    let mut link = receive.hash.into_tx_hash();
    for _ in 1..fragments {
        let send = signed_block(&clients[0], key, Transaction::Send(
            build_send_tx(&owner, &owner, 1, link, origin, now).unwrap()), params).await;
        submit(clients, &send).await;
        let receive = signed_block(&clients[0], key, Transaction::Receive(
            build_receive_tx(&owner, send.hash.into_tx_hash(), 1, now).unwrap()), params).await;
        submit(clients, &receive).await;
        link = receive.hash.into_tx_hash();
    }
    assert_fragmented_holdings(stores, &owner, origin, params);
    println!("fragmented_send_seed fragments={fragments} elapsed_ms={}", seed_started.elapsed().as_millis());
    (origin, link)
}

fn assert_fragmented_holdings(
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    owner: &WalletAddress,
    origin: TxHash,
    params: &ProtocolParams,
) {
    for store in stores {
        let batch = store.tx_begin_write().unwrap();
        let holdings = crate::trst_storage::load(&batch, params.trst_expiry_secs).unwrap();
        let tokens: Vec<_> = holdings.get_portfolio(owner).unwrap().tokens.iter()
            .filter(|token| token.origin == origin).collect();
        assert_eq!(tokens.len(), 8, "fixture must exercise fragmented debit");
        assert_eq!(tokens.iter().map(|token| token.amount).sum::<u128>(), 8);
        assert!(tokens.iter().all(|token| token.amount == 1));
    }
}

pub(super) async fn fragmented_send_round_trip(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    keys: &[KeyPair],
    params: &ProtocolParams,
    now: Timestamp,
) {
    let fragments = 8u128;
    let owner = burst_crypto::derive_address(&keys[1].public);
    let receiver = burst_crypto::derive_address(&keys[2].public);
    let before: Vec<_> = [&owner, &receiver].into_iter().map(|address|
        stores[0].account_store().get_account(address).unwrap()).collect();
    let (origin, mut link) = seed_fragmented_wallet(clients, stores, &keys[1], params, now).await;
    let transfer_started = Instant::now();
    for (from, to, key) in [(&owner, &receiver, &keys[1]), (&receiver, &owner, &keys[2])] {
        let send = signed_block(&clients[0], key, Transaction::Send(
            build_send_tx(from, to, fragments, link, origin, now).unwrap()), params).await;
        submit(clients, &send).await;
        let mut expected = None;
        for store in stores {
            let batch = store.tx_begin_write().unwrap();
            let pending = batch.get_pending(to, &send.hash.into_tx_hash()).unwrap();
            assert_eq!(pending.amount, fragments);
            assert_eq!(pending.provenance.iter().map(|part| part.amount).sum::<u128>(), fragments);
            assert!(pending.provenance.iter().all(|part| part.origin == origin));
            let bytes = bincode::serialize(&pending).unwrap();
            if let Some(expected) = &expected { assert_eq!(&bytes, expected); }
            else { expected = Some(bytes); }
        }
        let receiving_key = if to == &owner { &keys[1] } else { &keys[2] };
        let receive = signed_block(&clients[0], receiving_key, Transaction::Receive(
            build_receive_tx(to, send.hash.into_tx_hash(), fragments, now).unwrap()), params).await;
        submit(clients, &receive).await;
        link = receive.hash.into_tx_hash();
        for store in stores {
            let batch = store.tx_begin_write().unwrap();
            assert!(matches!(batch.get_pending(to, &send.hash.into_tx_hash()),
                Err(burst_store::StoreError::NotFound(_))));
        }
    }
    for store in stores {
        for (index, address) in [&owner, &receiver].into_iter().enumerate() {
            let account = store.account_store().get_account(address).unwrap();
            let added = if index == 0 { fragments } else { 0 };
            assert_eq!(account.trst_balance, before[index].trst_balance + added);
            assert_eq!(account.total_brn_burned, before[index].total_brn_burned + added);
            assert_eq!(account.block_count, before[index].block_count +
                if index == 0 { 2 * fragments as u64 + 2 } else { 2 });
            assert_eq!(account.confirmation_height, account.block_count);
        }
    }
    println!("fragmented_send_round_trip fragments={fragments} peers={} elapsed_ms={} scope=functional-not-throughput",
        stores.len(), transfer_started.elapsed().as_millis());
}

fn observe_confirmations(pending: &mut [Published], stores: &[Arc<burst_store_lmdb::LmdbStore>]) {
    for (peer, store) in stores.iter().enumerate() {
        let snapshot = store.tx_begin_read().unwrap();
        let accounts = store.account_store();
        let blocks = store.block_store();
        let mut confirmed = std::collections::HashMap::new();
        for item in pending.iter_mut().filter(|item| !item.observed[peer]) {
            let confirmed_height = *confirmed.entry(&item.block.account).or_insert_with(|| {
                accounts.get_account_txn(&snapshot, &item.block.account).unwrap().confirmation_height
            });
            if let Some(height) = blocks.height_of_block_txn(&snapshot, &item.block.hash).unwrap() {
                if confirmed_height >= height {
                    assert_eq!(blocks.block_at_height_txn(&snapshot, &item.block.account, height).unwrap(),
                        Some(item.block.hash));
                    item.observed[peer] = true;
                }
            }
        }
    }
}

#[test]
fn confirmation_observer_requires_each_peers_canonical_confirmed_height() {
    let params = burst_ledger::genesis::initial_params(&NetworkId::Dev);
    let first = crate::genesis_state::block(NetworkId::Dev, &params);
    let mut second = first.clone();
    second.previous = first.hash;
    second.timestamp = Timestamp::new(first.timestamp.as_secs() + 1);
    second.hash = second.compute_hash();
    let mut fork = second.clone();
    fork.timestamp = Timestamp::new(second.timestamp.as_secs() + 1);
    fork.hash = fork.compute_hash();
    let directories: Vec<_> = (0..2).map(|_| tempfile::tempdir().unwrap()).collect();
    let stores: Vec<_> = directories.iter().map(|dir|
        Arc::new(burst_store_lmdb::LmdbStore::open(dir.path(), 32, 16 << 20).unwrap())).collect();
    let install = |store: &burst_store_lmdb::LmdbStore, head: &StateBlock, confirmed: u64| {
        let mut batch = store.tx_begin_write().unwrap();
        if let Some(previous) = batch.block_at_height(&first.account, 2).unwrap() {
            if previous != head.hash { batch.delete_block(&previous).unwrap(); }
        }
        for (block, height) in [(&first, 1), (head, 2)] {
            batch.put_block_with_account(&block.hash, &bincode::serialize(block).unwrap(),
                &block.account, height).unwrap();
        }
        let mut account = burst_store::account::AccountInfo::unopened(first.account.clone());
        account.head = head.hash;
        account.block_count = 2;
        account.confirmation_height = confirmed;
        batch.put_account_info(&account, false).unwrap();
        batch.commit().unwrap();
    };
    for store in &stores { install(store, &second, 1); }
    let mut pending: Vec<_> = [&first, &second].into_iter().map(|block| Published {
        block: block.clone(), started: Instant::now(), accepted_ms: 0.0, observed: vec![false; 2],
    }).collect();
    observe_confirmations(&mut pending, &stores);
    assert_eq!(pending[0].observed, vec![true, true]);
    assert_eq!(pending[1].observed, vec![false, false]);
    install(&stores[0], &second, 2);
    install(&stores[1], &fork, 1);
    observe_confirmations(&mut pending, &stores);
    assert_eq!(pending[1].observed, vec![true, false]);
    install(&stores[1], &second, 2);
    observe_confirmations(&mut pending, &stores);
    assert_eq!(pending[1].observed, vec![true, true]);
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Workload {
    BurnReceive,
    FragmentedSend,
}

async fn account_load(
    client: &NodeClient,
    store: &burst_store_lmdb::LmdbStore,
    key: &KeyPair,
    params: &ProtocolParams,
    now: Timestamp,
    until: Instant,
    sender: mpsc::Sender<Published>,
    fragmented: Option<(TxHash, TxHash)>,
) -> usize {
    let address = burst_crypto::derive_address(&key.public);
    let mut count = 0;
    let mut fragmented = fragmented;
    for round in 1..=1024u128 {
        if Instant::now() >= until { return count; }
        if let Some((origin, link)) = fragmented {
            // Every seeded fragment contains one unit. Spending and returning one
            // replaces one fragment, preserving eight without a new burn.
            let send = signed_block(client, key, Transaction::Send(
                build_send_tx(&address, &address, 1, link, origin, now).unwrap()), params).await;
            let hash = send.hash.into_tx_hash();
            publish(client, store, send, &sender).await;
            let receive = signed_block(client, key, Transaction::Receive(
                build_receive_tx(&address, hash, 1, now).unwrap()), params).await;
            fragmented = Some((origin, receive.hash.into_tx_hash()));
            publish(client, store, receive, &sender).await;
            count += 2;
            continue;
        }
        // Distinct amounts bind distinct transaction identities even with the
        // fixture's fixed protocol clock. Total burn stays below one BRN.
        let prepared_at = Instant::now();
        let burn = signed_block(client, key,
            Transaction::Burn(build_burn_tx(&address, &address, round, now).unwrap()), params).await;
        if std::env::var_os("BURST_FINALITY_STAGE_LOG").is_some() {
            println!("payment_load_prepared hash={} operation=Burn elapsed_us={}", burn.hash, prepared_at.elapsed().as_micros());
        }
        let burn_hash = burn.hash;
        publish(client, store, burn, &sender).await;
        let prepared_at = Instant::now();
        let receive = signed_block(client, key,
            Transaction::Receive(build_receive_tx(&address, burn_hash.into_tx_hash(), round, now).unwrap()), params).await;
        if std::env::var_os("BURST_FINALITY_STAGE_LOG").is_some() {
            println!("payment_load_prepared hash={} operation=Receive elapsed_us={}", receive.hash, prepared_at.elapsed().as_micros());
        }
        publish(client, store, receive, &sender).await;
        count += 2;
    }
    assert!(Instant::now() >= until, "payment load exhausted its bounded fixture before the requested duration");
    count
}

async fn publish(client: &NodeClient, store: &burst_store_lmdb::LmdbStore, block: StateBlock, sender: &mpsc::Sender<Published>) {
    let started = Instant::now();
    let response = tokio::time::timeout(Duration::from_secs(25),
        client.process(&serde_json::to_string(&block).unwrap())).await.unwrap().unwrap();
    assert!(response.accepted, "payment load rejected {:?}: {response:?}", block.block_type);
    sender.send(Published { block: block.clone(), started, accepted_ms: started.elapsed().as_secs_f64() * 1000.0,
        observed: Vec::new() }).await.unwrap();
    // RPC acceptance includes queued work. The next signing-state query must not
    // race its predecessor's admission. Finality remains independently observed.
    await_admission(store, &block, started).await.expect("payment admission exceeded existing deadline");
}

async fn await_admission(
    store: &burst_store_lmdb::LmdbStore,
    block: &StateBlock,
    started: Instant,
) -> Result<(), tokio::time::error::Elapsed> {
    tokio::time::timeout_at((started + Duration::from_secs(25)).into(), async {
        loop {
            let account = store.account_store().get_account(&block.account).unwrap();
            if account.head == block.hash { return; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await
}

pub(super) async fn run(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    elections: &[Arc<tokio::sync::RwLock<burst_consensus::ActiveElections>>],
    keys: &[KeyPair],
    params: &ProtocolParams,
    now: Timestamp,
    duration: Duration,
    failure_path: &std::path::Path,
    workload: Workload,
) {
    assert_eq!(stores.len(), 4);
    let payment_keys = &keys[1..];
    let mut fragmented = Vec::new();
    for key in payment_keys {
        fragmented.push(match workload {
            Workload::BurnReceive => None,
            Workload::FragmentedSend => Some(seed_fragmented_wallet(clients, stores, key, params, now).await),
        });
    }
    assert!((3..=32).contains(&payment_keys.len()));
    let before: Vec<_> = payment_keys.iter().map(|key| {
        let address = burst_crypto::derive_address(&key.public);
        let account = stores[0].account_store().get_account(&address).unwrap();
        assert!(account.state.is_verified());
        assert!(keys[..4].iter().any(|representative|
            burst_crypto::derive_address(&representative.public) == account.representative),
            "load account delegates to an unavailable representative");
        for store in stores {
            let peer = store.account_store().get_account(&address).unwrap();
            assert!(peer.state.is_verified());
            assert_eq!(peer.head, account.head);
            assert_eq!(peer.confirmation_height, peer.block_count);
            assert_eq!(peer.representative, account.representative);
        }
        (address, account)
    }).collect();
    let independent_clients = match std::env::var("BURST_PAYMENT_LOAD_CLIENT_IPS").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("shared") => false,
        Ok("independent") => true,
        _ => panic!("BURST_PAYMENT_LOAD_CLIENT_IPS must be shared or independent"),
    };
    let account_clients = load_clients(&clients[0], payment_keys.len(), independent_clients);
    println!("payment_load_client_mode independent={independent_clients} accounts={}", payment_keys.len());
    let started = Instant::now();
    let until = started + duration;
    let (sender, mut receiver) = mpsc::channel::<Published>(1024);
    let producers = async {
        let mut tasks = tokio::task::JoinSet::new();
        for (index, key) in payment_keys.iter().enumerate() {
            let client = account_clients[index].clone();
            let store = stores[0].clone();
            let key = burst_crypto::keypair_from_private(burst_types::PrivateKey(key.private.0));
            let params = params.clone();
            let sender = sender.clone();
            let fragmented = fragmented[index];
            tasks.spawn(async move {
                (index, account_load(&client, &store, &key, &params, now, until, sender, fragmented).await)
            });
        }
        drop(sender);
        let mut counts = vec![0; payment_keys.len()];
        while let Some(result) = tasks.join_next().await {
            let (index, count) = result.expect("account load task failed");
            counts[index] = count;
        }
        counts
    };
    let observer = async {
        let mut pending: Vec<Published> = Vec::new();
        let mut samples = Vec::new();
        let mut closed = false;
        let mut peak_in_flight = 0;
        let mut poll = tokio::time::interval(Duration::from_millis(10));
        loop {
            tokio::select! {
                item = receiver.recv(), if !closed => match item {
                    Some(mut item) => {
                        item.observed.resize(stores.len(), false);
                        pending.push(item);
                        assert!(pending.len() <= 6144, "bounded load inventory exceeded");
                        peak_in_flight = peak_in_flight.max(pending.len());
                    }
                    None => closed = true,
                },
                _ = poll.tick() => {
                    observe_confirmations(&mut pending, stores);

                    if let Some(expired) = pending.iter().find(|item| item.started.elapsed() >= Duration::from_secs(25)) {
                        let hash = expired.block.hash;
                        capture_failure(stores, elections, &pending, &samples, peak_in_flight,
                            failure_path, hash).await;
                        panic!("payment confirmation exceeded existing deadline: {hash}");
                    }
                    pending.retain(|item| {
                        if item.observed.iter().all(|seen| *seen) {
                            samples.push((item.block.hash.to_string(), format!("{:?}", item.block.block_type),
                                item.accepted_ms, item.started.elapsed().as_secs_f64() * 1000.0));
                            false
                        } else { true }
                    });
                }
            }
            if closed && pending.is_empty() { break; }
        }
        (samples, peak_in_flight)
    };
    let (counts, (samples, peak_in_flight)) = tokio::join!(producers, observer);
    let elapsed = started.elapsed().as_secs_f64();
    assert!(counts.iter().all(|count| *count >= 2 && count % 2 == 0));
    assert_eq!(samples.len(), counts.iter().sum::<usize>());
    for (index, (address, old)) in before.iter().enumerate() {
        let rounds = (counts[index] / 2) as u128;
        let amount = match workload {
            Workload::BurnReceive => rounds * (rounds + 1) / 2,
            Workload::FragmentedSend => 0,
        };
        if let Some((origin, _)) = fragmented[index] {
            assert_fragmented_holdings(stores, address, origin, params);
        }
        for store in stores {
            let account = store.account_store().get_account(address).unwrap();
            assert_eq!(account.block_count, old.block_count + counts[index] as u64);
            assert_eq!(account.confirmation_height, account.block_count);
            assert_eq!(account.total_brn_burned, old.total_brn_burned + amount);
            assert_eq!(account.trst_balance, old.trst_balance + amount);
        }
    }
    println!("payment_load {}", json!({"workload":format!("{workload:?}"),"accounts":payment_keys.len(),"peers":stores.len(),"independent_client_ips":independent_clients,
        "requested_seconds":duration.as_secs_f64(),"elapsed_with_drain_seconds":elapsed,
        "confirmed_blocks":samples.len(),"confirmed_blocks_per_second":samples.len() as f64/elapsed,
        "per_account_counts":counts,"peak_in_flight":peak_in_flight,"samples":samples,
        "scope":"same-process loopback, mocked protocol clock, ordinary signing/admission/consensus; not release or Pi qualification"}));
}


async fn capture_failure(
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    elections: &[Arc<tokio::sync::RwLock<burst_consensus::ActiveElections>>],
    pending: &[Published],
    completed: &[(String, String, f64, f64)],
    peak_in_flight: usize,
    path: &std::path::Path,
    failed: BlockHash,
) {
    let mut details = Vec::new();
    for (_, item) in pending.iter().enumerate().filter(|(index, item)| *index < 16 || item.block.hash == failed) {
        let mut peers = Vec::new();
        for (index, store) in stores.iter().enumerate() {
            // Election and store snapshots are separate observations after the
            // deadline, not an atomic protocol cut or rollback authority.
            let election = {
                let ae = elections[index].read().await;
                ae.candidate_root(&item.block.hash).and_then(|root| ae.get_election(&root))
                    .map(|e| json!({"root":e.id,"state":e.state,"soft_quorum":e.has_quorum(),
                        "threshold":e.confirmation_threshold(),"votes":e.last_votes.values().collect::<Vec<_>>(),
                        "tally":e.tally.iter().map(|(hash, weight)| (hash.to_string(), weight)).collect::<Vec<_>>(),
                        "final_tally":e.final_tally.iter().map(|(hash, weight)| (hash.to_string(), weight)).collect::<Vec<_>>()}))
            };
            let durable = (|| -> Result<serde_json::Value, burst_store::StoreError> {
                let batch = store.tx_begin_write()?;
                let account = batch.get_account(&item.block.account)?;
                let pending_dependencies = crate::causal_cementation::pending_dependencies(&batch, &item.block)
                    .map_err(|error| error.to_string());
                // The writer excludes further commits while both readers open
                // their views. Compare queue cache and durable truth at this cut.
                let queue = store.meta_store().pending_confirmations()?;
                let selected = store.pending_confirmation_batch(None, 256)?;
                Ok(json!({"account_head":account.head,"height":account.block_count,
                    "confirmed_height":account.confirmation_height,
                    "block_height":batch.block_height(&item.block.account, &item.block.hash).map_err(|error| error.to_string()),
                    "pending_handoff":batch.confirmation_pending(&item.block.hash)?,
                    "queue_generation":store.env().info().last_txn_id,
                    "queue_selection_matches_durable":selected.iter().eq(queue.iter().take(256)),
                    "durable_queue_count":queue.len(),
                    "selected_queue":selected,
                    "unconfirmed_dependencies":pending_dependencies}))
            })();
            peers.push(json!({"peer":index,"previously_observed_confirmed":item.observed[index],
                "election":election,"durable":durable.map_err(|error| error.to_string())}));
        }
        details.push(json!({"block":item.block,"elapsed_ms":item.started.elapsed().as_millis(),
            "accepted_ms":item.accepted_ms,"peers":peers}));
    }
    let report = json!({"failed_hash":failed,"pending_count":pending.len(),
        "pending_hashes":pending.iter().map(|item| item.block.hash).collect::<Vec<_>>(),
        "peak_in_flight":peak_in_flight,"completed_samples":completed,"oldest_pending":details,
        "scope":"diagnostic observations after deadline; no timing extension or finality authority"});
    match std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()) {
        Ok(()) => eprintln!("payment_load_failure_report {}", path.display()),
        Err(error) => eprintln!("payment_load_failure_report_write_failed {error}; {report}"),
    }
}


#[tokio::test]
async fn failure_capture_records_election_and_durable_state_without_mutation() {
    let directory = tempfile::tempdir().unwrap();
    let config = NodeConfig { network: NetworkId::Dev, data_dir: directory.path().join("node"),
        enable_upnp: false, nts_sources: vec![], ..NodeConfig::default() };
    let params = config.params.clone();
    let node = BurstNode::new(config).await.unwrap();
    node.initialize_genesis().unwrap();
    let block = crate::genesis_state::block(NetworkId::Dev, &params);
    let mut batch = node.store.tx_begin_write().unwrap();
    batch.handoff_confirmation(&block.hash).unwrap();
    batch.commit().unwrap();
    let generation_before = node.store.env().info().last_txn_id;
    let account_before = node.store.account_store().get_account(&block.account).unwrap();
    let mut ae = burst_consensus::ActiveElections::new(4, 1000);
    ae.register_candidate(block.hash, block.hash, block.timestamp).unwrap();
    ae.get_election_mut(&block.hash).unwrap().tally.insert(block.hash, 123);
    let elections = Arc::new(tokio::sync::RwLock::new(ae));
    let path = directory.path().join("failure.json");
    let item = Published { block: block.clone(), started: Instant::now(), accepted_ms: 1.0,
        observed: vec![false] };
    capture_failure(&[node.store.clone()], &[elections.clone()], &[item], &[], 1, &path, block.hash).await;
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(report["pending_count"], 1);
    assert_eq!(report["oldest_pending"][0]["peers"][0]["election"]["tally"][0][1], 123);
    let durable = &report["oldest_pending"][0]["peers"][0]["durable"]["Ok"];
    assert_eq!(durable["pending_handoff"], true);
    assert_eq!(durable["queue_selection_matches_durable"], true);
    assert_eq!(durable["durable_queue_count"], 1);
    assert_eq!(durable["selected_queue"], json!([block.hash]));
    assert_eq!(durable["queue_generation"], generation_before);
    assert_eq!(node.store.env().info().last_txn_id, generation_before);
    let account_after = node.store.account_store().get_account(&block.account).unwrap();
    assert_eq!(account_before.head, account_after.head);
    assert_eq!(account_before.confirmation_height, account_after.confirmation_height);
    assert_eq!(elections.read().await.get_election(&block.hash).unwrap().tally[&block.hash], 123);
}


#[tokio::test]
async fn queued_payment_waits_for_exact_admission_without_waiting_for_finality() {
    let directory = tempfile::tempdir().unwrap();
    let config = NodeConfig { network: NetworkId::Dev, data_dir: directory.path().join("node"),
        enable_upnp: false, nts_sources: vec![], ..NodeConfig::default() };
    let params = config.params.clone();
    let node = BurstNode::new(config).await.unwrap();
    node.initialize_genesis().unwrap();
    let mut block = crate::genesis_state::block(NetworkId::Dev, &params);
    block.previous = block.hash;
    block.hash = block.compute_hash();
    let mut account = node.store.account_store().get_account(&block.account).unwrap();
    let confirmed = account.confirmation_height;
    let started = Instant::now();
    let wait = await_admission(&node.store, &block, started);
    tokio::pin!(wait);
    assert!(tokio::time::timeout(Duration::from_millis(20), &mut wait).await.is_err());
    // Another durable account update is not this queued block's admission.
    account.head = BlockHash::new([77; 32]);
    let mut batch = node.store.tx_begin_write().unwrap();
    batch.put_account_info(&account, false).unwrap();
    batch.commit().unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(20), &mut wait).await.is_err());
    account.head = block.hash;
    account.block_count += 1;
    let mut batch = node.store.tx_begin_write().unwrap();
    batch.put_block_with_account(&block.hash, &bincode::serialize(&block).unwrap(),
        &block.account, account.block_count).unwrap();
    batch.put_account_info(&account, false).unwrap();
    batch.commit().unwrap();
    tokio::time::timeout(Duration::from_secs(1), &mut wait).await.unwrap().unwrap();
    assert_eq!(node.store.account_store().get_account(&block.account).unwrap().confirmation_height, confirmed);
    assert!(confirmed < account.block_count);
    let mut absent = block.clone();
    absent.hash = BlockHash::new([88; 32]);
    assert!(await_admission(&node.store, &absent, Instant::now() - Duration::from_secs(25)).await.is_err());
}


// Complete every worker before the caller preserves a failed fixture and panics.
pub(super) async fn drain_workers(
    workers: impl IntoIterator<Item = tokio::task::JoinHandle<()>>,
    timeout: Duration,
) -> Vec<String> {
    let mut failures = Vec::new();
    for (index, mut worker) in workers.into_iter().enumerate() {
        match tokio::time::timeout(timeout, &mut worker).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => failures.push(format!("worker {index}: {error}")),
            Err(error) => {
                worker.abort();
                let _ = worker.await;
                failures.push(format!("worker {index} shutdown: {error}"));
            }
        }
    }
    failures
}

#[tokio::test]
async fn failed_worker_does_not_skip_remaining_cleanup_or_leave_timed_out_task() {
    let failed = tokio::spawn(async { panic!("injected worker failure"); });
    let complete = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let completed = complete.clone();
    let success = tokio::spawn(async move {
        completed.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let pending = tokio::spawn(std::future::pending::<()>());
    let pending_abort = pending.abort_handle();
    let failures = drain_workers([failed, success, pending], Duration::from_millis(100)).await;
    assert_eq!(failures.len(), 2);
    assert!(failures[0].contains("injected worker failure"));
    assert!(failures[1].starts_with("worker 2 shutdown:"));
    assert!(complete.load(std::sync::atomic::Ordering::SeqCst));
    assert!(pending_abort.is_finished());
}

/// Keep signed attempts intact until every peer confirms or certifies exclusion.
pub(super) async fn confirmed_payment(
    clients: &[NodeClient], key: &KeyPair, params: &ProtocolParams,
    transaction: impl Fn() -> Transaction,
) -> StateBlock {
    let started = Instant::now();
    for attempt in 0..4 {
        assert!(started.elapsed() < Duration::from_secs(25), "mixed payment exceeded25s");
        let block = signed_block(&clients[0], key, transaction(), params).await;
        let encoded = serde_json::to_string(&block).unwrap();
        let response = clients[0].process(&encoded).await.unwrap();
        eprintln!("mixed_lifecycle signed hash={} epoch={} attempt={attempt} accepted={}", block.hash, block.payment_epoch, response.accepted);
        loop {
            let mut confirmed = true;
            for client in clients {
                confirmed &= client.rpc_call("block_info", json!({"hash":block.hash.to_string()}))
                    .await.is_ok_and(|info| info["confirmed"] == true);
            }
            if confirmed { return block; }
            // Exclusion is a bounded storage read, needed only after the
            // authority moves. It is not a confirmation-polling endpoint.
            let authority = admitted_read(|| clients[0].rpc_call("params_hash", json!({}))).await;
            let mut excluded = authority["payment_epoch"].as_str()
                .is_some_and(|epoch| epoch != block.payment_epoch.to_string());
            if excluded {
                for client in clients {
                    excluded &= admitted_read(|| client.excluded_offline_payment(&block)).await.is_some();
                }
            }
            if excluded {
                eprintln!("mixed_lifecycle excluded hash={}", block.hash);
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(25), "unresolved signed mixed payment {}: {response:?}", block.hash);
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    panic!("mixed payment crossed more than four certified epochs");
}


// Match the existing dashboard admission policy: retry only the explicit
// bounded-read refusal, never transaction submission or arbitrary RPC errors.
pub(super) async fn admitted_read<T, F, Fut>(mut read: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, burst_wallet_core::WalletError>>,
{
    for attempt in 0..4 {
        match read().await {
            Ok(value) => return value,
            Err(burst_wallet_core::WalletError::Node(message))
                if message == "node error: rate limited" && attempt < 3 => {
                eprintln!("mixed_lifecycle read_admission_retry attempt={attempt}");
                tokio::time::sleep(Duration::from_millis(50 << attempt)).await;
            }
            Err(error) => panic!("mixed workload read failed: {error}"),
        }
    }
    unreachable!()
}
