use super::*;
use crate::checkpoint_store::record_storage;
use burst_store_lmdb::LmdbStore;
use burst_types::{BlockHash, NetworkId, PublicKey};

pub(super) fn record(large_envelope: bool) -> Vec<u8> {
    record_with_heads(large_envelope, 65)
}

pub(super) fn record_with_heads(large_envelope: bool, count: u64) -> Vec<u8> {
    use burst_consensus::checkpoint::{Checkpoint, CheckpointCertificate, CheckpointSignature};
    let heads: Vec<_> = (0u64..count)
        .map(|index| {
            let mut key = [0; 32];
            key[24..].copy_from_slice(&index.to_be_bytes());
            crate::confirmed_history::HistoryHead {
                account: burst_crypto::derive_address(&PublicKey(key)),
                height: 1,
                hash: BlockHash::new([7; 32]),
            }
        })
        .collect();
    let checkpoint = Checkpoint {
        network: NetworkId::Dev,
        genesis: BlockHash::new([1; 32]),
        parent: BlockHash::new([2; 32]),
        height: 1,
        cutoff: 1,
        ledger_root: crate::confirmed_history::head_commitment(
            NetworkId::Dev,
            BlockHash::new([1; 32]),
            1,
            &heads,
        )
        .unwrap(),
        params_hash: BlockHash::new([3; 32]),
        next_roster: BlockHash::new([4; 32]),
        selection_root: BlockHash::new([5; 32]),
        next_quorum_bps: 6667,
    };
    let signatures = if large_envelope {
        vec![
            CheckpointSignature {
                representative: heads[0].account.clone(),
                signature: burst_types::Signature([0; 64])
            };
            80
        ]
    } else {
        Vec::new()
    };
    let certificate = CheckpointCertificate {
        checkpoint: checkpoint.clone(),
        round: 0,
        signatures,
    };
    // Storage fixtures exercise exact bytes, not signature or economic authority.
    crate::checkpoint_store::chain::encode_governance_manifest(
        &certificate,
        &crate::checkpoint_governance::GovernanceManifest {
            checkpoint,
            heads,
            proposals: if large_envelope {
                vec![burst_types::TxHash::new([9; 32]); 500]
            } else {
                vec![burst_types::TxHash::new([9; 32])]
            },
        },
    )
    .unwrap()
}

#[test]
fn shared_record_preserves_legacy_bytes_abort_reopen_and_large_envelopes() {
    for paged in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let key = "checkpoint_chain/v1/dev/test/1";
        let bytes = record(true);
        assert!(prepare(&bytes).unwrap().is_some());
        let mut batch = store.tx_begin_write().unwrap();
        if paged {
            crate::paged_metadata::write_bytes_at(
                record_storage::RecordLocation::new(key),
                &mut batch,
                &bytes,
            )
            .unwrap();
        } else {
            batch.put_meta(key, &bytes).unwrap();
        }
        batch.commit().unwrap();
        {
            let mut aborted = store.tx_begin_write().unwrap();
            assert!(record_storage::normalize(&mut aborted, key).unwrap());
            assert!(is_shared(&aborted.get_meta(key).unwrap().unwrap()).unwrap());
            assert_eq!(
                record_storage::get_batch(&aborted, key).unwrap().unwrap(),
                bytes
            );
        }
        let mut batch = store.tx_begin_write().unwrap();
        assert!(!is_shared(&batch.get_meta(key).unwrap().unwrap()).unwrap());
        assert!(record_storage::normalize(&mut batch, key).unwrap());
        assert!(!record_storage::normalize(&mut batch, key).unwrap());
        assert_eq!(
            record_storage::matches_batch(&batch, key, &bytes).unwrap(),
            Some(true)
        );
        assert_eq!(
            record_storage::matches_batch(&batch, key, b"different").unwrap(),
            Some(false)
        );
        batch.commit().unwrap();
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let txn = store.tx_begin_read().unwrap();
        assert_eq!(
            record_storage::get_txn(&store.meta_store(), &txn, key)
                .unwrap()
                .unwrap(),
            bytes
        );
        let descriptor = store.meta_store().get_meta_txn(&txn, key).unwrap().unwrap();
        let commitment = record_storage::inspect_stored(
            key,
            &descriptor,
            |name| store.meta_store().get_meta_txn(&txn, name),
            |hash| store.read_view(&txn)?.checkpoint_head_node(hash),
            bytes.len(),
        )
        .unwrap();
        assert_eq!(commitment.size, bytes.len());
        assert_eq!(commitment.hash, burst_crypto::blake2b_256(&bytes));
    }
}

#[test]
fn shared_archive_roots_survive_move_replacement_and_pinned_readers() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let active = "checkpoint_chain/v1/dev/test/1";
    let archived = "checkpoint_record_archive/v1/dev/test/1";
    let bytes = record(false);
    assert!(prepare(&bytes).unwrap().is_some());
    let mut batch = store.tx_begin_write().unwrap();
    record_storage::put(&mut batch, active, &bytes).unwrap();
    batch.commit().unwrap();
    let pinned = store.tx_begin_read().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    record_storage::put(&mut batch, archived, &bytes).unwrap();
    assert!(is_shared(&batch.get_meta(active).unwrap().unwrap()).unwrap());
    assert!(is_shared(&batch.get_meta(archived).unwrap().unwrap()).unwrap());
    record_storage::delete(&mut batch, active).unwrap();
    batch.commit().unwrap();
    assert_eq!(
        record_storage::get_txn(&store.meta_store(), &pinned, active)
            .unwrap()
            .unwrap(),
        bytes
    );
    let mut batch = store.tx_begin_write().unwrap();
    assert_eq!(
        record_storage::get_batch(&batch, archived)
            .unwrap()
            .unwrap(),
        bytes
    );
    record_storage::put(&mut batch, archived, b"small").unwrap();
    batch.commit().unwrap();
    drop(pinned);
    loop {
        let mut batch = store.tx_begin_write().unwrap();
        let page = batch.reclaim_checkpoint_heads(7).unwrap();
        assert!(page.nodes <= 7);
        batch.commit().unwrap();
        if page.complete {
            break;
        }
    }
    let batch = store.tx_begin_write().unwrap();
    assert_eq!(
        record_storage::get_batch(&batch, archived)
            .unwrap()
            .unwrap(),
        b"small"
    );
    assert!(record_storage::get_batch(&batch, active).unwrap().is_none());
}

#[test]
fn shared_descriptor_and_noncanonical_tree_damage_cannot_hide_behind_comparison() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let key = "checkpoint_chain/v1/dev/test/1";
    let bytes = record(false);
    assert!(prepare(&bytes).unwrap().is_some());
    let mut batch = store.tx_begin_write().unwrap();
    record_storage::put(&mut batch, key, &bytes).unwrap();
    batch.commit().unwrap();
    for noncanonical in [false, true] {
        let mut batch = store.tx_begin_write().unwrap();
        let mut stored = batch.get_meta(key).unwrap().unwrap();
        assert!(
            is_shared(&stored).unwrap(),
            "corruption fixture must use shared storage"
        );
        if noncanonical {
            let header = Header::decode(&stored, bytes.len()).unwrap();
            let root = batch
                .read_view()
                .checkpoint_head_node(&header.root)
                .unwrap()
                .unwrap();
            let HeadNode::Branch { bit, left, right } = root else {
                panic!("expected branch");
            };
            assert!(bit > 0);
            let changed = batch
                .intern_checkpoint_head(&HeadNode::Branch {
                    bit: 0,
                    left,
                    right,
                })
                .unwrap();
            batch.retain_checkpoint_head(&changed).unwrap();
            batch.release_checkpoint_head(&header.root).unwrap();
            stored[56..88].copy_from_slice(&changed);
        } else {
            stored[24] ^= 1;
        }
        batch.put_meta(key, &stored).unwrap();
        assert!(record_storage::get_batch(&batch, key).is_err());
        assert!(record_storage::matches_batch(&batch, key, b"already different").is_err());
        assert!(
            record_storage::delete(&mut batch, key).is_err(),
            "noncanonical={noncanonical}"
        );
        // Discard the caller-owned writer after the failed integrity check.
    }
    let batch = store.tx_begin_write().unwrap();
    let stored = batch.get_meta(key).unwrap().unwrap();
    assert!(visit(
        key,
        &stored,
        |_| panic!("budget must reject before storage"),
        |_| panic!("no nodes"),
        bytes.len() - 1,
        |_| Ok(())
    )
    .is_err());
    assert!(visit(
        key,
        &stored,
        |name| batch.get_meta(name),
        |_| Ok(None),
        bytes.len(),
        |_| Ok(())
    )
    .is_err());
    assert!(visit(
        key,
        &stored,
        |name| {
            let mut value = batch.get_meta(name)?;
            if name.starts_with("checkpoint_heads/envelope-pages/") {
                value.as_mut().unwrap()[0] ^= 1;
            }
            Ok(value)
        },
        |hash| batch.read_view().checkpoint_head_node(hash),
        bytes.len(),
        |_| Ok(())
    )
    .is_err());
    assert_eq!(
        record_storage::get_batch(&batch, key).unwrap().unwrap(),
        bytes
    );
}

#[test]
#[ignore = "capped-host shared-node LMDB footprint on retained logical records; not a ledger acceptance run"]
fn historical_record_storage_probe() {
    let paths = super::super::history_reuse::record_paths();
    if let Some(path) = std::env::var_os("BURST_RECORD_STORAGE_MIGRATE") {
        // The driver supplies a disposable copy of the retained legacy database.
        let path = std::path::PathBuf::from(path);
        let store = LmdbStore::open(&path, 32, 256 << 20).unwrap();
        let all = std::env::var_os("BURST_RECORD_MIGRATION_ALL").is_some();
        let mut selected: Vec<_> = if all {
            paths.iter().collect()
        } else {
            vec![paths
                .iter()
                .max_by_key(|(_, path)| std::fs::metadata(path).unwrap().len())
                .unwrap()]
        };
        // Match the metadata cursor's lexical archive-key order.
        selected.sort_by_key(|(height, _)| height.to_string());
        let old_live = store.env().non_free_pages_size().unwrap();
        let old_extent = store.env().real_disk_size().unwrap();
        let mut peak_extent = old_extent;
        let mut max_boundary_live = old_live;
        let mut worker = migration::Worker::default();
        let shutdown = crate::shutdown::ShutdownController::new();
        let mut steps = 0usize;
        let mut migrated = 0usize;
        let mut already_shared = 0usize;
        let mut unsupported = 0usize;
        let started_all = std::time::Instant::now();
        let max_steps = (crate::confirmed_history::MAX_BLOCKS as usize)
            .div_ceil(migration::HEAD_ROWS_PER_BATCH)
            + 2 * (MAX_RECORD_BYTES as usize)
                .div_ceil(crate::paged_metadata::PAGE_BYTES)
                .div_ceil(crate::paged_metadata::staged::PAGES_PER_BATCH)
            + 8;
        for (height, original) in selected {
            let expected = std::fs::read(original).unwrap();
            let key = format!("checkpoint_record_archive/v1/dev/footprint/{height}");
            let shared = {
                let reader = store.tx_begin_read().unwrap();
                is_shared(
                    &store
                        .meta_store()
                        .get_meta_txn(&reader, &key)
                        .unwrap()
                        .unwrap(),
                )
                .unwrap()
            };
            if shared {
                already_shared += 1;
                println!("HISTORY_MIGRATION_SKIP height={height} reason=already_shared");
                continue;
            }
            let eligible = prepare(&expected).unwrap().is_some();
            let before = std::time::Instant::now();
            let selected = worker.select(&store, &key).unwrap();
            assert_eq!(
                selected, eligible,
                "selection must not silently skip supported records"
            );
            if !selected {
                unsupported += 1;
                println!("HISTORY_MIGRATION_SKIP height={height} reason=unsupported_or_small");
                continue;
            }
            println!(
                "HISTORY_MIGRATION_SELECT height={height} logical_bytes={} elapsed_us={}",
                expected.len(),
                before.elapsed().as_micros()
            );
            let mut completed = false;
            let before = std::time::Instant::now();
            for step in 0..max_steps {
                let started = std::time::Instant::now();
                let done = worker.step(&store, &shutdown).unwrap();
                println!(
                    "HISTORY_MIGRATION_STEP height={height} index={step} done={done} elapsed_us={}",
                    started.elapsed().as_micros()
                );
                steps += 1;
                // File extent is cheap to inspect every batch. Live-page accounting
                // scans freelist state, so measure it only at record boundaries.
                peak_extent = peak_extent.max(store.env().real_disk_size().unwrap());
                if done {
                    completed = true;
                    break;
                }
            }
            assert!(
                completed,
                "supported record must finish within its row/page batch bounds"
            );
            {
                let reader = store.tx_begin_read().unwrap();
                assert!(
                    is_shared(
                        &store
                            .meta_store()
                            .get_meta_txn(&reader, &key)
                            .unwrap()
                            .unwrap()
                    )
                    .unwrap(),
                    "completed job must publish the supported record"
                );
            }
            migrated += 1;
            let live = store.env().non_free_pages_size().unwrap();
            max_boundary_live = max_boundary_live.max(live);
            println!("HISTORY_MIGRATION_RECORD height={height} elapsed_us={} live={live} extent={} peak_extent={peak_extent}",
                before.elapsed().as_micros(), store.env().real_disk_size().unwrap());
        }
        let new_live = store.env().non_free_pages_size().unwrap();
        println!("HISTORY_MIGRATION_COMPLETE steps={steps} elapsed_us={} old_live={old_live} old_extent={old_extent} new_live={new_live} new_extent={} peak_extent={peak_extent} max_record_boundary_live={max_boundary_live}",
            started_all.elapsed().as_micros(), store.env().real_disk_size().unwrap());
        if all {
            assert_eq!(migrated + already_shared + unsupported, paths.len());
            // Compare against the fixture's fixed legacy live-page baseline.
            assert!(
                new_live < 75_509_760,
                "completed archive must use fewer live pages than its original legacy fixture"
            );
        }
        drop(worker);
        drop(store);
        let store = LmdbStore::open(&path, 32, 256 << 20).unwrap();
        let reader = store.tx_begin_read().unwrap();
        let mut logical = 0;
        for (height, path) in paths {
            let key = format!("checkpoint_record_archive/v1/dev/footprint/{height}");
            let expected = std::fs::read(path).unwrap();
            assert_eq!(
                record_storage::get_txn(&store.meta_store(), &reader, &key)
                    .unwrap()
                    .unwrap(),
                expected
            );
            logical += expected.len();
        }
        assert_eq!(logical, 70_833_688);
        println!("HISTORY_MIGRATION_SCOPE records_migrated={migrated} already_shared={already_shared} unsupported={unsupported} all_requested={all} all18_exact=true reopen=true release=false sustained_service=false");
        return;
    }
    if let Some(path) = std::env::var_os("BURST_RECORD_STORAGE_REOPEN") {
        // The caller supplies a disposable copy; store open may initialize metadata.
        let store = LmdbStore::open(&std::path::PathBuf::from(path), 32, 256 << 20).unwrap();
        let reader = store.tx_begin_read().unwrap();
        let started = std::time::Instant::now();
        let mut logical = 0usize;
        for (height, path) in paths {
            let expected = std::fs::read(path).unwrap();
            let key = format!("checkpoint_record_archive/v1/dev/footprint/{height}");
            let before = std::time::Instant::now();
            let bytes = record_storage::get_txn(&store.meta_store(), &reader, &key)
                .unwrap()
                .unwrap();
            let read_ms = before.elapsed().as_millis();
            assert_eq!(bytes, expected);
            logical += bytes.len();
            println!(
                "HISTORY_STORAGE_REOPEN height={height} logical_bytes={} read_ms={read_ms}",
                bytes.len()
            );
        }
        assert_eq!(logical, 70_833_688);
        println!("HISTORY_STORAGE_REOPEN_COMPLETE logical_bytes={logical} elapsed_ms={} records_written=0",
            started.elapsed().as_millis());
        return;
    }
    let legacy_dir = tempfile::tempdir().unwrap();
    let shared_dir = tempfile::tempdir().unwrap();
    // Keep the actual databases for follow-up page diagnosis when requested.
    let (legacy_path, shared_path) =
        if let Some(output) = std::env::var_os("BURST_RECORD_STORAGE_OUTPUT") {
            let output = std::path::PathBuf::from(output);
            std::fs::create_dir(&output).unwrap(); // Refuse to overwrite prior evidence.
            let old = tempfile::Builder::new()
                .prefix("legacy-")
                .tempdir_in(&output)
                .unwrap()
                .keep();
            let new = tempfile::Builder::new()
                .prefix("shared-")
                .tempdir_in(&output)
                .unwrap()
                .keep();
            println!(
                "HISTORY_STORAGE_FIXTURES legacy={} shared={}",
                old.display(),
                new.display()
            );
            (old, new)
        } else {
            (legacy_dir.path().to_owned(), shared_dir.path().to_owned())
        };
    let legacy = LmdbStore::open(&legacy_path, 32, 256 << 20).unwrap();
    let shared = LmdbStore::open(&shared_path, 32, 256 << 20).unwrap();
    let started = std::time::Instant::now();
    let mut logical = 0usize;
    for (height, path) in &paths {
        let bytes = std::fs::read(path).unwrap();
        assert!(bytes.len() <= MAX_RECORD_BYTES as usize);
        let key = format!("checkpoint_record_archive/v1/dev/footprint/{height}");
        let legacy_before = std::time::Instant::now();
        let mut batch = legacy.tx_begin_write().unwrap();
        crate::paged_metadata::write_bytes_at(
            record_storage::RecordLocation::new(&key),
            &mut batch,
            &bytes,
        )
        .unwrap();
        batch.commit().unwrap();
        let legacy_ms = legacy_before.elapsed().as_millis();
        let before = std::time::Instant::now();
        let mut batch = shared.tx_begin_write().unwrap();
        record_storage::put(&mut batch, &key, &bytes).unwrap();
        let put_ms = before.elapsed().as_millis();
        let read_before = std::time::Instant::now();
        // Small first records may remain inline; all representations emit exact original bytes.
        assert_eq!(
            record_storage::get_batch(&batch, &key).unwrap().unwrap(),
            bytes
        );
        let read_ms = read_before.elapsed().as_millis();
        let commit_before = std::time::Instant::now();
        batch.commit().unwrap();
        let commit_ms = commit_before.elapsed().as_millis();
        let combined_ms = before.elapsed().as_millis();
        logical += bytes.len();
        println!("HISTORY_STORAGE_ROW height={height} logical_bytes={} shared_write_read_commit_ms={} elapsed_ms={}",
            bytes.len(), combined_ms, started.elapsed().as_millis());
        println!("HISTORY_STORAGE_STAGES height={height} legacy_write_commit_ms={legacy_ms} shared_put_ms={put_ms} shared_read_ms={read_ms} shared_commit_ms={commit_ms}");
        // These calls open and close their own read views after the timed writer.
        println!("HISTORY_STORAGE_PAGES height={height} legacy_live={} legacy_extent={} shared_live={} shared_extent={}",
            legacy.env().non_free_pages_size().unwrap(), legacy.env().real_disk_size().unwrap(),
            shared.env().non_free_pages_size().unwrap(), shared.env().real_disk_size().unwrap());
    }
    assert_eq!(logical, 70_833_688);
    let old_live = legacy.env().non_free_pages_size().unwrap();
    let new_live = shared.env().non_free_pages_size().unwrap();
    let old_extent = legacy.env().real_disk_size().unwrap();
    let new_extent = shared.env().real_disk_size().unwrap();
    println!("HISTORY_STORAGE_SIZE logical_bytes={logical} legacy_live_bytes={old_live} shared_live_bytes={new_live} legacy_extent_bytes={old_extent} shared_extent_bytes={new_extent}");
    assert!(
        new_live < old_live,
        "shared layout must reduce actual live LMDB pages on this fixture"
    );
    drop(legacy);
    drop(shared);
    let shared = LmdbStore::open(&shared_path, 32, 256 << 20).unwrap();
    let reader = shared.tx_begin_read().unwrap();
    for (height, path) in paths {
        let key = format!("checkpoint_record_archive/v1/dev/footprint/{height}");
        assert_eq!(
            record_storage::get_txn(&shared.meta_store(), &reader, &key)
                .unwrap()
                .unwrap(),
            std::fs::read(path).unwrap()
        );
    }
    println!("HISTORY_STORAGE_SCOPE exact_record_bytes=true reopen=true signature_authentication=false replay=false startup=false release=false");
}
