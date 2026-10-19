use super::*;
use crate::{BurstNode, NodeConfig};
use burst_store::{account::AccountStore, block::BlockStore};
use burst_wallet_core::{transaction_builder::*, NodeClient};
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const PUBLICATION_CHILD: &str = "confirmed_replay::tests::certified_votes::installation::process_crash::publication::publication_child";

mod keys;
mod live;

fn port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn config(path: &Path, params: &ProtocolParams) -> NodeConfig {
    NodeConfig {
        network: NetworkId::Dev,
        data_dir: path.to_path_buf(),
        port: port(),
        rpc_port: port(),
        enable_upnp: false,
        enable_representative: false,
        enable_faucet: false,
        nts_sources: vec![],
        bootstrap_peers: vec![],
        params: params.clone(),
        ..NodeConfig::default()
    }
}

async fn runtime(
    node: &BurstNode,
    params: &ProtocolParams,
) -> crate::checkpoint_installation::Runtime {
    let obsolete = BlockHash::new([99; 32]);
    node.ledger_cache.reset((999, 99, 9));
    node.frontier.write().await.update(address(99), obsolete);
    node.priority_scheduler
        .lock()
        .await
        .push(obsolete, address(99), 1);
    node.backlog
        .lock()
        .await
        .insert(obsolete, address(99), 1, 1);
    node.local_broadcaster
        .lock()
        .await
        .track(obsolete, vec![99], 1);
    let parent =
        crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, params)
            .unwrap();
    let mut weights = node.rep_weights.write().await;
    weights.rebuild_from_accounts(
        parent
            .roster()
            .humans()
            .iter()
            .map(|human| (true, human.representative.clone())),
    );
    node.active_elections
        .write()
        .await
        .install_authority(parent.hash(), weights.all_weights(), parent.quorum_bps())
        .unwrap();
    drop(weights);
    crate::checkpoint_installation::Runtime {
        registry: node.connection_registry.clone(),
        ledger: crate::fork_resolution::Runtime {
            store: node.store.clone(),
            brn: node.brn_engine.clone(),
            trst: node.trst_engine.clone(),
            processor: node.block_processor.clone(),
            frontier: node.frontier.clone(),
            weights: node.rep_weights.clone(),
            scheduler: node.priority_scheduler.clone(),
            elections: node.active_elections.clone(),
            backlog: node.backlog.clone(),
            broadcaster: node.local_broadcaster.clone(),
            counts: node.ledger_cache.clone(),
        },
        verification: node.verification_orchestrator.clone(),
        governance: node.governance.clone(),
        delegation: node.delegation_engine.clone(),
        constitution: node.consti_engine.clone(),
        network: NetworkId::Dev,
        initial_params: params.clone(),
        shutdown: node.shutdown.clone(),
        last_complete: None,
        recovery: None,
        publication_probe: None,
    }
}

#[tokio::test]
#[ignore = "subprocess helper for checkpoint cache-publication crashes"]
async fn publication_child() {
    let directory = std::env::var("BURST_INSTALLATION_CRASH_DIRECTORY").unwrap();
    let fixture = publication_fixture(std::env::var_os("BURST_GOVERNANCE_CRASH").is_some());
    let node = BurstNode::new(config(Path::new(&directory), &fixture.params))
        .await
        .unwrap();
    let mut runtime = runtime(&node, &fixture.params).await;
    runtime.publication_probe = Some(|stage| {
        if stage == std::env::var("BURST_PUBLICATION_CRASH_STAGE").unwrap() {
            signal_ready();
            park();
        }
    });
    runtime.install_next().await.unwrap();
    panic!("publication crash boundary was not reached");
}

async fn check_caches(node: &BurstNode) {
    let store = &node.store;
    let authority = crate::payment_authority::PaymentAuthority::read(store)
        .unwrap()
        .unwrap();
    assert_eq!(
        node.active_elections.read().await.authority_epoch(),
        Some(authority.epoch)
    );
    assert_eq!(
        node.rep_weights.read().await.all_weights(),
        authority.weights().all_weights()
    );
    let brn = BrnEngine::load_from_store(&store.brn_store()).unwrap();
    let live = node.brn_engine.lock().await;
    assert_eq!(live.wallets, brn.wallets);
    assert_eq!(live.rate_history, brn.rate_history);
    assert_eq!(live.next_stake_id(), brn.next_stake_id());
    drop(live);
    let trst = node.trst_engine.lock().await;
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(
        trst.save_wallets(),
        crate::trst_storage::load(&batch, trst.expiry_secs)
            .unwrap()
            .save_wallets()
    );
    assert_eq!(
        trst.merger_graph.to_bytes(),
        crate::graph_storage::load(&batch).unwrap().to_bytes()
    );
    drop(batch);
    drop(trst);
    let verification = node
        .verification_orchestrator
        .lock()
        .await
        .snapshot()
        .encode()
        .unwrap();
    let persisted = store
        .meta_store()
        .get_meta(crate::endorsement_commit::SNAPSHOT_KEY)
        .unwrap();
    assert_eq!(
        logical_value(
            "meta",
            crate::endorsement_commit::SNAPSHOT_KEY.as_bytes(),
            &verification
        ),
        logical_value(
            "meta",
            crate::endorsement_commit::SNAPSHOT_KEY.as_bytes(),
            &persisted
        )
    );
    let governance: burst_governance::GovernanceEngine = bincode::deserialize(
        &store
            .brn_store()
            .get_meta(b"governance_engine")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(*node.governance.lock().await, governance);
    let constitution = bincode::serialize(&*node.consti_engine.lock().await).unwrap();
    let persisted = store.meta_store().get_meta("consti_engine").unwrap();
    assert_eq!(
        logical_value("meta", b"consti_engine", &constitution),
        logical_value("meta", b"consti_engine", &persisted)
    );
    assert_eq!(
        node.delegation_engine.lock().await.save_state(),
        store
            .meta_store()
            .get_meta(burst_governance::DelegationEngine::meta_key())
            .unwrap()
    );
    let counts = store.tx_begin_write().unwrap().ledger_counts().unwrap();
    assert_eq!(
        (
            node.ledger_cache.block_count(),
            node.ledger_cache.account_count(),
            node.ledger_cache.pending_count()
        ),
        counts
    );
    let frontier = node.frontier.read().await;
    let accounts: Vec<_> = store
        .account_store()
        .iter_accounts()
        .unwrap()
        .into_iter()
        .filter(|account| account.block_count > 0)
        .collect();
    assert_eq!(frontier.account_count(), accounts.len());
    for account in accounts {
        assert_eq!(frontier.get_head(&account.address), Some(&account.head));
    }
}

async fn restart_and_admit(directory: &Path, fixture: &Fixture, epoch: BlockHash) {
    let config = config(directory, &fixture.params);
    let client = NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap();
    let mut node = BurstNode::new(config).await.unwrap();
    let at = crate::payment_authority::PaymentAuthority::read(&node.store)
        .unwrap()
        .unwrap()
        .cutoff
        + 1;
    let clock = Arc::new(AtomicU64::new(at));
    node.block_processor.lock().await.test_time = Some(clock.clone());
    node.verification_fixture_clock = Some(Arc::new(move || {
        Some(crate::checkpoint_replay::SigningClock {
            now: clock.load(Ordering::SeqCst),
            error_bound: 0,
        })
    }));
    let shutdown = node.shutdown.clone();
    let mut task = tokio::spawn(async move {
        node.start().await.unwrap();
        node
    });
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if client.rpc_call("params_hash", json!({})).await.is_ok() {
                break;
            }
            assert!(
                !task.is_finished(),
                "restarted node exited before RPC readiness"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let keys = burst_crypto::keypair_from_seed(&[0; 32]);
        let state = client.signing_state(&address(0)).await.unwrap();
        assert_eq!(state.payment_epoch, epoch);
        let tx = burst_transactions::Transaction::Burn(
            build_burn_tx(&address(0), &address(1), 1, Timestamp::new(at)).unwrap(),
        );
        let valid =
            build_state_block(&state, &tx, TxHash::ZERO, fixture.params.params_hash()).unwrap();
        let mut stale = valid.clone();
        stale.payment_epoch = fixture.blocks[0].hash;
        stale.hash = stale.compute_hash();
        let stale = sign_state_block(stale, &keys.private);
        assert!(
            !client
                .process(&serde_json::to_string(&stale).unwrap())
                .await
                .unwrap()
                .accepted
        );
        let valid = sign_state_block(valid, &keys.private);
        let response = client
            .process(&serde_json::to_string(&valid).unwrap())
            .await
            .unwrap();
        assert!(
            response.accepted,
            "post-recovery payment rejected: {response:?}"
        );
        keys::check_recovery(&client, fixture, at).await;
        valid.hash
    })
    .await;
    shutdown.shutdown();
    let mut node = tokio::time::timeout(Duration::from_secs(20), &mut task)
        .await
        .unwrap()
        .unwrap();
    let payment = result.expect("restarted node failed to admit payment");
    assert!(node.store.block_store().exists(&payment).unwrap());
    check_caches(&node).await;
    node.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publication_crashes_restore_caches_and_admit_only_the_installed_epoch() {
    publication_crashes(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn governance_publication_crashes_restore_counted_votes_and_epoch() {
    publication_crashes(true).await;
}

fn publication_fixture(governance: bool) -> Fixture {
    if governance {
        keys::fixture()
    } else {
        Fixture::selection()
    }
}

async fn check_governance_counting(node: &BurstNode) {
    let engine = node.governance.lock().await;
    let proposals: Vec<_> = engine.all_proposals().collect();
    assert_eq!(proposals.len(), 2);
    for proposal in proposals {
        assert_eq!(proposal.phase, burst_governance::GovernancePhase::Cooldown);
        assert_eq!(proposal.exploration_votes_yea, 2);
        assert_eq!(proposal.exploration_votes_nay, 1);
    }
}

fn accept_publication(
    fixture: &Fixture,
    store: &LmdbStore,
    heads: Vec<HistoryHead>,
    governance: bool,
) {
    if governance {
        let candidate = crate::checkpoint_governance::PreparedGovernance::prepare_next(
            store,
            NetworkId::Dev,
            &fixture.params,
            141,
        )
        .unwrap()
        .unwrap();
        crate::checkpoint_store::chain::accept_manifest(
            store,
            NetworkId::Dev,
            &fixture.params,
            certify(&candidate.manifest().checkpoint, &[0]),
            &crate::checkpoint_agreement::ReplayManifest::Governance(candidate.manifest().clone()),
        )
        .unwrap();
    } else {
        accepted(fixture, store, heads);
    }
}

async fn publication_crashes(governance: bool) {
    let fixture = publication_fixture(governance);
    let (baseline, store, heads) = fixture.store();
    accept_publication(&fixture, &store, heads, governance);
    store.env().force_sync().unwrap();
    drop(store);
    let expected_dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        baseline.path().join("data.mdb"),
        expected_dir.path().join("data.mdb"),
    )
    .unwrap();
    let node = BurstNode::new(config(expected_dir.path(), &fixture.params))
        .await
        .unwrap();
    let mut normal = runtime(&node, &fixture.params).await;
    normal.install_next().await.unwrap().unwrap();
    check_caches(&node).await;
    if governance {
        check_governance_counting(&node).await;
    }
    assert!(node.priority_scheduler.lock().await.is_empty());
    assert!(node.backlog.lock().await.is_empty());
    assert!(node.local_broadcaster.lock().await.is_empty());
    let epoch = node.store.meta_store().payment_epoch().unwrap();
    let expected = contents(&node.store);
    drop(normal);
    drop(node);
    for stage in ["committed", "economics", "authority", "complete"] {
        let directory = tempfile::tempdir().unwrap();
        std::fs::copy(
            baseline.path().join("data.mdb"),
            directory.path().join("data.mdb"),
        )
        .unwrap();
        let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
        if governance {
            command.env("BURST_GOVERNANCE_CRASH", "1");
        } else {
            command.env_remove("BURST_GOVERNANCE_CRASH");
        }
        let mut child = command
            .args(["--exact", PUBLICATION_CHILD, "--ignored", "--nocapture"])
            .env("BURST_INSTALLATION_CRASH_DIRECTORY", directory.path())
            .env("BURST_PUBLICATION_CRASH_STAGE", stage)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
        tokio::time::timeout(Duration::from_secs(20), async {
            while let Some(line) = output.next_line().await.unwrap() {
                if line.contains(READY) {
                    return;
                }
            }
            panic!("child exited before {stage}");
        })
        .await
        .expect("publication child did not reach its boundary");
        child.start_kill().unwrap();
        let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.signal(), Some(9));
        let store = open(directory.path());
        assert_contents(&contents(&store), &expected);
        drop(store);
        restart_and_admit(directory.path(), &fixture, epoch).await;
        eprintln!("{stage}: durable projection, restarted caches and epoch-bound admission pass");
    }
}
