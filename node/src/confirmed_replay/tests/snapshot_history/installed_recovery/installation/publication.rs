use super::*;

#[test]
fn full_installation_rejects_a_compact_base_added_after_preparation() {
    let (fixture, _dir, source, _, _) = selection::setup();
    let candidate = crate::confirmed_replay::installation::PreparedInstallation::next(
        &source,
        NetworkId::Dev,
        &fixture.params,
    )
    .unwrap()
    .unwrap();
    source
        .meta_store()
        .put_meta(installed::BASE_KEY, b"concurrent base change")
        .unwrap();
    let before = source
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    assert!(candidate.commit(&source).is_err());
    assert_eq!(
        source
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_installation_publishes_live_engines_and_preserves_old_promises() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_ansi(false)
        .try_init();
    let (fixture, _source_dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let old =
        authenticate(&source, &fixture.params, manifest.checkpoint.parent).into_continuation();
    let expected =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    let (dir, local) = cemented::sparse(&old, &source);
    copy_installed_records(&local, &source, &fixture, &bytes);
    // A runnable sparse node retains its pinned network genesis even when the
    // genesis account has advanced. The head-only replay fixture omits it.
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
    let promised = BlockHash::new([99; 32]);
    local
        .meta_store()
        .reserve_consensus_vote_at(
            &old.anchor().hash(),
            &address(0),
            &address(1),
            &BlockHash::ZERO,
            &promised,
            true,
            0,
            Some(old.anchor().cutoff() + 1),
        )
        .unwrap();
    let promises = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"consensus_final/v2/")
        .unwrap();
    assert!(!promises.is_empty());
    local.force_sync().unwrap();
    drop(local);
    let mut node = crate::BurstNode::new(crate::NodeConfig {
        network: NetworkId::Dev,
        data_dir: dir.path().to_path_buf(),
        params: fixture.params.clone(),
        port: 0,
        rpc_port: 0,
        enable_upnp: false,
        enable_representative: false,
        enable_faucet: false,
        nts_sources: vec![],
        bootstrap_peers: vec![],
        ..crate::NodeConfig::default()
    })
    .await
    .unwrap();
    let service = WitnessService::start(Arc::new(source), node.store.clone(), 0).await;
    node.ledger_cache.reset((999, 99, 9));
    node.frontier.write().await.update(address(99), promised);
    node.priority_scheduler
        .lock()
        .await
        .push(promised, address(99), 1);
    node.backlog
        .lock()
        .await
        .insert(promised, address(99), 1, 1);
    node.local_broadcaster
        .lock()
        .await
        .track(promised, vec![99], 1);
    let mut runtime = crate::checkpoint_installation::Runtime {
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
        initial_params: fixture.params.clone(),
        shutdown: node.shutdown.clone(),
        registry: service.registry.clone(),
        last_complete: None,
        recovery: None,
        publication_probe: None,
    };
    let bad_journal = "consensus_final/unsupported";
    node.store
        .meta_store()
        .put_meta(bad_journal, b"invalid promise")
        .unwrap();
    let before = node
        .store
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    for _ in 0..2 {
        assert!(runtime.install_next().await.is_err());
        assert_eq!(
            runtime
                .recovery
                .as_ref()
                .expect("retain authenticated replay after a failed check")
                .recovered
                .continuation
                .anchor()
                .hash(),
            old.anchor().hash(),
        );
        assert_eq!(
            node.store.meta_store().payment_epoch().unwrap(),
            old.anchor().hash()
        );
        assert_eq!(
            node.store
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
        assert!(runtime.last_complete.is_none());
    }
    node.store.meta_store().delete_meta(bad_journal).unwrap();
    assert!(runtime.install_next().await.unwrap().is_some());
    let brn = node.brn_engine.lock().await;
    assert_eq!(
        serde_json::to_value(&brn.wallets).unwrap(),
        serde_json::to_value(&expected.state().brn.wallets).unwrap()
    );
    assert_eq!(brn.next_stake_id(), expected.state().brn.next_stake_id());
    drop(brn);
    let governance = expected.state().governance.as_ref().unwrap();
    assert_eq!(
        serde_json::to_value(&*node.governance.lock().await).unwrap(),
        serde_json::to_value(&governance.governance).unwrap()
    );
    assert_eq!(
        node.delegation_engine.lock().await.save_state(),
        governance.delegation.save_state()
    );
    assert_eq!(
        serde_json::to_value(&*node.consti_engine.lock().await).unwrap(),
        serde_json::to_value(&governance.constitution).unwrap()
    );
    assert!(node.priority_scheduler.lock().await.is_empty());
    assert!(node.backlog.lock().await.is_empty());
    assert!(node.local_broadcaster.lock().await.is_empty());
    assert_eq!(
        (
            node.ledger_cache.block_count(),
            node.ledger_cache.account_count(),
            node.ledger_cache.pending_count()
        ),
        node.store
            .tx_begin_write()
            .unwrap()
            .ledger_counts()
            .unwrap()
    );
    let frontier = node.frontier.read().await;
    let accounts = expected
        .state()
        .accounts
        .values()
        .filter(|account| account.block_count > 0)
        .collect::<Vec<_>>();
    assert_eq!(frontier.account_count(), accounts.len());
    for account in accounts {
        assert_eq!(frontier.get_head(&account.address), Some(&account.head));
    }
    drop(frontier);
    assert_eq!(
        node.store.meta_store().payment_epoch().unwrap(),
        expected.anchor().hash()
    );
    assert_eq!(
        node.active_elections.read().await.authority_epoch(),
        Some(expected.anchor().hash())
    );
    assert_eq!(
        node.verification_orchestrator
            .lock()
            .await
            .snapshot()
            .encode()
            .unwrap(),
        expected.state().verification.snapshot().encode().unwrap()
    );
    assert_eq!(
        node.trst_engine.lock().await.save_wallets(),
        expected.state().trst.save_wallets()
    );
    assert_eq!(
        node.store
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"consensus_final/v2/")
            .unwrap(),
        promises
    );
    assert_eq!(
        installed::read_stored_base_bytes(&node.store)
            .unwrap()
            .unwrap(),
        bytes
    );
    let before = node
        .store
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    assert!(runtime.install_next().await.unwrap().is_none());
    let cached_epoch = runtime
        .recovery
        .as_ref()
        .expect("idle installer retains replay")
        .recovered
        .continuation
        .anchor()
        .hash();
    assert_eq!(
        cached_epoch,
        node.store.meta_store().payment_epoch().unwrap()
    );
    // Force another observation rather than taking last_complete's fast path.
    runtime.last_complete = None;
    assert!(runtime.install_next().await.unwrap().is_none());
    assert_eq!(
        runtime
            .recovery
            .as_ref()
            .unwrap()
            .recovered
            .continuation
            .anchor()
            .hash(),
        cached_epoch
    );
    assert_eq!(
        node.store
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    drop(runtime);
    service.stop().await;
    node.stop().await.unwrap();
}
