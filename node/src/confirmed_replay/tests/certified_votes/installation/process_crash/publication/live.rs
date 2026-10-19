use super::*;
use std::sync::atomic::AtomicU16;

const LIVE_CHILD: &str = "confirmed_replay::tests::certified_votes::installation::process_crash::publication::live::live_child";
static RPC_PORT: AtomicU16 = AtomicU16::new(0);
static P2P_PORT: AtomicU16 = AtomicU16::new(0);

fn pause(stage: &str) {
    if stage != std::env::var("BURST_PUBLICATION_CRASH_STAGE").unwrap() {
        return;
    }
    let until = std::time::Instant::now() + Duration::from_secs(15);
    for port in [&RPC_PORT, &P2P_PORT] {
        let address = ([127, 0, 0, 1], port.load(Ordering::SeqCst)).into();
        loop {
            if std::net::TcpStream::connect_timeout(&address, Duration::from_millis(50)).is_ok() {
                break;
            }
            assert!(
                std::time::Instant::now() < until,
                "node listener never opened"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    signal_ready();
    park();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "subprocess helper for crashes with live node workers"]
async fn live_child() {
    let directory = std::env::var("BURST_INSTALLATION_CRASH_DIRECTORY").unwrap();
    let governance = std::env::var_os("BURST_GOVERNANCE_CRASH").is_some();
    let fixture = publication_fixture(governance);
    let now = if governance { 142 } else { CUTOFF + 1 };
    let config = config(Path::new(&directory), &fixture.params);
    RPC_PORT.store(config.rpc_port, Ordering::SeqCst);
    P2P_PORT.store(config.port, Ordering::SeqCst);
    let mut node = BurstNode::new(config).await.unwrap();
    node.block_processor.lock().await.test_time = Some(Arc::new(AtomicU64::new(now)));
    node.verification_fixture_clock = Some(Arc::new(move || {
        Some(crate::checkpoint_replay::SigningClock {
            now,
            error_bound: 0,
        })
    }));
    let info = node.vrf_client.chain_info().unwrap().clone();
    node.vrf_client = Arc::new(
        burst_vrf::DrandClient::with_chain("http://127.0.0.1:1", &info.chain_hash)
            .with_verifier(burst_vrf::DrandVerifier::mainnet().unwrap())
            .with_chain_info(info),
    );
    node.checkpoint_publication_probe = Some(pause);
    node.start().await.unwrap();
    panic!("node exited before publication crash");
}

fn baseline(fixture: &Fixture, store: &LmdbStore, governance: bool) {
    let state = fixture
        .replay(if governance { 140 } else { CUTOFF })
        .unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    if governance {
        // Model a node that initialized genesis before admitting its governance
        // tail. Startup must still authenticate both the authority and schedule.
        let genesis = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            NetworkId::Dev,
            &fixture.params,
        )
        .unwrap();
        let cutoff = fixture.blocks[0].timestamp.as_secs();
        assert_eq!(batch.payment_epoch().unwrap(), genesis.hash());
        batch
            .put_meta(
                INSTALLED_KEY,
                &crate::confirmed_replay::installation::authority_bytes(&genesis, cutoff).unwrap(),
            )
            .unwrap();
        batch.put_meta(CUTOFF_KEY, &cutoff.to_be_bytes()).unwrap();
        batch
            .put_meta(
                burst_store::meta::PAYMENT_VOTE_LIMIT_KEY,
                &crate::confirmed_replay::installation::vote_limit_bytes(
                    genesis.hash(),
                    cutoff,
                    None,
                )
                .unwrap(),
            )
            .unwrap();
        let mut schedule = crate::confirmed_governance::ConfirmedGovernance::initial(
            genesis.hash(),
            fixture.params.clone(),
        );
        for block in &fixture.blocks {
            schedule.promote(&batch, block).unwrap();
        }
        schedule.persist(&mut batch).unwrap();
    }
    for account in state.accounts.values() {
        batch.put_account_info(account, false).unwrap();
        if account.block_count > 0 {
            batch.put_frontier(&account.address, &account.head).unwrap();
        }
    }
    for ((account, source), pending) in &state.pending {
        batch
            .put_pending(
                account,
                source.as_bytes(),
                &bincode::serialize(pending).unwrap(),
            )
            .unwrap();
    }
    crate::ledger_updater::persist_economics(&mut batch, &state.brn, &state.trst).unwrap();
    batch
        .put_meta(
            crate::endorsement_commit::SNAPSHOT_KEY,
            &state.verification.snapshot().encode().unwrap(),
        )
        .unwrap();
    batch
        .put_brn_meta(
            burst_verification::bootstrap::BootstrapPhase::KEY,
            &state.bootstrap_phase.encoding(),
        )
        .unwrap();
    state
        .governance
        .unwrap_or_else(|| {
            crate::governance_commit::CandidateGovernance::initial(
                fixture.params.clone(),
                NetworkId::Dev,
                fixture.blocks[0].hash,
            )
        })
        .persist(&mut batch)
        .unwrap();
    batch.commit().unwrap();
    if governance {
        crate::payment_authority::initialize(store, NetworkId::Dev, &fixture.params).unwrap();
    }
}

fn projection(store: &LmdbStore, expiry_secs: u64) -> Contents {
    let mut data = contents(store);
    data.retain(|table, _| {
        matches!(
            table.as_str(),
            "accounts"
                | "blocks"
                | "block_heights"
                | "block_height_reverse"
                | "frontiers"
                | "brn_wallets"
                | "brn_meta"
                | "pending"
                | "pending_senders"
                | "trst_origins"
                | "trst_expiry"
                | "trst_reverse"
                | "meta"
        )
    });
    data.get_mut("meta").unwrap().retain(|(key, _)| {
        [
            "consti_engine",
            burst_governance::DelegationEngine::meta_key(),
            crate::endorsement_commit::SNAPSHOT_KEY,
            crate::confirmed_replay::installation::INSTALLED_KEY,
            crate::confirmed_replay::installation::CUTOFF_KEY,
            burst_store::meta::PAYMENT_EPOCH_KEY,
        ]
        .iter()
        .any(|name| key == name.as_bytes())
    });
    // Startup may migrate the aggregate fixture to rows. Compare every decoded
    // holding and provenance edge, while retaining exact checks for other state.
    let batch = store.tx_begin_write().unwrap();
    let meta = data.get_mut("meta").unwrap();
    meta.push((
        TrstEngine::meta_key().as_bytes().to_vec(),
        crate::trst_storage::load(&batch, expiry_secs)
            .unwrap()
            .save_wallets(),
    ));
    meta.push((
        b"merger_graph".to_vec(),
        crate::graph_storage::load(&batch).unwrap().to_bytes(),
    ));
    meta.sort_by(|a, b| a.0.cmp(&b.0));
    data
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_worker_publication_crashes_recover_epoch_and_economics() {
    live_publication_crashes(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_governance_publication_crashes_recover_counting_and_delegated_keys() {
    live_publication_crashes(true).await;
}

async fn live_publication_crashes(governance: bool) {
    let fixture = publication_fixture(governance);
    let (directory, store, heads) = fixture.store();
    baseline(&fixture, &store, governance);
    accept_publication(&fixture, &store, heads, governance);
    store.env().force_sync().unwrap();
    drop(store);
    let expected_dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        directory.path().join("data.mdb"),
        expected_dir.path().join("data.mdb"),
    )
    .unwrap();
    let node = BurstNode::new(config(expected_dir.path(), &fixture.params))
        .await
        .unwrap();
    runtime(&node, &fixture.params)
        .await
        .install_next()
        .await
        .unwrap()
        .unwrap();
    if governance {
        check_governance_counting(&node).await;
    }
    let expected = projection(&node.store, fixture.params.trst_expiry_secs);
    let epoch = node.store.meta_store().payment_epoch().unwrap();
    drop(node);
    for stage in ["committed", "economics", "authority", "complete"] {
        let crashed = tempfile::tempdir().unwrap();
        std::fs::copy(
            directory.path().join("data.mdb"),
            crashed.path().join("data.mdb"),
        )
        .unwrap();
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        if governance {
            command.env("BURST_GOVERNANCE_CRASH", "1");
        } else {
            command.env_remove("BURST_GOVERNANCE_CRASH");
        }
        let mut child = command
            .args(["--exact", LIVE_CHILD, "--ignored", "--nocapture"])
            .env("BURST_INSTALLATION_CRASH_DIRECTORY", crashed.path())
            .env("BURST_PUBLICATION_CRASH_STAGE", stage)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
        tokio::time::timeout(Duration::from_secs(30), async {
            while let Some(line) = output.next_line().await.unwrap() {
                if line.contains(READY) {
                    return;
                }
            }
            panic!("live child exited before {stage}");
        })
        .await
        .expect("live child did not reach publication with open listeners");
        child.start_kill().unwrap();
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.signal(), Some(9));
        let store = open(crashed.path());
        assert_contents(
            &projection(&store, fixture.params.trst_expiry_secs),
            &expected,
        );
        drop(store);
        restart_and_admit(crashed.path(), &fixture, epoch).await;
        eprintln!(
            "live {stage}: worker crash, durable projection and restarted payment admission pass"
        );
    }
}
