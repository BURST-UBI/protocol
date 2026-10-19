use super::*;
use std::process::Stdio;

const CHILD: &str = "confirmed_replay::tests::snapshot_history::installed_recovery::installation::live::retirement::process::isolated_retired_history_receiver";
const COMPLETE: &str = "BURST_ISOLATED_RETIRED_HISTORY_COMPLETE";

pub(super) fn initialize_diagnostics() {
    // The receiver is a new process and cannot inherit the parent's subscriber.
    // Its stdout is a bounded completion channel, not a trace sink.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .try_init();
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Receiver {
    config: crate::NodeConfig,
    // NodeConfig intentionally omits protocol parameters from its serialized form.
    params: ProtocolParams,
    epoch: BlockHash,
    now: u64,
}

pub(super) async fn cold_receiver(config: &crate::NodeConfig, epoch: BlockHash, now: u64) {
    let stderr = match std::env::var_os("BURST_RETIRED_HISTORY_RECEIVER_LOG") {
        Some(path) => {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            Stdio::from(options.open(path).expect("new receiver diagnostic log"))
        }
        None => Stdio::inherit(),
    };
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD, "--ignored", "--nocapture"])
        .env(
            "BURST_RETIRED_HISTORY_RECEIVER",
            serde_json::to_string(&Receiver {
                config: config.clone(),
                params: config.params.clone(),
                epoch,
                now,
            })
            .unwrap(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let output = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut bytes = Vec::new();
        stdout
            .take(64 * 1024)
            .read_to_end(&mut bytes)
            .await
            .unwrap();
        bytes
    });
    // Includes startup and graceful shutdown; Running::ready still allows only 90 seconds.
    let status = match tokio::time::timeout(Duration::from_secs(150), child.wait()).await {
        Ok(status) => status.unwrap(),
        Err(error) => {
            child.kill().await.unwrap();
            child.wait().await.unwrap();
            output.abort();
            panic!("isolated receiver did not finish: {error}");
        }
    };
    let output = String::from_utf8(output.await.unwrap()).unwrap();
    assert!(
        status.success(),
        "isolated receiver failed: {status}\n{output}"
    );
    assert!(
        output.contains(COMPLETE),
        "receiver test did not run: {output}"
    );
    eprintln!("isolated receiver: cold catch-up, payment balance and graceful shutdown pass");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "subprocess entry point; invoked by the cold-start retirement journey"]
async fn isolated_retired_history_receiver() {
    initialize_diagnostics();
    let mut receiver: Receiver = serde_json::from_str(
        &std::env::var("BURST_RETIRED_HISTORY_RECEIVER").expect("receiver configuration"),
    )
    .unwrap();
    receiver.config.params = receiver.params;
    // This process has neither generated the source history nor warmed its signature cache.
    let node = Running::start(receiver.config.clone(), receiver.now).await;
    let started = std::time::Instant::now();
    {
        let ready = node.ready(receiver.epoch);
        tokio::pin!(ready);
        let mut progress = tokio::time::interval(Duration::from_secs(10));
        loop {
            tokio::select! {
                _ = &mut ready => break,
                _ = progress.tick() => {
                    let accepted = CertifiedCheckpoint::stored_tip(
                        &node.store, NetworkId::Dev, &receiver.config.params,
                    );
                    eprintln!("isolated receiver: elapsed={:?}, accepted={accepted:?}", started.elapsed());
                }
            }
        }
    }
    eprintln!("isolated receiver ready after {:?}", started.elapsed());
    assert_eq!(
        CertifiedCheckpoint::stored_tip(&node.store, NetworkId::Dev, &receiver.config.params)
            .unwrap(),
        130
    );
    assert_eq!(
        node.store
            .account_store()
            .get_account(&address(1))
            .unwrap()
            .trst_balance,
        4
    );
    assert!(installed::read_stored_base_bytes(&node.store)
        .unwrap()
        .is_none());
    node.stop().await;
    println!("{COMPLETE}");
}

#[test]
fn isolated_receiver_configuration_preserves_fixture_parameters() {
    let mut config = crate::NodeConfig::default();
    config.params.bootstrap_exit_threshold = 2;
    config.params.min_work_difficulty = 0;
    let expected = config.params.params_hash();
    let receiver = Receiver {
        params: config.params.clone(),
        config,
        epoch: BlockHash::ZERO,
        now: 123,
    };
    let decoded: Receiver =
        serde_json::from_str(&serde_json::to_string(&receiver).unwrap()).unwrap();
    assert_eq!(decoded.params.params_hash(), expected);
    assert_ne!(decoded.config.params.params_hash(), expected);
    assert_eq!(decoded.now, 123);
}
