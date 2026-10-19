use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn certified_recovery_installs_merge_rows_and_rewinds_matching_journals() {
    for row_format in [false, true] {
        let (mut fixture, _source_dir, source, manifest, certificate) = selection::setup();
        let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
        let expected = authenticate(&source, &fixture.params, certificate.checkpoint.hash())
            .into_continuation();
        let (_local_dir, local) = cemented::sparse(&expected, &source);
        records(&local, &source, &fixture, &bytes);
        let mut batch = local.tx_begin_write().unwrap();
        let original = crate::graph_storage::load(&batch).unwrap();
        let original_holdings = crate::trst_storage::load(&batch, fixture.params.trst_expiry_secs)
            .unwrap()
            .save_wallets();
        if row_format {
            crate::graph_storage::stage_rows_for_test(&mut batch, &original).unwrap();
            let holdings =
                crate::trst_storage::load(&batch, fixture.params.trst_expiry_secs).unwrap();
            crate::trst_storage::stage_rows_for_test(&mut batch, &holdings).unwrap();
        }
        batch.commit().unwrap();
        let start = fixture.blocks.len();
        let at = expected.anchor().cutoff() + 1;
        let epoch = expected.anchor().hash();
        let burn = fixture.push(0, BlockType::Burn, at, |block| {
            block.payment_epoch = epoch;
            block.link = destination(8);
            block.brn_balance += 1;
        });
        let first = fixture.push(8, BlockType::Receive, at + 1, |block| {
            block.payment_epoch = epoch;
            block.link = burn;
            block.trst_balance += 1;
        });
        let burn = fixture.push(0, BlockType::Burn, at + 2, |block| {
            block.payment_epoch = epoch;
            block.link = destination(8);
            block.brn_balance += 1;
        });
        let second = fixture.push(8, BlockType::Receive, at + 3, |block| {
            block.payment_epoch = epoch;
            block.link = burn;
            block.trst_balance += 1;
        });
        let merged = fixture.push(8, BlockType::Merge, at + 4, |block| {
            block.payment_epoch = epoch;
            block.merge_sources = vec![first.into_tx_hash(), second.into_tx_hash()];
        });
        for block in &fixture.blocks[start..] {
            let mut batch = local.tx_begin_write().unwrap();
            crate::payment_transition::apply(&mut batch, block, &fixture.params, block.timestamp)
                .unwrap();
            batch.commit().unwrap();
        }
        let batch = local.tx_begin_write().unwrap();
        let graph = crate::graph_storage::load(&batch).unwrap();
        let holdings = crate::trst_storage::load(&batch, fixture.params.trst_expiry_secs)
            .unwrap()
            .save_wallets();
        assert_ne!(graph.to_bytes(), original.to_bytes());
        drop(batch);
        let local = Arc::new(local);
        let service = Service::start(Arc::new(source), Arc::clone(&local), 0).await;
        let checked = installed::recover_metadata_from_peers(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&service.registry),
        )
        .await
        .unwrap()
        .unwrap();
        service.stop().await;
        let projected = checked
            .project_token_indexes_at(&local, Timestamp::new(at + 4))
            .unwrap();
        let journal = projected
            .economic_changes()
            .into_iter()
            .find(|row| row.block == merged)
            .unwrap();
        assert!(journal.changes.iter().any(|change| change
            .key
            .starts_with(burst_trst::merger_graph::GraphRecord::STORAGE_PREFIX.as_bytes())));
        assert!(projected.economic_changes().iter().all(|row| row
            .changes
            .iter()
            .all(|change| change.key != b"merger_graph"
                && change.key != burst_trst::TrstEngine::meta_key().as_bytes())));
        let rewind: Vec<_> = projected
            .economic_changes()
            .iter()
            .rev()
            .map(|row| row.block)
            .collect();
        projected.stage_undo().unwrap().commit_for_test().unwrap();
        let mut batch = local.tx_begin_write().unwrap();
        assert!(crate::graph_storage::row_format(&batch).unwrap());
        assert!(crate::trst_storage::row_format(&batch).unwrap());
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
        batch.commit().unwrap();
    }
}
