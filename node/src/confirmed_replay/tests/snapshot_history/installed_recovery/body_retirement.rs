use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn physical_prefix_retirement_preserves_roots_and_recovers_after_reopen() {
    let fixture = history_fixture();
    let (fixture, _source_dir, source, manifest, _) = selection::setup_fixture(fixture);
    let epoch = manifest.checkpoint.parent;
    let bytes = archive(&source, &fixture, epoch);
    let expected = authenticate(&source, &fixture.params, epoch).into_continuation();
    let rows = source
        .block_store()
        .indexed_blocks_txn(&source.tx_begin_read().unwrap(), 131_072, 64 << 20)
        .unwrap();
    let source = Arc::new(source);
    for mode in 0..6 {
        let (directory, local) = cemented::sparse(&expected, &source);
        copy_installed_records(&local, &source, &fixture, &bytes);
        {
            let mut batch = local.tx_begin_write().unwrap();
            for row in &rows {
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
        }
        let promised = &fixture.blocks[0];
        let election = crate::ledger_updater::election_root(promised);
        assert!(!election.is_zero());
        assert!(!local.block_store().exists(&election).unwrap());
        let promise_key = format!(
            "consensus_final/v2/{epoch}/{}/{}/{election}",
            address(0),
            promised.account
        );
        local
            .meta_store()
            .put_meta(&promise_key, promised.hash.as_bytes())
            .unwrap();
        local
            .meta_store()
            .put_meta(
                &format!("consensus_time/v1/{epoch}/{}", address(0)),
                &expected.anchor().cutoff().to_be_bytes(),
            )
            .unwrap();
        local
            .meta_store()
            .put_meta(
                &format!("consensus_sequence/v1/{}", address(0)),
                &1u64.to_be_bytes(),
            )
            .unwrap();
        let closed_epoch = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            NetworkId::Dev,
            &fixture.params,
        )
        .unwrap()
        .hash();
        assert_ne!(closed_epoch, epoch);
        let excluded = BlockHash::new([77; 32]);
        assert!(!local.block_store().exists(&excluded).unwrap());
        let closed_promise = format!(
            "consensus_final/v2/{closed_epoch}/{}/{}/{election}",
            address(0),
            promised.account
        );
        local
            .meta_store()
            .put_meta(&closed_promise, excluded.as_bytes())
            .unwrap();
        local
            .meta_store()
            .put_meta(
                &format!("consensus_time/v1/{closed_epoch}/{}", address(0)),
                &0u64.to_be_bytes(),
            )
            .unwrap();
        let mut closed = epoch.as_bytes().to_vec();
        closed.extend_from_slice(closed_epoch.as_bytes());
        local
            .meta_store()
            .put_meta(
                &burst_store::meta::closed_payment_epoch_key(&closed_epoch),
                &closed,
            )
            .unwrap();
        normalize_retention_fixture(&local, &fixture.params);
        let local = Arc::new(local);
        let mut before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let count_before = local.block_store().block_count().unwrap();
        let counts_before = local.tx_begin_write().unwrap().ledger_counts().unwrap();
        let counts = crate::ledger_cache::LedgerCache::new(
            counts_before.0,
            counts_before.1,
            counts_before.2,
        );
        let service = WitnessService::start(Arc::clone(&source), Arc::clone(&local), 0).await;
        let checked = installed::recover_metadata_from_peers(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&service.registry),
        )
        .await
        .unwrap()
        .unwrap();
        let worker_caches = if mode >= 2 {
            let cache = installed::recover_current_tip_with_cache(
                Arc::clone(&local),
                NetworkId::Dev,
                fixture.params.clone(),
                Arc::clone(&service.registry),
                None,
            )
            .await
            .unwrap();
            let expected_cache = cache.recovered.fork(local.env().path()).unwrap();
            let sync = cache.fork(local.env().path()).unwrap();
            let beacon = cache.fork(local.env().path()).unwrap();
            let agreement = cache.fork(local.env().path()).unwrap();
            Some((expected_cache, [sync, beacon, agreement]))
        } else {
            None
        };
        service.stop().await;
        let stopped_registry = Arc::new(RwLock::new(ConnectionRegistry::new()));
        let selected;
        let mut required = Vec::new();
        {
            let (captured, mut changes) = if mode == 5 {
                let writer = local.tx_begin_write().unwrap();
                let changes = writer.watch_body_changes(65_536, 64 << 20).unwrap();
                let reader = local.tx_begin_read().unwrap();
                assert!(writer.matches_clean_storage_cut(&reader.clean_storage_cut().unwrap()));
                drop(writer);
                (Some(reader), Some(changes))
            } else {
                (None, None)
            };
            let prepared = match &captured {
                Some(view) => checked.prepare_token_indexes_view(
                    &local,
                    view,
                    Timestamp::new(expected.anchor().cutoff()),
                ),
                None => checked
                    .prepare_token_indexes_at(&local, Timestamp::new(expected.anchor().cutoff())),
            }
            .unwrap();
            if mode == 0 {
                let closed = prepared
                    .body_requirements(&local)
                    .unwrap()
                    .close(Default::default())
                    .unwrap();
                let work = (
                    closed.work().bodies,
                    closed.work().bytes,
                    closed.work().edges,
                );
                let mut after = None;
                let mut examined = 0;
                loop {
                    let page = closed
                        .prepare_retirement_page(Timestamp::new(0), 2, 64 << 20, after, 1)
                        .unwrap();
                    assert!(page.hashes().is_empty());
                    let progress = page.progress();
                    assert!(progress.examined <= 1);
                    examined += progress.examined;
                    assert!(progress.blocked_body_bytes.is_none());
                    assert!(progress.resume_after > after);
                    after = progress.resume_after;
                    drop(page);
                    if progress.complete {
                        break;
                    }
                }
                assert_eq!(examined as u64, count_before);
                assert_eq!(
                    (
                        closed.work().bodies,
                        closed.work().bytes,
                        closed.work().edges
                    ),
                    work
                );
                let exhausted = closed
                    .prepare_retirement_page(Timestamp::new(0), 2, 64 << 20, after, 1)
                    .unwrap();
                assert_eq!(exhausted.progress().examined, 0);
                assert!(exhausted.progress().complete);
                drop(exhausted);
                assert!(closed
                    .prepare_retirement_page(Timestamp::new(0), 2, 64 << 20, None, 0)
                    .is_err());
                let blocked = closed
                    .prepare_retirement_page(
                        Timestamp::new(expected.anchor().cutoff()),
                        2,
                        1,
                        None,
                        131_072,
                    )
                    .unwrap();
                let progress = blocked.progress();
                assert!(!progress.complete);
                assert!(progress.blocked_body_bytes.unwrap() > 1);
                assert!(blocked.hashes().is_empty());
                drop(blocked);
                let retry = closed
                    .prepare_retirement_page(
                        Timestamp::new(expected.anchor().cutoff()),
                        1,
                        64 << 20,
                        progress.resume_after,
                        1,
                    )
                    .unwrap();
                assert_eq!(retry.progress().examined, 1);
                assert_eq!(retry.hashes().len(), 1);
                drop(retry);
                for (before, byte_limit) in [(0, 64 << 20), (expected.anchor().cutoff(), 1)] {
                    let empty = closed
                        .prepare_retirement(burst_types::Timestamp::new(before), 2, byte_limit)
                        .unwrap();
                    assert!(empty.hashes().is_empty());
                    assert_eq!(empty.commit_counted(&counts).unwrap(), (0, 0));
                }
                assert!(closed
                    .prepare_retirement(
                        burst_types::Timestamp::new(expected.anchor().cutoff()),
                        0,
                        64 << 20
                    )
                    .is_err());
            }
            let closed = match &captured {
                Some(view) => prepared.body_requirements_view(&local, view),
                None => prepared.body_requirements(&local),
            }
            .unwrap()
            .close(Default::default())
            .unwrap();
            closed
                .visit_bodies(|hash| {
                    required.push(hash);
                    Ok(())
                })
                .unwrap();
            assert!(required.contains(&promised.hash));
            assert!(
                !required.contains(&excluded),
                "closed-epoch promise is retained as a journal, not a live body root"
            );
            assert!(
                !required.contains(&election),
                "election identity is not a body dependency"
            );
            let batch = closed
                .prepare_retirement(
                    burst_types::Timestamp::new(expected.anchor().cutoff()),
                    2,
                    64 << 20,
                )
                .unwrap();
            selected = batch.hashes().to_vec();
            assert_eq!(
                selected.len(),
                2,
                "fixture must have disposable prefix bodies"
            );
            assert!(selected.iter().all(|hash| !required.contains(hash)));
            match mode {
                0 => drop(batch),
                1 => {
                    local
                        .meta_store()
                        .put_meta("retirement_stale_cut", &[1])
                        .unwrap();
                    assert!(batch.commit_counted(&counts).is_err());
                    local
                        .meta_store()
                        .delete_meta("retirement_stale_cut")
                        .unwrap();
                }
                2 => {
                    let invalid_counts = crate::ledger_cache::LedgerCache::new(0, 0, 0);
                    let uncounted = closed
                        .prepare_retirement(Timestamp::new(expected.anchor().cutoff()), 2, 64 << 20)
                        .unwrap();
                    assert!(uncounted.commit_counted(&invalid_counts).is_err());
                    assert_eq!(invalid_counts.block_count(), 0);
                    let sibling = closed
                        .prepare_retirement(Timestamp::new(expected.anchor().cutoff()), 2, 64 << 20)
                        .unwrap();
                    assert_eq!(sibling.hashes(), selected);
                    // A validated soft reservation changes only the durable
                    // sequence, not this complete body's dependency closure.
                    let sequence = local
                        .meta_store()
                        .reserve_consensus_vote_at(
                            &epoch,
                            &address(0),
                            &promised.account,
                            &election,
                            &promised.hash,
                            false,
                            0,
                            Some(expected.anchor().cutoff()),
                        )
                        .unwrap();
                    assert_eq!(sequence, 2);
                    let key = format!("consensus_sequence/v1/{}", address(0));
                    let row = before
                        .iter_mut()
                        .find(|(name, _)| name == key.as_bytes())
                        .unwrap();
                    assert_eq!(row.1, 1u64.to_be_bytes());
                    row.1 = sequence.to_be_bytes().to_vec();
                    // Model an independent additive publication that must not be
                    // overwritten by an old physical inventory count.
                    counts.inc_block_count();
                    let (count, bytes) = batch.commit_counted(&counts).unwrap();
                    assert_eq!(counts.block_count(), count_before - count as u64 + 1);
                    counts.dec_block_count();
                    assert_eq!(count, selected.len());
                    assert!(bytes > 0);
                    assert!(
                        sibling.commit_counted(&counts).is_err(),
                        "a committed page invalidates sibling plans"
                    );
                    let stale = closed
                        .prepare_retirement_page(
                            Timestamp::new(expected.anchor().cutoff()),
                            2,
                            64 << 20,
                            Some(selected[1]),
                            1,
                        )
                        .unwrap();
                    assert!(
                        stale.commit_counted(&counts).is_err(),
                        "later pages cannot publish from the old cut"
                    );
                }
                3 | 4 | 5 => {
                    if mode != 5 {
                        assert!(closed
                            .own_retirement(
                                Timestamp::new(expected.anchor().cutoff()),
                                None,
                                || true
                            )
                            .is_err());
                    }
                    let promised_sequence = std::cell::Cell::new(None);
                    let promise = || {
                        if promised_sequence.get().is_none() {
                            let protected = selected[0];
                            let row = rows.iter().find(|row| row.hash == protected).unwrap();
                            let block: StateBlock = bincode::deserialize(&row.bytes).unwrap();
                            let root = crate::ledger_updater::election_root(&block);
                            promised_sequence.set(Some(
                                local
                                    .meta_store()
                                    .reserve_consensus_vote_at(
                                        &epoch,
                                        &address(0),
                                        &block.account,
                                        &root,
                                        &protected,
                                        true,
                                        0,
                                        Some(expected.anchor().cutoff()),
                                    )
                                    .unwrap(),
                            ));
                        }
                    };
                    let mut owned = if mode == 5 {
                        // The promise precedes classification; transferring the original
                        // watch must retain it even though captured roots cannot see it.
                        promise();
                        assert!(closed
                            .own_retirement(
                                Timestamp::new(expected.anchor().cutoff()),
                                None,
                                || false
                            )
                            .is_err());
                        closed
                            .own_captured_retirement(
                                Timestamp::new(expected.anchor().cutoff()),
                                None,
                                || false,
                                changes.take().unwrap(),
                            )
                            .unwrap()
                    } else {
                        closed
                            .own_retirement(
                                Timestamp::new(expected.anchor().cutoff()),
                                None,
                                || {
                                    if mode == 4 {
                                        promise();
                                    }
                                    false
                                },
                            )
                            .unwrap()
                    };

                    drop(batch);
                    drop(closed);
                    drop(prepared);
                    drop(captured);
                    // The catalogue survives its complete source proof and reader.
                    local.tx_begin_read().unwrap();
                    if mode >= 4 {
                        assert!(
                            !owned.is_current(&local).unwrap(),
                            "classification-time promise must be reconciled"
                        );
                        owned.reconcile(&local).unwrap();
                    }
                    assert!(owned.is_current(&local).unwrap());
                    let blocked = owned.prepare_page(&local, 2, 1, 131_072).unwrap();
                    assert!(blocked.hashes().is_empty());
                    assert!(blocked.progress().blocked_body_bytes.unwrap() > 1);
                    assert_eq!(blocked.commit_counted(&local, &counts).unwrap(), (0, 0));
                    if mode >= 4 {
                        let protected = selected[0];
                        let row = rows.iter().find(|row| row.hash == protected).unwrap();
                        let block: StateBlock = bincode::deserialize(&row.bytes).unwrap();
                        let root = crate::ledger_updater::election_root(&block);
                        let sequence = promised_sequence
                            .get()
                            .expect("promise committed during classification");
                        assert_eq!(sequence, 2);
                        let sequence_key = format!("consensus_sequence/v1/{}", address(0));
                        before
                            .iter_mut()
                            .find(|(key, _)| key == sequence_key.as_bytes())
                            .unwrap()
                            .1 = sequence.to_be_bytes().to_vec();
                        let key = format!(
                            "consensus_final/v2/{epoch}/{}/{}/{root}",
                            address(0),
                            block.account
                        );
                        assert!(!before.iter().any(|(name, _)| name == key.as_bytes()));
                        before.push((key.into_bytes(), protected.as_bytes().to_vec()));
                        before.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                        required.push(protected);
                    }
                    let mut deleted = Vec::new();
                    while deleted.len() < if mode >= 4 { 1 } else { 2 } {
                        let page = owned.prepare_page(&local, 1, 64 << 20, 1).unwrap();
                        assert!(page.progress().examined <= 1);
                        assert!(
                            !page.progress().complete || !page.hashes().is_empty(),
                            "catalogue exhausted before expected deletion"
                        );
                        deleted.extend_from_slice(page.hashes());
                        page.commit_counted(&local, &counts).unwrap();
                        assert!(
                            owned.is_current(&local).unwrap(),
                            "own deletion preserves the remaining checked catalogue"
                        );
                    }
                    assert_eq!(
                        deleted,
                        if mode >= 4 {
                            selected[1..].to_vec()
                        } else {
                            selected.clone()
                        }
                    );
                    let page = owned.prepare_page(&local, 1, 64 << 20, 1).unwrap();
                    local
                        .meta_store()
                        .put_meta("owned_retirement_gap", &[1])
                        .unwrap();
                    local
                        .meta_store()
                        .delete_meta("owned_retirement_gap")
                        .unwrap();
                    assert!(page.commit_counted(&local, &counts).is_err());
                    assert!(!owned.is_current(&local).unwrap());
                    assert_eq!(
                        counts.block_count(),
                        count_before - if mode >= 4 { 1 } else { 2 }
                    );
                }
                _ => unreachable!(),
            }
        }
        // Every plan/read view has been dropped before opening another reader.
        assert_eq!(
            local.block_store().block_count().unwrap(),
            count_before
                - if mode >= 4 {
                    1
                } else if mode >= 2 {
                    2
                } else {
                    0
                }
        );
        assert_eq!(
            counts.block_count(),
            local.block_store().block_count().unwrap()
        );
        assert_eq!(counts.account_count(), counts_before.1);
        assert_eq!(counts.pending_count(), counts_before.2);
        for hash in &required {
            assert!(local.block_store().exists(hash).unwrap());
        }
        for hash in &selected {
            let retained = mode < 2 || (mode >= 4 && *hash == selected[0]);
            assert_eq!(local.block_store().exists(hash).unwrap(), retained);
            assert_eq!(
                local.block_store().height_of_block(hash).unwrap().is_some(),
                retained
            );
            let row = rows.iter().find(|row| row.hash == *hash).unwrap();
            assert_eq!(
                local
                    .block_store()
                    .block_at_height(&row.account, row.height)
                    .unwrap()
                    .is_some(),
                retained
            );
        }
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
        if mode < 2 {
            continue;
        }
        let (expected_cache, worker_caches) = worker_caches.unwrap();
        for cache in worker_caches {
            let base = cache.base;
            let target = cache.recovered.continuation.anchor().hash();
            // Direct cache recovery must return Some: the full-recovery fallback
            // would hide an invalidated worker cache. The witness service is stopped.
            let reused = cache
                .recover(
                    base,
                    Arc::clone(&local),
                    Arc::clone(&stopped_registry),
                    target,
                )
                .await
                .unwrap()
                .expect("physical prefix deletion must preserve checkpoint replay cache");
            compact::assert_same(
                reused.continuation.state(),
                expected_cache.continuation.state(),
            );
            assert_eq!(reused.continuation.anchor().hash(), target);
        }
        local.force_sync().unwrap();
        drop(local);
        let local = Arc::new(LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
        let service = WitnessService::start(Arc::clone(&source), Arc::clone(&local), 0).await;
        let recovered = installed::recover_metadata_from_peers(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&service.registry),
        )
        .await
        .unwrap()
        .unwrap();
        compact::assert_same(recovered.recovered().continuation.state(), expected.state());
        recovered
            .verify_unchanged(&local.tx_begin_write().unwrap())
            .unwrap();
        let hash = selected[usize::from(mode >= 4)];
        let row = rows.iter().find(|row| row.hash == hash).unwrap();
        let cursor = recovered
            .recovered()
            .continuation
            .history_cursor(&row.account)
            .unwrap()
            .unwrap();
        let proofs = crate::checkpoint_sync::witness::fetch(&service.registry, cursor, vec![hash])
            .await
            .unwrap();
        assert_eq!(proofs.len(), 1);
        assert_eq!(proofs[0].block().hash, hash);
        assert_eq!(bincode::serialize(proofs[0].block()).unwrap(), row.bytes);
        if mode >= 4 {
            assert!(local.block_store().exists(&selected[0]).unwrap());
            let protected = recovered
                .prepare_token_indexes_at(&local, Timestamp::new(expected.anchor().cutoff()))
                .unwrap();
            let requirements = protected
                .body_requirements(&local)
                .unwrap()
                .close(Default::default())
                .unwrap();
            assert!(requirements.has_body(selected[0]).unwrap());
        }

        assert!(service.stop().await > 0);
        assert!(!local.block_store().exists(&hash).unwrap());
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            before
        );
    }
}

fn history_fixture() -> Fixture {
    let mut fixture = Fixture::selection();
    for offset in 0..4 {
        // Preserve the fixed signer roster while adding disposable history.
        fixture.push(
            0,
            BlockType::ChangeRepresentative,
            1_595_431_120 + offset,
            |_| {},
        );
    }
    fixture
}

#[cfg(unix)]
pub(super) async fn crash_child(directory: &std::path::Path, owned: bool, probe: impl Fn(&str)) {
    let (fixture, _source_dir, source, _, _) = selection::setup_fixture(history_fixture());
    let local = Arc::new(LmdbStore::open(directory, 32, 32 << 20).unwrap());
    let service = WitnessService::start(Arc::new(source), local.clone(), 0).await;
    let checked = installed::recover_metadata_from_peers(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
    )
    .await
    .unwrap()
    .unwrap();
    service.stop().await;
    let cutoff = checked.recovered().continuation.anchor().cutoff();
    let prepared = checked
        .prepare_token_indexes_at(&local, Timestamp::new(cutoff))
        .unwrap();
    let closed = prepared
        .body_requirements(&local)
        .unwrap()
        .close(Default::default())
        .unwrap();
    let save_selection = |hashes: &[BlockHash]| {
        assert_eq!(hashes.len(), 2);
        std::fs::write(
            directory.join("retirement-selected.json"),
            serde_json::to_vec(hashes).unwrap(),
        )
        .unwrap();
    };
    if owned {
        let mut catalogue = closed
            .own_retirement(Timestamp::new(cutoff), None, || false)
            .unwrap();
        drop(closed);
        drop(prepared);
        let (blocks, accounts, pending) = local.tx_begin_write().unwrap().ledger_counts().unwrap();
        let counts = crate::ledger_cache::LedgerCache::new(blocks, accounts, pending);
        let page = catalogue
            .prepare_page(&local, 2, 64 << 20, 131_072)
            .unwrap();
        save_selection(page.hashes());
        page.commit_with_probe(&local, &counts, probe).unwrap();
    } else {
        let plan = closed
            .prepare_retirement(Timestamp::new(cutoff), 2, 64 << 20)
            .unwrap();
        save_selection(plan.hashes());
        plan.commit_with_probe(probe).unwrap();
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn physical_retirement_is_atomic_after_process_kill() {
    assert_retirement_atomic_after_process_kill("retirement").await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn owned_catalogue_retirement_is_atomic_after_process_kill() {
    assert_retirement_atomic_after_process_kill("owned-retirement").await;
}

#[cfg(unix)]
async fn assert_retirement_atomic_after_process_kill(kind: &str) {
    let (fixture, _source_dir, source, manifest, _) = selection::setup_fixture(history_fixture());
    let epoch = manifest.checkpoint.parent;
    let bytes = archive(&source, &fixture, epoch);
    let expected = authenticate(&source, &fixture.params, epoch).into_continuation();
    let rows = source
        .block_store()
        .indexed_blocks_txn(&source.tx_begin_read().unwrap(), 131_072, 64 << 20)
        .unwrap();
    let source = Arc::new(source);
    for stage in ["deleted", "prepared", "committed"] {
        let (directory, local) = cemented::sparse(&expected, &source);
        copy_installed_records(&local, &source, &fixture, &bytes);
        {
            let mut batch = local.tx_begin_write().unwrap();
            for row in &rows {
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
        }
        normalize_retention_fixture(&local, &fixture.params);
        let counts = local.tx_begin_write().unwrap().ledger_counts().unwrap();
        let metadata = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        local.force_sync().unwrap();
        drop(local);
        publication_crash::kill_at_kind(directory.path(), stage, kind).await;
        let selected: Vec<BlockHash> = serde_json::from_slice(
            &std::fs::read(directory.path().join("retirement-selected.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(selected.len(), 2);
        let local = Arc::new(LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
        let committed = stage == "committed";
        let after = local.tx_begin_write().unwrap().ledger_counts().unwrap();
        assert_eq!(
            after,
            (counts.0 - if committed { 2 } else { 0 }, counts.1, counts.2),
            "{stage}"
        );
        for hash in selected {
            let row = rows.iter().find(|row| row.hash == hash).unwrap();
            assert_eq!(
                local.block_store().exists(&hash).unwrap(),
                !committed,
                "{stage}"
            );
            assert_eq!(
                local
                    .block_store()
                    .height_of_block(&hash)
                    .unwrap()
                    .is_some(),
                !committed,
                "{stage}"
            );
            assert_eq!(
                local
                    .block_store()
                    .block_at_height(&row.account, row.height)
                    .unwrap()
                    .is_some(),
                !committed,
                "{stage}"
            );
        }
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap(),
            metadata
        );
        let service = WitnessService::start(source.clone(), local.clone(), 0).await;
        let recovered = installed::recover_metadata_from_peers(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
        )
        .await
        .unwrap()
        .unwrap();
        compact::assert_same(recovered.recovered().continuation.state(), expected.state());
        recovered
            .verify_unchanged(&local.tx_begin_write().unwrap())
            .unwrap();
        service.stop().await;
    }
}

pub(in crate::confirmed_replay::tests) async fn assert_governed_restoration(
    fixture: &Fixture,
    source: Arc<LmdbStore>,
    base_epoch: BlockHash,
    next_epoch: BlockHash,
    claims: &[StateBlock],
    expected: &ConfirmedReplay,
) {
    let (directory, local, retired, base_height) =
        pruned_base(fixture, source.clone(), base_epoch).await;
    let chain = format!("checkpoint_chain/v1/dev/{}", fixture.blocks[0].hash);
    let height = base_height + 1;
    let key = format!("{chain}/{height}");
    local
        .meta_store()
        .put_meta(&key, &source.meta_store().get_meta(&key).unwrap())
        .unwrap();
    local
        .meta_store()
        .put_meta(&format!("{chain}/head"), &height.to_be_bytes())
        .unwrap();
    let service = WitnessService::start(source.clone(), local.clone(), 0).await;
    let checked = installed::recover_metadata_from_peers(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
    )
    .await
    .unwrap()
    .unwrap();
    let next = checked
        .prepare_next_from_peers(local.clone(), service.registry.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(next.anchor().hash(), next_epoch);
    let params = next.state().params().clone();
    assert_eq!(params.trst_expiry_secs, 20);
    assert_eq!(
        next.state()
            .portfolio(&address(0))
            .unwrap()
            .cached_transferable,
        40
    );
    next.commit(&local).unwrap();
    service.stop().await;
    assert_eq!(claims.len(), 3, "return claim, send, receive");
    for block in claims {
        let mut batch = local.tx_begin_write().unwrap();
        crate::payment_transition::apply(&mut batch, block, &params, block.timestamp).unwrap();
        batch.commit().unwrap();
    }
    {
        let batch = local.tx_begin_write().unwrap();
        assert_eq!(
            crate::trst_storage::load(&batch, params.trst_expiry_secs)
                .unwrap()
                .save_wallets(),
            expected.trst.save_wallets()
        );
        for seed in [0, 1] {
            let actual = batch.get_account(&address(seed)).unwrap();
            let wanted = expected.account(&address(seed)).unwrap();
            // Claims are provisional here; compare economic/account-chain state
            // without claiming that applying a signed block confirms it.
            assert_eq!(
                (
                    actual.head,
                    actual.block_count,
                    actual.trst_balance,
                    actual.total_brn_burned,
                    actual.total_brn_staked,
                    actual.expired_trst,
                    actual.revoked_trst
                ),
                (
                    wanted.head,
                    wanted.block_count,
                    wanted.trst_balance,
                    wanted.total_brn_burned,
                    wanted.total_brn_staked,
                    wanted.expired_trst,
                    wanted.revoked_trst
                )
            );
        }
    }
    for hash in &retired {
        assert!(!local.block_store().exists(hash).unwrap());
    }
    local.force_sync().unwrap();
    drop(local);
    let local = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
    let batch = local.tx_begin_write().unwrap();
    assert_eq!(
        crate::trst_storage::load(&batch, params.trst_expiry_secs)
            .unwrap()
            .save_wallets(),
        expected.trst.save_wallets()
    );
}

async fn retirement_base(
    fixture: &Fixture,
    source: &LmdbStore,
    base_epoch: BlockHash,
) -> (
    tempfile::TempDir,
    Arc<LmdbStore>,
    crate::confirmed_replay::snapshot::SnapshotContinuation,
) {
    let base = authenticate(source, &fixture.params, base_epoch).into_continuation();
    let export =
        SnapshotExport::prepare(source, NetworkId::Dev, &fixture.params, base_epoch).unwrap();
    let signatures: Vec<_> = (0..32)
        .filter(|seed| base.anchor().roster().weight(&address(*seed)) > 0)
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
    let (directory, local) = cemented::sparse(&base, source);
    local
        .meta_store()
        .put_meta(installed::BASE_KEY, &bytes)
        .unwrap();
    let chain = format!("checkpoint_chain/v1/dev/{}", fixture.blocks[0].hash);
    local
        .meta_store()
        .put_meta(
            &format!("{chain}/head"),
            &base.anchor().height().to_be_bytes(),
        )
        .unwrap();
    let rows = source
        .block_store()
        .indexed_blocks_txn(&source.tx_begin_read().unwrap(), 131_072, 64 << 20)
        .unwrap();
    {
        let mut batch = local.tx_begin_write().unwrap();
        for row in rows {
            if base
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
    }
    normalize_retention_fixture(&local, &fixture.params);
    let local = Arc::new(local);
    (directory, local, base)
}

async fn pruned_base(
    fixture: &Fixture,
    source: Arc<LmdbStore>,
    base_epoch: BlockHash,
) -> (tempfile::TempDir, Arc<LmdbStore>, Vec<BlockHash>, u64) {
    let (directory, local, base) = retirement_base(fixture, &source, base_epoch).await;
    let service = WitnessService::start(source.clone(), local.clone(), 0).await;
    let checked = installed::recover_metadata_from_peers(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
    )
    .await
    .unwrap()
    .unwrap();
    let retired = {
        let prepared = checked
            .prepare_token_indexes_at(&local, Timestamp::new(base.anchor().cutoff()))
            .unwrap();
        let closed = prepared
            .body_requirements(&local)
            .unwrap()
            .close(Default::default())
            .unwrap();
        let plan = closed
            .prepare_retirement(Timestamp::new(base.anchor().cutoff()), 2, 64 << 20)
            .unwrap();
        let hashes = plan.hashes().to_vec();
        assert_eq!(hashes.len(), 2);
        assert_eq!(plan.commit().unwrap().0, 2);
        hashes
    };
    service.stop().await;
    local.force_sync().unwrap();
    drop(local);
    let local = Arc::new(LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
    for hash in &retired {
        assert!(!local.block_store().exists(hash).unwrap());
    }
    (directory, local, retired, base.anchor().height())
}

pub(in crate::confirmed_replay::tests) async fn assert_pruned_claims(
    fixture: &Fixture,
    source: Arc<LmdbStore>,
    epoch: BlockHash,
    accepted: &[StateBlock],
    rejected: &[StateBlock],
    expected: &ConfirmedReplay,
) {
    let (directory, local, retired, _) = pruned_base(fixture, source, epoch).await;
    let params = expected.params();
    assert!(!accepted.is_empty());
    for block in accepted {
        let mut batch = local.tx_begin_write().unwrap();
        crate::payment_transition::apply(&mut batch, block, params, block.timestamp).unwrap();
        batch.commit().unwrap();
    }
    let check = |store: &LmdbStore| {
        let batch = store.tx_begin_write().unwrap();
        let payment = crate::payment_transition::restore(&batch, params).unwrap();
        assert_eq!(payment.trst.save_wallets(), expected.trst.save_wallets());
        for block in accepted {
            let owner = &block.account;
            assert_eq!(
                payment.brn.get_wallet(owner),
                expected.brn.get_wallet(owner)
            );
            let actual = batch.get_account(owner).unwrap();
            let wanted = expected.account(owner).unwrap();
            assert_eq!(
                (
                    actual.head,
                    actual.block_count,
                    actual.trst_balance,
                    actual.total_brn_burned,
                    actual.total_brn_staked,
                    actual.expired_trst,
                    actual.revoked_trst
                ),
                (
                    wanted.head,
                    wanted.block_count,
                    wanted.trst_balance,
                    wanted.total_brn_burned,
                    wanted.total_brn_staked,
                    wanted.expired_trst,
                    wanted.revoked_trst
                )
            );
            assert!(batch
                .get_pending(owner, &block.link.into_tx_hash())
                .is_err());
        }
    };
    check(&local);
    for block in rejected {
        crate::payment_transition::authenticate(block, params, block.timestamp).unwrap();
        let mut batch = local.tx_begin_write().unwrap();
        assert_eq!(
            block.previous,
            batch.get_account(&block.account).unwrap().head
        );
        assert!(
            crate::payment_transition::apply(&mut batch, block, params, block.timestamp).is_err()
        );
        drop(batch);
        check(&local);
        assert!(!local.block_store().exists(&block.hash).unwrap());
    }
    for hash in retired {
        assert!(!local.block_store().exists(&hash).unwrap());
    }
    local.force_sync().unwrap();
    drop(local);
    let local = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
    check(&local);
}

struct OriginHistory(crate::historical_lookup::Reader);
impl burst_rpc::BlockProcessorCallback for OriginHistory {
    fn process_block(&self, _: &[u8]) -> Result<burst_rpc::ProcessResult, String> {
        Err("read-only origin fixture".into())
    }
    fn historical_blocks(
        &self,
        hashes: Vec<BlockHash>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<StateBlock>, String>> + Send + '_>,
    > {
        Box::pin(self.0.read_many(hashes))
    }
}

async fn origin_queries(
    node: &crate::BurstNode,
    local: Arc<LmdbStore>,
    params: &ProtocolParams,
    registry: Arc<RwLock<ConnectionRegistry>>,
    burn: BlockHash,
    merged: BlockHash,
) -> Vec<Result<serde_json::Value, String>> {
    let trst = {
        let batch = local.tx_begin_write().unwrap();
        crate::payment_transition::restore(&batch, params)
            .unwrap()
            .trst
    };
    let mut state = crate::test_rpc::state(node, false);
    state.block_store = Arc::new(local.block_store());
    state.account_store = Arc::new(local.account_store());
    state.meta_store = Arc::new(local.meta_store());
    state.params = Arc::new(params.clone());
    state.trst_engine = Arc::new(tokio::sync::Mutex::new(trst));
    state.block_processor = Arc::new(OriginHistory(crate::historical_lookup::Reader {
        preparation: Default::default(),
        store: local,
        registry,
        network: NetworkId::Dev,
        initial_params: params.clone(),
        active: Arc::new(tokio::sync::Mutex::new(())),
        admitted: Arc::new(tokio::sync::Semaphore::new(3)),
    }));
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let rpc = burst_rpc::RpcServer::new(port, Arc::new(state));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        rpc.start(async {
            let _ = stopped.await;
        })
        .await
    });
    let client = burst_wallet_core::NodeClient::new(format!("http://127.0.0.1:{port}")).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            assert!(!task.is_finished(), "origin RPC exited before readiness");
            if let Ok(connection) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                drop(connection);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    client
        .rpc_call("peers", serde_json::json!({}))
        .await
        .expect("origin RPC application readiness");
    let mut results = Vec::new();
    for (action, origin) in [("trace_origin", burn), ("trst_origins", merged)] {
        results.push(
            client
                .rpc_call(action, serde_json::json!({"origin":origin.to_string()}))
                .await
                .map_err(|error| error.to_string()),
        );
    }
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    results
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn origin_rpc_recovers_retired_burn_merge_and_predecessor_without_installation() {
    let mut fixture = Fixture::selection();
    let mut burns = Vec::new();
    let mut receives = Vec::new();
    for offset in 0..2 {
        let burn = fixture.push(0, BlockType::Burn, 1_595_431_110 + 2 * offset, |block| {
            block.link = destination(8);
            block.brn_spent += 1;
        });
        burns.push(burn);
        receives.push(
            fixture.push(8, BlockType::Receive, 1_595_431_111 + 2 * offset, |block| {
                block.link = burn;
                block.trst_balance += 1;
            }),
        );
    }
    let merged = fixture.push(8, BlockType::Merge, 1_595_431_114, |block| {
        block.merge_sources = receives.iter().map(|hash| hash.into_tx_hash()).collect();
    });
    fixture.push(8, BlockType::ChangeRepresentative, 1_595_431_115, |_| {});
    for offset in 0..4 {
        fixture.push(
            0,
            BlockType::ChangeRepresentative,
            1_595_431_120 + offset,
            |_| {},
        );
    }
    let previous = fixture
        .blocks
        .iter()
        .find(|block| block.hash == burns[0])
        .unwrap()
        .previous;
    let (fixture, _source_dir, source, manifest, _) = selection::setup_fixture(fixture);
    let source = Arc::new(source);
    let (directory, local, base) =
        retirement_base(&fixture, &source, manifest.checkpoint.parent).await;
    let service = WitnessService::start(source.clone(), local.clone(), 0).await;
    let checked = installed::recover_metadata_from_peers(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
    )
    .await
    .unwrap()
    .unwrap();
    let node_directory = tempfile::tempdir().unwrap();
    let node = crate::BurstNode::new(crate::NodeConfig {
        network: NetworkId::Dev,
        data_dir: node_directory.path().to_path_buf(),
        port: 0,
        enable_rpc: false,
        enable_upnp: false,
        enable_representative: false,
        bootstrap_peers: vec![],
        nts_sources: vec![],
        ..crate::NodeConfig::default()
    })
    .await
    .unwrap();
    let before_requests = service.requests.load(Ordering::SeqCst);
    let before = origin_queries(
        &node,
        local.clone(),
        &fixture.params,
        service.registry.clone(),
        burns[0],
        merged,
    )
    .await;
    assert!(before.iter().all(Result::is_ok), "{before:?}");
    assert_eq!(
        service.requests.load(Ordering::SeqCst),
        before_requests,
        "local origin reads must not fetch history"
    );
    let trace = before[0].as_ref().unwrap();
    assert_eq!(trace["amount"], "1");
    assert_eq!(fixture.params.trst_expiry_secs, 1000);
    assert!(
        fixture
            .blocks
            .iter()
            .find(|block| block.hash == burns[0])
            .unwrap()
            .timestamp
            .as_secs()
            + fixture.params.trst_expiry_secs
            <= base.anchor().cutoff()
    );
    assert_eq!(trace["status"], "expired");
    assert_eq!(trace["forward_merges"].as_array().unwrap().len(), 1);
    assert_eq!(trace["forward_merges"][0]["merge_hash"], merged.to_string());
    assert_eq!(trace["forward_merges"][0]["amount"], "2");
    let origins = before[1].as_ref().unwrap();
    assert_eq!(origins["confirmed"], true);
    assert_eq!(origins["complete"], true);
    assert_eq!(origins["origins"].as_array().unwrap().len(), 2);
    assert!(origins["origins"]
        .as_array()
        .unwrap()
        .iter()
        .all(|origin| origin["confirmed"] == true));
    let retired = {
        let prepared = checked
            .prepare_token_indexes_at(&local, Timestamp::new(base.anchor().cutoff()))
            .unwrap();
        let closed = prepared
            .body_requirements(&local)
            .unwrap()
            .close(Default::default())
            .unwrap();
        let plan = closed
            .prepare_retirement(Timestamp::new(base.anchor().cutoff()), 131_072, 64 << 20)
            .unwrap();
        let hashes = plan.hashes().to_vec();
        for hash in [burns[0], burns[1], previous, merged] {
            assert!(
                hashes.contains(&hash),
                "required origin fixture body was not eligible: {hash}"
            );
        }
        assert_eq!(plan.commit().unwrap().0, hashes.len());
        hashes
    };
    let count = local.block_store().block_count().unwrap();
    let metadata = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let after = origin_queries(
        &node,
        local.clone(),
        &fixture.params,
        service.registry.clone(),
        burns[0],
        merged,
    )
    .await;
    assert_eq!(after, before);
    assert!(service.requests.load(Ordering::SeqCst) > before_requests);
    assert_eq!(local.block_store().block_count().unwrap(), count);
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        metadata
    );
    for hash in &retired {
        assert!(!local.block_store().exists(hash).unwrap());
        assert_eq!(local.block_store().height_of_block(hash).unwrap(), None);
    }
    service.stop().await;
    local.force_sync().unwrap();
    drop(local);
    let local = Arc::new(LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
    let service = WitnessService::start(source, local.clone(), 0).await;
    assert_eq!(
        origin_queries(
            &node,
            local.clone(),
            &fixture.params,
            service.registry.clone(),
            burns[0],
            merged
        )
        .await,
        before
    );
    service.stop().await;
    let unavailable = origin_queries(
        &node,
        local.clone(),
        &fixture.params,
        Arc::new(RwLock::new(ConnectionRegistry::new())),
        burns[0],
        merged,
    )
    .await;
    assert!(
        unavailable.iter().all(Result::is_err),
        "missing peer history is not an empty origin result: {unavailable:?}"
    );
    assert_eq!(local.block_store().block_count().unwrap(), count);
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        metadata
    );
    for hash in &retired {
        assert!(!local.block_store().exists(hash).unwrap());
    }
}
