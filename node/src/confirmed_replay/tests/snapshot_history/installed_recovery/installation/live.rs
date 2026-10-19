use super::*;
use burst_store::block::BlockStore;
use burst_wallet_core::{transaction_builder::*, NodeClient};
use std::{sync::atomic::AtomicU64, time::Duration};

pub(in crate::confirmed_replay::tests) mod bootstrap;
#[cfg(unix)]
mod crash;
mod protocol_epoch;
mod retirement;

struct Prepared {
    _source_dir: tempfile::TempDir,
    _target_dir: tempfile::TempDir,
    source_config: crate::NodeConfig,
    target_config: crate::NodeConfig,
    params: ProtocolParams,
    bytes: Vec<u8>,
    old_epoch: BlockHash,
    epoch: BlockHash,
    now: u64,
}

struct Running {
    data_dir: std::path::PathBuf,
    stop: Arc<crate::shutdown::ShutdownController>,
    task: Option<tokio::task::JoinHandle<(crate::BurstNode, Result<(), crate::NodeError>)>>,
    client: NodeClient,
    store: Arc<LmdbStore>,
    installer: crate::snapshot_installation::SnapshotInstaller,
    registry: Arc<tokio::sync::RwLock<crate::ConnectionRegistry>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.shutdown();
    }
}

impl Running {
    async fn start(config: crate::NodeConfig, now: u64) -> Self {
        Self::start_prefilled(config, now, None).await
    }

    async fn start_prefilled(
        config: crate::NodeConfig,
        now: u64,
        prefill: Option<&StateBlock>,
    ) -> Self {
        let data_dir = config.data_dir.clone();
        let client = NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap();
        let mut node = crate::BurstNode::new(config).await.unwrap();
        node.block_processor.lock().await.test_time = Some(Arc::new(AtomicU64::new(now)));
        node.verification_fixture_clock = Some(Arc::new(|| None));
        if let Some(block) = prefill {
            let queue = node.block_queue();
            for _ in 0..4096 {
                assert!(queue.push(block.clone()).await);
            }
            assert!(
                !queue.push(block.clone()).await,
                "real ingress capacity must reject overflow"
            );
            assert_eq!(queue.len().await, 4096);
        }
        let stop = node.shutdown.clone();
        let store = node.store.clone();
        let installer = node.snapshot_installer();
        let registry = node.connection_registry.clone();
        let task = tokio::spawn(async move {
            let result = node.start().await;
            if let Err(error) = &result {
                eprintln!("compact live node exited: {error}");
            }
            (node, result)
        });
        Self {
            data_dir,
            stop,
            store,
            installer,
            registry,
            client,
            task: Some(task),
        }
    }

    async fn ready(&self, epoch: BlockHash) {
        // Startup workers share one proof transfer. The two-height journey
        // revalidates retained inventories across those serialized requests.
        let mut last_response = String::from("no RPC response");
        let ready = tokio::time::timeout(Duration::from_secs(90), async {
            loop {
                assert!(
                    !self.task.as_ref().unwrap().is_finished(),
                    "node exited before readiness: {:?}",
                    self.stop.failure()
                );
                match self.client.signing_state(&address(0)).await {
                    Ok(state) => {
                        if state.payment_epoch == epoch {
                            break;
                        }
                        last_response = format!("installed epoch {}", state.payment_epoch);
                    }
                    Err(error) => last_response = error.to_string(),
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        if ready.is_err() {
            let registry = self.registry.read().await;
            panic!(
            "node {} did not install {epoch}; last RPC: {last_response}; peers: {:?}; recovering: {}",
            self.data_dir.display(),
            registry.peer_ids(),
            registry.is_recovering(),
            );
        }
    }

    async fn stop(mut self) -> crate::BurstNode {
        self.stop.shutdown();
        let mut task = self.task.take().unwrap();
        let (mut node, result) =
            match tokio::time::timeout(Duration::from_secs(20), &mut task).await {
                Ok(result) => result.unwrap(),
                Err(error) => {
                    task.abort();
                    let _ = task.await;
                    panic!("node shutdown timed out: {error}");
                }
            };
        result.unwrap();
        node.stop().await.unwrap();
        node
    }
}

async fn submit(node: &Running, payload: &str) -> burst_wallet_core::wallet::ProcessResult {
    for _ in 0..100 {
        match node.client.process(payload).await {
            Ok(result) => return result,
            Err(error) if error.to_string().contains("block queue full") => {
                assert!(
                    !node.task.as_ref().unwrap().is_finished(),
                    "node exited during queue admission: {:?}",
                    node.stop.failure()
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => panic!(
                "payment RPC failed: {error}; shutdown: {:?}",
                node.stop.failure()
            ),
        }
    }
    panic!("payment queue remained unavailable after bounded retries");
}

async fn burn(
    node: &Running,
    params: &ProtocolParams,
    epoch: BlockHash,
    stale_epoch: BlockHash,
    now: u64,
) -> BlockHash {
    let state = node.client.signing_state(&address(0)).await.unwrap();
    assert_eq!(state.payment_epoch, epoch);
    let transaction = burst_transactions::Transaction::Burn(
        build_burn_tx(&address(0), &address(1), 1, Timestamp::new(now)).unwrap(),
    );
    let block =
        build_state_block(&state, &transaction, TxHash::ZERO, params.params_hash()).unwrap();
    let key = burst_crypto::keypair_from_seed(&[0; 32]);
    let mut stale = block.clone();
    stale.payment_epoch = stale_epoch;
    stale.hash = stale.compute_hash();
    let stale = sign_state_block(stale, &key.private);
    let stale_json = serde_json::to_string(&stale).unwrap();
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let response = submit(node, &stale_json).await;
            assert!(
                !node.store.block_store().exists(&stale.hash).unwrap(),
                "old epoch payment entered the ledger"
            );
            // RPC acknowledges queued submissions before their final result.
            if !response.accepted {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("old epoch payment was not rejected");
    let block = sign_state_block(block, &key.private);
    let response = submit(node, &serde_json::to_string(&block).unwrap()).await;
    assert!(
        response.accepted,
        "payment after compact installation rejected: {response:?}"
    );
    tokio::time::timeout(Duration::from_secs(90), async {
        while !node.store.block_store().exists(&block.hash).unwrap() {
            assert!(
                !node.task.as_ref().unwrap().is_finished(),
                "node exited during payment admission: {:?}",
                node.stop.failure()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("valid payment remained queued");
    block.hash
}

fn prepare() -> Prepared {
    prepare_with_fixture().0
}

fn prepare_with_fixture() -> (Prepared, Fixture) {
    let (mut fixture, source_dir, source, selected, _) = selection::setup();
    let bytes = archive(&source, &fixture, selected.checkpoint.parent);
    let old =
        authenticate(&source, &fixture.params, selected.checkpoint.parent).into_continuation();
    let second =
        authenticate(&source, &fixture.params, selected.checkpoint.hash()).into_continuation();
    while let Some(candidate) = crate::confirmed_replay::installation::PreparedInstallation::next(
        &source,
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap()
    {
        candidate.commit(&source).unwrap();
    }
    fixture.push(
        1,
        BlockType::ChangeRepresentative,
        selected.checkpoint.cutoff + 2,
        |block| {
            block.payment_epoch = selected.checkpoint.hash();
            block.representative = address(0);
        },
    );
    fixture.append_confirmed_prefix(&source, selected.checkpoint.cutoff + 3);
    let roster = crate::checkpoint_roster::PreparedRoster::prepare_next(
        &source,
        NetworkId::Dev,
        &fixture.params,
        selected.checkpoint.cutoff + 4,
    )
    .unwrap()
    .unwrap()
    .manifest()
    .clone();
    let certificate = certified_votes::certify(&roster.checkpoint, &[0, 1, 2, 3, 4, 5, 6]);
    crate::checkpoint_store::chain::accept_manifest(
        &source,
        NetworkId::Dev,
        &fixture.params,
        certificate.clone(),
        &crate::checkpoint_agreement::ReplayManifest::Roster(roster.clone()),
    )
    .unwrap();
    let (target_dir, local) = cemented::sparse(&old, &source);
    records(&local, &source, &fixture, &bytes);
    let key = format!("checkpoint_chain/v1/dev/{}/3", old.anchor().genesis_hash());
    local
        .meta_store()
        .put_meta(&key, &source.meta_store().get_meta(&key).unwrap())
        .unwrap();
    let suffix = fixture.blocks.last().unwrap();
    local
        .meta_store()
        .put_meta(
            &format!("checkpoint_history/v1/{}", suffix.hash),
            &bincode::serialize(suffix).unwrap(),
        )
        .unwrap();
    let genesis = &fixture.blocks[0];
    let mut batch = local.tx_begin_write().unwrap();
    batch
        .put_block_with_account(
            &genesis.hash,
            &bincode::serialize(genesis).unwrap(),
            &genesis.account,
            1,
        )
        .unwrap();
    batch.commit().unwrap();
    assert!(!second
        .plan_tail(&local, roster.checkpoint.cutoff, &roster.heads)
        .unwrap()
        .witness_hashes()
        .is_empty());
    assert!(crate::checkpoint_store::chain::load(&local, NetworkId::Dev, &fixture.params).is_err());
    local.force_sync().unwrap();
    drop(local);
    while let Some(candidate) = crate::confirmed_replay::installation::PreparedInstallation::next(
        &source,
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap()
    {
        candidate.commit(&source).unwrap();
    }
    source.force_sync().unwrap();
    drop(source);
    let reservations = (0..4)
        .map(|_| std::net::TcpListener::bind("0.0.0.0:0").unwrap())
        .collect::<Vec<_>>();
    let ports = reservations
        .iter()
        .map(|listener| listener.local_addr().unwrap().port())
        .collect::<Vec<_>>();
    let config = |index: usize, directory: &std::path::Path| crate::NodeConfig {
        network: NetworkId::Dev,
        data_dir: directory.to_path_buf(),
        params: fixture.params.clone(),
        port: ports[index * 2],
        rpc_port: ports[index * 2 + 1],
        enable_upnp: false,
        enable_peer_reachout: false,
        enable_representative: false,
        enable_faucet: false,
        nts_sources: vec![],
        drand_relays: vec!["http://127.0.0.1:1".into()],
        bootstrap_peers: if index == 0 {
            vec![]
        } else {
            vec![format!("127.0.0.1:{}", ports[0])]
        },
        ..crate::NodeConfig::default()
    };
    let source_config = config(0, source_dir.path());
    let target_config = config(1, target_dir.path());
    drop(reservations);
    let epoch = certificate.checkpoint.hash();
    let now = roster.checkpoint.cutoff + 1;
    let prepared = Prepared {
        _source_dir: source_dir,
        _target_dir: target_dir,
        source_config,
        target_config,
        params: fixture.params.clone(),
        bytes,
        old_epoch: old.anchor().hash(),
        epoch,
        now,
    };
    (prepared, fixture)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_import_uses_committed_parent_without_installing_the_new_decision() {
    let (prepared, fixture) = prepare_with_fixture();
    let source =
        Arc::new(LmdbStore::open(prepared.source_config.data_dir.as_path(), 32, 32 << 20).unwrap());
    let local =
        Arc::new(LmdbStore::open(prepared.target_config.data_dir.as_path(), 32, 32 << 20).unwrap());
    let prefix = format!("checkpoint_chain/v1/dev/{}", fixture.blocks[0].hash);
    let mut batch = local.tx_begin_write().unwrap();
    batch.delete_meta(&format!("{prefix}/3")).unwrap();
    batch
        .put_meta(&format!("{prefix}/head"), &2u64.to_be_bytes())
        .unwrap();
    batch.commit().unwrap();
    let service =
        crate::confirmed_replay::tests::snapshot_history::installed_recovery::Service::start(
            source.clone(),
            local.clone(),
            0,
        )
        .await;
    let candidate = installed::recover_metadata_from_peers(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
    )
    .await
    .unwrap()
    .unwrap()
    .prepare_next_from_peers(local.clone(), service.registry.clone())
    .await
    .unwrap()
    .unwrap();
    let (_, recovery, handoff) = candidate.commit_with_recovery(&local).unwrap();
    let parent = recovery.recovered.continuation.anchor().hash();
    let shared = service.registry.read().await.recovery_projection.clone();
    handoff.publish(&shared, &local).unwrap();
    assert_eq!(shared.replay_handoffs(), 0);
    let transfer = crate::checkpoint_store::CertifiedCheckpoint::export_transfer(
        &source,
        NetworkId::Dev,
        &fixture.params,
        3,
    )
    .unwrap()
    .unwrap();
    let mut imports = crate::checkpoint_store::historical::snapshot_transfer::Session::default();
    assert_eq!(
        imports
            .import(
                local.clone(),
                NetworkId::Dev,
                fixture.params.clone(),
                3,
                transfer,
                service.registry.clone()
            )
            .await
            .unwrap(),
        Some(parent)
    );
    assert_eq!(
        shared.replay_handoffs(),
        1,
        "import must consume the committed parent handoff"
    );
    assert_eq!(imports.recovery_anchor(), Some(parent));
    assert_eq!(
        local.meta_store().payment_epoch().unwrap(),
        parent,
        "accepting a transferred decision must not install it"
    );
    let next = installed::recover_metadata_from_peers(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
    )
    .await
    .unwrap()
    .unwrap()
    .prepare_next_from_peers(local.clone(), service.registry.clone())
    .await
    .unwrap()
    .unwrap();
    assert_eq!(next.anchor().hash(), prepared.epoch);
    next.commit(&local).unwrap();
    assert_eq!(local.meta_store().payment_epoch().unwrap(), prepared.epoch);
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_nodes_install_two_epochs_fetch_proofs_and_restart_payment_admission() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let Prepared {
        _source_dir,
        _target_dir,
        source_config,
        target_config,
        params,
        bytes,
        old_epoch,
        epoch,
        now,
    } = prepare();
    let source = Running::start(source_config, now + 1).await;
    source.ready(epoch).await;
    let target = Running::start(target_config.clone(), now).await;
    target.ready(epoch).await;
    for store in [&source.store, &target.store] {
        let batch = store.tx_begin_write().unwrap();
        assert!(crate::graph_storage::row_format(&batch).unwrap());
        assert!(batch.get_meta("merger_graph").unwrap().is_none());
    }
    assert!(crate::checkpoint_store::chain::load(&target.store, NetworkId::Dev, &params).is_err());
    let first = burn(&target, &params, epoch, old_epoch, now).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !source.store.block_store().exists(&first).unwrap() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("normal peer path did not relay the admitted payment");
    target.stop().await;
    let target = Running::start(target_config, now + 1).await;
    target.ready(epoch).await;
    {
        let batch = target.store.tx_begin_write().unwrap();
        assert!(crate::graph_storage::row_format(&batch).unwrap());
    }
    assert!(target.store.block_store().exists(&first).unwrap());
    assert_eq!(
        target.client.signing_state(&address(0)).await.unwrap().head,
        first
    );
    assert!(crate::checkpoint_store::chain::load(&target.store, NetworkId::Dev, &params).is_err());
    let second = burn(&target, &params, epoch, old_epoch, now + 1).await;
    assert!(target.store.block_store().exists(&second).unwrap());
    assert_eq!(
        installed::read_stored_base_bytes(&target.store)
            .unwrap()
            .unwrap(),
        bytes
    );
    target.stop().await;
    source.stop().await;
}
