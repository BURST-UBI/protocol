use super::*;
use std::{io::Write, path::Path, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, BufReader};

const CHILD: &str = "confirmed_replay::tests::snapshot_history::installed_recovery::publication_crash::compact_publication_child";
const READY: &str = "BURST_COMPACT_PUBLICATION_READY";

async fn node(
    local: Arc<LmdbStore>,
    params: ProtocolParams,
) -> (tempfile::TempDir, crate::node::BurstNode) {
    let directory = tempfile::tempdir().unwrap();
    let mut node = crate::node::BurstNode::new(crate::config::NodeConfig {
        data_dir: directory.path().to_path_buf(),
        network: NetworkId::Dev,
        params,
        enable_representative: false,
        nts_sources: vec![],
        ..crate::config::NodeConfig::default()
    })
    .await
    .unwrap();
    node.store = local;
    node.verification_fixture_clock = Some(Arc::new(|| None));
    (directory, node)
}

async fn recover(
    source: Arc<LmdbStore>,
    local: Arc<LmdbStore>,
    params: ProtocolParams,
) -> installed::RecoveredInstalledMetadata {
    let service = Service::start(source, Arc::clone(&local), 0).await;
    let recovered = installed::recover_metadata_from_peers(
        local,
        NetworkId::Dev,
        params,
        Arc::clone(&service.registry),
    )
    .await
    .unwrap()
    .unwrap();
    service.stop().await;
    recovered
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "subprocess helper for compact publication crash boundaries"]
async fn compact_publication_child() {
    let directory = std::env::var("BURST_COMPACT_CRASH_DIRECTORY").unwrap();
    let (fixture, _dir, source, _, certificate) = selection::setup();
    let local = Arc::new(LmdbStore::open(Path::new(&directory), 32, 32 << 20).unwrap());
    let recovered = recover(Arc::new(source), Arc::clone(&local), fixture.params.clone()).await;
    let (_runtime_dir, mut node) = node(local, fixture.params).await;
    node.checkpoint_publication_probe = Some(|stage| {
        if stage == std::env::var("BURST_COMPACT_CRASH_STAGE").unwrap() {
            println!("{READY}");
            std::io::stdout().flush().unwrap();
            loop {
                std::thread::park();
            }
        }
    });
    node.publish_recovered(recovered, Timestamp::new(certificate.checkpoint.cutoff))
        .await
        .unwrap();
    panic!("compact publication crash boundary was not reached");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_publication_recovers_after_abrupt_exit_at_each_boundary() {
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let expected =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    let source = Arc::new(source);
    for stage in ["prepared", "committed", "economics", "complete"] {
        let (directory, local) = cemented::sparse(&expected, &source);
        records(&local, &source, &fixture, &bytes);
        local
            .meta_store()
            .put_meta("application-preference", b"keep")
            .unwrap();
        local.force_sync().unwrap();
        drop(local);
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD, "--ignored", "--nocapture"])
            .env("BURST_COMPACT_CRASH_DIRECTORY", directory.path())
            .env("BURST_COMPACT_CRASH_STAGE", stage)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let reached = tokio::time::timeout(Duration::from_secs(30), async {
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
            panic!("compact publication did not reach {stage}: {reached:?}");
        }
        child.kill().await.unwrap();
        let status = child.wait().await.unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(status.signal(), Some(9), "{stage}");

        let local = Arc::new(LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
        let recovered = recover(
            Arc::clone(&source),
            Arc::clone(&local),
            fixture.params.clone(),
        )
        .await;
        compact::assert_same(recovered.recovered().continuation.state(), expected.state());
        let (_runtime_dir, mut restarted) = node(Arc::clone(&local), fixture.params.clone()).await;
        restarted.ledger_cache.reset((999, 99, 9));
        restarted
            .frontier
            .write()
            .await
            .update(address(99), BlockHash::new([99; 32]));
        restarted
            .publish_recovered(recovered, Timestamp::new(expected.anchor().cutoff()))
            .await
            .unwrap();
        assert!(restarted.connection_registry.read().await.is_recovering());
        assert_eq!(
            restarted.active_elections.read().await.authority_epoch(),
            Some(expected.anchor().hash())
        );
        let persisted_trst = {
            let batch = local.tx_begin_write().unwrap();
            crate::trst_storage::load(&batch, fixture.params.trst_expiry_secs)
                .unwrap()
                .save_wallets()
        };
        assert_eq!(
            restarted.trst_engine.lock().await.save_wallets(),
            persisted_trst
        );
        let snapshot = restarted.verification_orchestrator.lock().await.snapshot();
        let persisted = burst_verification::OrchestratorSnapshot::decode(
            &local
                .meta_store()
                .get_meta(crate::endorsement_commit::SNAPSHOT_KEY)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(snapshot.encode().unwrap(), persisted.encode().unwrap());
        let counts = local.tx_begin_write().unwrap().ledger_counts().unwrap();
        assert_eq!(
            (
                restarted.ledger_cache.block_count(),
                restarted.ledger_cache.account_count(),
                restarted.ledger_cache.pending_count()
            ),
            counts
        );
        let accounts = local
            .tx_begin_write()
            .unwrap()
            .bounded_accounts(131_072)
            .unwrap();
        let frontier = restarted.frontier.read().await;
        assert_eq!(
            frontier.account_count(),
            accounts
                .iter()
                .filter(|account| account.block_count > 0)
                .count()
        );
        for account in accounts.iter().filter(|account| account.block_count > 0) {
            assert_eq!(frontier.get_head(&account.address), Some(&account.head));
        }
        assert_eq!(
            local
                .meta_store()
                .get_meta("application-preference")
                .unwrap(),
            b"keep"
        );
        assert_eq!(
            installed::read_stored_base_bytes(&local).unwrap().unwrap(),
            bytes
        );
    }
}
