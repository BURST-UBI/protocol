use super::*;

fn economic_state(
    store: &burst_store_lmdb::LmdbStore,
    addresses: &[WalletAddress],
    source: TxHash,
) -> serde_json::Value {
    let accounts: Vec<_> = addresses
        .iter()
        .map(|address| store.account_store().get_account(address).unwrap())
        .collect();
    let pending: Vec<_> = addresses
        .iter()
        .map(
            |address| match store.pending_store().get_pending(address, &source) {
                Ok(entry) => Some(entry),
                Err(burst_store::StoreError::NotFound(_)) => None,
                Err(error) => panic!("pending lookup failed: {error}"),
            },
        )
        .collect();
    json!({
        "epoch": store.meta_store().payment_epoch().unwrap(),
        "accounts": accounts,
        "pending": pending,
    })
}

pub(super) async fn advance(
    registries: &[partition::Registry],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    clock: &AtomicU64,
    now: u64,
    addresses: &[WalletAddress],
    source: TxHash,
) {
    let partition = partition::Partition::split(registries, 2).await;
    let before: Vec<_> = stores
        .iter()
        .map(|store| economic_state(store, addresses, source))
        .collect();
    clock.store(now, Ordering::SeqCst);
    let until = tokio::time::Instant::now() + Duration::from_secs(6);
    while tokio::time::Instant::now() < until {
        for (index, store) in stores.iter().enumerate() {
            assert_eq!(
                economic_state(store, addresses, source),
                before[index],
                "node {index} changed economic state without a governance quorum at {now}"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    partition.heal().await;
}
