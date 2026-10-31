use super::*;
use std::{io::Write, path::Path, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, BufReader};

mod pressure;

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

fn legacy_verification_fixture(store: &LmdbStore) {
    let mut batch = store.tx_begin_write().unwrap();
    let snapshot = crate::verification_storage::load(&batch).unwrap().snapshot().encode().unwrap();
    let keys: Vec<_> = batch.checkpoint_metadata_entries(b"verification_state/").unwrap()
        .map(|row| String::from_utf8(row.unwrap().0.to_vec()).unwrap()).collect();
    for key in keys { batch.delete_meta(&key).unwrap(); }
    batch.delete_meta("verification_records_format").unwrap();
    batch.put_meta(crate::endorsement_commit::SNAPSHOT_KEY, &snapshot).unwrap();
    assert!(!batch.uses_verification_records().unwrap());
    assert_eq!(crate::verification_storage::load(&batch).unwrap().snapshot().encode().unwrap(), snapshot);
    batch.commit().unwrap();
}

async fn recover(
    source: Arc<LmdbStore>,
    local: Arc<LmdbStore>,
    params: ProtocolParams,
) -> installed::RecoveredInstalledMetadata {
    let service = WitnessService::start(source, Arc::clone(&local), 0).await;
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
    if std::env::var("BURST_COMPACT_CRASH_KIND").as_deref() == Ok("recovery-inputs") {
        durable_inputs_child(Path::new(&directory));
    }
    if std::env::var("BURST_COMPACT_CRASH_KIND").as_deref() == Ok("pressure-inputs") {
        pressure::child(Path::new(&directory)).await;
    }
    if std::env::var("BURST_COMPACT_CRASH_KIND").as_deref() == Ok("inventory-inputs") {
        durable_inventory_child(Path::new(&directory)).await;
    }
    let kind = std::env::var("BURST_COMPACT_CRASH_KIND").unwrap_or_default();
    if matches!(kind.as_str(), "retirement" | "owned-retirement") {
        body_retirement::crash_child(Path::new(&directory), kind == "owned-retirement", |stage| {
            if stage == std::env::var("BURST_COMPACT_CRASH_STAGE").unwrap() {
                println!("{READY}");
                std::io::stdout().flush().unwrap();
                loop { std::thread::park(); }
            }
        }).await;
        panic!("retirement crash boundary was not reached");
    }
    if std::env::var("BURST_COMPACT_CRASH_KIND").as_deref() == Ok("verification") {
        let store = LmdbStore::open(Path::new(&directory), 32, 32 << 20).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        batch.normalize_verification_records().unwrap();
        if std::env::var("BURST_COMPACT_CRASH_STAGE").unwrap() == "committed" {
            batch.commit().unwrap();
        }
        println!("{READY}");
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }

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
    node.publish_recovered(recovered)
        .await
        .unwrap();
    panic!("compact publication crash boundary was not reached");
}

async fn kill_at_boundary(directory: &Path, stage: &str, verification: bool) {
    kill_at_kind(directory, stage, if verification { "verification" } else { "publication" }).await;
}

pub(super) async fn kill_at_kind(directory: &Path, stage: &str, kind: &str) {
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD, "--ignored", "--nocapture"])
        .env("BURST_COMPACT_CRASH_DIRECTORY", directory)
        .env("BURST_COMPACT_CRASH_STAGE", stage)
        .env("BURST_COMPACT_CRASH_KIND", kind)
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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn verification_normalization_recovers_after_abrupt_exit_at_commit_boundary() {
    use burst_store_lmdb::block_undo::{BlockUndo, UndoChange, UndoTable};
    use burst_verification::VerificationOrchestrator;
    for legacy in [false, true] {
        for stage in ["prepared", "committed"] {
            let directory = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
            let mut engine = VerificationOrchestrator::new();
            let target = address(1);
            engine.initialize_genesis(&target);
            let expected = engine.snapshot().encode().unwrap();
            let (key, value) = engine
                .target_record(&target)
                .unwrap()
                .storage_entry()
                .unwrap();
            let journal = BlockUndo {
                block: BlockHash::new([65; 32]),
                account: target,
                height: 1,
                changes: vec![UndoChange {
                    table: UndoTable::Meta,
                    key: key.into_bytes(),
                    before: None,
                    after: Some(value),
                }],
            };
            let journal_bytes = bincode::serialize(&journal).unwrap();
            let journal_key = format!("block_undo/v1/{}", "A".repeat(32));
            let mut batch = store.tx_begin_write().unwrap();
            batch
                .put_meta(crate::endorsement_commit::SNAPSHOT_KEY, &expected)
                .unwrap();
            if !legacy {
                batch.migrate_verification_records().unwrap();
            }
            batch.put_meta(&journal_key, &journal_bytes).unwrap();
            batch.commit().unwrap();
            store.force_sync().unwrap();
            drop(store);
            kill_at_boundary(directory.path(), stage, true).await;
            let store = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            assert_eq!(
                batch.uses_normalized_verification_records().unwrap(),
                stage == "committed"
            );
            assert_eq!(
                crate::verification_storage::load(&batch)
                    .unwrap()
                    .snapshot()
                    .encode()
                    .unwrap(),
                expected
            );
            assert_eq!(
                batch.get_meta(&journal_key).unwrap(),
                Some(journal_bytes.clone())
            );
            batch.normalize_verification_records().unwrap();
            assert_eq!(batch.get_meta(&journal_key).unwrap(), Some(journal_bytes));
            assert_eq!(
                batch
                    .read_verification_records()
                    .unwrap()
                    .snapshot()
                    .encode()
                    .unwrap(),
                expected
            );
            batch.commit().unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_publication_recovers_after_abrupt_exit_at_each_boundary() {
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let expected =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    let source = Arc::new(source);
    for (normalized, stage) in [false, true].into_iter().flat_map(|normalized| {
        ["prepared", "committed", "economics", "complete"].map(move |stage| (normalized, stage))
    }) {
        let (directory, local) = cemented::sparse(&expected, &source);
        copy_installed_records(&local, &source, &fixture, &bytes);
        if !normalized { legacy_verification_fixture(&local); }
        assert_eq!(local.tx_begin_write().unwrap().uses_normalized_verification_records().unwrap(), normalized);
        local
            .meta_store()
            .put_meta("application-preference", b"keep")
            .unwrap();
        local.force_sync().unwrap();
        drop(local);
        kill_at_boundary(directory.path(), stage, false).await;

        let local = Arc::new(LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .uses_stable_trst_records()
                .unwrap(),
            stage != "prepared",
            "format activation must share the commit boundary: {stage}"
        );
        assert_eq!(
            local.tx_begin_write().unwrap().uses_normalized_verification_records().unwrap(),
            normalized || stage != "prepared",
            "verification normalization must share the commit boundary: {stage}"
        );
        let recovered = recover(
            Arc::clone(&source),
            Arc::clone(&local),
            fixture.params.clone(),
        )
        .await;
        compact::assert_same(recovered.recovered().continuation.state(), expected.state());
        let retained: Vec<_> = recovered
            .applied()
            .tail()
            .retained()
            .iter()
            .map(|proof| {
                let proof = proof.unwrap();
                (
                    format!("checkpoint_history/v1/{}", proof.block().hash),
                    bincode::serialize(proof.block()).unwrap(),
                )
            })
            .collect();
        assert!(!retained.is_empty());
        let (_runtime_dir, mut restarted) = node(Arc::clone(&local), fixture.params.clone()).await;
        restarted.ledger_cache.reset((999, 99, 9));
        restarted
            .frontier
            .write()
            .await
            .update(address(99), BlockHash::new([99; 32]));
        restarted
            .publish_recovered(recovered)
            .await
            .unwrap();
        for (key, bytes) in retained {
            assert_eq!(local.meta_store().get_meta(&key).unwrap(), bytes);
        }
        assert!(local
            .tx_begin_write()
            .unwrap()
            .uses_stable_trst_records()
            .unwrap());
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
        let persisted = {
            let batch = local.tx_begin_write().unwrap();
            assert!(batch.uses_normalized_verification_records().unwrap());
            crate::verification_storage::load(&batch).unwrap().snapshot()
        };
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

#[tokio::test(flavor = "current_thread")]
async fn compact_publication_worker_keeps_locks_when_caller_is_cancelled() {
    type Gate = (
        tokio::sync::oneshot::Sender<()>,
        std::sync::mpsc::Receiver<()>,
    );
    static GATE: std::sync::Mutex<Option<Gate>> = std::sync::Mutex::new(None);
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let expected =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    let (_directory, local) = cemented::sparse(&expected, &source);
    copy_installed_records(&local, &source, &fixture, &bytes);
    legacy_verification_fixture(&local);
    let local = Arc::new(local);
    let recovered = recover(Arc::new(source), local.clone(), fixture.params.clone()).await;
    let (_runtime_dir, mut node) = node(local.clone(), fixture.params).await;
    let governance = node.governance.clone();
    let elections = node.active_elections.clone();
    let registry = node.connection_registry.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    *GATE.lock().unwrap() = Some((started, wait));
    node.checkpoint_publication_probe = Some(|stage| {
        if stage == "prepared" {
            let (started, wait) = GATE.lock().unwrap().take().unwrap();
            started.send(()).unwrap();
            // This finite wait also makes the old async-thread implementation
            // fail without leaving a hung test process.
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    });
    let publication = tokio::spawn(async move {
        node.publish_recovered(recovered)
            .await
            .unwrap();
    });
    tokio::time::timeout(Duration::from_secs(30), ready)
        .await
        .unwrap()
        .unwrap();
    assert!(governance.try_lock().is_err());
    publication.abort();
    assert!(publication.await.unwrap_err().is_cancelled());
    assert!(
        governance.try_lock().is_err(),
        "worker must own publication locks"
    );
    assert!(registry.read().await.is_recovering());
    release.send(()).unwrap();
    let _guard = tokio::time::timeout(Duration::from_secs(5), governance.lock())
        .await
        .unwrap();
    assert_eq!(
        elections.read().await.authority_epoch(),
        Some(expected.anchor().hash())
    );
    assert!(
        registry.read().await.is_recovering(),
        "cancelled startup cannot open signing"
    );
    assert!(local
        .tx_begin_write()
        .unwrap()
        .uses_stable_trst_records()
        .unwrap());
    assert!(local.tx_begin_write().unwrap().uses_normalized_verification_records().unwrap());
    assert!(!crate::verification_storage::migrate(&local).unwrap(),
        "startup must not repeat the committed verification conversion");
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_during_publication_commits_then_closes_startup() {
    static STOP: std::sync::Mutex<Option<Arc<crate::shutdown::ShutdownController>>> =
        std::sync::Mutex::new(None);
    static COMPLETED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let expected =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    let (_directory, local) = cemented::sparse(&expected, &source);
    copy_installed_records(&local, &source, &fixture, &bytes);
    legacy_verification_fixture(&local);
    let local = Arc::new(local);
    let recovered = recover(Arc::new(source), local.clone(), fixture.params.clone()).await;
    let (_runtime_dir, mut node) = node(local.clone(), fixture.params).await;
    assert!(!node.stop_requested_startup().await.unwrap());
    *STOP.lock().unwrap() = Some(node.shutdown.clone());
    COMPLETED.store(false, Ordering::SeqCst);
    node.checkpoint_publication_probe = Some(|stage| match stage {
        "prepared" => STOP.lock().unwrap().take().unwrap().shutdown(),
        "complete" => COMPLETED.store(true, Ordering::SeqCst),
        _ => (),
    });
    node.publish_recovered(recovered)
        .await
        .unwrap();
    assert!(COMPLETED.load(Ordering::SeqCst));
    assert_eq!(
        node.active_elections.read().await.authority_epoch(),
        Some(expected.anchor().hash())
    );
    assert!(local
        .tx_begin_write()
        .unwrap()
        .uses_stable_trst_records()
        .unwrap());
    assert!(local.tx_begin_write().unwrap().uses_normalized_verification_records().unwrap());
    assert!(!crate::verification_storage::migrate(&local).unwrap(),
        "startup must not repeat the committed verification conversion");
    assert!(node.connection_registry.read().await.is_recovering());
    assert!(node.stop_requested_startup().await.unwrap());
    assert!(!node.connection_registry.read().await.is_recovering());
    // Cleanup consumes startup ownership; a stopped publication cannot later
    // reopen the node and start its signing/admission workers.
    assert!(node
        .start_with_services(burst_daemon::start_public_services)
        .await
        .unwrap_err()
        .to_string()
        .contains("already been started"));
}

#[tokio::test(flavor = "current_thread")]
async fn private_recovery_shutdown_drains_work_and_never_returns_publishable_state() {
    use crate::shutdown::ShutdownController;
    type Gate = (
        tokio::sync::oneshot::Sender<()>,
        std::sync::mpsc::Receiver<()>,
    );
    static GATE: std::sync::Mutex<Option<Gate>> = std::sync::Mutex::new(None);
    static STOP: std::sync::Mutex<Option<(&'static str, Arc<ShutdownController>)>> =
        std::sync::Mutex::new(None);
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let expected =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    let (_directory, local) = cemented::sparse(&expected, &source);
    copy_installed_records(&local, &source, &fixture, &bytes);
    let local = Arc::new(local);
    let source = Arc::new(source);
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let blocks = local.block_store().block_count().unwrap();
    for stage in [
        "private-recovery:start",
        "private-recovery:epoch",
        "private-recovery:account-inventory-page",
        "private-recovery:inventory-cache-page",
        "private-recovery:retained-inventory-page",
        "private-recovery:committed-history-compaction",
        "private-recovery:committed-history-page",
        "private-recovery:retained-position-page",
        "private-recovery:retained-planned",
        "private-recovery:inventory",
        "private-recovery:tails-finished",
        "private-recovery:before-economics",
        "private-recovery:replay-start",
        "private-recovery:replayed-economics",
        "private-recovery:economics",
        "private-recovery:publication-guards",
    ] {
        let stop = Arc::new(ShutdownController::new());
        *STOP.lock().unwrap() = Some((stage, stop.clone()));
        let service = WitnessService::start(source.clone(), local.clone(), 0).await;
        service.registry.write().await.recovery_probe = Some(|at| {
            let state = STOP.lock().unwrap();
            if let Some((stage, stop)) = state.as_ref() {
                if *stage == at {
                    stop.shutdown();
                }
            }
        });
        let limit = crate::confirmed_replay::scratch_file::ScratchLimit::acquire_reserved(
            local.env().path(), 64 << 20,
            crate::confirmed_replay::snapshot::history::DurableHistoryInputs::reservation(16 << 20).unwrap(),
        ).unwrap();
        let outcome = installed::recover_metadata_with_durable_inputs(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
            stop.clone(),
            Some((limit, 16 << 20)),
        )
        .await
        .unwrap();
        assert!(stop.is_requested(), "checkpoint not reached: {stage}");
        assert!(matches!(outcome, installed::StartupRecovery::Cancelled));
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
        assert_eq!(local.block_store().block_count().unwrap(), blocks);
        service.stop().await;
        *STOP.lock().unwrap() = None;
    }
    // The retirement worker must release its captured reader and original watch
    // at every ownership boundary, without returning a publishable catalogue.
    {
        let (_captured_dir, captured_store) = cemented::sparse(&expected, &source);
        copy_installed_records(&captured_store, &source, &fixture, &bytes);
        normalize_retention_fixture(&captured_store, &fixture.params);
        let captured_store = Arc::new(captured_store);
        let captured_before = captured_store.tx_begin_write().unwrap().checkpoint_metadata(b"").unwrap();
        let captured_blocks = captured_store.block_store().block_count().unwrap();
        static CAPTURE_POLLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        for stage in [
            "private-recovery:source-reader", "private-recovery:source-page",
            "snapshot:read", "snapshot:authority-step", "snapshot:certificate",
            "snapshot:restore-state", "snapshot:state-restored", "snapshot:selection-step",
            "snapshot:selections-finished",
            "snapshot:decode-bytes", "snapshot:decoded-state", "snapshot:restore-brn",
            "snapshot:restore-wallets", "snapshot:restore-graph", "snapshot:restore-continuation",
            "snapshot:receipt-context", "snapshot:roster-context", "snapshot:governance-context",
            "snapshot:canonical-state", "snapshot:canonical-bytes", "snapshot:canonical-finished",
            "retirement-capture:before-reader", "retirement-capture:reader",
            "private-recovery:retained-inventory-page", "private-recovery:retained-planned",
            "private-recovery:tails-finished", "private-recovery:replayed-economics",
            "private-recovery:publication-guards", "retirement-capture:before-roots",
            "retirement-capture:before-transfer", "private-recovery:applied-transition",
            "retirement-capture:root-page", "retirement-capture:projection-account",
            "retirement-capture:rollback-transition", "root-builder:index",
            "metadata:inventory-row", "metadata:economic-account", "metadata:journal-row",
            "metadata:verification-complete", "metadata:record-observation",
            "metadata:tail-account", "metadata:governance-tail-snapshot",
            "journal:epoch-namespace", "journal:record-authority", "journal:round-authority",
            "journal:promise-authority", "journal:capture-complete", "storage:lookup-account",
            "metadata:governance-record", "metadata:governance-inventory", "metadata:governance-canonical",
            "storage:verification-row", "storage:verification-finish",
            "metadata:verification-expected", "metadata:verification-record", "metadata:verification-canonical",
            "storage:brn-wallet", "storage:trst-header", "storage:trst-holding",
            "storage:trst-restored-holding", "storage:trst-inventory", "storage:trst-record",
            "storage:trst-validation", "storage:trst-restored",
            "trst:holding-link", "trst:portfolio-token", "trst:wallet-expiry",
            "trst:basis-token", "trst:restored-token", "trst:index-token",
            "trst:restored-origin", "trst:dependency-token",
            "trst:comparison-wallet-balance", "trst:comparison-balance", "trst:comparison-wallet",
            "trst:comparison-token", "trst:comparison-owner", "trst:comparison-origin",
            "trst:comparison-graph-parent", "trst:comparison-graph-revoked",
        ] {
            // The ordinary base predates the first selection. Receipt-row
            // cancellation needs a base that actually contains that receipt.
            let receipt_fixture = if stage == "snapshot:receipt-context" {
                let (directory, store) = cemented::sparse(&expected, &source);
                let receipt_bytes = archive(&source, &fixture, certificate.checkpoint.hash());
                let genesis = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
                    NetworkId::Dev, &fixture.params,
                ).unwrap();
                let snapshot = AuthenticatedSnapshot::read_in(
                    &genesis, &mut receipt_bytes.as_slice(), directory.path(),
                ).unwrap();
                assert!(!snapshot.recover_selections().unwrap().is_empty(),
                    "receipt cancellation requires a populated base");
                drop(snapshot);
                copy_installed_records(&store, &source, &fixture, &receipt_bytes);
                normalize_retention_fixture(&store, &fixture.params);
                Some((directory, Arc::new(store)))
            } else { None };
            let captured_store = receipt_fixture.as_ref()
                .map(|(_, store)| store.clone()).unwrap_or_else(|| captured_store.clone());
            let captured_before = captured_store.tx_begin_write().unwrap().checkpoint_metadata(b"").unwrap();
            let captured_blocks = captured_store.block_store().block_count().unwrap();
            CAPTURE_POLLS.store(0, std::sync::atomic::Ordering::SeqCst);
            let stop = Arc::new(ShutdownController::new());
            *STOP.lock().unwrap() = Some((stage, stop.clone()));
            let service = WitnessService::start(source.clone(), captured_store.clone(), 0).await;
            service.registry.write().await.recovery_probe = Some(|at| {
                if let Some((stage, stop)) = STOP.lock().unwrap().as_ref() {
                    if *stage == at {
                        let required = match at {
                            "retirement-capture:root-page" => 4,
                            "private-recovery:applied-transition" | "retirement-capture:rollback-transition" | "root-builder:index" | "metadata:inventory-row" => 2,
                            _ => 1,
                        };
                        if CAPTURE_POLLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 >= required {
                            stop.shutdown();
                        }
                    }
                }
            });
            let limit = crate::confirmed_replay::scratch_file::ScratchLimit::acquire_reserved(
                captured_store.env().path(), 64 << 20,
                crate::confirmed_replay::snapshot::history::DurableHistoryInputs::reservation(16 << 20).unwrap(),
            ).unwrap();
            let outcome = installed::recover_retirement_catalogue(
                captured_store.clone(), NetworkId::Dev, fixture.params.clone(), service.registry.clone(),
                stop.clone(), (limit.clone(), 16 << 20), 0, None, crate::config::BodyRetirementConfig::default().capture_limits(),
            ).await.unwrap();
            assert!(stop.is_requested(), "captured checkpoint not reached: {stage}");
            assert!(outcome.is_none(), "cancelled capture returned a catalogue: {stage}");
            assert_eq!(limit.used_temporary_bytes().unwrap(), 0, "captured scratch leaked: {stage}");
            assert_eq!(captured_store.tx_begin_write().unwrap().checkpoint_metadata(b"").unwrap(), captured_before);
            assert_eq!(captured_store.block_store().block_count().unwrap(), captured_blocks);
            captured_store.tx_begin_write().unwrap().watch_body_changes(1, 4096).unwrap();
            service.stop().await;
            *STOP.lock().unwrap() = None;
        }
        static BUDGET_READER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        for headroom in [false, true] {
            BUDGET_READER.store(false, std::sync::atomic::Ordering::SeqCst);
            let stop = Arc::new(ShutdownController::new());
            let service = WitnessService::start(source.clone(), captured_store.clone(), 0).await;
            service.registry.write().await.recovery_probe = Some(|at| {
                if matches!(at, "private-recovery:source-reader" | "retirement-capture:reader") {
                    BUDGET_READER.store(true, std::sync::atomic::Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(30));
                }
            });
            let limit = crate::confirmed_replay::scratch_file::ScratchLimit::acquire_reserved(
                captured_store.env().path(), 64 << 20,
                crate::confirmed_replay::snapshot::history::DurableHistoryInputs::reservation(16 << 20).unwrap(),
            ).unwrap();
            let mut budget = crate::config::BodyRetirementConfig::default().capture_limits();
            if headroom { budget.headroom_bytes = u64::MAX; }
            else { budget.duration = std::time::Duration::from_millis(1); }
            let outcome = installed::recover_retirement_catalogue(
                captured_store.clone(), NetworkId::Dev, fixture.params.clone(), service.registry.clone(),
                stop.clone(), (limit.clone(), 16 << 20), 0, None, budget,
            ).await.unwrap();
            assert!(outcome.is_none(), "over-budget capture returned a catalogue");
            assert!(!stop.is_requested(), "local budget stopped the node");
            assert_eq!(BUDGET_READER.load(std::sync::atomic::Ordering::SeqCst), !headroom);
            assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
            assert_eq!(captured_store.tx_begin_write().unwrap().checkpoint_metadata(b"").unwrap(), captured_before);
            assert_eq!(captured_store.block_store().block_count().unwrap(), captured_blocks);
            captured_store.tx_begin_write().unwrap().watch_body_changes(1, 4096).unwrap();
            service.stop().await;
        }
        // Exercise the actual capture lease, not a separately installed fixture
        // guard. Native rejection must cancel and drain this owned worker.
        static PRESSURE_STAGE: std::sync::Mutex<&'static str> = std::sync::Mutex::new("");
        for (index, phase) in ["private-recovery:source-reader", "retirement-capture:reader"].into_iter().enumerate() {
            *PRESSURE_STAGE.lock().unwrap() = phase;
            let pressure_before = captured_store.tx_begin_write().unwrap().checkpoint_metadata(b"").unwrap();
            let stop = Arc::new(ShutdownController::new());
            let service = WitnessService::start(source.clone(), captured_store.clone(), 0).await;
            let (started, ready) = tokio::sync::oneshot::channel();
            let (release, wait) = std::sync::mpsc::channel();
            *GATE.lock().unwrap() = Some((started, wait));
            service.registry.write().await.recovery_probe = Some(|at| {
                if at == *PRESSURE_STAGE.lock().unwrap() {
                    let (started, wait) = GATE.lock().unwrap().take().unwrap();
                    started.send(()).unwrap();
                    wait.recv_timeout(Duration::from_secs(5)).unwrap();
                }
            });
            let limit = crate::confirmed_replay::scratch_file::ScratchLimit::acquire_reserved(
                captured_store.env().path(), 64 << 20,
                crate::confirmed_replay::snapshot::history::DurableHistoryInputs::reservation(16 << 20).unwrap(),
            ).unwrap();
            let mut budget = crate::config::BodyRetirementConfig::default().capture_limits();
            budget.growth_bytes = 1 << 20;
            let capture = tokio::spawn(installed::recover_retirement_catalogue(
                captured_store.clone(), NetworkId::Dev, fixture.params.clone(), service.registry.clone(),
                stop.clone(), (limit.clone(), 16 << 20), 0, None, budget,
            ));
            tokio::time::timeout(Duration::from_secs(5), ready).await.unwrap().unwrap();
            let generation = captured_store.env().info().last_txn_id;
            let extent = captured_store.env().real_disk_size().unwrap();
            let map = captured_store.env().info().map_size;
            let mut writer = captured_store.tx_begin_write().unwrap();
            let outcome = writer.growth_outcome().expect("capture installed native limit");
            let retry = writer.reserve_growth_retry().unwrap().expect("guarded retry priority");
            assert!(matches!(writer.put_meta("test/captured-pressure", &vec![index as u8; 2 << 20]),
                Err(burst_store::StoreError::GrowthLimit(_))));
            drop(writer);
            assert!(outcome.was_rejected());
            let watch = outcome.watch();
            assert!(!watch.wait_released(Duration::ZERO));
            assert!(!capture.is_finished(), "capture detached its blocked worker");
            assert_eq!(captured_store.env().info().last_txn_id, generation);
            assert!(captured_store.env().real_disk_size().unwrap() <= extent + budget.growth_bytes);
            release.send(()).unwrap();
            assert!(tokio::time::timeout(Duration::from_secs(5), capture).await.unwrap().unwrap().unwrap().is_none());
            assert!(watch.wait_released(Duration::ZERO));
            assert!(!stop.is_requested(), "capture pressure stopped the node");
            assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
            assert_eq!(captured_store.tx_begin_write().unwrap().checkpoint_metadata(b"").unwrap(), pressure_before);
            assert_eq!(captured_store.block_store().block_count().unwrap(), captured_blocks);
            service.registry.write().await.recovery_probe = Some(|at| {
                assert_ne!(at, "private-recovery:start", "capture replayed metadata despite admitted retry priority");
                assert_ne!(at, "private-recovery:source-worker", "capture opened a source reader despite retry priority");
                assert_ne!(at, "retirement-capture:reader", "capture ignored admitted retry priority");
            });
            assert!(installed::recover_retirement_catalogue(
                captured_store.clone(), NetworkId::Dev, fixture.params.clone(), service.registry.clone(),
                stop, (limit.clone(), 16 << 20), 0, None, budget,
            ).await.unwrap().is_none());
            // The fresh admitted writer is no longer limited while its reservation
            // prevents another capture from intervening.
            let mut writer = captured_store.tx_begin_write().unwrap();
            assert!(writer.growth_outcome().is_none());
            writer.put_meta("test/captured-pressure", &vec![index as u8; 2 << 20]).unwrap();
            writer.commit().unwrap();
            drop(retry);
            assert_eq!(captured_store.env().info().map_size, map);
            service.stop().await;
        }
    }
    // A stop during an owned blocking phase waits for its result. Storage errors
    // that race the stop remain errors, rather than becoming graceful cancellation.
    for corrupt in [false, true] {
        let stop = Arc::new(ShutdownController::new());
        let service = WitnessService::start(source.clone(), local.clone(), 0).await;
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        *GATE.lock().unwrap() = Some((started, wait));
        service.registry.write().await.recovery_probe = Some(|at| {
            if at == "private-recovery:source-worker" {
                let (started, wait) = GATE.lock().unwrap().take().unwrap();
                started.send(()).unwrap();
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        });
        let recovery = tokio::spawn(installed::recover_metadata_until_shutdown(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
            stop.clone(),
        ));
        tokio::time::timeout(Duration::from_secs(5), ready)
            .await
            .unwrap()
            .unwrap();
        stop.shutdown();
        tokio::task::yield_now().await;
        assert!(!recovery.is_finished(), "must await active private worker");
        if corrupt {
            local
                .meta_store()
                .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &[1])
                .unwrap();
        }
        release.send(()).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), recovery)
            .await
            .unwrap()
            .unwrap();
        if corrupt {
            assert!(result
                .err()
                .unwrap()
                .to_string()
                .contains("epoch is malformed"));
            local
                .meta_store()
                .put_meta(
                    burst_store::meta::PAYMENT_EPOCH_KEY,
                    expected.anchor().hash().as_bytes(),
                )
                .unwrap();
        } else {
            assert!(matches!(
                result.unwrap(),
                installed::StartupRecovery::Cancelled
            ));
        }
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
        assert_eq!(local.block_store().block_count().unwrap(), blocks);
        service.stop().await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn applied_replay_releases_writer_and_rejects_concurrent_epoch_change() {
    type Gate = (tokio::sync::oneshot::Sender<()>, std::sync::mpsc::Receiver<()>);
    static GATE: std::sync::Mutex<Option<Gate>> = std::sync::Mutex::new(None);
    for change_epoch in [false, true] {
        let (fixture, _dir, source, manifest, certificate) = selection::setup();
        let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
        let expected = authenticate(&source, &fixture.params, certificate.checkpoint.hash())
            .into_continuation();
        let (_directory, local) = cemented::sparse(&expected, &source);
        copy_installed_records(&local, &source, &fixture, &bytes);
        let local = Arc::new(local);
        let source = Arc::new(source);
        let service = WitnessService::start(source, local.clone(), 0).await;
        let (entered, entered_rx) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        *GATE.lock().unwrap() = Some((entered, released));
        service.registry.write().await.recovery_probe = Some(|stage| {
            if stage == "private-recovery:replay-start" {
                let (entered, released) = GATE.lock().unwrap().take().unwrap();
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        });
        let recovery = tokio::spawn(installed::recover_metadata_until_shutdown(
            local.clone(), NetworkId::Dev, fixture.params.clone(), service.registry.clone(),
            Arc::new(crate::shutdown::ShutdownController::new()),
        ));
        tokio::time::timeout(Duration::from_secs(5), entered_rx).await.unwrap().unwrap();
        let writer_store = local.clone();
        let writer = tokio::task::spawn_blocking(move || {
            let mut batch = writer_store.tx_begin_write()?;
            if change_epoch {
                batch.put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &[0x91; 32])?;
                batch.commit()?;
            }
            Ok::<_, burst_store::StoreError>(())
        });
        // Release the recovery gate even on a failed timeout, so a lock-lifetime
        // regression reports an assertion instead of leaving blocked work behind.
        let writer_result = tokio::time::timeout(Duration::from_secs(2), writer).await;
        release.send(()).unwrap();
        let recovered = tokio::time::timeout(Duration::from_secs(5), recovery)
            .await.unwrap().unwrap();
        service.stop().await;
        writer_result.expect("pure replay held the ledger writer").unwrap().unwrap();
        if change_epoch {
            assert!(recovered.is_err(), "changed epoch produced publishable recovery state");
        } else {
            assert!(matches!(recovered.unwrap(), installed::StartupRecovery::Ready(Some(_))));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn metadata_recovery_diagnoses_valid_payment_between_reads() {
    type Gate = (tokio::sync::oneshot::Sender<()>, std::sync::mpsc::Receiver<()>);
    static GATE: std::sync::Mutex<Option<Gate>> = std::sync::Mutex::new(None);
    for mode in 0..3 {
        let captured = mode > 0;
        let (mut fixture, _dir, source, manifest, certificate) = selection::setup();
        let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
        let expected = authenticate(&source, &fixture.params, certificate.checkpoint.hash())
            .into_continuation();
        let (_directory, local) = cemented::sparse(&expected, &source);
        copy_installed_records(&local, &source, &fixture, &bytes);
        let epoch = expected.anchor().hash();
        fixture.push(0, BlockType::ChangeRepresentative, expected.anchor().cutoff() + 1, |block| {
            block.payment_epoch = epoch;
            block.representative = address(1);
        });
        let block = fixture.blocks.last().unwrap().clone();
        let local = Arc::new(local);
        let service = WitnessService::start(Arc::new(source), local.clone(), 0).await;
        if mode == 1 {
            let mut writer = local.tx_begin_write().unwrap();
            let lease = writer.try_limit_file_extent(local.env().info().map_size).unwrap().unwrap();
            drop(writer);
            let denied = installed::recover_checkpoint_authority(local.clone(), NetworkId::Dev,
                fixture.params.clone(), service.registry.clone(), None, &mut None).await;
            assert!(matches!(denied, Err(burst_store::StoreError::Backend(_))),
                "budget refusal must not look like an absent authority");
            drop(lease);
        }
        let original_limit = local.meta_store().get_meta(burst_store::meta::PAYMENT_VOTE_LIMIT_KEY).unwrap();
        let (entered, entered_rx) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        *GATE.lock().unwrap() = Some((entered, released));
        service.registry.write().await.recovery_probe = Some(|stage| {
            if stage == "private-recovery:replay-start" {
                let (entered, released) = GATE.lock().unwrap().take().unwrap();
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            }
        });
        let recovery_store = local.clone();
        let recovery_params = fixture.params.clone();
        let registry = service.registry.clone();
        let recovery = tokio::spawn(async move {
            if captured {
                let cache = installed::recover_checkpoint_authority(
                    recovery_store, NetworkId::Dev, recovery_params, registry, None, &mut None,
                ).await?.expect("installed authority");
                assert_eq!(cache.recovered.continuation.anchor().hash(), epoch);
            } else {
                installed::recover_metadata_until_shutdown(
                    recovery_store, NetworkId::Dev, recovery_params, registry,
                    Arc::new(crate::shutdown::ShutdownController::new()),
                ).await?;
            }
            Ok::<_, burst_store::StoreError>(())
        });
        tokio::time::timeout(Duration::from_secs(5), entered_rx).await.unwrap().unwrap();
        let writer_store = local.clone();
        let params = fixture.params.clone();
        let writer = tokio::task::spawn_blocking(move || {
            let mut batch = writer_store.tx_begin_write().unwrap();
            // Use the ordinary authenticated transition, including economics and
            // undo state; editing only the account row would manufacture corruption.
            crate::payment_transition::apply(&mut batch, &block, &params, block.timestamp).unwrap();
            batch.commit().unwrap();
            if mode == 2 {
                // Consensus metadata is deliberately outside payment undo.
                writer_store.meta_store().put_meta(burst_store::meta::PAYMENT_VOTE_LIMIT_KEY, &[0x91; 32]).unwrap();
            }
        });
        let writer_result = tokio::time::timeout(Duration::from_secs(2), writer).await;
        release.send(()).unwrap();
        let raced = tokio::time::timeout(Duration::from_secs(5), recovery).await.unwrap().unwrap();
        service.registry.write().await.recovery_probe = None;
        writer_result.expect("replay held the ledger writer").unwrap();
        if mode == 1 {
            raced.expect("consistent authority recovery must tolerate a valid concurrent payment");
        } else if mode == 2 {
            let error = raced.expect_err("live authority tampering must fail handoff");
            assert!(error.to_string().contains("installed metadata differs"), "{error}");
            local.meta_store().put_meta(burst_store::meta::PAYMENT_VOTE_LIMIT_KEY, &original_limit).unwrap();
        } else {
            let error = raced.expect_err("strict startup recovery must reject the advanced applied tail");
            assert!(error.to_string().contains("retained history changed during recovery"), "{error}");
        }
        // A fresh full recovery must accept the exact same durable state. This
        // distinguishes a concurrent-read rejection from an invalid test update.
        let recovered = installed::recover_metadata_from_peers(
            local.clone(), NetworkId::Dev, fixture.params.clone(), service.registry.clone(),
        ).await.unwrap().unwrap();
        let authority = recovered.into_checkpoint_recovery(&local, None).unwrap();
        assert_eq!(authority.recovered.continuation.anchor().hash(), epoch);
        service.stop().await;
    }
}

fn durable_inputs_child(directory: &Path) -> ! {
    use crate::confirmed_replay::snapshot::history::DurableHistoryInputs;
    let fixture = Fixture::selection();
    let (_dir, source, snapshot) = super::super::snapshot(&fixture);
    let cursor = snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let height = cursor.next().unwrap().height;
    assert!(height > 2);
    let context = DurableHistoryInputs::input_context(&cursor, height - 2).unwrap();
    let mut inputs = DurableHistoryInputs::open(directory, cursor, height - 2, 4 << 20, 1 << 20).unwrap();
    let stage = std::env::var("BURST_COMPACT_CRASH_STAGE").unwrap();
    assert!(["opened", "appended", "sealed", "reset-start", "reset-page", "reset-done"].contains(&stage.as_str()));
    if stage != "opened" {
        inputs.append(&super::super::read_chunk(&source, inputs.cursor().next().unwrap().hash, 1).unwrap()).unwrap();
    }
    if stage != "opened" && stage != "appended" {
        inputs.append(&super::super::read_chunk(&source, inputs.cursor().next().unwrap().hash, 1).unwrap()).unwrap();
        inputs.seal().unwrap();
    }
    if stage.starts_with("reset-") {
        use burst_store_lmdb::recovery_inputs::{Limits, RecoveryInputs};
        let raw = RecoveryInputs::open(&directory.join(crate::confirmed_replay::scratch_file::PERSISTENT_INPUTS_DIRECTORY),
            context, Limits { chunks: 131_072, bytes: 1 << 20, map_bytes: 1 << 20 }).unwrap();
        raw.begin_reset(context).unwrap();
        if stage != "reset-start" {
            assert!(!raw.reset_page(1, (1 << 20) + 64).unwrap().complete);
        }
        if stage == "reset-done" {
            let mut complete = false;
            for _ in 0..32 {
                if raw.reset_page(1, (1 << 20) + 64).unwrap().complete { complete = true; break; }
            }
            assert!(complete);
        }
    }
    println!("{READY}");
    std::io::stdout().flush().unwrap();
    // Keep every handle live: SIGKILL, not a destructor, closes the environment.
    loop { std::thread::park(); }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn durable_history_inputs_reauthenticate_after_process_kill() {
    use crate::confirmed_replay::snapshot::history::DurableHistoryInputs;
    use crate::confirmed_replay::scratch_file::{ScratchFile, ScratchLimit};
    let fixture = Fixture::selection();
    let (_dir, source, snapshot) = super::super::snapshot(&fixture);
    let cursor = snapshot.history_cursor(&address(0)).unwrap().unwrap();
    let head = cursor.next().unwrap();
    let first = super::super::read_chunk(&source, head.hash, 2).unwrap();
    assert_eq!(first.len(), 2);
    for (stage, committed) in [("opened", 0), ("appended", 1), ("sealed", 2), ("reset-start", 0), ("reset-page", 0), ("reset-done", 0)] {
        let directory = tempfile::tempdir().unwrap();
        kill_at_kind(directory.path(), stage, "recovery-inputs").await;
        if stage.starts_with("reset-") {
            use burst_store_lmdb::recovery_inputs::{Limits, RecoveryInputs};
            let context = DurableHistoryInputs::input_context(&cursor, head.height - 2).unwrap();
            let raw = RecoveryInputs::open(&directory.path().join(crate::confirmed_replay::scratch_file::PERSISTENT_INPUTS_DIRECTORY),
                context, Limits { chunks: 131_072, bytes: 1 << 20, map_bytes: 1 << 20 }).unwrap();
            if stage == "reset-done" {
                assert_eq!(raw.progress().unwrap().positions, 0);
            } else {
                assert!(raw.progress().is_err());
                let mut complete = false;
                for _ in 0..32 {
                    if raw.reset_page(1, (1 << 20) + 64).unwrap().complete { complete = true; break; }
                }
                assert!(complete);
            }
        }
        // There is no surviving child registry or budget owner. Retained files
        // must still be charged by this new process's ordinary scratch policy.
        assert!(ScratchLimit::acquire(directory.path(), 1).is_err());
        let mut inputs = DurableHistoryInputs::open(directory.path(), cursor.clone(), head.height - 2, 4 << 20, 1 << 20).unwrap();
        assert_eq!(inputs.cursor().next(), Some(head.clone()));
        assert!(inputs.seal().is_err(), "saved completion is not proof authority");
        let proofs = inputs.replay_page().unwrap();
        assert_eq!(proofs.len(), committed);
        for (proof, bytes) in proofs.iter().zip(&first) {
            assert_eq!(bincode::serialize(proof.block()).unwrap(), *bytes);
            assert_eq!(proof.context(), cursor.context());
        }
        assert!(inputs.replay_page().unwrap().is_empty());
        let mut scratch = ScratchFile::new_in(directory.path()).unwrap();
        scratch.write_all(&vec![0; (4 << 20) - (1 << 20) - (64 << 10)]).unwrap();
        assert!(scratch.write_all(&[0]).is_err());
        drop(scratch);
        if committed < 2 { inputs.append(&first[committed..]).unwrap(); }
        inputs.seal().unwrap();
        assert_eq!(inputs.cursor().next().unwrap().height, head.height - 2);
        drop(inputs);
        let mut inputs = DurableHistoryInputs::open(directory.path(), cursor.clone(), head.height - 2, 4 << 20, 1 << 20).unwrap();
        assert_eq!(inputs.replay_page().unwrap().len(), 2);
        inputs.seal().unwrap();
    }
}

async fn durable_inventory_child(directory: &Path) -> ! {
    let (fixture, _dir, source, _, _) = selection::setup();
    let local = Arc::new(LmdbStore::open(directory, 32, 32 << 20).unwrap());
    let service = WitnessService::start(Arc::new(source), local.clone(), 0).await;
    service.registry.write().await.recovery_probe = Some(|stage| {
        if stage == std::env::var("BURST_COMPACT_CRASH_STAGE").unwrap() {
            println!("{READY}");
            std::io::stdout().flush().unwrap();
            loop { std::thread::park(); }
        }
    });
    let limit = crate::confirmed_replay::scratch_file::ScratchLimit::acquire_reserved(
        directory, 64 << 20,
        crate::confirmed_replay::snapshot::history::DurableHistoryInputs::reservation(16 << 20).unwrap(),
    ).unwrap();
    installed::recover_metadata_with_durable_inputs(local, NetworkId::Dev, fixture.params,
        service.registry.clone(), Arc::new(crate::shutdown::ShutdownController::new()),
        Some((limit, 16 << 20))).await.unwrap();
    panic!("inventory crash boundary was not reached");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn durable_inventory_reauthenticates_after_process_kill() {
    use crate::confirmed_replay::{scratch_file::ScratchLimit, snapshot::history::DurableHistoryInputs};
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let expected = authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    let source = Arc::new(source);
    for stage in ["private-recovery:inventory-cache-page", "private-recovery:retained-inventory-page",
        "private-recovery:retained-planned"] {
        let (directory, local) = cemented::sparse(&expected, &source);
        copy_installed_records(&local, &source, &fixture, &bytes);
        let before = local.tx_begin_write().unwrap().checkpoint_metadata(b"").unwrap();
        let count = local.block_store().block_count().unwrap();
        drop(local);
        kill_at_kind(directory.path(), stage, "inventory-inputs").await;
        assert!(ScratchLimit::acquire(directory.path(), 1).is_err());
        let local = Arc::new(LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
        assert_eq!(local.tx_begin_write().unwrap().checkpoint_metadata(b"").unwrap(), before);
        assert_eq!(local.block_store().block_count().unwrap(), count);
        let service = WitnessService::start(source.clone(), local.clone(), 0).await;
        let limit = ScratchLimit::acquire_reserved(directory.path(), 64 << 20,
            DurableHistoryInputs::reservation(16 << 20).unwrap()).unwrap();
        let result = installed::recover_metadata_with_durable_inputs(local.clone(), NetworkId::Dev,
            fixture.params.clone(), service.registry.clone(), Arc::new(crate::shutdown::ShutdownController::new()),
            Some((limit, 16 << 20))).await.unwrap();
        let installed::StartupRecovery::Ready(Some(recovered)) = result else { panic!("recovery did not finish at {stage}"); };
        recovered.verify_unchanged(&local.tx_begin_write().unwrap()).unwrap();
        assert_eq!(local.tx_begin_write().unwrap().checkpoint_metadata(b"").unwrap(), before);
        assert_eq!(local.block_store().block_count().unwrap(), count);
        service.stop().await;
    }
}
