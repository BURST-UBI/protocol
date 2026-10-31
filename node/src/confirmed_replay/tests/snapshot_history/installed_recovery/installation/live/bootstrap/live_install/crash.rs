use super::*;
use std::{
    io::Write,
    os::unix::process::ExitStatusExt,
    process::Stdio,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::io::{AsyncBufReadExt, BufReader};

const CHILD: &str = "confirmed_replay::tests::snapshot_history::installed_recovery::installation::live::bootstrap::live_install::crash::compact_live_snapshot_crash_child";
const READY: &str = "BURST_LIVE_SNAPSHOT_CRASH_READY";
static ARMED: AtomicBool = AtomicBool::new(false);
static STORE: std::sync::OnceLock<Arc<LmdbStore>> = std::sync::OnceLock::new();

fn pause(stage: &str) {
    if !ARMED.load(Ordering::Acquire)
        || stage != std::env::var("BURST_LIVE_SNAPSHOT_STAGE").unwrap()
    {
        return;
    }
    println!(
        "{READY}:{}",
        STORE.get().unwrap().block_store().block_count().unwrap()
    );
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::park();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "subprocess helper for live snapshot installation crashes"]
async fn compact_live_snapshot_crash_child() {
    let mut config: crate::NodeConfig =
        serde_json::from_str(&std::env::var("BURST_LIVE_SNAPSHOT_CONFIG").unwrap()).unwrap();
    config.params = Fixture::selection().params;
    let now: u64 = std::env::var("BURST_LIVE_SNAPSHOT_NOW")
        .unwrap()
        .parse()
        .unwrap();
    let previous: BlockHash =
        serde_json::from_str(&std::env::var("BURST_LIVE_SNAPSHOT_PREVIOUS").unwrap()).unwrap();
    let client = NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap();
    let mut node = crate::BurstNode::new(config).await.unwrap();
    node.block_processor.lock().await.test_time = Some(Arc::new(AtomicU64::new(now)));
    node.verification_fixture_clock = Some(Arc::new(|| None));
    node.checkpoint_publication_probe = Some(pause);
    let installer = node.snapshot_installer();
    let store = node.store.clone();
    assert!(STORE.set(store.clone()).is_ok());
    let running = tokio::spawn(async move { node.start_with_services(burst_daemon::start_public_services).await });
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            assert!(
                !running.is_finished(),
                "node stopped before live installation"
            );
            if client
                .signing_state(&address(0))
                .await
                .is_ok_and(|state| state.payment_epoch == previous)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    store
        .meta_store()
        .reserve_consensus_vote_at(
            &previous,
            &address(0),
            &address(30),
            &BlockHash::ZERO,
            &BlockHash::new([98; 32]),
            true,
            0,
            Some(0),
        )
        .unwrap();
    ARMED.store(true, Ordering::Release);
    let result = installer
        .install(std::env::var("BURST_LIVE_SNAPSHOT_ARCHIVE").unwrap().into())
        .await;
    panic!("live snapshot install returned before crash boundary: {result:?}");
}

async fn kill_at(
    config: &crate::NodeConfig,
    path: &std::path::Path,
    now: u64,
    stage: &str,
    previous: BlockHash,
) -> u64 {
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD, "--ignored", "--nocapture"])
        .env(
            "BURST_LIVE_SNAPSHOT_CONFIG",
            serde_json::to_string(config).unwrap(),
        )
        .env("BURST_LIVE_SNAPSHOT_ARCHIVE", path)
        .env("BURST_LIVE_SNAPSHOT_NOW", now.to_string())
        .env("BURST_LIVE_SNAPSHOT_STAGE", stage)
        .env(
            "BURST_LIVE_SNAPSHOT_PREVIOUS",
            serde_json::to_string(&previous).unwrap(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let reached = tokio::time::timeout(Duration::from_secs(120), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            if let Some((_, count)) = line.split_once(&format!("{READY}:")) {
                return Some(count.parse::<u64>().unwrap());
            }
        }
        None
    })
    .await;
    if !matches!(reached, Ok(Some(_))) {
        let _ = child.kill().await;
        let _ = child.wait().await;
        panic!("live snapshot child did not reach {stage}: {reached:?}");
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
    reached.unwrap().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_snapshot_installation_crashes_recover_and_admit_payments() {
    crash_installation(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_snapshot_replacement_crashes_recover_and_admit_payments() {
    crash_installation(true).await;
}

async fn crash_installation(replace: bool) {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let (prepared, _) = prepare_active();
    let source = Running::start(prepared.source_config.clone(), prepared.now + 1).await;
    source.ready(prepared.epoch).await;
    let bytes = export_bytes(&source.store, &prepared.params, prepared.epoch);
    let expected = authenticate(&source.store, &prepared.params, prepared.epoch).into_state();
    let previous = if replace {
        prepared.old_epoch
    } else {
        crate::genesis_state::block(NetworkId::Dev, &prepared.params).hash
    };
    let old_state = replace
        .then(|| authenticate(&source.store, &prepared.params, prepared.old_epoch).into_state());
    let original = source.registry.read().await.checkpoint_inbox().unwrap();
    let (sender, _withheld) = tokio::sync::mpsc::channel(32);
    source.registry.write().await.set_checkpoint_inbox(sender);
    for stage in [
        "prepared",
        "committed",
        "economics",
        "authority",
        "complete",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("live.snapshot");
        std::fs::write(&path, &bytes).unwrap();
        let mut config = prepared.target_config.clone();
        config.data_dir = directory.path().join("node");
        if replace {
            let old_path = directory.path().join("old.snapshot");
            std::fs::write(&old_path, &prepared.bytes).unwrap();
            config.snapshot_bootstrap = Some(old_path);
        }
        let count = kill_at(&config, &path, prepared.now + 1, stage, previous).await;
        config.snapshot_bootstrap = None;
        let local = Arc::new(LmdbStore::open(&config.data_dir, 32, 32 << 20).unwrap());
        let promises = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"consensus_final/")
            .unwrap();
        assert_eq!(promises.len(), 1);
        assert_eq!(
            promises[0],
            (
                format!(
                    "consensus_final/v2/{previous}/{}/{}/{}",
                    address(0),
                    address(30),
                    BlockHash::ZERO
                )
                .into_bytes(),
                BlockHash::new([98; 32]).as_bytes().to_vec()
            )
        );
        assert_eq!(local.block_store().block_count().unwrap(), count);
        if stage == "prepared" {
            assert_eq!(local.meta_store().payment_epoch().unwrap(), previous);
            if !replace {
                assert!(
                    installed::load_base(&local, NetworkId::Dev, &prepared.params)
                        .unwrap()
                        .is_none()
                );
            }
            config.snapshot_bootstrap = Some(path);
        } else {
            assert_eq!(local.meta_store().payment_epoch().unwrap(), prepared.epoch);
        }
        if replace || stage != "prepared" {
            assert_eq!(
                installed::read_stored_base_bytes(&local).unwrap().unwrap(),
                if stage == "prepared" {
                    &prepared.bytes
                } else {
                    &bytes
                }
                .clone()
            );
            let proofs = WitnessService::start(source.store.clone(), local.clone(), 0).await;
            let recovered = installed::recover_metadata_from_peers(
                local.clone(),
                NetworkId::Dev,
                prepared.params.clone(),
                proofs.registry.clone(),
            )
            .await
            .unwrap()
            .unwrap();
            compact::assert_same(
                recovered.recovered().continuation.state(),
                if stage == "prepared" {
                    old_state.as_ref().unwrap()
                } else {
                    &expected
                },
            );
            drop(recovered);
            proofs.stop().await;
        }
        drop(local);
        let target = Running::start(config, prepared.now + 2).await;
        target.ready(prepared.epoch).await;
        assert_retained(&target.store, &promises);
        burn(
            &target,
            &prepared.params,
            prepared.epoch,
            previous,
            prepared.now + 2,
        )
        .await;
        target.stop().await;
        eprintln!(
            "live snapshot {stage}: SIGKILL, durable state, promises and restarted payments pass"
        );
    }
    source.registry.write().await.set_checkpoint_inbox(original);
    source.stop().await;
}
