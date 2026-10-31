use super::*;

#[cfg(unix)]
mod crash;
#[cfg(unix)]
mod download_crash;
mod live_install;
pub(in crate::confirmed_replay::tests) mod progression;
mod record_catchup;
#[cfg(unix)]
mod record_crash;
#[cfg(unix)]
mod replacement_crash;
mod sparse_source;

fn assert_trst_state(source: &LmdbStore, target: &LmdbStore, expiry: u64) {
    let source = source.tx_begin_write().unwrap();
    let target = target.tx_begin_write().unwrap();
    assert_eq!(
        crate::trst_storage::load(&source, expiry)
            .unwrap()
            .save_wallets(),
        crate::trst_storage::load(&target, expiry)
            .unwrap()
            .save_wallets()
    );
    assert_eq!(
        crate::graph_storage::load(&source).unwrap().to_bytes(),
        crate::graph_storage::load(&target).unwrap().to_bytes()
    );
}

fn export_bytes(source: &LmdbStore, params: &ProtocolParams, epoch: BlockHash) -> Vec<u8> {
    let snapshot = authenticate(source, params, epoch);
    let export = SnapshotExport::prepare(source, NetworkId::Dev, params, epoch).unwrap();
    let signatures: Vec<_> = (0..32)
        .filter(|seed| snapshot.anchor().roster().weight(&address(*seed)) > 0)
        .map(|seed| {
            export
                .attest(
                    source,
                    &burst_crypto::keypair_from_seed(&[seed; 32]).private,
                )
                .unwrap()
        })
        .collect();
    let mut bytes = Vec::new();
    export.write(&signatures, &mut bytes).unwrap();
    bytes
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_bootstrap_archive_fetches_heads_and_restarts_payments() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let prepared = prepare();
    run_bootstrap(prepared).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_bootstrap_active_panel_preserves_committee_and_deadline() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let (prepared, _) = prepare_active();
    run_bootstrap(prepared).await;
}

fn prepare_active() -> (Prepared, Fixture) {
    let (mut prepared, fixture) = prepare_with_fixture();
    let source = LmdbStore::open(&prepared.source_config.data_dir, 32, 32 << 20).unwrap();
    let events =
        crate::checkpoint_store::chain::load(&source, NetworkId::Dev, &prepared.params).unwrap();
    let mut selection = events
        .into_iter()
        .find_map(|event| match event {
            crate::checkpoint_store::chain::CertifiedEvent::Selection(value) => Some(*value),
            _ => None,
        })
        .unwrap();
    let beacons: Vec<burst_vrf::DrandBeacon> = serde_json::from_str(include_str!(
        "../../../../../../../tests/fixtures/selection-beacons.json"
    ))
    .unwrap();
    selection
        .attach_beacon(
            &source,
            &prepared.params,
            burst_vrf::DrandClient::beacon_to_random_output(&beacons[0]).unwrap(),
        )
        .unwrap();
    let snapshot = authenticate(&source, &prepared.params, prepared.epoch);
    let signers: Vec<_> = (0..32)
        .filter(|seed| snapshot.anchor().roster().weight(&address(*seed)) > 0)
        .collect();
    let activation = crate::checkpoint_activation::PreparedActivation::prepare_next(
        &source,
        NetworkId::Dev,
        &prepared.params,
        prepared.now + 31,
    )
    .unwrap()
    .unwrap();
    let manifest = activation.manifest().clone();
    let certificate = certified_votes::certify(&manifest.checkpoint, &signers);
    prepared.epoch = certificate.checkpoint.hash();
    prepared.now = certificate.checkpoint.cutoff + 1;
    crate::checkpoint_store::chain::accept_manifest(
        &source,
        NetworkId::Dev,
        &prepared.params,
        certificate,
        &crate::checkpoint_agreement::ReplayManifest::Activation(manifest),
    )
    .unwrap();
    crate::confirmed_replay::installation::PreparedInstallation::next(
        &source,
        NetworkId::Dev,
        &prepared.params,
    )
    .unwrap()
    .unwrap()
    .commit(&source)
    .unwrap();
    source.force_sync().unwrap();
    drop(source);
    (prepared, fixture)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_bootstrap_settled_panel_preserves_verification_and_rewards() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let (mut prepared, mut fixture) = prepare_active();
    let source = LmdbStore::open(&prepared.source_config.data_dir, 32, 32 << 20).unwrap();
    let snapshot = authenticate(&source, &prepared.params, prepared.epoch);
    let selection = snapshot.recover_selections().unwrap().remove(0);
    let context = selection
        .replayed()
        .request()
        .vote_context(&selection.certificate().checkpoint)
        .unwrap();
    for seed in certified_votes::panel(&selection) {
        fixture.push(seed, BlockType::VerificationVote, prepared.now, |block| {
            block.payment_epoch = prepared.epoch;
            block.link =
                BlockHash::new(burst_crypto::decode_address(address(20).as_str()).unwrap());
            block.origin = context.into_tx_hash();
            let mut choice = [0; 32];
            choice[0] = 1;
            block.transaction = TxHash::new(choice);
            block.brn_spent += prepared.params.verifier_stake_amount;
        });
    }
    fixture.append_confirmed_prefix(&source, prepared.now + 1);
    let roster = crate::checkpoint_roster::PreparedRoster::prepare_next(
        &source,
        NetworkId::Dev,
        &prepared.params,
        prepared.now + 1,
    )
    .unwrap()
    .unwrap()
    .manifest()
    .clone();
    let signers: Vec<_> = (0..32)
        .filter(|seed| snapshot.anchor().roster().weight(&address(*seed)) > 0)
        .collect();
    let certificate = certified_votes::certify(&roster.checkpoint, &signers);
    prepared.epoch = certificate.checkpoint.hash();
    prepared.now = certificate.checkpoint.cutoff + 1;
    crate::checkpoint_store::chain::accept_manifest(
        &source,
        NetworkId::Dev,
        &prepared.params,
        certificate,
        &crate::checkpoint_agreement::ReplayManifest::Roster(roster),
    )
    .unwrap();
    crate::confirmed_replay::installation::PreparedInstallation::next(
        &source,
        NetworkId::Dev,
        &prepared.params,
    )
    .unwrap()
    .unwrap()
    .commit(&source)
    .unwrap();
    assert_eq!(
        restore_lifecycle(&source)
            .get_state(&address(20))
            .unwrap()
            .phase,
        burst_verification::state::VerificationPhase::Verified
    );
    source.force_sync().unwrap();
    drop(source);
    run_bootstrap(prepared).await;
}

async fn run_bootstrap(prepared: Prepared) {
    run_bootstrap_via(prepared, BootstrapMode::File).await;
}

pub(in crate::confirmed_replay::tests) async fn assert_lifecycle_archive(
    fixture: Fixture,
    history_dir: tempfile::TempDir,
    history_store: LmdbStore,
) {
    run_bootstrap(prepare_lifecycle_archive(
        fixture,
        history_dir,
        history_store,
        None,
    ))
    .await;
}

fn prepare_lifecycle_archive(
    fixture: Fixture,
    _history_dir: tempfile::TempDir,
    history_store: LmdbStore,
    height: Option<usize>,
) -> Prepared {
    let mut events =
        crate::checkpoint_store::chain::load(&history_store, NetworkId::Dev, &fixture.params)
            .unwrap();
    if let Some(height) = height {
        assert!(height > 0 && height < events.len());
        events.truncate(height);
    }
    let last = &events.last().unwrap().certificate().checkpoint;
    let epoch = last.hash();
    let now = last.cutoff + 1;
    let old_epoch = events[0].certificate().checkpoint.hash();
    let initial = Fixture {
        params: fixture.params.clone(),
        blocks: vec![fixture.blocks[0].clone()],
    };
    let (source_dir, source, _) = initial.store();
    for event in &events {
        let checkpoint = &event.certificate().checkpoint;
        let history = ConfirmedHistory::read_certified(
            &history_store,
            NetworkId::Dev,
            fixture.blocks[0].hash,
            checkpoint.cutoff,
            event.heads(),
            &[],
        )
        .unwrap();
        let hashes: HashSet<_> = history
            .replay_blocks()
            .iter()
            .map(|block| block.hash)
            .chain([fixture.blocks[0].hash])
            .collect();
        let prefix = Fixture {
            params: fixture.params.clone(),
            blocks: fixture
                .blocks
                .iter()
                .filter(|block| hashes.contains(&block.hash))
                .cloned()
                .collect(),
        };
        let transfer = CertifiedCheckpoint::export_transfer(
            &history_store,
            NetworkId::Dev,
            &fixture.params,
            checkpoint.height,
        )
        .unwrap()
        .unwrap();
        CertifiedCheckpoint::import_transfer(
            &source,
            NetworkId::Dev,
            &fixture.params,
            checkpoint.height,
            &transfer,
            &crate::checkpoint_store::historical::HistoricalAdmissions::default(),
        )
        .unwrap();
        prefix.append_confirmed_prefix(&source, checkpoint.cutoff);
        crate::confirmed_replay::installation::PreparedInstallation::next(
            &source,
            NetworkId::Dev,
            &fixture.params,
        )
        .unwrap()
        .unwrap()
        .commit(&source)
        .unwrap();
    }
    drop(history_store);
    source.force_sync().unwrap();
    drop(source);
    let target_dir = tempfile::tempdir().unwrap();
    let reservations: Vec<_> = (0..4)
        .map(|_| std::net::TcpListener::bind("0.0.0.0:0").unwrap())
        .collect();
    let ports: Vec<_> = reservations
        .iter()
        .map(|socket| socket.local_addr().unwrap().port())
        .collect();
    let config = |index: usize, path: &std::path::Path| crate::NodeConfig {
        network: NetworkId::Dev,
        data_dir: path.to_path_buf(),
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
    Prepared {
        _source_dir: source_dir,
        _target_dir: target_dir,
        source_config,
        target_config,
        params: fixture.params,
        bytes: vec![],
        old_epoch,
        epoch,
        now,
    }
}

enum BootstrapMode {
    File,
    Download,
    Replace,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_bootstrap_downloads_active_archive_before_admission() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let (prepared, _) = prepare_active();
    run_bootstrap_via(prepared, BootstrapMode::Download).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_bootstrap_replaces_existing_base_and_preserves_accepted_history() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let (prepared, _) = prepare_active();
    run_bootstrap_via(prepared, BootstrapMode::Replace).await;
}

async fn run_bootstrap_via(prepared: Prepared, mode: BootstrapMode) {
    let download = matches!(mode, BootstrapMode::Download);
    let replacement = matches!(mode, BootstrapMode::Replace);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bootstrap.snapshot");
    let store = LmdbStore::open(&prepared.source_config.data_dir, 32, 32 << 20).unwrap();
    let effective_params = authenticate(&store, &prepared.params, prepared.epoch)
        .into_state()
        .params()
        .clone();
    let bytes = export_bytes(&store, &prepared.params, prepared.epoch);
    drop(store);
    let mut source_config = prepared.source_config.clone();
    source_config.snapshot_transfer.max_archive_bytes = bytes.len() as u64;
    if download {
        let serve = directory.path().join("source.snapshot");
        std::fs::write(&serve, &bytes).unwrap();
        source_config.snapshot_transfer.serve = Some(serve);
        assert!(!path.exists());
    } else {
        std::fs::write(&path, &bytes).unwrap();
    }
    let source = if download {
        None
    } else {
        let source = Running::start(source_config.clone(), prepared.now + 1).await;
        source.ready(prepared.epoch).await;
        Some(source)
    };
    let mut config = prepared.target_config.clone();
    config.snapshot_transfer.max_archive_bytes = bytes.len() as u64;
    let retained = if replacement {
        let store = LmdbStore::open(&config.data_dir, 32, 32 << 20).unwrap();
        assert_eq!(
            installed::read_stored_base_bytes(&store).unwrap().unwrap(),
            prepared.bytes
        );
        assert_ne!(prepared.bytes, bytes);
        let old = installed::load_base(&store, NetworkId::Dev, &prepared.params)
            .unwrap()
            .unwrap();
        store
            .meta_store()
            .reserve_consensus_vote_at(
                &old.anchor().hash(),
                &address(0),
                &address(30),
                &BlockHash::ZERO,
                &BlockHash::new([98; 32]),
                true,
                0,
                Some(old.anchor().cutoff()),
            )
            .unwrap();
        let mut rows: Vec<_> = store
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"checkpoint_chain/")
            .unwrap()
            .into_iter()
            .filter(|(key, _)| !key.ends_with(b"/head"))
            .collect();
        assert!(!rows.is_empty());
        let promises = store
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"consensus_final/")
            .unwrap();
        assert_eq!(promises.len(), 1);
        rows.extend(promises);
        rows
    } else {
        config.data_dir = directory.path().join("node");
        Vec::new()
    };
    if download {
        config.snapshot_transfer.fetch = Some(crate::checkpoint_sync::snapshot::Fetch {
            epoch: Some(prepared.epoch.to_string()),
            minimum_height: 1,
            output: path.clone(),
        });
    }
    config.snapshot_bootstrap = Some(path.clone());
    let target = Running::start(config.clone(), prepared.now).await;
    let source = if let Some(source) = source {
        source
    } else {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if tokio::net::TcpStream::connect(("127.0.0.1", config.port))
                    .await
                    .is_ok()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", config.rpc_port))
                .await
                .is_err()
        );
        assert!(!path.exists());
        assert!(target
            .store
            .meta_store()
            .get_meta(installed::BASE_KEY)
            .is_err());
        let source = Running::start(source_config, prepared.now + 1).await;
        source.ready(prepared.epoch).await;
        source
    };
    target.ready(prepared.epoch).await;
    for node in [&source, &target] {
        let response = node
            .client
            .rpc_call("params_hash", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(
            response["params_hash"],
            effective_params.params_hash().to_string()
        );
    }
    assert_retained(&target.store, &retained);
    assert_trst_state(
        &source.store,
        &target.store,
        effective_params.trst_expiry_secs,
    );
    assert_lifecycle(&source.store, &target.store, None);
    assert_eq!(
        installed::read_stored_base_bytes(&target.store)
            .unwrap()
            .unwrap(),
        bytes
    );
    assert!(
        crate::checkpoint_store::chain::load(&target.store, NetworkId::Dev, &prepared.params)
            .is_err()
    );
    let first = burn(
        &target,
        &effective_params,
        prepared.epoch,
        prepared.old_epoch,
        prepared.now,
    )
    .await;
    target.stop().await;
    let target = Running::start(config, prepared.now + 1).await;
    target.ready(prepared.epoch).await;
    assert_retained(&target.store, &retained);
    assert_lifecycle(&source.store, &target.store, Some(first.into_tx_hash()));
    assert!(target.store.block_store().exists(&first).unwrap());
    burn(
        &target,
        &effective_params,
        prepared.epoch,
        prepared.old_epoch,
        prepared.now + 1,
    )
    .await;
    target.stop().await;
    source.stop().await;
}

fn assert_retained(store: &LmdbStore, retained: &[(Vec<u8>, Vec<u8>)]) {
    let transaction = store.tx_begin_read().unwrap();
    for (key, bytes) in retained {
        let key = std::str::from_utf8(key).unwrap();
        let mut actual = store.meta_store().get_meta_txn(&transaction, key).unwrap();
        if actual.is_none() && !key.ends_with("/head") {
            if let Some(suffix) = key.strip_prefix("checkpoint_chain/v1/") {
                actual = store
                    .meta_store()
                    .get_meta_txn(
                        &transaction,
                        &format!("checkpoint_record_archive/v1/{suffix}"),
                    )
                    .unwrap();
            }
        }
        assert_eq!(actual.as_ref(), Some(bytes), "retained bytes for {key}");
    }
}

fn restore_lifecycle(store: &LmdbStore) -> burst_verification::VerificationOrchestrator {
    crate::verification_storage::load(&store.tx_begin_write().unwrap()).unwrap()
}

fn assert_lifecycle(source: &LmdbStore, target: &LmdbStore, payment: Option<TxHash>) {
    use burst_store::brn::BrnStore;
    let expected = restore_lifecycle(source);
    let actual = restore_lifecycle(target);
    for prefix in [
        b"verification_receipt/".as_slice(),
        b"verification_latest_outcome/",
        b"verification_certified_outcome/",
    ] {
        assert_eq!(
            target
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(prefix)
                .unwrap(),
            source
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(prefix)
                .unwrap()
        );
    }
    for seed in [1, 8, 20] {
        let expected = source.account_store().get_account(&address(seed));
        let actual = target.account_store().get_account(&address(seed));
        match (expected, actual) {
            (Ok(expected), Ok(actual)) => {
                assert_eq!(actual.state, expected.state);
                assert_eq!(actual.trst_balance, expected.trst_balance);
            }
            (
                Err(burst_store::StoreError::NotFound(_)),
                Err(burst_store::StoreError::NotFound(_)),
            ) => {}
            other => panic!("account {seed} differs after archive recovery: {other:?}"),
        }
    }
    assert_eq!(
        actual.snapshot().encode().unwrap(),
        expected.snapshot().encode().unwrap()
    );
    for seed in 0..=20 {
        let wallet = address(seed);
        if seed != 0 {
            assert_eq!(
                target.brn_store().get_wallet_state(&wallet).unwrap(),
                source.brn_store().get_wallet_state(&wallet).unwrap(),
                "BRN state for {seed}"
            );
        }
        let pending = |store: &LmdbStore| {
            let mut entries = store
                .pending_store()
                .get_pending_for_account_with_hashes(&wallet)
                .unwrap();
            entries.retain(|(hash, _)| Some(*hash) != payment);
            bincode::serialize(&entries).unwrap()
        };
        assert_eq!(
            pending(target),
            pending(source),
            "pending rewards for {seed}"
        );
    }
    assert_eq!(
        actual.get_state(&address(20)).unwrap().phase,
        expected.get_state(&address(20)).unwrap().phase
    );
    assert_eq!(
        actual.selection_context(&address(20)),
        expected.selection_context(&address(20))
    );
    assert_eq!(
        actual.get_state(&address(20)).unwrap().selected_verifiers,
        expected.get_state(&address(20)).unwrap().selected_verifiers
    );
    assert_eq!(
        target
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"verification_activation/")
            .unwrap(),
        source
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"verification_activation/")
            .unwrap()
    );
}
