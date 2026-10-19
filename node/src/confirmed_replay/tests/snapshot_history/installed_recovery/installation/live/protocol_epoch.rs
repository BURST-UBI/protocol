use super::*;

async fn stored(node: &Running, hash: BlockHash) {
    tokio::time::timeout(Duration::from_secs(90), async {
        while !node.store.block_store().exists(&hash).unwrap() {
            assert!(
                !node.task.as_ref().unwrap().is_finished(),
                "node exited: {:?}",
                node.stop.failure()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("protocol epoch action did not reach the ledger");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saturated_ingress_recovers_dependency_and_persists_owner_chain() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let prepared = prepare();
    let store = LmdbStore::open(&prepared.source_config.data_dir, 32, 64 * 1024 * 1024).unwrap();
    let before = store.account_store().get_account(&address(1)).unwrap();
    let previous: StateBlock =
        bincode::deserialize(&store.block_store().get_block(&before.head).unwrap()).unwrap();
    drop(store);
    let mut parent = previous.clone();
    parent.block_type = BlockType::ChangeRepresentative;
    parent.previous = previous.hash;
    parent.signing_grant = previous.active_signing_grant();
    parent.payment_epoch = prepared.epoch;
    parent.params_hash = prepared.params.params_hash();
    parent.timestamp = Timestamp::new(prepared.now);
    parent.link = BlockHash::ZERO;
    parent.origin = TxHash::ZERO;
    parent.transaction = TxHash::ZERO;
    parent.target_head = BlockHash::ZERO;
    parent.merge_sources.clear();
    parent.release_sources.clear();
    parent.delegation_payload.clear();
    sign(1, &mut parent);
    let mut child = parent.clone();
    child.previous = parent.hash;
    child.representative = address(2);
    sign(1, &mut child);
    let source =
        Running::start_prefilled(prepared.source_config.clone(), prepared.now, Some(&child)).await;
    source.ready(prepared.epoch).await;
    let target = Running::start(prepared.target_config.clone(), prepared.now).await;
    target.ready(prepared.epoch).await;
    let response = submit(&source, &serde_json::to_string(&parent).unwrap()).await;
    assert!(
        response.accepted,
        "parent could not progress after ingress pressure: {response:?}"
    );
    stored(&source, child.hash).await;
    stored(&target, child.hash).await;
    for node in [&source, &target] {
        let after = node.store.account_store().get_account(&address(1)).unwrap();
        assert_eq!(after.head, child.hash);
        assert_eq!(after.block_count, before.block_count + 2);
        assert_eq!(after.total_brn_burned, before.total_brn_burned);
        assert_eq!(after.trst_balance, before.trst_balance);
    }
    drop(source.stop().await);
    let source = Running::start(prepared.source_config.clone(), prepared.now + 1).await;
    source.ready(prepared.epoch).await;
    assert_eq!(
        source.client.signing_state(&address(1)).await.unwrap().head,
        child.hash
    );
    assert_eq!(
        source.store.block_store().get_block(&child.hash).unwrap(),
        bincode::serialize(&child).unwrap()
    );
    source.stop().await;
    target.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protocol_epoch_crosses_peer_transport_and_restarts_with_owner_successor() {
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let prepared = prepare();
    let source = Running::start(prepared.source_config.clone(), prepared.now).await;
    source.ready(prepared.epoch).await;
    let target = Running::start(prepared.target_config.clone(), prepared.now).await;
    target.ready(prepared.epoch).await;
    let owner = address(1);
    let before = source.store.account_store().get_account(&owner).unwrap();
    let previous: StateBlock =
        bincode::deserialize(&source.store.block_store().get_block(&before.head).unwrap()).unwrap();
    let mut marker = previous.clone();
    marker.block_type = BlockType::Epoch;
    marker.previous = previous.hash;
    marker.signing_grant = previous.active_signing_grant();
    marker.payment_epoch = prepared.epoch;
    marker.params_hash = prepared.params.params_hash();
    marker.timestamp = Timestamp::new(prepared.now);
    marker.link = BlockHash::ZERO;
    marker.origin = TxHash::ZERO;
    marker.transaction = TxHash::ZERO;
    marker.target_head = BlockHash::ZERO;
    marker.merge_sources.clear();
    marker.release_sources.clear();
    marker.delegation_payload.clear();
    sign(0, &mut marker);
    let response = submit(&source, &serde_json::to_string(&marker).unwrap()).await;
    assert!(response.accepted, "creator marker rejected: {response:?}");
    stored(&source, marker.hash).await;
    stored(&target, marker.hash).await;
    for node in [&source, &target] {
        let after = node.store.account_store().get_account(&owner).unwrap();
        assert_eq!(after.epoch, marker.version);
        assert_eq!(after.head, marker.hash);
        assert_eq!(after.block_count, before.block_count + 1);
        assert_eq!(after.representative, before.representative);
        assert_eq!(after.state, before.state);
        assert_eq!(after.verified_at, before.verified_at);
        assert_eq!(after.trst_balance, before.trst_balance);
        assert_eq!(after.total_brn_burned, before.total_brn_burned);
        assert_eq!(after.total_brn_staked, before.total_brn_staked);
    }
    drop(target.stop().await);
    let target = Running::start(prepared.target_config.clone(), prepared.now + 1).await;
    target.ready(prepared.epoch).await;
    let state = target.client.signing_state(&owner).await.unwrap();
    assert_eq!(state.head, marker.hash);
    assert_eq!(state.signing_grant, marker.signing_grant);
    assert_eq!(
        target
            .store
            .account_store()
            .get_account(&owner)
            .unwrap()
            .epoch,
        marker.version
    );

    let mut successor = marker.clone();
    successor.block_type = BlockType::ChangeRepresentative;
    successor.previous = marker.hash;
    successor.timestamp = Timestamp::new(prepared.now + 1);
    successor.representative = address(2);
    sign(1, &mut successor);
    let response = submit(&target, &serde_json::to_string(&successor).unwrap()).await;
    assert!(response.accepted, "owner successor rejected: {response:?}");
    stored(&target, successor.hash).await;
    stored(&source, successor.hash).await;
    assert_eq!(
        target
            .store
            .account_store()
            .get_account(&owner)
            .unwrap()
            .representative,
        address(2)
    );
    drop(target.stop().await);
    let target = Running::start(prepared.target_config.clone(), prepared.now + 2).await;
    target.ready(prepared.epoch).await;
    assert_eq!(
        target.client.signing_state(&owner).await.unwrap().head,
        successor.hash
    );
    assert_eq!(
        target
            .store
            .account_store()
            .get_account(&owner)
            .unwrap()
            .epoch,
        marker.version
    );
    target.stop().await;
    source.stop().await;
}
