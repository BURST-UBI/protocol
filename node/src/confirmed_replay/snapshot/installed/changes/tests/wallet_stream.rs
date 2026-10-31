use super::*;
use burst_types::{NetworkId, ProtocolParams, WalletAddress};
use std::time::Instant;

#[test]
fn streamed_wallet_projection_round_trips_without_indexing_proofs() {
    wallet_projection_case(1024, false);
}

#[test]
#[ignore = "large-history projection diagnostic; run on the capped validation host"]
fn streamed_wallet_projection_scaling() {
    wallet_projection_case(100_000, true);
}

fn wallet_projection_case(releases: usize, report: bool) {
    let started = Instant::now();
    let mut state = crate::confirmed_replay::snapshot::SnapshotContinuation::from_genesis(
        NetworkId::Dev,
        &ProtocolParams::default(),
    )
    .unwrap()
    .into_state();
    let addresses = [
        WalletAddress::new("brst_stream_first"),
        WalletAddress::new("brst_stream_second"),
    ];
    let mut wallet = burst_brn::BrnWalletState::new(Timestamp::EPOCH);
    for id in 1..=releases {
        let mut hash = [0; 32];
        hash[..8].copy_from_slice(&(id as u64).to_be_bytes());
        wallet
            .record_unproven_release(BlockHash::new(hash), 1, Timestamp::new(10))
            .unwrap();
    }
    for address in &addresses {
        state.brn.track_wallet(address.clone(), wallet.clone());
    }
    let construction_ms = started.elapsed().as_secs_f64() * 1000.0;
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 128 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    for account in state.accounts.values() {
        batch.put_account_info(account, false).unwrap();
    }
    crate::ledger_updater::persist_brn(&mut batch, &state.brn).unwrap();
    batch.commit().unwrap();
    let at = Timestamp::new(100);
    let mut partial =
        Rows::capture_projection_records(&state, at, dir.path(), false, true, true).unwrap();
    partial.source_wallet_records = false;
    let mut batch = store.tx_begin_write().unwrap();
    assert!(partial
        .stage_caches(&mut batch)
        .unwrap_err()
        .to_string()
        .contains("partial wallet"));
    assert!(!batch.uses_brn_wallet_records().unwrap());
    drop(batch);

    let started = Instant::now();
    let mut rows =
        Rows::capture_projection_records(&state, at, dir.path(), true, true, true).unwrap();
    rows.source_wallet_records = false;
    let capture_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert!(rows.complete_wallets);
    let mut counted_bytes = 0usize;
    rows.values
        .visit(|table, key, value| {
            assert!(table != UndoTable::BrnWallets);
            assert!(
                !(table == UndoTable::BrnMeta
                    && key.starts_with(burst_brn::wallet_storage::ROW_PREFIX))
            );
            counted_bytes += key.len() + value.len();
            Ok(())
        })
        .unwrap();
    let mut wallet_rows = 0usize;
    for (address, wallet) in &state.brn.wallets {
        recorded::visit_wallet_delta(address, None, Some(wallet), |change| {
            counted_bytes += change.key.len() + change.after.unwrap().len();
            wallet_rows += 1;
            Ok(())
        })
        .unwrap();
    }
    assert_eq!(
        counted_bytes, rows.bytes,
        "streaming must retain every physical byte charge"
    );
    assert!(rows.bytes < MAX_BYTES);
    assert!(rows.values.len() + rows.wallets.len() < MAX_ROWS);
    if report {
        assert!(
            wallet_rows > MAX_ROWS,
            "fixture must cross the old physical-row regression"
        );
    }
    let indexed_rows = rows.values.len();
    let physical_bytes = rows.bytes;
    let mut stage_ms = Vec::new();
    for commit in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        let started = Instant::now();
        rows.stage_caches(&mut batch).unwrap();
        stage_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        let restored = batch.read_brn_engine(MAX_ROWS, MAX_BYTES).unwrap();
        assert_eq!(restored.wallets, state.brn.wallets);
        for address in &addresses {
            let expected = state.brn.get_wallet(address).unwrap();
            let actual = restored.get_wallet(address).unwrap();
            let (key, proof) = expected.pending_releases.iter().next().unwrap();
            assert!(
                std::ptr::eq(proof, actual.pending_releases.get(key).unwrap()),
                "projection publication rebuilt an authenticated proof"
            );
        }
        if commit {
            batch.commit().unwrap();
        } else {
            drop(batch);
            let batch = store.tx_begin_write().unwrap();
            assert!(!batch.uses_brn_wallet_records().unwrap());
            assert_eq!(
                batch.read_brn_engine(MAX_ROWS, MAX_BYTES).unwrap().wallets,
                state.brn.wallets
            );
        }
    }
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 128 << 20).unwrap();
    let started = Instant::now();
    let restored = burst_brn::BrnEngine::load_from_store(&store.brn_store()).unwrap();
    let cold_read_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(restored.wallets, state.brn.wallets);
    assert_eq!(restored.rate_history, state.brn.rate_history);
    assert_eq!(restored.next_stake_id(), state.brn.next_stake_id());
    if report {
        println!(
            "wallet_projection_streaming {}",
            serde_json::json!({
                "releases_per_wallet": releases, "wallets": state.brn.wallets.len(),
                "indexed_rows": indexed_rows, "physical_wallet_rows": wallet_rows,
                "physical_bytes": physical_bytes, "construction_ms": construction_ms,
                "capture_ms": capture_ms, "stage_ms": stage_ms, "cold_read_ms": cold_read_ms,
            })
        );
    }
}
