use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn certified_recovery_installs_merge_rows_and_rewinds_matching_journals() {
    for (row_format, migrate, prune) in [
        (0, false, false),
        (1, false, false),
        (2, false, false),
        (0, true, false),
        (1, true, false),
        (2, false, true),
    ] {
        merge_recovery_case(row_format, migrate, prune, 1, false, false).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "capped-host signed multi-parent closure and retirement resource probe"]
async fn signed_merge_graph_closure_resource_probe() {
    let limit = merge_recovery_case(2, false, true, 256, false, false)
        .await
        .unwrap();
    assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
    eprintln!("MERGE_GRAPH owners_dropped scratch_bytes=0");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn signed_payments_preserve_owned_retirement_across_abort_commit_and_recovery() {
    merge_recovery_case(2, false, true, 1, true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "capped-host periodic retirement with a fixed signed arrival workload"]
async fn signed_payments_with_periodic_retirement_resource_probe() {
    let limit = merge_recovery_case(2, false, true, 16, false, true)
        .await
        .unwrap();
    assert_eq!(limit.used_temporary_bytes().unwrap(), 0);
}

async fn merge_recovery_case(
    row_format: u8,
    migrate: bool,
    prune: bool,
    groups: usize,
    observe: bool,
    periodic: bool,
) -> Option<crate::confirmed_replay::scratch_file::ScratchLimit> {
    assert!(groups > 0);
    let baseline = std::env::var_os("BURST_MERGE_GRAPH_BASELINE").map(std::path::PathBuf::from);
    if baseline.is_some() {
        assert!(
            row_format == 2 && !migrate && prune && groups == 256 && !observe && !periodic,
            "retained graph baseline belongs only to the256-group resource probe"
        );
    }
    let started = std::time::Instant::now();
    let (mut fixture, _source_dir, source, manifest, certificate) = if prune {
        let mut fixture = Fixture::selection();
        for offset in 0..if periodic { 128 } else { 4 } {
            fixture.push(
                0,
                BlockType::ChangeRepresentative,
                1_595_431_120 + offset,
                |_| {},
            );
        }
        selection::setup_fixture(fixture)
    } else {
        selection::setup()
    };
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let expected =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    let (_local_dir, local) = cemented::sparse(&expected, &source);
    let resource_limit = (groups > 1).then(|| {
        crate::confirmed_replay::scratch_file::ScratchLimit::acquire_reserved(
            local.env().path(),
            64 << 20,
            crate::confirmed_replay::snapshot::history::DurableHistoryInputs::reservation(16 << 20)
                .unwrap(),
        )
        .unwrap()
    });
    copy_installed_records(&local, &source, &fixture, &bytes);
    if prune {
        let rows = source
            .block_store()
            .indexed_blocks_txn(&source.tx_begin_read().unwrap(), 131_072, 64 << 20)
            .unwrap();
        let mut batch = local.tx_begin_write().unwrap();
        for row in rows {
            if expected
                .state()
                .account(&row.account)
                .is_some_and(|account| row.height <= account.block_count)
            {
                batch
                    .put_block_with_account(&row.hash, &row.bytes, &row.account, row.height)
                    .unwrap();
            }
        }
        batch.commit().unwrap();
        normalize_retention_fixture(&local, &fixture.params);
    }
    let mut batch = local.tx_begin_write().unwrap();
    let original_rates = batch
        .read_brn_engine(100_000, 64 << 20)
        .unwrap()
        .rate_history;
    let legacy_rates = row_format == 0;
    if legacy_rates {
        for index in 0..original_rates.segments().len() {
            batch
                .delete_brn_meta(&burst_brn::rate_storage::row_key(index as u64))
                .unwrap();
        }
        batch
            .delete_brn_meta(burst_brn::rate_storage::HEADER)
            .unwrap();
        batch
            .put_brn_meta(
                b"rate_history",
                &bincode::serialize(&original_rates).unwrap(),
            )
            .unwrap();
    }
    assert_eq!(batch.uses_brn_rate_records().unwrap(), !legacy_rates);
    let original = crate::graph_storage::load(&batch).unwrap();
    let original_holdings = crate::trst_storage::load(&batch, fixture.params.trst_expiry_secs)
        .unwrap()
        .save_wallets();
    if row_format > 0 {
        crate::graph_storage::stage_rows_for_test(&mut batch, &original).unwrap();
        let holdings = crate::trst_storage::load(&batch, fixture.params.trst_expiry_secs).unwrap();
        if row_format == 2 {
            crate::trst_storage::stage_stable_rows_for_test(&mut batch, &holdings).unwrap();
        } else {
            crate::trst_storage::stage_rows_for_test(&mut batch, &holdings).unwrap();
        }
    }
    batch.commit().unwrap();
    let local = if let Some(baseline) = &baseline {
        let (version, saved_groups, params, epoch, at, edges): (
            u8,
            u64,
            ProtocolParams,
            BlockHash,
            u64,
            u64,
        ) = bincode::deserialize(&std::fs::read(baseline.join("merge-resource.bin")).unwrap())
            .unwrap();
        assert_eq!(version, 1);
        assert_eq!(saved_groups, groups as u64);
        assert_eq!(params, fixture.params);
        assert_eq!(epoch, expected.anchor().hash());
        assert_eq!(at, expected.anchor().cutoff() + 1);
        assert_eq!(edges, (groups * 7) as u64);
        assert!(!baseline
            .join(crate::confirmed_replay::scratch_file::PERSISTENT_INPUTS_DIRECTORY)
            .exists());
        drop(local);
        std::fs::copy(
            baseline.join("data.mdb"),
            _local_dir.path().join("data.mdb"),
        )
        .unwrap();
        LmdbStore::open(_local_dir.path(), 64, 128 << 20).unwrap()
    } else {
        local
    };
    let local = Arc::new(local);
    let source = Arc::new(source);
    let mut observed_retirement = if observe {
        assert!(prune && groups == 1);
        let service = WitnessService::start(Arc::clone(&source), Arc::clone(&local), 0).await;
        let phase = std::time::Instant::now();
        let checked = installed::recover_metadata_from_peers(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&service.registry),
        )
        .await
        .unwrap()
        .unwrap();
        eprintln!(
            "OWNED_RETIREMENT stage=authenticate elapsed_us={}",
            phase.elapsed().as_micros()
        );
        let owned = {
            let phase = std::time::Instant::now();
            let prepared = checked
                .prepare_token_indexes_at(&local, Timestamp::new(expected.anchor().cutoff()))
                .unwrap();
            eprintln!(
                "OWNED_RETIREMENT stage=projection elapsed_us={}",
                phase.elapsed().as_micros()
            );
            let phase = std::time::Instant::now();
            let closed = prepared
                .body_requirements(&local)
                .unwrap()
                .close(Default::default())
                .unwrap();
            eprintln!(
                "OWNED_RETIREMENT stage=closure elapsed_us={} bodies={} edges={} bytes={}",
                phase.elapsed().as_micros(),
                closed.work().bodies,
                closed.work().edges,
                closed.work().bytes
            );
            let phase = std::time::Instant::now();
            let owned = closed
                .own_retirement(Timestamp::new(expected.anchor().cutoff()), None, || false)
                .unwrap();
            eprintln!(
                "OWNED_RETIREMENT stage=catalogue elapsed_us={}",
                phase.elapsed().as_micros()
            );
            owned
        };
        service.stop().await;
        Some(owned)
    } else {
        None
    };
    let start = fixture.blocks.len();
    let at = expected.anchor().cutoff() + 1;
    let epoch = expected.anchor().hash();
    let timestamp = |offset| if groups == 1 { at + offset } else { at + 4 };
    let mut merged = BlockHash::ZERO;
    for _ in 0..groups {
        let burn = fixture.push(0, BlockType::Burn, timestamp(0), |block| {
            block.payment_epoch = epoch;
            block.link = destination(8);
            block.brn_spent += 1;
        });
        let first = fixture.push(8, BlockType::Receive, timestamp(1), |block| {
            block.payment_epoch = epoch;
            block.link = burn;
            block.trst_balance += 1;
        });
        let burn = fixture.push(0, BlockType::Burn, timestamp(2), |block| {
            block.payment_epoch = epoch;
            block.link = destination(8);
            block.brn_spent += 1;
        });
        let second = fixture.push(8, BlockType::Receive, timestamp(3), |block| {
            block.payment_epoch = epoch;
            block.link = burn;
            block.trst_balance += 1;
        });
        merged = fixture.push(8, BlockType::Merge, timestamp(4), |block| {
            block.payment_epoch = epoch;
            block.merge_sources = vec![first.into_tx_hash(), second.into_tx_hash()];
        });
    }
    assert_eq!(fixture.blocks.len() - start, groups * 5);
    // Merge sources are TxHash identities, not extra body-dependency edges.
    let suffix_edges: u64 = fixture.blocks[start..]
        .iter()
        .map(|block| block.dependencies().count() as u64)
        .sum();
    assert_eq!(suffix_edges, (groups * 7) as u64);
    if periodic {
        periodic_signed_arrivals(
            &local,
            &source,
            &fixture.params,
            epoch,
            &fixture.blocks[start..],
        )
        .await;
    }
    if baseline.is_some() {
        let reader = local.tx_begin_read().unwrap();
        for block in &fixture.blocks[start..] {
            let bytes = local
                .block_store()
                .get_block_txn(&reader, &block.hash)
                .unwrap();
            assert_eq!(
                bytes,
                bincode::serialize(block).unwrap(),
                "retained signed suffix differs"
            );
        }
        eprintln!(
            "MERGE_GRAPH reused_signed_suffix blocks={} edges={suffix_edges}",
            groups * 5
        );
    }
    for block in fixture.blocks[start..]
        .iter()
        .filter(|_| !periodic && baseline.is_none())
    {
        if let Some(owned) = observed_retirement.as_mut() {
            let before = local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap();
            let before_count = local.block_store().block_count().unwrap();
            let before_account = local
                .tx_begin_write()
                .unwrap()
                .get_account(&block.account)
                .unwrap();
            let mut aborted = local.tx_begin_write().unwrap();
            crate::payment_transition::apply(&mut aborted, block, &fixture.params, block.timestamp)
                .unwrap();
            drop(aborted);
            let phase = std::time::Instant::now();
            owned.reconcile(&local).unwrap();
            eprintln!(
                "OWNED_RETIREMENT stage=reconcile-after-abort elapsed_us={} block={}",
                phase.elapsed().as_micros(),
                block.hash
            );
            assert!(owned.is_current(&local).unwrap());
            assert_eq!(
                local
                    .tx_begin_write()
                    .unwrap()
                    .checkpoint_metadata(b"")
                    .unwrap(),
                before
            );
            assert_eq!(local.block_store().block_count().unwrap(), before_count);
            assert_eq!(
                local
                    .tx_begin_write()
                    .unwrap()
                    .get_account(&block.account)
                    .unwrap(),
                before_account
            );
            assert!(!local.block_store().exists(&block.hash).unwrap());
        }
        let phase = std::time::Instant::now();
        let mut batch = local.tx_begin_write().unwrap();
        crate::payment_transition::apply(&mut batch, block, &fixture.params, block.timestamp)
            .unwrap();
        batch.commit().unwrap();
        if observe {
            eprintln!(
                "OWNED_RETIREMENT stage=signed-apply-commit elapsed_us={} block={}",
                phase.elapsed().as_micros(),
                block.hash
            );
        }
    }
    if groups > 1 {
        let local_path = _local_dir.keep();
        let source_path = _source_dir.keep();
        std::fs::write(
            local_path.join("merge-resource.bin"),
            bincode::serialize(&(
                1u8,
                groups as u64,
                fixture.params.clone(),
                epoch,
                at,
                suffix_edges,
            ))
            .unwrap(),
        )
        .unwrap();
        if let Some(baseline) = &baseline {
            eprintln!(
                "MERGE_GRAPH reused_immutable_baseline={}",
                baseline.display()
            );
        } else {
            let baseline_path = tempfile::tempdir().unwrap().keep();
            std::fs::copy(local_path.join("data.mdb"), baseline_path.join("data.mdb")).unwrap();
            std::fs::copy(
                local_path.join("merge-resource.bin"),
                baseline_path.join("merge-resource.bin"),
            )
            .unwrap();
            eprintln!("MERGE_GRAPH immutable_baseline={}", baseline_path.display());
        }
        eprintln!("MERGE_GRAPH stage=applied groups={groups} blocks={} edges={suffix_edges} elapsed_ms={} local={} source={}",
            groups * 5, started.elapsed().as_millis(), local_path.display(), source_path.display());
    }
    let batch = local.tx_begin_write().unwrap();
    let graph = crate::graph_storage::load(&batch).unwrap();
    let holdings = crate::trst_storage::load(&batch, fixture.params.trst_expiry_secs)
        .unwrap()
        .save_wallets();
    assert_ne!(graph.to_bytes(), original.to_bytes());
    drop(batch);
    let service = WitnessService::start(Arc::clone(&source), Arc::clone(&local), 0).await;
    let mut checked = installed::recover_metadata_from_peers(
        Arc::clone(&local),
        NetworkId::Dev,
        fixture.params.clone(),
        Arc::clone(&service.registry),
    )
    .await
    .unwrap()
    .unwrap();
    if groups > 1 {
        eprintln!(
            "MERGE_GRAPH stage=recovered elapsed_ms={}",
            started.elapsed().as_millis()
        );
    }
    if groups == 1 && !observe && !periodic {
        // Force a distinct committed view so cancellation still exercises the
        // full validation rather than the recovery result's verified read cut.
        local
            .meta_store()
            .put_meta("test/read-guard-revision", b"next")
            .unwrap();
        let reader = local.tx_begin_read().unwrap();
        let view = local.read_view(&reader).unwrap();
        assert!(!checked.has_verified_view(&view).unwrap());
        let polls = std::cell::Cell::new(0);
        assert!(!checked
            .verify_view_until(&view, &|stage| {
                if stage != "metadata:inventory-row" {
                    return false;
                }
                polls.set(polls.get() + 1);
                polls.get() == 2
            })
            .unwrap());
        assert_eq!(polls.get(), 2);
        assert!(
            !checked.has_verified_view(&view).unwrap(),
            "partial verification entered cache"
        );
        checked.verify_view(&view).unwrap();
        assert!(checked.has_verified_view(&view).unwrap());
        let mut writer = local.tx_begin_write().unwrap();
        checked.verify_unchanged(&writer).unwrap();
        let original_params = writer
            .undo_value_ref(
                burst_store_lmdb::block_undo::UndoTable::BrnMeta,
                b"protocol_params",
            )
            .unwrap()
            .unwrap()
            .to_vec();
        writer
            .put_brn_meta(b"protocol_params", b"invalid captured-comparison fixture")
            .unwrap();
        assert!(
            checked.verify_unchanged(&writer).is_err(),
            "dirty writer reused a verified read cut"
        );
        checked.verify_view(&view).unwrap();
        writer.commit().unwrap();
        checked.verify_view(&view).unwrap();
        assert!(checked.has_verified_view(&view).unwrap());
        let mut writer = local.tx_begin_write().unwrap();
        assert!(checked.verify_unchanged(&writer).is_err());
        assert!(!checked.has_verified_view(&writer.read_view()).unwrap());
        assert!(checked.verify_view(&writer.read_view()).is_err());
        assert!(checked.has_verified_view(&view).unwrap());
        writer
            .put_brn_meta(b"protocol_params", &original_params)
            .unwrap();
        writer.commit().unwrap();
        checked.verify_view(&view).unwrap();
        assert!(checked.has_verified_view(&view).unwrap());
        let hash = fixture.blocks.last().unwrap().hash;
        let mut writer = local.tx_begin_write().unwrap();
        let original_body = writer.get_block(&hash).unwrap();
        writer.put_block(&hash, b"changed physical body").unwrap();
        writer.commit().unwrap();
        checked.verify_view(&view).unwrap();
        assert!(checked.has_verified_view(&view).unwrap());
        let mut writer = local.tx_begin_write().unwrap();
        assert!(checked.verify_unchanged(&writer).is_err());
        assert!(!checked.has_verified_view(&writer.read_view()).unwrap());
        assert!(checked.verify_view(&writer.read_view()).is_err());
        assert!(checked.has_verified_view(&view).unwrap());
        writer.put_block(&hash, &original_body).unwrap();
        writer.commit().unwrap();
        checked.verify_view(&view).unwrap();
        assert!(checked.has_verified_view(&view).unwrap());
        drop(view);
        drop(reader);
        checked
            .verify_unchanged(&local.tx_begin_write().unwrap())
            .unwrap();
    }

    if prune && !periodic {
        let before_count = local.block_store().block_count().unwrap();
        let before_metadata = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        // The owned catalogue must open its reader after borrowed cuts drop.
        if let Some(owned) = observed_retirement.as_mut() {
            let counts = local.tx_begin_write().unwrap().ledger_counts().unwrap();
            let counts = crate::ledger_cache::LedgerCache::new(counts.0, counts.1, counts.2);
            let phase = std::time::Instant::now();
            owned.reconcile(&local).unwrap();
            eprintln!(
                "OWNED_RETIREMENT stage=reconcile-before-page elapsed_us={}",
                phase.elapsed().as_micros()
            );
            let phase = std::time::Instant::now();
            let retirement = owned.prepare_page(&local, 2, 64 << 20, 131_072).unwrap();
            eprintln!("OWNED_RETIREMENT stage=prepare-page elapsed_us={} examined={} selected={} complete={}",
                phase.elapsed().as_micros(), retirement.progress().examined, retirement.hashes().len(), retirement.progress().complete);
            assert_eq!(retirement.hashes().len(), 2);
            assert!(retirement
                .hashes()
                .iter()
                .all(|hash| fixture.blocks[start..]
                    .iter()
                    .all(|block| block.hash != *hash)));
            let phase = std::time::Instant::now();
            let (deleted, bytes) = retirement.commit_counted(&local, &counts).unwrap();
            eprintln!(
                "OWNED_RETIREMENT stage=commit-page elapsed_us={} deleted={deleted} bytes={bytes}",
                phase.elapsed().as_micros()
            );
            assert_eq!(deleted, 2);
            let phase = std::time::Instant::now();
            owned.reconcile(&local).unwrap();
            eprintln!(
                "OWNED_RETIREMENT stage=reconcile-own-deletion elapsed_us={}",
                phase.elapsed().as_micros()
            );
            assert!(owned.is_current(&local).unwrap());
            assert_eq!(counts.block_count(), before_count - 2);
            for block in &fixture.blocks[start..] {
                assert!(local.block_store().exists(&block.hash).unwrap());
            }
        } else {
            let prepared = if groups == 1 {
                let reader = local.tx_begin_read().unwrap();
                let hash = fixture.blocks.last().unwrap().hash;
                let mut writer = local.tx_begin_write().unwrap();
                let body = writer.get_block(&hash).unwrap();
                writer
                    .put_block(&hash, b"corrupt live body during captured root derivation")
                    .unwrap();
                writer.commit().unwrap();
                let prepared = checked
                    .prepare_token_indexes_view(&local, &reader, Timestamp::new(at + 4))
                    .unwrap();
                {
                    let closed = prepared
                        .body_requirements_view(&local, &reader)
                        .unwrap()
                        .close(Default::default())
                        .unwrap();
                    assert_eq!(
                        bincode::serialize(&closed.body(hash).unwrap().unwrap()).unwrap(),
                        body
                    );
                    assert!(closed
                        .check_unchanged_cut(&local.tx_begin_write().unwrap())
                        .is_err());
                }
                let mut writer = local.tx_begin_write().unwrap();
                writer.put_block(&hash, &body).unwrap();
                writer.commit().unwrap();
                drop(reader);
                prepared
            } else {
                checked
                    .prepare_token_indexes_at(&local, Timestamp::new(at + 4))
                    .unwrap()
            };
            if groups > 1 {
                eprintln!(
                    "MERGE_GRAPH stage=prepared elapsed_ms={}",
                    started.elapsed().as_millis()
                );
            }
            let closed = prepared
                .body_requirements(&local)
                .unwrap()
                .close(Default::default())
                .unwrap();
            if groups > 1 {
                eprintln!("MERGE_GRAPH stage=closed bodies={} edges={} bytes={} elapsed_ms={} scratch_bytes={}",
                    closed.work().bodies, closed.work().edges, closed.work().bytes,
                    started.elapsed().as_millis(), resource_limit.as_ref().unwrap().used_temporary_bytes().unwrap());
                assert!(closed.work().bodies >= (groups * 5) as u64);
                assert!(closed.work().edges >= suffix_edges);
            }
            let retirement = closed
                .prepare_retirement(Timestamp::new(expected.anchor().cutoff()), 2, 64 << 20)
                .unwrap();
            assert_eq!(retirement.hashes().len(), 2);
            assert_eq!(retirement.commit().unwrap().0, 2);
        }
        assert_eq!(local.block_store().block_count().unwrap(), before_count - 2);
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before_metadata
        );
        if groups > 1 {
            eprintln!(
                "MERGE_GRAPH stage=retired elapsed_ms={}",
                started.elapsed().as_millis()
            );
        }
        checked = installed::recover_metadata_from_peers(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&service.registry),
        )
        .await
        .unwrap()
        .unwrap();
        if groups > 1 {
            eprintln!(
                "MERGE_GRAPH stage=recovered-after-retirement elapsed_ms={}",
                started.elapsed().as_millis()
            );
        }
    }
    if migrate {
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let staged = checked
            .project_token_indexes_with_stable_holdings(&local, Timestamp::new(at + 4))
            .unwrap()
            .stage_undo()
            .unwrap();
        drop(staged);
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
        {
            let batch = local.tx_begin_write().unwrap();
            assert_eq!(batch.uses_brn_rate_records().unwrap(), !legacy_rates);
            assert_eq!(
                batch
                    .read_brn_engine(100_000, 64 << 20)
                    .unwrap()
                    .rate_history,
                original_rates
            );
        }
        checked = installed::recover_metadata_from_peers(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&service.registry),
        )
        .await
        .unwrap()
        .unwrap();
    }
    service.stop().await;
    let projected = if migrate {
        checked.project_token_indexes_with_stable_holdings(&local, Timestamp::new(at + 4))
    } else {
        checked.project_token_indexes_at(&local, Timestamp::new(at + 4))
    }
    .unwrap();
    if groups > 1 {
        eprintln!(
            "MERGE_GRAPH stage=projected elapsed_ms={}",
            started.elapsed().as_millis()
        );
    }
    let journal = projected
        .economic_changes()
        .into_iter()
        .find(|row| row.block == merged)
        .unwrap();
    if row_format == 2 || migrate {
        assert!(journal.changes.iter().any(|change| change
            .key
            .starts_with(burst_trst::engine::holding_rows::PREFIX.as_bytes())));
        assert!(projected
            .economic_changes()
            .iter()
            .flat_map(|row| &row.changes)
            .all(|change| !change.key.starts_with(b"trst_state/v1/0/")
                && !change.key.starts_with(b"trst_state/v1/1/")));
    }
    assert!(journal.changes.iter().any(|change| change
        .key
        .starts_with(burst_trst::merger_graph::GraphRecord::STORAGE_PREFIX.as_bytes())));
    assert!(projected.economic_changes().iter().all(|row| row
        .changes
        .iter()
        .all(|change| change.key != b"merger_graph"
            && change.key != burst_trst::TrstEngine::meta_key().as_bytes())));
    assert!(projected
        .economic_changes()
        .iter()
        .flat_map(|record| &record.changes)
        .all(
            |change| change.table != burst_store_lmdb::block_undo::UndoTable::BrnMeta
                || change.key != b"rate_history"
        ));
    let rewind: Vec<_> = projected
        .economic_changes()
        .iter()
        .rev()
        .map(|row| row.block)
        .collect();
    projected.stage_undo().unwrap().commit_for_test().unwrap();
    let mut batch = local.tx_begin_write().unwrap();
    assert!(batch.uses_brn_rate_records().unwrap());
    assert!(batch
        .undo_value(
            burst_store_lmdb::block_undo::UndoTable::BrnMeta,
            b"rate_history"
        )
        .unwrap()
        .is_none());
    assert_eq!(
        batch
            .read_brn_engine(100_000, 64 << 20)
            .unwrap()
            .rate_history,
        original_rates
    );
    assert!(batch.uses_graph_records().unwrap());
    assert!(crate::trst_storage::uses_row_storage(&batch).unwrap());
    assert_eq!(
        batch.uses_stable_trst_records().unwrap(),
        row_format == 2 || migrate
    );
    assert_eq!(
        crate::trst_storage::load(&batch, fixture.params.trst_expiry_secs)
            .unwrap()
            .save_wallets(),
        holdings
    );
    assert_eq!(
        crate::graph_storage::load(&batch).unwrap().to_bytes(),
        graph.to_bytes()
    );
    for hash in rewind {
        batch.rewind_block(&hash).unwrap();
    }
    assert_eq!(
        crate::trst_storage::load(&batch, fixture.params.trst_expiry_secs)
            .unwrap()
            .save_wallets(),
        original_holdings
    );
    assert_eq!(
        crate::graph_storage::load(&batch).unwrap().to_bytes(),
        original.to_bytes()
    );
    assert!(batch.uses_brn_rate_records().unwrap());
    assert_eq!(
        batch
            .read_brn_engine(100_000, 64 << 20)
            .unwrap()
            .rate_history,
        original_rates
    );
    if prune {
        // These are the same economic engine operations used by expiry
        // governance and identity correction, not authorization of an appeal.
        let now = Timestamp::new(at + 5);
        let mut restored =
            crate::trst_storage::load(&batch, fixture.params.trst_expiry_secs).unwrap();
        assert_eq!(restored.transferable_balance(&address(8), now), Some(0));
        restored.set_expiry_period(u64::MAX, now).unwrap();
        for holder in 1..=8 {
            assert_eq!(
                restored.transferable_balance(&address(holder), now),
                Some(1)
            );
        }
        let before_revoke = restored.save_wallets();
        assert!(!restored.revoke_by_originator(&address(0)).is_empty());
        for holder in 1..=8 {
            assert_eq!(
                restored.transferable_balance(&address(holder), now),
                Some(0)
            );
        }
        restored.restore_by_originator(&address(7), now).unwrap();
        assert_eq!(restored.transferable_balance(&address(8), now), Some(0));
        assert!(!restored
            .restore_by_originator(&address(0), now)
            .unwrap()
            .is_empty());
        for holder in 1..=8 {
            assert_eq!(
                restored.transferable_balance(&address(holder), now),
                Some(1)
            );
        }
        assert_eq!(restored.save_wallets(), before_revoke);
        let origin = restored.get_portfolio(&address(8)).unwrap().tokens[0].origin;
        let origins = [(address(0), origin)];
        let first_cause = BlockHash::new([81; 32]);
        let second_cause = BlockHash::new([82; 32]);
        restored
            .invalidate_outcome_origins(first_cause, &origins)
            .unwrap();
        restored
            .invalidate_outcome_origins(second_cause, &origins)
            .unwrap();
        assert_eq!(restored.transferable_balance(&address(8), now), Some(0));
        restored
            .restore_outcome_origins(first_cause, &origins, now)
            .unwrap();
        assert_eq!(restored.transferable_balance(&address(8), now), Some(0));
        restored
            .restore_outcome_origins(second_cause, &origins, now)
            .unwrap();
        assert_eq!(restored.transferable_balance(&address(8), now), Some(1));
        assert_eq!(restored.save_wallets(), before_revoke);
    }
    batch.commit().unwrap();
    if groups > 1 {
        eprintln!(
            "MERGE_GRAPH stage=rewound-restored elapsed_ms={}",
            started.elapsed().as_millis()
        );
    }
    resource_limit
}

async fn periodic_signed_arrivals(
    local: &Arc<LmdbStore>,
    source: &Arc<LmdbStore>,
    params: &ProtocolParams,
    epoch: BlockHash,
    blocks: &[StateBlock],
) {
    use crate::body_retirement::{Probe, Worker};
    let service = WitnessService::start(source.clone(), local.clone(), 0).await;
    let initial = local.block_store().block_count().unwrap();
    let count_rows = local.tx_begin_write().unwrap().ledger_counts().unwrap();
    let counts = Arc::new(crate::ledger_cache::LedgerCache::new(
        count_rows.0,
        count_rows.1,
        count_rows.2,
    ));
    let processor = Arc::new(tokio::sync::Mutex::new(
        crate::block_processor::BlockProcessor::new(params.min_work_difficulty, epoch),
    ));
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = events.clone();
    let started = std::time::Instant::now();
    let shutdown = Arc::new(crate::shutdown::ShutdownController::new());
    let worker = Arc::new(Worker {
        store: local.clone(),
        network: NetworkId::Dev,
        params: params.clone(),
        registry: service.registry.clone(),
        processor: processor.clone(),
        counts: counts.clone(),
        shutdown: shutdown.clone(),
        prepared_probe: Some(Arc::new(move |event| {
            eprintln!(
                "PERIODIC_RETIREMENT elapsed_ms={} event={event:?}",
                started.elapsed().as_millis()
            );
            recorded.lock().unwrap().push((started.elapsed(), event));
        })),
    });
    let task = worker.start(crate::config::BodyRetirementConfig {
        enabled: true,
        max_scratch_bytes: Some(64 << 20),
        interval_secs: 1,
        retain_seconds: 0,
        max_examined: 8,
        max_bodies: 4,
        max_bytes: 4 << 20,
        ..Default::default()
    });
    // Absolute arrivals reveal scheduler lag instead of hiding it in per-write sleeps.
    let arrival_start = tokio::time::Instant::now();
    for (index, block) in blocks.iter().enumerate() {
        tokio::time::sleep_until(
            arrival_start + std::time::Duration::from_millis(index as u64 * 100),
        )
        .await;
        let write_started = std::time::Instant::now();
        let _publication = processor.lock().await;
        let mut batch = local.tx_begin_write().unwrap();
        crate::payment_transition::apply(&mut batch, block, params, block.timestamp).unwrap();
        batch.commit().unwrap();
        counts.inc_block_count();
        eprintln!(
            "PERIODIC_PAYMENT index={index} elapsed_ms={} apply_us={}",
            arrival_start.elapsed().as_millis(),
            write_started.elapsed().as_micros()
        );
    }
    let arrivals_done = started.elapsed();
    // Stop after the fixed workload: no quiet drain phase can disguise starvation.
    shutdown.shutdown();
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    let events = events.lock().unwrap();
    let deleted: usize = events
        .iter()
        .map(|(_, event)| match event {
            Probe::Finished { deleted, .. } => *deleted,
            _ => 0,
        })
        .sum();
    let during_writes: usize = events
        .iter()
        .filter(|(at, _)| *at <= arrivals_done)
        .map(|(_, event)| match event {
            Probe::Finished { deleted, .. } => *deleted,
            _ => 0,
        })
        .sum();
    let rebuilt = events
        .iter()
        .filter(|(_, event)| matches!(event, Probe::Prepared(true)))
        .count();
    let deferred = events
        .iter()
        .filter(|(_, event)| matches!(event, Probe::Deferred))
        .count();
    let mut reuse_pending = false;
    for (_, event) in events.iter() {
        match event {
            Probe::Deferred => reuse_pending = true,
            Probe::Prepared(built) if reuse_pending => {
                assert!(!built, "covered publication deferral rebuilt the catalogue");
                reuse_pending = false;
            }
            Probe::Finished { error: Some(_), .. } => reuse_pending = false,
            _ => {}
        }
    }
    let failed = events
        .iter()
        .filter(|(_, event)| matches!(event, Probe::Finished { error: Some(_), .. }))
        .count();
    let examined: usize = events
        .iter()
        .map(|(_, event)| match event {
            Probe::Finished { examined, .. } => *examined,
            _ => 0,
        })
        .sum();
    let retired_bytes: u64 = events
        .iter()
        .map(|(_, event)| match event {
            Probe::Finished { bytes, .. } => *bytes,
            _ => 0,
        })
        .sum();
    let attempt_us: u128 = events
        .iter()
        .map(|(_, event)| match event {
            Probe::Finished { elapsed, .. } => elapsed.as_micros(),
            _ => 0,
        })
        .sum();
    eprintln!("PERIODIC_SUMMARY signed={} arrivals_ms={} deleted={deleted} during_writes={during_writes} examined={examined} bytes={retired_bytes} rebuilt={rebuilt} failed={failed} deferred={deferred} attempt_us={attempt_us}", blocks.len(), arrivals_done.as_millis());
    assert!(
        during_writes > 0,
        "retirement made no deletion progress during signed arrivals"
    );
    assert_eq!(
        local.block_store().block_count().unwrap(),
        initial + blocks.len() as u64 - deleted as u64
    );
    assert_eq!(
        counts.block_count(),
        local.block_store().block_count().unwrap()
    );
    for block in blocks {
        assert!(local.block_store().exists(&block.hash).unwrap());
    }
    drop(events);
    service.stop().await;
}
