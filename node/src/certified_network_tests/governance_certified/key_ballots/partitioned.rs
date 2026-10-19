use super::*;

pub(super) async fn ballot(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    registries: &[partition::Registry],
    signed: &StateBlock,
    revocation: Option<&StateBlock>,
) {
    let revocation_heights: Vec<_> = stores
        .iter()
        .map(|store| {
            revocation.map(|block| {
                store
                    .account_store()
                    .get_account(&block.account)
                    .unwrap()
                    .confirmation_height
            })
        })
        .collect();
    let before: Vec<_> = stores
        .iter()
        .map(|store| {
            (
                store
                    .account_store()
                    .get_account(&signed.account)
                    .unwrap()
                    .confirmation_height,
                store.meta_store().payment_epoch().unwrap(),
            )
        })
        .collect();
    let split = partition::Partition::split(registries, 2).await;
    let result = clients[0]
        .process(&serde_json::to_string(signed).unwrap())
        .await
        .unwrap();
    assert!(
        result.accepted,
        "delegated ballot admission failed: {result:?}"
    );
    if let Some(revocation) = revocation {
        assert_ne!(revocation.account, signed.account);
        let result = clients[2]
            .process(&serde_json::to_string(revocation).unwrap())
            .await
            .unwrap();
        assert!(
            result.accepted,
            "owner revocation admission failed: {result:?}"
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            while !stores[1].block_store().exists(&signed.hash).unwrap() {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("ballot did not reach its partition peer");
        assert!(stores[2].block_store().exists(&revocation.hash).unwrap());
    }
    let until = tokio::time::Instant::now() + Duration::from_secs(6);
    while tokio::time::Instant::now() < until {
        for (index, store) in stores.iter().enumerate() {
            if let Some(revocation) = revocation {
                assert_eq!(
                    store
                        .account_store()
                        .get_account(&revocation.account)
                        .unwrap()
                        .confirmation_height,
                    revocation_heights[index].unwrap(),
                    "node {index} cemented a revocation without quorum"
                );
                assert!(
                    !store
                        .block_store()
                        .exists(&if index < 2 {
                            revocation.hash
                        } else {
                            signed.hash
                        })
                        .unwrap(),
                    "partition did not preserve opposite arrival orders"
                );
            }
            assert_eq!(
                store
                    .account_store()
                    .get_account(&signed.account)
                    .unwrap()
                    .confirmation_height,
                before[index].0,
                "node {index} cemented a secondary ballot without quorum"
            );
            assert_eq!(store.meta_store().payment_epoch().unwrap(), before[index].1);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    split.heal().await;
    for client in clients {
        client
            .wait_confirmed(&signed.hash.to_string(), Duration::from_secs(25))
            .await
            .unwrap();
        if let Some(revocation) = revocation {
            client
                .wait_confirmed(&revocation.hash.to_string(), Duration::from_secs(25))
                .await
                .unwrap();
        }
    }
}

pub(super) async fn count(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    registries: &[partition::Registry],
    clock: &AtomicU64,
    cutoff: u64,
    late: &StateBlock,
) {
    let epochs: Vec<_> = stores
        .iter()
        .map(|store| store.meta_store().payment_epoch().unwrap())
        .collect();
    let split = partition::Partition::split(registries, 2).await;
    clock.store(cutoff, Ordering::SeqCst);
    let confirmation_height = stores[2]
        .account_store()
        .get_account(&late.account)
        .unwrap()
        .confirmation_height;
    let result = clients[2]
        .process(&serde_json::to_string(late).unwrap())
        .await
        .unwrap();
    assert!(
        result.accepted,
        "boundary revocation admission failed: {result:?}"
    );
    let until = tokio::time::Instant::now() + Duration::from_secs(6);
    while tokio::time::Instant::now() < until {
        assert_eq!(
            stores[2]
                .account_store()
                .get_account(&late.account)
                .unwrap()
                .confirmation_height,
            confirmation_height,
            "old-epoch boundary revocation finalized before counting"
        );
        for (index, store) in stores.iter().enumerate() {
            assert_eq!(
                store.meta_store().payment_epoch().unwrap(),
                epochs[index],
                "node {index} installed a delegated tally without quorum"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    split.heal().await;
}
