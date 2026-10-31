use super::*;

#[test]
fn committed_selection_heads_authenticate_exact_bodies_without_newer_ancestry() {
    let (prepared, _) = prepare_active();
    let store = LmdbStore::open(&prepared.source_config.data_dir, 32, 32 << 20).unwrap();
    let snapshot = authenticate(&store, &prepared.params, prepared.epoch);
    let selections = snapshot.recover_selections().unwrap();
    let base = snapshot.into_continuation();
    let mut historical = false;
    for head in selections.iter().flat_map(|selection| selection.heads()) {
        let mut cursor = base.committed_history_cursor(head.hash).unwrap().unwrap();
        assert_eq!(cursor.context(), base.history_context());
        assert_eq!(cursor.next().as_ref(), Some(head));
        let bytes = store.block_store().get_block(&head.hash).unwrap();
        let mut changed: burst_ledger::StateBlock = bincode::deserialize(&bytes).unwrap();
        changed.signature.0[0] ^= 1;
        assert!(cursor
            .accept(&[bincode::serialize(&changed).unwrap()])
            .is_err());
        assert_eq!(cursor.next().as_ref(), Some(head));
        let proofs = cursor.accept(std::slice::from_ref(&bytes)).unwrap();
        assert_eq!(proofs[0].height(), head.height);
        let mut current = base.history_cursor(&head.account).unwrap().unwrap();
        if current.next().unwrap().height > head.height {
            historical = true;
            assert!(current.accept(&[bytes]).is_err());
        }
    }
    assert!(historical);
    assert!(base
        .committed_history_cursor(BlockHash::new([99; 32]))
        .unwrap()
        .is_none());
    let genesis = crate::genesis_state::block(NetworkId::Dev, &prepared.params);
    let mut cursor = base
        .committed_history_cursor(genesis.hash)
        .unwrap()
        .unwrap();
    assert_eq!(cursor.next().unwrap().height, 1);
    cursor
        .accept(&[bincode::serialize(&genesis).unwrap()])
        .unwrap();
    let mut wanted: Vec<_> = selections
        .iter()
        .flat_map(|selection| selection.heads())
        .map(|head| head.hash)
        .collect();
    wanted.extend([genesis.hash, genesis.hash, BlockHash::new([99; 32])]);
    wanted.reverse();
    let mut seen = std::collections::HashSet::new();
    let expected: Vec<_> = wanted
        .iter()
        .copied()
        .filter(|hash| *hash != BlockHash::new([99; 32]) && seen.insert(*hash))
        .collect();
    let cursors = base.committed_history_cursors(&wanted).unwrap();
    assert_eq!(
        cursors
            .iter()
            .map(|cursor| cursor.next().unwrap().hash)
            .collect::<Vec<_>>(),
        expected
    );
    let mut streamed = std::collections::HashSet::new();
    base.visit_committed_history(
        |hash| Ok(wanted.contains(&hash)),
        |mut cursor| {
            assert_eq!(cursor.context(), base.history_context());
            let head = cursor.next().unwrap();
            let proofs = cursor.accept(&[store.block_store().get_block(&head.hash).unwrap()])?;
            assert_eq!(proofs[0].height(), head.height);
            streamed.insert(head.hash);
            Ok(())
        },
    ).unwrap();
    assert_eq!(streamed, expected.iter().copied().collect());
    let ordered: Vec<_> = std::iter::once(genesis.hash).chain(selections.iter()
        .flat_map(|receipt| receipt.heads()).map(|head| head.hash)
        .filter(|hash| *hash != genesis.hash)).collect();
    for budget in [1, 2, 128] {
        let mut walk = base.committed_history_walk();
        let mut actual = Vec::new();
        loop {
            let before = actual.len();
            let done = walk.advance(budget, |_| Ok(true), |cursor| {
                actual.push(cursor.next().unwrap().hash);
                Ok(())
            }).unwrap();
            assert!(actual.len() - before <= budget);
            if done { break; }
        }
        assert_eq!(actual, ordered);
        assert!(walk.advance(budget, |_| panic!("completed"), |_| panic!("completed")).unwrap());
    }
    let mut failed = base.committed_history_walk();
    assert!(failed.advance(1, |_| Err(burst_store::StoreError::Corruption("stop".into()).into()), |_| Ok(())).is_err());
    assert!(failed.advance(1, |_| Ok(true), |_| Ok(())).is_err());
    let mut failed = base.committed_history_walk();
    assert!(failed.advance(1, |_| Ok(true), |_| Err(burst_store::StoreError::Corruption("stop".into()).into())).is_err());
    assert!(failed.advance(1, |_| Ok(true), |_| Ok(())).is_err());
    let mut zero = base.committed_history_walk();
    assert!(zero.advance(0, |_| Ok(false), |_| Ok(())).is_err());
    assert!(zero.advance(1, |_| Ok(false), |_| Ok(())).is_err());

    base.visit_committed_history(|_| Ok(false), |_| panic!("unrequested committed head")).unwrap();
    assert!(base.visit_committed_history(|_| Err(burst_store::StoreError::Corruption("stop".into()).into()), |_| Ok(())).is_err());
    assert!(base.visit_committed_history(|_| Ok(true), |_| Err(burst_store::StoreError::Corruption("stop".into()).into())).is_err());
    for mut cursor in cursors {
        assert_eq!(cursor.context(), base.history_context());
        let head = cursor.next().unwrap();
        let proofs = cursor
            .accept(&[store.block_store().get_block(&head.hash).unwrap()])
            .unwrap();
        assert_eq!(proofs[0].height(), head.height);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_live_sparse_source_bootstraps_an_observer_without_archival_peer() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let (prepared, _) = prepare_active();
    let store = LmdbStore::open(&prepared.source_config.data_dir, 32, 32 << 20).unwrap();
    let bytes = export_bytes(&store, &prepared.params, prepared.epoch);
    let full_count = store.block_store().block_count().unwrap();
    drop(store);
    let directory = tempfile::tempdir().unwrap();
    let archive_path = directory.path().join("relay.snapshot");
    std::fs::write(&archive_path, &bytes).unwrap();
    let source = Running::start(prepared.source_config.clone(), prepared.now + 1).await;
    source.ready(prepared.epoch).await;
    let mut relay_config = prepared.target_config.clone();
    relay_config.data_dir = directory.path().join("relay");
    relay_config.snapshot_bootstrap = Some(archive_path.clone());
    relay_config.snapshot_transfer.serve = Some(archive_path);
    let relay = Running::start(relay_config.clone(), prepared.now + 1).await;
    relay.ready(prepared.epoch).await;
    assert!(relay.store.block_store().block_count().unwrap() < full_count);
    assert!(
        crate::checkpoint_store::chain::load(&relay.store, NetworkId::Dev, &prepared.params)
            .is_err()
    );
    assert_lifecycle(&source.store, &relay.store, None);
    let expected = restore_lifecycle(&relay.store).snapshot().encode().unwrap();
    relay.stop().await;
    source.stop().await;
    relay_config.snapshot_bootstrap = None;
    relay_config.bootstrap_peers.clear();
    let relay = Running::start(relay_config.clone(), prepared.now + 2).await;
    relay.ready(prepared.epoch).await;
    let observer_path = directory.path().join("observer.snapshot");
    let mut observer_config = prepared.source_config.clone();
    observer_config.data_dir = directory.path().join("observer");
    observer_config.bootstrap_peers = vec![format!("127.0.0.1:{}", relay_config.port)];
    observer_config.snapshot_bootstrap = Some(observer_path.clone());
    observer_config.snapshot_transfer.fetch = Some(crate::checkpoint_sync::snapshot::Fetch {
        epoch: Some(prepared.epoch.to_string()),
        minimum_height: 1,
        output: observer_path.clone(),
    });
    assert!(!observer_path.exists());
    let observer = Running::start(observer_config, prepared.now + 2).await;
    observer.ready(prepared.epoch).await;
    assert_eq!(std::fs::read(&observer_path).unwrap(), bytes);
    assert_eq!(
        restore_lifecycle(&observer.store)
            .snapshot()
            .encode()
            .unwrap(),
        expected
    );
    assert!(observer.store.block_store().block_count().unwrap() < full_count);
    burn(
        &observer,
        &prepared.params,
        prepared.epoch,
        prepared.old_epoch,
        prepared.now + 2,
    )
    .await;
    observer.stop().await;
    relay.stop().await;
}
