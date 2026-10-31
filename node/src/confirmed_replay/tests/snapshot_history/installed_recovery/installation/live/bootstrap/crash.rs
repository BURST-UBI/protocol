use super::*;
use std::{os::unix::process::ExitStatusExt, process::Stdio};
use tokio::io::{AsyncBufReadExt, BufReader};

pub(super) async fn kill_at(config: &crate::NodeConfig, now: u64, stage: &str) {
    kill_with_admission(config, now, stage, true).await;
}

pub(super) async fn kill_running_at(config: &crate::NodeConfig, now: u64, stage: &str) {
    kill_with_admission(config, now, stage, false).await;
}

async fn kill_with_admission(config: &crate::NodeConfig, now: u64, stage: &str, closed: bool) {
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            super::super::crash::CHILD,
            "--ignored",
            "--nocapture",
        ])
        .env(
            "BURST_COMPACT_LIVE_CONFIG",
            serde_json::to_string(config).unwrap(),
        )
        .env("BURST_COMPACT_LIVE_STAGE", stage)
        .env("BURST_COMPACT_LIVE_OCCURRENCE", "1")
        .env("BURST_COMPACT_LIVE_NOW", now.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut probes = std::collections::VecDeque::with_capacity(32);
    let reached = tokio::time::timeout(Duration::from_secs(120), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            if let Some(probe) = line.strip_prefix("BURST_COMPACT_LIVE_PROBE ") {
                if probes.len() == 32 {
                    probes.pop_front();
                }
                probes.push_back(probe.chars().take(128).collect::<String>());
            }
            if line.contains(super::super::crash::READY) {
                return true;
            }
        }
        false
    })
    .await;
    if !matches!(reached, Ok(true)) {
        let _ = child.kill().await;
        let _ = child.wait().await;
        panic!("bootstrap child did not reach {stage}: {reached:?}; recent probes: {probes:?}");
    }
    if closed {
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", config.rpc_port))
                .await
                .is_err(),
            "RPC opened before {stage}"
        );
    }
    tokio::net::TcpStream::connect(("127.0.0.1", config.port))
        .await
        .unwrap();
    child.start_kill().unwrap();
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.signal(), Some(9));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_bootstrap_crashes_preserve_atomic_state_and_closed_admission() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let prepared = prepare();
    let source = Running::start(prepared.source_config.clone(), prepared.now + 1).await;
    source.ready(prepared.epoch).await;
    let bytes = export_bytes(&source.store, &prepared.params, prepared.epoch);
    let expected = authenticate(&source.store, &prepared.params, prepared.epoch).into_state();
    for stage in [
        "bootstrap_prepared",
        "bootstrap_committed",
        "bootstrap_synced",
        "economics",
        "complete",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bootstrap.snapshot");
        std::fs::write(&path, &bytes).unwrap();
        let mut config = prepared.target_config.clone();
        config.data_dir = directory.path().join("node");
        config.snapshot_bootstrap = Some(path);
        kill_at(&config, prepared.now, stage).await;
        let local = Arc::new(LmdbStore::open(&config.data_dir, 32, 32 << 20).unwrap());
        let genesis = crate::genesis_state::block(NetworkId::Dev, &prepared.params);
        if stage == "bootstrap_prepared" {
            assert_eq!(local.meta_store().payment_epoch().unwrap(), genesis.hash);
            assert_eq!(local.block_store().block_count().unwrap(), 1);
            assert!(
                installed::load_base(&local, NetworkId::Dev, &prepared.params)
                    .unwrap()
                    .is_none()
            );
        } else {
            assert_eq!(local.meta_store().payment_epoch().unwrap(), prepared.epoch);
            assert_eq!(
                installed::read_stored_base_bytes(&local).unwrap().unwrap(),
                bytes
            );
            let service = WitnessService::start(source.store.clone(), local.clone(), 0).await;
            let recovered = installed::recover_metadata_from_peers(
                local.clone(),
                NetworkId::Dev,
                prepared.params.clone(),
                service.registry.clone(),
            )
            .await
            .unwrap()
            .unwrap();
            compact::assert_same(recovered.recovered().continuation.state(), &expected);
            drop(recovered);
            service.stop().await;
        }
        drop(local);
        let target = Running::start(config, prepared.now + 1).await;
        target.ready(prepared.epoch).await;
        burn(
            &target,
            &prepared.params,
            prepared.epoch,
            prepared.old_epoch,
            prepared.now + 1,
        )
        .await;
        assert!(crate::checkpoint_store::chain::load(
            &target.store,
            NetworkId::Dev,
            &prepared.params
        )
        .is_err());
        target.stop().await;
        eprintln!("bootstrap {stage}: closed RPC, SIGKILL, atomic state and restarted payment admission pass");
    }
    source.stop().await;
}
