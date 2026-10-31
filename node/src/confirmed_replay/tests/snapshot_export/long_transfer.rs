use super::*;
use crate::checkpoint_sync::{
    snapshot::{Config, Fetch, Transfer},
    Incoming,
};
use std::time::{Duration, Instant};

#[test]
fn snapshot_long_transfer_completes_from_replayed_accounts_without_absolute_deadline() {
    transfer(4000, false);
}

#[test]
#[ignore = "manual growing-state resource probe; run through limited-run.sh"]
fn snapshot_growing_state_resource_probe() {
    let accounts = std::env::var("BURST_SNAPSHOT_PROBE_ACCOUNTS")
        .expect("set BURST_SNAPSHOT_PROBE_ACCOUNTS")
        .parse::<u32>()
        .expect("account count must be an integer");
    assert!((4000..=32000).contains(&accounts));
    transfer(accounts, true);
}

fn transfer(account_count: u32, measure: bool) {
    let measured_at = Instant::now();
    let report = |stage: &str| {
        if measure {
            let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
            let rss = status.lines().find(|line| line.starts_with("VmRSS:"));
            let peak = status.lines().find(|line| line.starts_with("VmHWM:"));
            eprintln!("snapshot resource probe accounts={account_count} stage={stage} elapsed_ms={} rss={} peak={}",
                measured_at.elapsed().as_millis(), rss.unwrap_or("unavailable"), peak.unwrap_or("unavailable"));
        }
    };
    let mut fixture = Fixture::selection();
    // Real signed opens enlarge the replayed economic body, not padding that a
    // decoder would reject or a test-only archive format.
    for index in 0..account_count {
        let mut seed = [42; 32];
        seed[..4].copy_from_slice(&index.to_be_bytes());
        let key = burst_crypto::keypair_from_seed(&seed);
        let account = burst_crypto::derive_address(&key.public);
        let mut block = fixture.blocks[0].clone();
        block.block_type = BlockType::Open;
        block.payment_epoch = fixture.blocks[0].hash;
        block.account = account.clone();
        block.representative = account;
        block.previous = BlockHash::ZERO;
        block.brn_spent = 0;
        block.trst_balance = 0;
        block.timestamp = Timestamp::new(1_595_434_000);
        block.hash = block.compute_hash();
        block.signature = burst_crypto::sign_message(block.hash.as_bytes(), &key.private);
        fixture.blocks.push(block);
    }
    let (_directory, store, heads) = if measure {
        fixture.store_with_map_size(128 << 20)
    } else {
        fixture.store()
    };
    report("stored-fixture");
    let candidate = CertifiedCheckpoint::prepare(
        &store,
        NetworkId::Dev,
        &fixture.params,
        1_595_434_050,
        &heads,
        &address(20),
    )
    .unwrap();
    let certificate = certified_votes::certify(candidate.checkpoint(), &[0]);
    let epoch = certificate.checkpoint.hash();
    CertifiedCheckpoint::accept(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certificate,
        heads,
        address(20),
    )
    .unwrap();
    report("certified");
    let export = SnapshotExport::prepare(&store, NetworkId::Dev, &fixture.params, epoch).unwrap();
    report("prepared-export");
    assert!(
        export.manifest().state_bytes > 1024 * 1024,
        "replayed economic body must exercise file-backed staging"
    );
    let attestations = (0..7)
        .map(|seed| {
            export
                .attest(
                    &store,
                    &burst_crypto::keypair_from_seed(&[seed; 32]).private,
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mut archive = Vec::new();
    export.write(&attestations, &mut archive).unwrap();
    report("written-archive");
    if measure {
        eprintln!(
            "snapshot resource probe state_bytes={} archive_bytes={}",
            export.manifest().state_bytes,
            archive.len()
        );
    }
    assert!(
        archive.len() > 31 * 16 * 1024,
        "fixture must exceed the former time limit at four seconds per chunk"
    );
    let base = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.snapshot");
    let output = directory.path().join("received.snapshot");
    std::fs::write(&source, &archive).unwrap();
    let mut server = Transfer::new(
        Config {
            max_archive_bytes: Config::default().max_archive_bytes,
            serve: Some(source),
            fetch: None,
        },
        base.clone(),
        directory.path(),
    )
    .unwrap();
    let mut client = Transfer::new(
        Config {
            max_archive_bytes: Config::default().max_archive_bytes,
            serve: None,
            fetch: Some(Fetch {
                epoch: Some(epoch.to_string()),
                minimum_height: 1,
                output: output.clone(),
            }),
        },
        base.clone(),
        directory.path(),
    )
    .unwrap();
    let started = Instant::now();
    let mut now = started;
    let mut request = client.poll(vec!["source".into()], now);
    let mut chunks = 0;
    while let Some(requested) = request {
        let peer = requested.peer;
        assert!(!output.exists(), "partial archive must not be published");
        let chunk = server
            .handle(
                Incoming {
                    peer: "receiver".into(),
                    message: requested.message,
                },
                now,
            )
            .unwrap()
            .unwrap();
        now += Duration::from_secs(4);
        request = client
            .handle(
                Incoming {
                    peer: peer.clone(),
                    message: chunk.message.clone(),
                },
                now,
            )
            .unwrap();
        chunks += 1;
        if request.is_some() {
            assert!(
                client
                    .handle(
                        Incoming {
                            peer: peer.clone(),
                            message: chunk.message
                        },
                        now
                    )
                    .unwrap()
                    .is_none(),
                "duplicate chunk must not advance the download"
            );
        }
        if chunks == 2 {
            now += Duration::from_secs(5);
            let resumed = client.poll(vec!["backup".into()], now).unwrap();
            assert_eq!(resumed.peer, "backup");
            match &resumed.message {
                crate::checkpoint_sync::CheckpointSyncMessage::SnapshotRequest {
                    offset,
                    digest,
                    ..
                } => {
                    assert_eq!(
                        *offset,
                        2 * 16 * 1024,
                        "peer loss must retain accepted chunks"
                    );
                    assert!(digest.is_some(), "resumed requests must pin the same bytes");
                }
                _ => panic!("expected resumed snapshot request"),
            }
            request = Some(resumed);
        }
    }
    assert!(chunks > 30);
    assert!(now.duration_since(started) > Duration::from_secs(120));
    assert_eq!(std::fs::read(&output).unwrap(), archive);
    report("transferred");
    let snapshot = AuthenticatedSnapshot::read_in(
        &base,
        &mut std::fs::File::open(&output).unwrap(),
        directory.path(),
    )
    .unwrap();
    assert_eq!(snapshot.anchor().hash(), epoch);
    assert_eq!(&snapshot.certificate().manifest, export.manifest());
    report("restored");
    let minimal = Fixture {
        params: fixture.params.clone(),
        blocks: vec![fixture.blocks[0].clone()],
    };
    let (local_directory, local, _) = minimal.store_with_map_size(128 << 20);
    let needs = snapshot.installation_needs(&local).unwrap();
    assert!(needs.snapshot_heads.len() >= account_count as usize);
    let mut requested = std::collections::HashSet::new();
    let mut proofs = Vec::new();
    let hashes: Vec<_> = needs
        .snapshot_heads
        .iter()
        .chain(&needs.cemented_heads)
        .map(|head| head.hash)
        .collect();
    let mut cursors: std::collections::HashMap<_, _> = snapshot
        .committed_history_cursors(&hashes)
        .unwrap()
        .into_iter()
        .map(|cursor| (cursor.next().unwrap().hash, cursor))
        .collect();
    for head in needs.snapshot_heads.iter().chain(&needs.cemented_heads) {
        if !requested.insert(head.hash) {
            continue;
        }
        let mut cursor = cursors.remove(&head.hash).unwrap();
        proofs.extend(
            cursor
                .accept(&[store.block_store().get_block(&head.hash).unwrap()])
                .unwrap(),
        );
    }
    assert!(needs.confirmation_targets.is_empty());
    snapshot
        .reconcile_installation(
            local.tx_begin_write().unwrap(),
            NetworkId::Dev,
            &fixture.params,
            &proofs,
        )
        .unwrap()
        .project_ledger(&[])
        .unwrap()
        .project_economics()
        .unwrap()
        .commit_for_startup()
        .unwrap();
    local.force_sync().unwrap();
    drop(proofs);
    report("installed");
    drop(local);
    let reopened = LmdbStore::open(local_directory.path(), 32, 128 << 20).unwrap();
    let recovered = crate::confirmed_replay::snapshot::installed::load_base(
        &reopened,
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap()
    .unwrap()
    .into_continuation();
    assert_eq!(recovered.anchor().hash(), epoch);
    assert!(recovered.state().accounts.len() >= account_count as usize);
    let plan = recovered.plan_cemented_tail(&reopened).unwrap();
    let witnesses =
        crate::checkpoint_sync::witness::read_plan(&store, &recovered, plan.witness_plan())
            .unwrap();
    let tail = plan.finish(witnesses).unwrap();
    recovered
        .verify_installed_metadata(&reopened, &tail)
        .unwrap();
    tail.verify_unchanged(&reopened.tx_begin_write().unwrap())
        .unwrap();
    report("reopened-startup-checked");
    if measure && std::env::var_os("BURST_SNAPSHOT_PROBE_RETAIN").is_some() {
        drop(tail);
        drop(recovered);
        reopened.force_sync().unwrap();
        drop(reopened);
        let metadata = (1u8, &fixture.params, epoch, account_count, 1_595_434_051u64);
        std::fs::write(local_directory.path().join("resource-probe.bin"),
            bincode::serialize(&metadata).unwrap()).unwrap();
        let retained = local_directory.keep();
        eprintln!("SNAPSHOT_NODE_BASELINE path={} accounts={account_count} epoch={epoch}", retained.display());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "explicit capped-host startup-only resource probe from retained authenticated baseline"]
async fn snapshot_retained_full_node_startup_resource_probe() {
    tracing_subscriber::fmt()
        .with_env_filter("burst_node::node=debug,burst_node::confirmed_replay::snapshot::installed=debug,burst_node::confirmed_replay::events=debug,burst_node::checkpoint_sync::witness=debug")
        .with_ansi(false).with_test_writer().try_init()
        .expect("isolated startup diagnostic subscriber");
    use std::sync::{Arc, atomic::AtomicU64};
    let reopen = std::env::var_os("BURST_SNAPSHOT_REOPEN_LEDGER");
    if let Some(path) = &reopen {
        assert!(!std::path::Path::new(path).join(crate::confirmed_replay::scratch_file::PERSISTENT_INPUTS_DIRECTORY).exists());
    }
    let (working, params, epoch, accounts, now) = retained_probe_copy_from(reopen.as_deref().map(std::path::Path::new), 100_000);
    let working = working.keep();
    eprintln!("SNAPSHOT_NODE_WORKING path={}", working.display());
    let started = Instant::now();
    let report = |stage: &str| {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let fields: Vec<_> = status.lines().filter(|line|
            line.starts_with("VmRSS:") || line.starts_with("VmHWM:")).collect();
        eprintln!("SNAPSHOT_NODE_RESOURCE accounts={accounts} stage={stage} elapsed_ms={} {}",
            started.elapsed().as_millis(), fields.join(" "));
    };
    report("before-node");
    let rpc_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut node = crate::BurstNode::new(crate::NodeConfig {
        data_dir: working, network: NetworkId::Dev, params, port: 0, rpc_port,
        rpc_bind: "127.0.0.1".into(), enable_upnp: false, enable_peer_reachout: false,
        enable_representative: false, enable_faucet: false, bootstrap_peers: vec![],
        recovery_scratch_bytes: Some(64 << 20),
        nts_sources: vec![], drand_relays: vec!["http://127.0.0.1:1".into()],
        ..crate::NodeConfig::default()
    }).await.unwrap();
    node.block_processor.lock().await.test_time = Some(Arc::new(AtomicU64::new(now)));
    node.verification_fixture_clock = Some(Arc::new(|| None));
    report("constructed");
    let stop = node.shutdown.clone();
    let task = tokio::spawn(async move {
        let result = node.start_with_services(burst_daemon::start_public_services).await;
        (node, result)
    });
    let client = burst_wallet_core::NodeClient::new(format!("http://127.0.0.1:{rpc_port}")).unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if task.is_finished() { return Err("node exited before readiness"); }
            if let Ok(telemetry) = client.rpc_call("telemetry", serde_json::json!({})).await {
                assert!(telemetry["account_count"].as_u64().unwrap() >= accounts as u64);
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }).await;
    report("readiness-finished");
    eprintln!("SNAPSHOT_NODE_READINESS {ready:?}");
    stop.shutdown();
    let (node, result) = tokio::time::timeout(Duration::from_secs(15), task).await.unwrap().unwrap();
    eprintln!("SNAPSHOT_NODE_OUTCOME readiness={ready:?} shutdown={result:?}");
    ready.expect("retained full node readiness timed out").unwrap();
    result.unwrap();
    assert_eq!(node.store.meta_store().payment_epoch().unwrap(), epoch);
    assert!(node.store.account_store().account_count().unwrap() >= accounts as u64);
    drop(node);
    report("stopped");
}

fn retained_probe_copy() -> (tempfile::TempDir, ProtocolParams, BlockHash, u32, u64) {
    retained_probe_copy_from(None, 32_000)
}

#[test]
#[ignore = "capped-host parameter preflight only; does not start a node or qualify readiness"]
fn snapshot_retained_daemon_parameter_preflight() {
    let (working, params, epoch, accounts, now) = retained_probe_copy_from(None, 100_000);
    let map_size = retained_probe_map_size(crate::node::DEFAULT_MAP_SIZE);
    let store = LmdbStore::open(working.path(), crate::node::MAX_DBS, map_size).unwrap();
    assert_eq!(store.env().info().map_size, map_size);
    let before_accounts = store.account_store().account_count().unwrap();
    let before_blocks = store.block_store().block_count().unwrap();
    assert!(before_accounts >= accounts as u64);
    assert_eq!(store.meta_store().payment_epoch().unwrap(), epoch);
    assert!(store.meta_store().get_meta("genesis_params/v1").is_err());
    let genesis = crate::genesis_state::block(NetworkId::Dev, &params);
    let genesis_bytes = store.block_store().get_block(&genesis.hash).unwrap();
    let mut wrong = params.clone();
    wrong.brn_rate = wrong.brn_rate.checked_add(1).unwrap();
    assert!(crate::network_identity::genesis_params(&store, NetworkId::Dev, &wrong).is_err());
    assert!(store.meta_store().get_meta("genesis_params/v1").is_err());
    crate::network_identity::verify_network(&store, NetworkId::Dev).unwrap();
    let pinned = crate::network_identity::genesis_params(&store, NetworkId::Dev, &params).unwrap();
    assert_eq!(pinned.params_hash(), params.params_hash());
    let daemon_view = crate::network_identity::genesis_params(
        &store, NetworkId::Dev, &ProtocolParams::devnet_defaults()).unwrap();
    assert_eq!(daemon_view.params_hash(), params.params_hash());
    assert_eq!(store.account_store().account_count().unwrap(), before_accounts);
    assert_eq!(store.block_store().block_count().unwrap(), before_blocks);
    assert_eq!(store.meta_store().payment_epoch().unwrap(), epoch);
    assert_eq!(store.block_store().get_block(&genesis.hash).unwrap(), genesis_bytes);
    store.force_sync().unwrap();
    drop(store);
    std::fs::write(working.path().join("resource-probe.bin"),
        bincode::serialize(&(1u8, &params, epoch, accounts, now)).unwrap()).unwrap();
    let prepared = working.keep();
    eprintln!("DAEMON_PARAMETER_PREFLIGHT {}", serde_json::json!({
        "path": prepared, "accounts": accounts, "epoch": epoch,
        "genesis": genesis.hash, "params_hash": params.params_hash(),
        "fixture_clock": now,
        "database_map_size_bytes": map_size as u64,
        "scope": "parameter identity only; no node startup, clock compatibility or readiness qualification",
    }));
}

#[test]
#[ignore = "capped-host legacy verification migration diagnosis; not startup acceptance"]
fn snapshot_retained_verification_migration_resource_probe() {
    let (working, _, epoch, accounts, _) = retained_probe_copy_from(None, 100_000);
    let store = LmdbStore::open(working.path(), crate::node::MAX_DBS,
        crate::node::DEFAULT_MAP_SIZE).unwrap();
    let legacy = store.meta_store().get_meta("verification_orchestrator").unwrap();
    let expected = burst_verification::OrchestratorSnapshot::decode(&legacy).unwrap()
        .encode().unwrap();
    let counts = store.tx_begin_write().unwrap().ledger_counts().unwrap();
    let started = Instant::now();
    let mut previous = started;
    let mut report = |stage: &str| {
        let now = Instant::now();
        eprintln!("VERIFICATION_MIGRATION_RESOURCE {}", serde_json::json!({
            "stage": stage, "accounts": accounts,
            "stage_ms": now.duration_since(previous).as_secs_f64() * 1000.0,
            "elapsed_ms": now.duration_since(started).as_secs_f64() * 1000.0,
            "scope": "validation-profile legacy migration only; not readiness qualification",
        }));
        previous = now;
    };
    let mut batch = store.tx_begin_write().unwrap();
    assert!(!batch.uses_verification_records().unwrap());
    report("begin-writer");
    assert!(batch.normalize_verification_records().unwrap());
    report("normalize");
    let restored = batch.read_verification_records().unwrap();
    report("warm-cache");
    batch.commit().unwrap();
    report("commit");
    store.force_sync().unwrap();
    report("sync");
    assert_eq!(restored.snapshot().encode().unwrap(), expected);
    assert_eq!(store.tx_begin_write().unwrap().ledger_counts().unwrap(), counts);
    assert_eq!(store.meta_store().payment_epoch().unwrap(), epoch);
    assert!(!crate::verification_storage::migrate(&store).unwrap());
    drop(store);
    let reopened = LmdbStore::open(working.path(), crate::node::MAX_DBS,
        crate::node::DEFAULT_MAP_SIZE).unwrap();
    let batch = reopened.tx_begin_write().unwrap();
    assert!(batch.uses_normalized_verification_records().unwrap());
    assert_eq!(batch.read_verification_records().unwrap().snapshot().encode().unwrap(), expected);
    assert_eq!(batch.ledger_counts().unwrap(), counts);
    eprintln!("VERIFICATION_MIGRATION_PRESERVED accounts={accounts} epoch={epoch}");
}

fn retained_probe_map_size(default: usize) -> usize {
    let mut config = crate::NodeConfig::default();
    config.database_map_size_bytes = default as u64;
    if let Some(bytes) = std::env::var_os("BURST_SNAPSHOT_MAP_BYTES") {
        config.database_map_size_bytes = bytes.to_str().unwrap().parse().unwrap();
    }
    config.database_map_size().unwrap()
}

fn retained_probe_copy_from(source: Option<&std::path::Path>, max_accounts: u32) -> (tempfile::TempDir, ProtocolParams, BlockHash, u32, u64) {
    let baseline = std::path::PathBuf::from(std::env::var_os("BURST_SNAPSHOT_NODE_BASELINE")
        .expect("set retained authenticated baseline"));
    let (version, params, epoch, accounts, now): (u8, ProtocolParams, BlockHash, u32, u64) =
        bincode::deserialize(&std::fs::read(baseline.join("resource-probe.bin")).unwrap()).unwrap();
    assert_eq!(version, 1);
    assert!((4000..=max_accounts).contains(&accounts));
    let working = tempfile::tempdir().unwrap();
    let source = source.unwrap_or(&baseline);
    std::fs::copy(source.join("data.mdb"), working.path().join("data.mdb")).unwrap();
    let input_name = crate::confirmed_replay::scratch_file::PERSISTENT_INPUTS_DIRECTORY;
    if source.join(input_name).exists() {
        let input = source.join(input_name).join("data.mdb");
        assert!(input.metadata().unwrap().len() > 0);
        std::fs::create_dir(working.path().join(input_name)).unwrap();
        std::fs::copy(input, working.path().join(input_name).join("data.mdb")).unwrap();
        // Retained recovery inputs must count toward the copied fixture's budget.
        assert!(crate::confirmed_replay::scratch_file::ScratchLimit::acquire(working.path(), 1).is_err());
    }
    (working, params, epoch, accounts, now)
}

#[test]
#[ignore = "explicit capped-host history preparation diagnostic from retained authenticated baseline"]
fn snapshot_retained_history_preparation_resource_probe() {
    let (working, params, epoch, accounts, _) = retained_probe_copy();
    let store = LmdbStore::open(working.path(), 32, 128 << 20).unwrap();
    let started = Instant::now();
    let mut previous = started;
    let mut report = |stage: &str| {
        let now = Instant::now();
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let memory: Vec<_> = status.lines().filter(|line|
            line.starts_with("VmRSS:") || line.starts_with("VmHWM:")).collect();
        eprintln!("HISTORY_PREPARATION {}", serde_json::json!({
            "accounts":accounts,"stage":stage,
            "stage_ms":now.duration_since(previous).as_secs_f64()*1000.0,
            "elapsed_ms":now.duration_since(started).as_secs_f64()*1000.0,
            "memory":memory,
            "scope":"retained signed-open fixture; validation profile; preparation only"
        }));
        previous = now;
    };
    use crate::historical_lookup::preparation::{Cache, Prepared, Request};
    use std::sync::Arc;
    let request = Request::Batch(vec![crate::genesis_state::block(NetworkId::Dev, &params).hash]);
    let mut cache = Cache::default();
    report("before-cold-preparation");
    let cold = cache.prepare(&store, NetworkId::Dev, &params, request.clone()).unwrap();
    let Prepared::Batch(Some((roots, committed))) = cold.as_ref() else { panic!("missing prepared base") };
    assert!(roots.len() >= accounts as usize);
    assert_eq!(committed.len(), 1);
    assert_eq!(committed[0].context().epoch, epoch);
    report("cold-prepared");
    let warm = cache.prepare(&store, NetworkId::Dev, &params, request.clone()).unwrap();
    assert!(Arc::ptr_eq(&cold, &warm));
    report("warm-prepared");
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta("history-cache-probe-unrelated", b"tracked").unwrap();
    batch.commit().unwrap();
    assert!(Arc::ptr_eq(&cold, &cache.prepare(&store, NetworkId::Dev, &params, request.clone()).unwrap()));
    report("warm-after-unrelated-commit");
    // Change a page without changing its descriptor: descriptor-only caches fail this check.
    let descriptor = store.meta_store().get_meta(crate::confirmed_replay::snapshot::installed::BASE_KEY).unwrap();
    let page_key = "checkpoint_snapshot/pages/v1/00000000";
    let page = store.meta_store().get_meta(page_key).unwrap();
    assert!(!page.is_empty(), "retained fixture must use paged snapshot storage");
    let mut corrupt = page.clone();
    corrupt[0] ^= 1;
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(page_key, &corrupt).unwrap();
    batch.commit().unwrap();
    assert_eq!(store.meta_store().get_meta(crate::confirmed_replay::snapshot::installed::BASE_KEY).unwrap(), descriptor);
    assert!(cache.prepare(&store, NetworkId::Dev, &params, request.clone()).is_err());
    report("changed-page-rejected");
    let mut batch = store.tx_begin_write().unwrap();
    batch.put_meta(page_key, &page).unwrap();
    batch.commit().unwrap();
    let restored = cache.prepare(&store, NetworkId::Dev, &params, request).unwrap();
    assert!(!Arc::ptr_eq(&cold, &restored));
    let Prepared::Batch(Some((restored_roots, restored_committed))) = restored.as_ref() else { panic!("missing restored base") };
    assert_eq!(restored_roots.len(), roots.len());
    assert_eq!(restored_committed[0].next(), committed[0].next());
    report("restored-page-reauthenticated");
    drop((cold, warm, restored, cache));
    report("preparations-dropped");
}


#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "capped-host signed retained closure probe; reuses immutable account baseline"]
async fn snapshot_retained_body_closure_resource_probe() {
    use crate::confirmed_replay::{scratch_file::ScratchLimit, snapshot::installed};
    use std::sync::Arc;
    let verified_humans = std::env::var("BURST_SNAPSHOT_CLOSURE_VERIFIED_HUMANS")
        .map(|value| value.parse::<u32>().expect("verified human count")).ok();
    let preflight_only = match std::env::var("BURST_SNAPSHOT_CLOSURE_PREFLIGHT_ONLY").as_deref() {
        Ok("1") => { assert!(verified_humans.is_some()); true }
        Err(std::env::VarError::NotPresent) => false,
        other => panic!("invalid closure preflight mode: {other:?}"),
    };
    let migration_only = match std::env::var("BURST_SNAPSHOT_CLOSURE_MIGRATION_ONLY").as_deref() {
        Ok("1") => { assert!(verified_humans.is_some() && !preflight_only); true }
        Err(std::env::VarError::NotPresent) => false,
        other => panic!("invalid closure migration mode: {other:?}"),
    };
    let (working, params, epoch, accounts, _) = match verified_humans {
        Some(expected) => {
            assert!((4000..=100_000).contains(&expected));
            let copied = retained_probe_copy_from(None, expected);
            assert_eq!(copied.3, expected);
            assert_eq!(copied.1, crate::verification_test_support::governance_fixture_params(
                copied.1.bootstrap_exit_threshold));
            copied
        }
        None => retained_probe_copy(),
    };
    let working = working.keep();
    eprintln!("CLOSURE_WORKING path={}", working.display());
    let ledger_map_bytes = if verified_humans.is_some() { crate::node::DEFAULT_MAP_SIZE } else { 128 << 20 };
    let store = Arc::new(LmdbStore::open(&working, 64, ledger_map_bytes).unwrap());
    if let Some(expected) = verified_humans {
        // Certified migration is a separate stage. The closure/preflight paths
        // still require normalized records before inspecting retention roots.
        let writer = store.tx_begin_write().unwrap();
        let (blocks, live_accounts, pending) = writer.ledger_counts().unwrap();
        assert_eq!(live_accounts, u64::from(expected));
        let stable = writer.uses_stable_trst_records().unwrap();
        let graph = writer.uses_graph_records().unwrap();
        let rates = writer.uses_brn_rate_records().unwrap();
        let wallets = writer.uses_brn_wallet_records().unwrap();
        let verification = writer.uses_normalized_verification_records().unwrap();
        let governance = writer.uses_governance_records().unwrap();
        eprintln!("CLOSURE_PREFLIGHT verified_humans={expected} blocks={blocks} live_accounts={live_accounts} pending={pending} map_bytes={ledger_map_bytes} fixture_rewritten=false stable={stable} graph={graph} rates={rates} wallets={wallets} verification={verification} governance={governance}");
        if migration_only {
            assert!(!stable, "migration fixture is already stable; reuse its migration evidence");
        } else {
            assert!(stable && graph && rates && wallets && verification && governance,
                "closure requires normalized storage");
        }
        drop(writer);
        assert_eq!(store.meta_store().payment_epoch().unwrap(), epoch);
        if preflight_only {
            eprintln!("CLOSURE_SCOPE preflight_only=true recovery=false closure=false physical_deletion=false");
            return;
        }
    } else {
        // Reconstruct only the original tiny selection fixture. Signed opens
        // and the authenticated economic snapshot come from the retained baseline.
        let fixture = Fixture::selection();
        assert_eq!(fixture.params, params);
        let mut heights = std::collections::HashMap::new();
        let mut added = 0;
        let mut writer = store.tx_begin_write().unwrap();
        for block in &fixture.blocks {
            assert_eq!(block.hash, block.compute_hash());
            let height = heights.entry(block.account.clone()).or_insert(0u64);
            *height += 1;
            let bytes = bincode::serialize(block).unwrap();
            match writer.get_block(&block.hash) {
                Ok(current) => assert_eq!(current, bytes, "baseline ancestry differs from original fixture"),
                Err(burst_store::StoreError::NotFound(_)) => {
                    writer.put_block_with_account(&block.hash, &bytes, &block.account, *height).unwrap();
                    added += 1;
                }
                Err(error) => panic!("ancestry preflight: {error}"),
            }
        }
        writer.commit().unwrap();
        drop(fixture);
        eprintln!("CLOSURE_PREFLIGHT accounts={accounts} added_original_prefix_rows={added}");
        crate::confirmed_replay::tests::snapshot_history::installed_recovery::normalize_retention_fixture(
            &store, &params);
        eprintln!("CLOSURE_PREFLIGHT normalized_retention_records=true");
    }
    let ledger_counts_before = store.tx_begin_write().unwrap().ledger_counts().unwrap();
    let map_bytes = 16 << 20;
    let limit = ScratchLimit::acquire_reserved(&working, 64 << 20,
        crate::confirmed_replay::snapshot::history::DurableHistoryInputs::reservation(map_bytes).unwrap()).unwrap();
    let started = Instant::now();
    let report = |stage: &str| {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let fields: Vec<_> = status.lines().filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:")).collect();
        eprintln!("SIGNED_CLOSURE accounts={accounts} stage={stage} elapsed_ms={} scratch_bytes={} scratch_capacity={} {}",
            started.elapsed().as_millis(), limit.used_temporary_bytes().unwrap(), limit.temporary_capacity(), fields.join(" "));
        if std::env::var_os("BURST_SCRATCH_ALLOCATION_DIAGNOSTIC").is_some() {
            for (bytes, allocation) in limit.diagnostic_files().unwrap() {
                eprintln!("SCRATCH_OWNER stage={stage} bytes={bytes} allocation=\n{allocation}");
            }
        }
    };
    report("before-recovery");
    let registry = Arc::new(tokio::sync::RwLock::new(crate::connection_registry::ConnectionRegistry::new()));
    registry.write().await.recovery_probe = Some(|stage| {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let rss = status.lines().find(|line| line.starts_with("VmRSS:")).unwrap();
        eprintln!("SIGNED_RECOVERY stage={stage} {rss}");
    });
    let recovered = installed::recover_metadata_with_durable_inputs(store.clone(), NetworkId::Dev,
        params, registry.clone(), Arc::new(crate::shutdown::ShutdownController::new()), Some((limit.clone(), map_bytes))).await;
    report("recovery-returned");
    let checked = match recovered.unwrap() {
        installed::StartupRecovery::Ready(Some(checked)) => checked,
        _ => panic!("signed retained recovery did not complete"),
    };
    assert_eq!(checked.recovered().continuation.anchor().hash(), epoch);
    assert!(checked.recovered().continuation.state().accounts.len() >= accounts as usize);
    if let Some(expected) = verified_humans {
        assert_eq!(checked.recovered().continuation.anchor().roster().total_humans(), u64::from(expected));
        assert_eq!(checked.recovered().continuation.state().accounts.len(), expected as usize);
    }
    let cutoff = checked.recovered().continuation.anchor().cutoff();
    if migration_only {
        let projected = checked.project_token_indexes_with_stable_holdings(&store, Timestamp::new(cutoff));
        report("migration-projected");
        let staged = projected.unwrap().stage_undo();
        report("migration-staged");
        let staged = staged.unwrap();
        assert_eq!(staged.runtime().anchor.hash(), epoch);
        assert_eq!(staged.runtime().anchor.roster().total_humans(), u64::from(accounts));
        staged.commit_for_test().unwrap();
        store.force_sync().unwrap();
        report("migration-committed");
        {
            let writer = store.tx_begin_write().unwrap();
            assert_eq!(writer.ledger_counts().unwrap(), ledger_counts_before);
            assert!(writer.uses_stable_trst_records().unwrap());
            assert!(writer.uses_graph_records().unwrap());
            assert!(writer.uses_brn_rate_records().unwrap());
            assert!(writer.uses_brn_wallet_records().unwrap());
            assert!(writer.uses_normalized_verification_records().unwrap());
            assert!(writer.uses_governance_records().unwrap());
        }
        assert_eq!(store.meta_store().payment_epoch().unwrap(), epoch);
        let baseline = std::path::PathBuf::from(std::env::var_os("BURST_SNAPSHOT_NODE_BASELINE").unwrap());
        std::fs::copy(baseline.join("resource-probe.bin"), working.join("resource-probe.bin")).unwrap();
        drop(registry);
        drop(store);
        report("migration-owners-dropped");
        assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
        eprintln!("CLOSURE_MIGRATED path={} accounts={accounts} epoch={epoch} map_bytes={ledger_map_bytes}", working.display());
        eprintln!("CLOSURE_SCOPE migration_only=true closure=false full_node_readiness=false physical_deletion=false");
        return;
    }
    let projection = checked.prepare_token_indexes_at(&store, Timestamp::new(cutoff));
    report("projection-returned");
    let projection = projection.unwrap();
    let required = projection.body_requirements(&store);
    report("roots-returned");
    let required = required.unwrap();
    let mut root_count = 0u64;
    required.visit_roots(|_| { root_count += 1; Ok(()) }).unwrap();
    assert!(root_count >= accounts as u64);
    eprintln!("SIGNED_CLOSURE root_count={root_count}");
    report("roots-enumerated");
    let closed = required.close_retained(Default::default()).await;
    report("closure-returned");
    let closed = closed.unwrap();
    assert!(closed.work().bodies >= root_count);
    eprintln!("SIGNED_CLOSURE bodies={} bytes={} edges={}", closed.work().bodies, closed.work().bytes, closed.work().edges);
    let catalogue = closed.own_retirement(Timestamp::new(0), None, || false);
    report("catalogue-returned");
    let mut catalogue = catalogue.unwrap();
    drop(closed);
    drop(projection);
    report("detached-catalogue");
    let (blocks, accounts_before, pending) = store.tx_begin_write().unwrap().ledger_counts().unwrap();
    let counters = crate::ledger_cache::LedgerCache::new(blocks, accounts_before, pending);
    let before = store.block_store().block_count().unwrap();
    let mut examined = 0usize;
    loop {
        let page = catalogue.prepare_page(&store, 256, 4 << 20, 4096).unwrap();
        assert!(page.hashes().is_empty(), "age-ineligible fixture must not delete bodies");
        let progress = page.progress();
        assert!(progress.examined > 0 && progress.examined <= 4096);
        assert!(progress.blocked_body_bytes.is_none());
        examined += progress.examined;
        assert_eq!(page.commit_counted(&store, &counters).unwrap(), (0, 0));
        if progress.complete { break; }
    }
    assert_eq!(examined as u64, before);
    assert_eq!(store.block_store().block_count().unwrap(), before);
    assert_eq!(counters.block_count(), blocks);
    report("catalogue-exhausted");
    drop(catalogue);
    drop(registry);
    report("owners-dropped");
    assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    eprintln!("CLOSURE_SCOPE accounts={accounts} full_node_readiness=false physical_deletion=false");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "capped-host cold reopen of the retained signed closure ledger; authentication only"]
async fn snapshot_retained_reopen_recovery_resource_probe() {
    use crate::confirmed_replay::{scratch_file::ScratchLimit, snapshot::installed};
    use std::sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}};
    let source = std::path::PathBuf::from(std::env::var_os("BURST_SNAPSHOT_REOPEN_LEDGER")
        .expect("set completed signed closure ledger"));
    let baseline = std::path::PathBuf::from(std::env::var_os("BURST_SNAPSHOT_NODE_BASELINE")
        .expect("set immutable signed fixture metadata"));
    let (version, params, epoch, accounts, _): (u8, ProtocolParams, BlockHash, u32, u64) =
        bincode::deserialize(&std::fs::read(baseline.join("resource-probe.bin")).unwrap()).unwrap();
    assert_eq!(version, 1);
    let verified_humans = std::env::var("BURST_SNAPSHOT_REOPEN_VERIFIED_HUMANS")
        .map(|value| value.parse::<u32>().expect("verified human count")).ok();
    let ledger_map_bytes = if let Some(expected) = verified_humans {
        assert!((4000..=100_000).contains(&expected));
        assert_eq!(accounts, expected);
        assert_eq!(params, crate::verification_test_support::governance_fixture_params(
            params.bootstrap_exit_threshold));
        retained_probe_map_size(crate::node::DEFAULT_MAP_SIZE)
    } else {
        assert_eq!(accounts, 32000);
        retained_probe_map_size(128 << 20)
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter("burst_node::confirmed_replay::snapshot::installed=debug")
        .with_ansi(false).with_test_writer().try_init();
    // The completed fixture used only local witnesses. Never silently discard
    // persistent inputs if a later fixture adds them: that needs a separate scope.
    let retain_inputs = match std::env::var("BURST_SNAPSHOT_REOPEN_RETAIN_INPUTS").as_deref() {
        Ok("1") => true,
        Err(std::env::VarError::NotPresent) => false,
        other => panic!("invalid retained-input mode: {other:?}"),
    };
    let input_name = crate::confirmed_replay::scratch_file::PERSISTENT_INPUTS_DIRECTORY;
    assert_eq!(source.join(input_name).exists(), retain_inputs);
    let working = tempfile::tempdir().unwrap().keep();
    std::fs::copy(source.join("data.mdb"), working.join("data.mdb")).unwrap();
    if retain_inputs {
        let input = source.join(input_name).join("data.mdb");
        assert!(input.metadata().unwrap().len() > 0);
        std::fs::create_dir(working.join(input_name)).unwrap();
        std::fs::copy(input, working.join(input_name).join("data.mdb")).unwrap();
        // A new process must account for the retained map before any recovery.
        assert!(ScratchLimit::acquire(&working, 1).is_err());
    }
    eprintln!("REOPEN_WORKING path={} retain_inputs={retain_inputs}", working.display());
    let store = Arc::new(LmdbStore::open(&working, 32, ledger_map_bytes).unwrap());
    let before = store.tx_begin_write().unwrap().ledger_counts().unwrap();
    assert_eq!(store.meta_store().payment_epoch().unwrap(), epoch);
    let map_bytes = 16 << 20;
    let limit = ScratchLimit::acquire_reserved(&working, 64 << 20,
        crate::confirmed_replay::snapshot::history::DurableHistoryInputs::reservation(map_bytes).unwrap()).unwrap();
    let registry = Arc::new(tokio::sync::RwLock::new(crate::connection_registry::ConnectionRegistry::new()));
    static STAGES: Mutex<Option<(Instant, Instant, String)>> = Mutex::new(None);
    static CACHED_PAGES: AtomicU64 = AtomicU64::new(0);
    static PHYSICAL_PAGES: AtomicU64 = AtomicU64::new(0);
    *STAGES.lock().unwrap() = None;
    CACHED_PAGES.store(0, Ordering::SeqCst);
    PHYSICAL_PAGES.store(0, Ordering::SeqCst);
    registry.write().await.recovery_probe = Some(|stage| {
        match stage {
            "private-recovery:inventory-cached-page" => { CACHED_PAGES.fetch_add(1, Ordering::SeqCst); return; }
            "private-recovery:inventory-physical-page" => { PHYSICAL_PAGES.fetch_add(1, Ordering::SeqCst); return; }
            _ => {}
        }
        let now = Instant::now();
        let mut previous = STAGES.lock().unwrap();
        match previous.as_ref() {
            Some((start, last, old)) if old.as_str() != stage => {
                eprintln!("REOPEN_STAGE event={stage} previous={old} since_previous_ms={} elapsed_ms={}",
                    now.duration_since(*last).as_millis(), now.duration_since(*start).as_millis());
                *previous = Some((*start, now, stage.to_owned()));
            }
            None => { eprintln!("REOPEN_STAGE event={stage} elapsed_ms=0"); *previous = Some((now, now, stage.to_owned())); }
            _ => {}
        }
    });
    let started = Instant::now();
    let checked = match installed::recover_metadata_with_durable_inputs(store.clone(), NetworkId::Dev,
        params, registry.clone(), Arc::new(crate::shutdown::ShutdownController::new()), Some((limit.clone(), map_bytes))).await.unwrap() {
        installed::StartupRecovery::Ready(Some(checked)) => checked,
        _ => panic!("cold reopen did not authenticate retained metadata"),
    };
    assert_eq!(checked.recovered().continuation.anchor().hash(), epoch);
    assert!(checked.recovered().continuation.state().accounts.len() >= accounts as usize);
    if let Some(expected) = verified_humans {
        assert_eq!(checked.recovered().continuation.anchor().roster().total_humans(), u64::from(expected));
        assert_eq!(checked.recovered().continuation.state().accounts.len(), expected as usize);
        eprintln!("REOPEN_SCOPE verified_humans={expected} authentication_only=true full_node_readiness=false");
    }
    checked.verify_unchanged(&store.tx_begin_write().unwrap()).unwrap();
    assert_eq!(store.tx_begin_write().unwrap().ledger_counts().unwrap(), before);
    let cached = CACHED_PAGES.load(Ordering::SeqCst);
    let physical = PHYSICAL_PAGES.load(Ordering::SeqCst);
    if retain_inputs {
        assert!(cached >= u64::from(accounts).div_ceil(100));
        assert_eq!(physical, 0, "retained inventory was discarded or only partially reused");
    } else {
        assert_eq!(cached, 0);
        assert!(physical >= u64::from(accounts).div_ceil(100));
    }
    eprintln!("REOPEN_INVENTORY cached_pages={cached} physical_pages={physical}");
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let memory: Vec<_> = status.lines().filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:")).collect();
    eprintln!("REOPEN_AUTHENTICATED accounts={accounts} elapsed_ms={} scratch_bytes={} {}",
        started.elapsed().as_millis(), limit.used_temporary_bytes().unwrap(), memory.join(" "));
    drop(checked); drop(registry); drop(store);
    assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    eprintln!("REOPEN_OWNERS_DROPPED scratch_bytes=0");
}
