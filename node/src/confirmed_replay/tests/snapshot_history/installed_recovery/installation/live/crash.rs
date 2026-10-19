use super::*;
use std::{
    io::Write,
    os::unix::process::ExitStatusExt,
    process::Stdio,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::io::{AsyncBufReadExt, BufReader};

pub(super) const CHILD: &str = "confirmed_replay::tests::snapshot_history::installed_recovery::installation::live::crash::compact_live_crash_child";
pub(super) const READY: &str = "BURST_COMPACT_LIVE_CRASH_READY";
static OCCURRENCES: AtomicUsize = AtomicUsize::new(0);

fn pause(stage: &str) {
    println!("BURST_COMPACT_LIVE_PROBE {stage}");
    tracing::debug!(stage, "compact crash probe");
    if stage != std::env::var("BURST_COMPACT_LIVE_STAGE").unwrap() {
        return;
    }
    // Startup publishes the base first. The authority probe belongs only to
    // successor installation, while the other probes also run during startup.
    let expected = std::env::var("BURST_COMPACT_LIVE_OCCURRENCE")
        .map(|value| value.parse().unwrap())
        .unwrap_or(if stage == "authority" { 2 } else { 3 });
    if OCCURRENCES.fetch_add(1, Ordering::SeqCst) + 1 != expected {
        return;
    }
    println!("{READY}");
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::park();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "subprocess helper for compact live successor publication crashes"]
async fn compact_live_crash_child() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .try_init();
    let mut config: crate::NodeConfig =
        serde_json::from_str(&std::env::var("BURST_COMPACT_LIVE_CONFIG").unwrap()).unwrap();
    // NodeConfig deliberately omits protocol parameters from serialized config.
    config.params = Fixture::selection().params;
    let now = std::env::var("BURST_COMPACT_LIVE_NOW")
        .unwrap()
        .parse()
        .unwrap();
    let mut node = crate::BurstNode::new(config).await.unwrap();
    node.block_processor.lock().await.test_time = Some(Arc::new(AtomicU64::new(now)));
    node.verification_fixture_clock = Some(Arc::new(|| None));
    node.checkpoint_publication_probe = Some(pause);
    let result = node.start().await;
    panic!(
        "compact crash child exited before its boundary: {result:?}; {:?}",
        node.shutdown.failure()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_second_successor_crashes_recover_and_admit_payments() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let prepared = prepare();
    let source = Running::start(prepared.source_config.clone(), prepared.now + 1).await;
    source.ready(prepared.epoch).await;
    let expected =
        authenticate(&source.store, &prepared.params, prepared.epoch).into_continuation();
    let chain =
        crate::checkpoint_store::chain::load(&source.store, NetworkId::Dev, &prepared.params)
            .unwrap();
    let previous_epoch = chain[1].certificate().checkpoint.hash();
    let previous =
        authenticate(&source.store, &prepared.params, previous_epoch).into_continuation();
    for stage in [
        "prepared",
        "committed",
        "economics",
        "authority",
        "complete",
    ] {
        let directory = tempfile::tempdir().unwrap();
        std::fs::copy(
            prepared.target_config.data_dir.join("data.mdb"),
            directory.path().join("data.mdb"),
        )
        .unwrap();
        let mut config = prepared.target_config.clone();
        config.data_dir = directory.path().to_path_buf();
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD, "--ignored", "--nocapture"])
            .env(
                "BURST_COMPACT_LIVE_CONFIG",
                serde_json::to_string(&config).unwrap(),
            )
            .env("BURST_COMPACT_LIVE_STAGE", stage)
            .env("BURST_COMPACT_LIVE_NOW", prepared.now.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let reached = tokio::time::timeout(Duration::from_secs(120), async {
            while let Some(line) = lines.next_line().await.unwrap() {
                if line.contains(READY) {
                    return true;
                }
            }
            false
        })
        .await;
        if !matches!(reached, Ok(true)) {
            let _ = child.kill().await;
            let _ = child.wait().await;
            panic!("compact live child did not reach {stage}: {reached:?}");
        }
        for port in [config.port, config.rpc_port] {
            tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::TcpStream::connect(("127.0.0.1", port)),
            )
            .await
            .unwrap()
            .unwrap();
        }
        child.start_kill().unwrap();
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.signal(), Some(9), "{stage}");

        let local = Arc::new(LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
        assert_eq!(
            local.meta_store().payment_epoch().unwrap(),
            if stage == "prepared" {
                previous_epoch
            } else {
                prepared.epoch
            },
            "{stage}"
        );
        assert_eq!(
            installed::read_stored_base_bytes(&local).unwrap().unwrap(),
            prepared.bytes
        );
        assert!(
            crate::checkpoint_store::chain::load(&local, NetworkId::Dev, &prepared.params).is_err()
        );
        let service = Service::start(source.store.clone(), local.clone(), 0).await;
        let recovered = installed::recover_metadata_from_peers(
            local.clone(),
            NetworkId::Dev,
            prepared.params.clone(),
            service.registry.clone(),
        )
        .await
        .unwrap()
        .unwrap();
        compact::assert_same(
            recovered.recovered().continuation.state(),
            if stage == "prepared" {
                previous.state()
            } else {
                expected.state()
            },
        );
        drop(recovered);
        service.stop().await;
        drop(local);

        let target = Running::start(config, prepared.now + 1).await;
        target.ready(prepared.epoch).await;
        let payment = burn(
            &target,
            &prepared.params,
            prepared.epoch,
            prepared.old_epoch,
            prepared.now + 1,
        )
        .await;
        let node = target.stop().await;
        assert!(node.store.block_store().exists(&payment).unwrap());
        assert_eq!(
            node.active_elections.read().await.authority_epoch(),
            Some(prepared.epoch)
        );
        assert_eq!(
            node.brn_engine.lock().await.wallets,
            BrnEngine::load_from_store(&node.store.brn_store())
                .unwrap()
                .wallets
        );
        {
            let trst = node.trst_engine.lock().await;
            let persisted = node.store.tx_begin_write().unwrap();
            assert_eq!(
                trst.save_wallets(),
                crate::trst_storage::load(&persisted, trst.expiry_secs)
                    .unwrap()
                    .save_wallets()
            );
        }
        let counts = node
            .store
            .tx_begin_write()
            .unwrap()
            .ledger_counts()
            .unwrap();
        assert_eq!(
            (
                node.ledger_cache.block_count(),
                node.ledger_cache.account_count(),
                node.ledger_cache.pending_count()
            ),
            counts
        );
        eprintln!("compact live {stage}: SIGKILL, authenticated projection, restarted caches and payment admission pass");
    }
    source.stop().await;
}
