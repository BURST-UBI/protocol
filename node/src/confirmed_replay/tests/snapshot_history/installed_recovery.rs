use super::*;
use crate::{confirmed_replay::snapshot::installed, connection_registry::ConnectionRegistry};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::{mpsc, RwLock};

mod authority;
mod compact_journal;
mod graph;
pub(in crate::confirmed_replay::tests) mod installation;
#[cfg(unix)]
mod publication_crash;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_candidate_peer_preparation_rechecks_authority_without_writes() {
    let (fixture, _source_dir, source, manifest, _) = selection::setup();
    let source = Arc::new(source);
    for change in 0..4 {
        let snapshot = authenticate(&source, &fixture.params, manifest.checkpoint.parent);
        let base =
            authenticate(&source, &fixture.params, manifest.checkpoint.parent).into_continuation();
        let (_local_dir, local) = cemented::sparse(&base, &source);
        let tip = format!(
            "checkpoint_chain/v1/dev/{}/head",
            base.anchor().genesis_hash()
        );
        local
            .meta_store()
            .put_meta(&tip, &base.anchor().height().to_be_bytes())
            .unwrap();
        let recovered = snapshot
            .recover_stored_successors(&local, |_, _, _| panic!("base has no successors"))
            .unwrap();
        let mut candidate = manifest.clone();
        match change {
            1 => local
                .meta_store()
                .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &[99; 32])
                .unwrap(),
            2 => local
                .meta_store()
                .put_meta(&tip, &(base.anchor().height() + 1).to_be_bytes())
                .unwrap(),
            3 => candidate.checkpoint.parent = BlockHash::new([99; 32]),
            _ => (),
        }
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let count = local.block_store().block_count().unwrap();
        let local = Arc::new(local);
        let service = Service::start(Arc::clone(&source), Arc::clone(&local), 0).await;
        let recovered = Arc::new(recovered);
        if change == 0 {
            let mut rejected = candidate.clone();
            rejected.checkpoint.parent = BlockHash::new([99; 32]);
            assert!(recovered
                .clone()
                .selection_coverage_shared(local.clone(), service.registry.clone(), rejected,)
                .await
                .is_err());
        }
        let result = recovered
            .clone()
            .selection_coverage_shared(Arc::clone(&local), Arc::clone(&service.registry), candidate)
            .await;
        // This selection uses the frozen base only, so no ancestry request is
        // necessary. Missing dependencies are covered by witness-service tests.
        assert_eq!(service.stop().await, 0);
        if change == 0 {
            let coverage = result.unwrap();
            assert_eq!(coverage.replay().manifest(), manifest);
            compact::assert_same(recovered.continuation.state(), base.state());
            assert_eq!(recovered.continuation.anchor().hash(), base.anchor().hash());
        } else {
            assert!(result.is_err(), "change {change}");
        }
        assert_eq!(local.block_store().block_count().unwrap(), count);
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

pub(in crate::confirmed_replay::tests) fn archive(
    source: &LmdbStore,
    fixture: &Fixture,
    epoch: BlockHash,
) -> Vec<u8> {
    let export = SnapshotExport::prepare(source, NetworkId::Dev, &fixture.params, epoch).unwrap();
    let signatures: Vec<_> = (0..7)
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
    bytes
}

pub(in crate::confirmed_replay::tests) struct Service {
    pub(in crate::confirmed_replay::tests) registry: Arc<RwLock<ConnectionRegistry>>,
    requests: Arc<AtomicUsize>,
    worker: tokio::task::JoinHandle<()>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn compact_candidate_rejects_authority_changes_during_peer_retrieval() {
    let (mut fixture, _source_dir, source, original, _) = selection::setup();
    let parent = original.checkpoint.parent;
    let cutoff = original.checkpoint.cutoff;
    fixture.push(0, BlockType::ChangeRepresentative, cutoff, |block| {
        block.payment_epoch = parent;
    });
    let block = fixture.blocks.last().unwrap();
    let (_full_dir, full, heads) = fixture.store();
    let prefix = format!("checkpoint_chain/v1/dev/{}", fixture.blocks[0].hash);
    full.meta_store()
        .put_meta(
            &format!("{prefix}/1"),
            &source
                .meta_store()
                .get_meta(&format!("{prefix}/1"))
                .unwrap(),
        )
        .unwrap();
    full.meta_store()
        .put_meta(&format!("{prefix}/head"), &1u64.to_be_bytes())
        .unwrap();
    let manifest = CertifiedCheckpoint::prepare(
        &full,
        NetworkId::Dev,
        &fixture.params,
        cutoff,
        &heads,
        &address(20),
    )
    .unwrap()
    .manifest();
    let source = Arc::new(source);
    for (change, discover) in [
        (0, false),
        (1, false),
        (4, false),
        (0, true),
        (1, true),
        (4, true),
        (6, true),
    ] {
        let snapshot = authenticate(&source, &fixture.params, parent);
        let base = authenticate(&source, &fixture.params, parent).into_continuation();
        let (_dir, local) = cemented::sparse(&base, &source);
        local
            .meta_store()
            .put_meta(&format!("{prefix}/head"), &1u64.to_be_bytes())
            .unwrap();
        local
            .meta_store()
            .put_meta(
                &format!("checkpoint_history/v1/{}", block.hash),
                &bincode::serialize(block).unwrap(),
            )
            .unwrap();
        if discover {
            cemented::append(&local, block, true);
        }
        let mut expected_accounts = local
            .tx_begin_write()
            .unwrap()
            .bounded_accounts(131_072)
            .unwrap();
        if change == 6 {
            expected_accounts
                .iter_mut()
                .find(|account| account.address == address(0))
                .unwrap()
                .trst_balance += 1;
        }
        let recovered = snapshot
            .recover_stored_successors(&local, |_, _, _| unreachable!())
            .unwrap();
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let local = Arc::new(local);
        let service = Service::start(Arc::clone(&source), Arc::clone(&local), change).await;
        let result = if discover {
            Arc::new(recovered)
                .discover_cemented_selection_from_peers(
                    Arc::clone(&local),
                    Arc::clone(&service.registry),
                    cutoff,
                )
                .await
                .map(|coverage| coverage.unwrap().replay().manifest())
        } else {
            recovered
                .selection_coverage_from_peers(
                    Arc::clone(&local),
                    Arc::clone(&service.registry),
                    manifest.clone(),
                )
                .await
                .map(|(_, coverage)| coverage.replay().manifest())
        };
        assert!(service.stop().await > 0);
        assert_eq!(
            bincode::serialize(
                &local
                    .tx_begin_write()
                    .unwrap()
                    .bounded_accounts(131_072)
                    .unwrap()
            )
            .unwrap(),
            bincode::serialize(&expected_accounts).unwrap()
        );
        if change == 0 {
            assert_eq!(result.unwrap(), manifest);
            assert_eq!(
                local
                    .tx_begin_write()
                    .unwrap()
                    .checkpoint_metadata(b"")
                    .unwrap(),
                before
            );
        } else {
            assert!(result.is_err(), "change {change}");
            // Only the injected marker or account change may survive. Discovery
            // must neither write a signer journal nor install a new checkpoint.
            let after = local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap();
            let key = if change == 1 {
                burst_store::meta::PAYMENT_EPOCH_KEY.to_owned()
            } else {
                format!("{prefix}/head")
            };
            let unchanged = |rows: Vec<(Vec<u8>, Vec<u8>)>| {
                rows.into_iter()
                    .filter(|(name, _)| name != key.as_bytes())
                    .collect::<Vec<_>>()
            };
            assert_eq!(unchanged(after), unchanged(before));
        }
    }
}

impl Service {
    pub(in crate::confirmed_replay::tests) async fn start(
        source: Arc<LmdbStore>,
        local: Arc<LmdbStore>,
        change: usize,
    ) -> Self {
        let registry = Arc::new(RwLock::new(ConnectionRegistry::new()));
        let (sender, mut receiver) = mpsc::channel::<crate::checkpoint_sync::witness::Command>(1);
        registry.write().await.set_witness_requests(sender);
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&requests);
        let worker = tokio::spawn(async move {
            while let Some(command) = receiver.recv().await {
                let index = counter.fetch_add(1, Ordering::SeqCst);
                if index == 0 {
                    match change {
                        1 => local
                            .meta_store()
                            .put_meta(
                                burst_store::meta::PAYMENT_EPOCH_KEY,
                                BlockHash::new([77; 32]).as_bytes(),
                            )
                            .unwrap(),
                        2 => local
                            .meta_store()
                            .put_meta(installed::BASE_KEY, b"changed during retrieval")
                            .unwrap(),
                        3 | 4 => {
                            let rows = local
                                .tx_begin_write()
                                .unwrap()
                                .checkpoint_metadata(b"checkpoint_chain/v1/")
                                .unwrap();
                            let ending = if change == 3 {
                                b"/2".as_slice()
                            } else {
                                b"/head".as_slice()
                            };
                            let key = rows
                                .iter()
                                .find(|(key, _)| key.ends_with(ending))
                                .unwrap()
                                .0
                                .clone();
                            local
                                .meta_store()
                                .put_meta(
                                    std::str::from_utf8(&key).unwrap(),
                                    b"changed during retrieval",
                                )
                                .unwrap();
                        }
                        5 => {
                            let rows = local
                                .tx_begin_write()
                                .unwrap()
                                .checkpoint_metadata(b"checkpoint_round/v1/")
                                .unwrap();
                            assert_eq!(rows.len(), 1);
                            let (key, mut value) = rows.into_iter().next().unwrap();
                            *value.last_mut().unwrap() ^= 1;
                            local
                                .meta_store()
                                .put_meta(std::str::from_utf8(&key).unwrap(), &value)
                                .unwrap();
                        }
                        6 => {
                            let mut batch = local.tx_begin_write().unwrap();
                            let mut account = batch.get_account(&address(0)).unwrap();
                            account.trst_balance += 1;
                            batch.put_account_info(&account, false).unwrap();
                            batch.commit().unwrap();
                        }
                        7 => {
                            let bytes = source.meta_store().get_meta(installed::BASE_KEY).unwrap();
                            let mut batch = local.tx_begin_write().unwrap();
                            batch.put_meta(installed::BASE_KEY, &bytes).unwrap();
                            for (key, _) in
                                batch.checkpoint_metadata(b"checkpoint_chain/v1/").unwrap()
                            {
                                if !key.ends_with(b"/head") {
                                    batch
                                        .delete_meta(std::str::from_utf8(&key).unwrap())
                                        .unwrap();
                                }
                            }
                            batch.commit().unwrap();
                        }
                        9 => {
                            let mut batch = local.tx_begin_write().unwrap();
                            let (key, bytes) = batch
                                .checkpoint_metadata(b"checkpoint_chain/v1/")
                                .unwrap()
                                .into_iter()
                                .find(|(key, _)| key.ends_with(b"/2"))
                                .unwrap();
                            let mut record: (
                                u8,
                                burst_consensus::checkpoint::CheckpointCertificate,
                                Vec<crate::confirmed_history::HistoryHead>,
                                WalletAddress,
                                Option<burst_vrf::RandomOutput>,
                            ) = bincode::deserialize(&bytes).unwrap();
                            let beacons: Vec<burst_vrf::DrandBeacon> =
                                serde_json::from_str(include_str!(concat!(
                                    env!("CARGO_MANIFEST_DIR"),
                                    "/tests/fixtures/selection-beacons.json"
                                )))
                                .unwrap();
                            record.4 = Some(
                                burst_vrf::DrandClient::beacon_to_random_output(&beacons[0])
                                    .unwrap(),
                            );
                            batch
                                .put_meta(
                                    std::str::from_utf8(&key).unwrap(),
                                    &bincode::serialize(&record).unwrap(),
                                )
                                .unwrap();
                            batch.commit().unwrap();
                        }
                        _ => (),
                    }
                }
                let source = Arc::clone(&source);
                let local = Arc::clone(&local);
                let result = tokio::task::spawn_blocking(move || {
                    witness_transfer::exchange(command.roots, command.wanted, &source, &local)
                })
                .await
                .unwrap();
                let _ = command.result.send(result);
            }
        });
        Self {
            registry,
            requests,
            worker,
        }
    }

    pub(in crate::confirmed_replay::tests) async fn stop(self) -> usize {
        drop(self.registry);
        tokio::time::timeout(std::time::Duration::from_secs(5), self.worker)
            .await
            .unwrap()
            .unwrap();
        self.requests.load(Ordering::SeqCst)
    }
}

fn records(local: &LmdbStore, source: &LmdbStore, fixture: &Fixture, bytes: &[u8]) {
    let prefix = format!("checkpoint_chain/v1/dev/{}", fixture.blocks[0].hash);
    for key in [format!("{prefix}/2"), format!("{prefix}/head")] {
        local
            .meta_store()
            .put_meta(&key, &source.meta_store().get_meta(&key).unwrap())
            .unwrap();
    }
    local
        .meta_store()
        .put_meta(installed::BASE_KEY, bytes)
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_recovery_preserves_admitted_identity_arrival_order() {
    for order in [[0, 1, 2], [1, 0, 2], [1, 2, 0]] {
        // Bootstrap identity arrival is the subject of this test. Its base
        // must be an authority cut, not a forbidden pre-bootstrap selection.
        let mut fixture = Fixture::selection_with(|params| params.bootstrap_exit_threshold = 10);
        let (_source_dir, source, _) = fixture.store();
        let roster = crate::checkpoint_roster::PreparedRoster::prepare_next(
            &source,
            NetworkId::Dev,
            &fixture.params,
            1_595_434_049,
        )
        .unwrap()
        .unwrap();
        let epoch = roster.manifest().checkpoint.hash();
        crate::checkpoint_store::chain::accept_manifest(
            &source,
            NetworkId::Dev,
            &fixture.params,
            certified_votes::certify(&roster.manifest().checkpoint, &[0]),
            &crate::checkpoint_agreement::ReplayManifest::Roster(roster.manifest().clone()),
        )
        .unwrap();
        let bytes = archive(&source, &fixture, epoch);
        let expected = authenticate(&source, &fixture.params, epoch).into_continuation();
        let (_local_dir, local) = cemented::sparse(&expected, &source);
        local
            .meta_store()
            .put_meta(installed::BASE_KEY, &bytes)
            .unwrap();
        local
            .meta_store()
            .put_meta(
                &format!("checkpoint_chain/v1/dev/{}/head", fixture.blocks[0].hash),
                &expected.anchor().height().to_be_bytes(),
            )
            .unwrap();
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .verified_account_count()
                .unwrap(),
            9
        );
        let at = expected.anchor().cutoff() + 1;
        fixture.push(0, BlockType::Endorse, at, |block| {
            block.payment_epoch = expected.anchor().hash();
            block.brn_balance += 1;
            block.link = destination(30);
        });
        fixture.push(30, BlockType::Open, at + 1, |block| {
            block.payment_epoch = expected.anchor().hash();
        });
        fixture.push(30, BlockType::ChangeRepresentative, at + 2, |block| {
            block.payment_epoch = expected.anchor().hash();
            block.representative = address(0);
        });
        let blocks = &fixture.blocks[fixture.blocks.len() - 3..];
        for index in order {
            let block = &blocks[index];
            let mut batch = local.tx_begin_write().unwrap();
            if block.block_type == BlockType::Endorse {
                crate::payment_transition::authenticate_contents(block, &fixture.params).unwrap();
                let previous = batch.get_account(&block.account).unwrap();
                let prior: StateBlock =
                    bincode::deserialize(&batch.get_block(&block.previous).unwrap()).unwrap();
                let mut engines =
                    crate::payment_transition::restore(&batch, &fixture.params).unwrap();
                let effect = crate::economic_candidate::apply(
                    block,
                    crate::economic_candidate::EconomicContext {
                        account: Some(&previous),
                        previous_brn: prior.brn_balance,
                        pending: None,
                        params: &fixture.params,
                    },
                    &mut engines.brn,
                    &mut engines.trst,
                )
                .unwrap();
                crate::ledger_updater::stage_block(
                    &mut batch,
                    block,
                    Some(&previous),
                    prior.brn_balance,
                    effect.pending.as_ref(),
                    effect.indices.as_ref(),
                )
                .unwrap();
                let snapshot = burst_verification::OrchestratorSnapshot::decode(
                    &batch
                        .get_meta(crate::endorsement_commit::SNAPSHOT_KEY)
                        .unwrap()
                        .unwrap(),
                )
                .unwrap();
                let mut verification =
                    burst_verification::VerificationOrchestrator::restore(snapshot);
                let crate::ledger_bridge::EconomicResult::Endorse { burn_amount, .. } =
                    effect.effect
                else {
                    panic!("expected endorsement");
                };
                crate::endorsement_commit::apply(
                    &mut batch,
                    block,
                    burn_amount,
                    NetworkId::Dev,
                    &fixture.params,
                    &mut verification,
                    (&mut engines.brn, &mut engines.trst),
                )
                .unwrap();
                crate::ledger_updater::persist_economics(&mut batch, &engines.brn, &engines.trst)
                    .unwrap();
            } else {
                crate::payment_transition::apply(
                    &mut batch,
                    block,
                    &fixture.params,
                    block.timestamp,
                )
                .unwrap();
            }
            batch.commit().unwrap();
        }
        let original = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_lifecycle()
            .unwrap();
        let local = Arc::new(local);
        let source = Arc::new(source);
        let service = Service::start(Arc::clone(&source), Arc::clone(&local), 0).await;
        assert!(
            crate::checkpoint_store::chain::load(&local, NetworkId::Dev, &fixture.params).is_err()
        );
        crate::checkpoint_sync::authority::authenticate(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&service.registry),
        )
        .await
        .unwrap();
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
        let projected = checked.project_token_indexes(&local).unwrap();
        let hashes: Vec<_> = projected
            .economic_changes()
            .iter()
            .map(|record| record.block)
            .collect();
        assert_eq!(hashes, order.map(|index| blocks[index].hash));
        let staged = projected.stage_undo().unwrap();
        for block in blocks {
            assert!(staged.journal(&block.hash).unwrap().is_some());
        }
        staged.commit_for_test().unwrap();
        let service = Service::start(source, Arc::clone(&local), 0).await;
        let recovered = installed::recover_metadata_from_peers(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&service.registry),
        )
        .await
        .unwrap()
        .unwrap();
        service.stop().await;
        let runtime_dir = tempfile::tempdir().unwrap();
        let mut node = crate::node::BurstNode::new(crate::config::NodeConfig {
            data_dir: runtime_dir.path().to_path_buf(),
            network: NetworkId::Dev,
            params: fixture.params.clone(),
            enable_representative: false,
            nts_sources: vec![],
            ..crate::config::NodeConfig::default()
        })
        .await
        .unwrap();
        node.store = Arc::clone(&local);
        node.verification_fixture_clock = Some(Arc::new(|| None));
        node.publish_recovered(recovered, Timestamp::new(expected.anchor().cutoff()))
            .await
            .unwrap();
        assert!(node.connection_registry.read().await.is_recovering());
        assert_eq!(
            node.active_elections.read().await.authority_epoch(),
            Some(expected.anchor().hash())
        );
        let persisted_trst = crate::trst_storage::load(
            &local.tx_begin_write().unwrap(),
            fixture.params.trst_expiry_secs,
        )
        .unwrap();
        assert!(node
            .trst_engine
            .lock()
            .await
            .wallet_snapshot_eq(&persisted_trst));
        let verification = node.verification_orchestrator.lock().await.snapshot();
        let persisted = burst_verification::OrchestratorSnapshot::decode(
            &local
                .meta_store()
                .get_meta(crate::endorsement_commit::SNAPSHOT_KEY)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(verification.encode().unwrap(), persisted.encode().unwrap());
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_lifecycle()
                .unwrap(),
            original
        );
        let mut batch = local.tx_begin_write().unwrap();
        for hash in hashes.iter().rev() {
            batch.rewind_block(hash).unwrap();
        }
        assert!(batch.get_account(&address(30)).is_err());
        assert_eq!(batch.verified_account_count().unwrap(), 9);
        let genesis = batch.get_account(&address(0)).unwrap();
        let before = expected.state().account(&address(0)).unwrap();
        assert_eq!(genesis.head, before.head);
        assert_eq!(genesis.block_count, before.block_count);
        assert_eq!(genesis.confirmation_height, before.confirmation_height);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_recovery_derives_payment_deltas_without_trusting_local_undo() {
    payment_delta_recovery(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_recovery_derives_row_payment_deltas_without_trusting_local_undo() {
    payment_delta_recovery(true).await;
}

async fn payment_delta_recovery(row_format: bool) {
    use burst_store_lmdb::block_undo::UndoTable;
    let (mut fixture, _dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let expected =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    let (_dir, local) = cemented::sparse(&expected, &source);
    records(&local, &source, &fixture, &bytes);
    if row_format {
        let mut batch = local.tx_begin_write().unwrap();
        let engine = crate::trst_storage::load(&batch, fixture.params.trst_expiry_secs).unwrap();
        crate::trst_storage::stage_rows_for_test(&mut batch, &engine).unwrap();
        let graph = crate::graph_storage::load(&batch).unwrap();
        crate::graph_storage::stage_rows_for_test(&mut batch, &graph).unwrap();
        batch.commit().unwrap();
    }
    let at = expected.anchor().cutoff() + 1;
    let burn = fixture.push(0, BlockType::Burn, at, |block| {
        block.payment_epoch = expected.anchor().hash();
        block.link = destination(30);
        block.brn_balance += 1;
    });
    let received = fixture.push(30, BlockType::Open, at + 1, |block| {
        block.payment_epoch = expected.anchor().hash();
        block.link = burn;
        block.trst_balance = 1;
    });
    let returned_burn = fixture.push(0, BlockType::Burn, at + 2, |block| {
        block.payment_epoch = expected.anchor().hash();
        block.link = destination(30);
        block.brn_balance += 1;
    });
    let rejection = fixture.push(30, BlockType::RejectReceive, at + 3, |block| {
        block.payment_epoch = expected.anchor().hash();
        block.link = returned_burn;
    });
    let claimed_return_block = fixture.push(0, BlockType::Receive, at + 4, |block| {
        block.payment_epoch = expected.anchor().hash();
        block.link = rejection;
        block.trst_balance += 1;
    });
    let blocks = &fixture.blocks[fixture.blocks.len() - 5..];
    let mut recorded = Vec::new();
    for block in blocks {
        let mut batch = local.tx_begin_write().unwrap();
        crate::payment_transition::apply(&mut batch, block, &fixture.params, block.timestamp)
            .unwrap();
        batch.commit().unwrap();
        recorded.push(
            local
                .tx_begin_write()
                .unwrap()
                .block_undo(&block.hash)
                .unwrap()
                .unwrap(),
        );
    }
    let mut batch = local.tx_begin_write().unwrap();
    let mut payment = crate::payment_transition::restore(&batch, &fixture.params).unwrap();
    let later = burst_types::Timestamp::new(at + fixture.params.trst_expiry_secs + 2);
    payment.trst.flush_all_expired(later);
    crate::ledger_updater::persist_trst(&mut batch, &payment.trst).unwrap();
    crate::ledger_updater::refresh_token_counters(&mut batch, &address(30), &payment.trst, later)
        .unwrap();
    crate::ledger_updater::refresh_token_counters(&mut batch, &address(0), &payment.trst, later)
        .unwrap();
    assert_eq!(batch.get_account(&address(30)).unwrap().expired_trst, 1);
    batch.commit().unwrap();
    let local = Arc::new(local);
    let source = Arc::new(source);
    let service = Service::start(Arc::clone(&source), Arc::clone(&local), 0).await;
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
    let projected = checked.project_token_indexes(&local).unwrap();
    let derived = projected.economic_changes();
    assert_eq!(derived.len(), 5);
    assert_eq!(
        derived
            .iter()
            .map(|row| row.block)
            .collect::<std::collections::HashSet<_>>(),
        recorded
            .iter()
            .map(|row| row.block)
            .collect::<std::collections::HashSet<_>>()
    );
    let position = |hash| derived.iter().position(|row| row.block == hash).unwrap();
    for (before, after) in [
        (burn, received),
        (burn, returned_burn),
        (received, rejection),
        (returned_burn, rejection),
        (rejection, claimed_return_block),
    ] {
        assert!(
            position(before) < position(after),
            "replay must preserve causal dependencies"
        );
    }
    for actual in &derived {
        let expected = recorded
            .iter()
            .find(|row| row.block == actual.block)
            .unwrap();
        assert_eq!(actual.block, expected.block);
        assert_eq!(actual.account, expected.account);
        assert_eq!(actual.height, expected.height);
        for table in [
            UndoTable::Blocks,
            UndoTable::Heights,
            UndoTable::BlockHeights,
            UndoTable::Frontiers,
            UndoTable::Accounts,
            UndoTable::Pending,
            UndoTable::BrnWallets,
            UndoTable::TrstOrigins,
            UndoTable::TrstExpiry,
            UndoTable::TrstReverse,
        ] {
            let actual: Vec<_> = actual
                .changes
                .iter()
                .filter(|change| change.table == table)
                .collect();
            let expected: Vec<_> = expected
                .changes
                .iter()
                .filter(|change| change.table == table)
                .collect();
            assert_eq!(actual, expected, "economic table {table:?}");
        }
    }
    let change_for = |hash| derived.iter().find(|row| row.block == hash).unwrap();
    let inserted = change_for(burn)
        .changes
        .iter()
        .find(|change| change.table == UndoTable::Pending)
        .unwrap();
    let consumed = change_for(received)
        .changes
        .iter()
        .find(|change| change.table == UndoTable::Pending)
        .unwrap();
    assert!(inserted.before.is_none());
    assert_eq!(inserted.key, consumed.key);
    assert_eq!(inserted.after, consumed.before);
    assert!(consumed.after.is_none());
    let sent = change_for(returned_burn)
        .changes
        .iter()
        .find(|row| row.table == UndoTable::Pending)
        .unwrap();
    let returned = change_for(rejection)
        .changes
        .iter()
        .find(|row| row.table == UndoTable::Pending && row.before.is_none())
        .unwrap();
    let claimed_return = change_for(claimed_return_block)
        .changes
        .iter()
        .find(|row| row.table == UndoTable::Pending)
        .unwrap();
    assert_ne!(sent.key, returned.key);
    assert_eq!(
        sent.after, returned.after,
        "rejection must retain the exact source and provenance"
    );
    assert_eq!(returned.key, claimed_return.key);
    assert_eq!(returned.after, claimed_return.before);
    assert!(claimed_return.after.is_none());
    projected.verify_unchanged().unwrap();
    let staged = projected.stage_undo().unwrap();
    for original in &recorded {
        assert_eq!(
            staged.journal(&original.block).unwrap().unwrap().height,
            original.height
        );
    }
    drop(staged);
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .get_account(&address(30))
            .unwrap()
            .expired_trst,
        1
    );
    for original in &recorded {
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .block_undo(&original.block)
                .unwrap()
                .as_ref(),
            Some(original)
        );
    }
    let successor = fixture.push(0, BlockType::ChangeRepresentative, at + 5, |block| {
        block.payment_epoch = expected.anchor().hash();
        block.representative = address(3);
    });
    let block = fixture.blocks.last().unwrap();
    let mut batch = local.tx_begin_write().unwrap();
    crate::payment_transition::apply(&mut batch, block, &fixture.params, block.timestamp).unwrap();
    batch.commit().unwrap();
    let cemented = crate::causal_cementation::cement(&local, burn).unwrap();
    assert!(cemented.complete);
    assert_eq!(cemented.hashes, vec![burn]);
    let confirmation = recorded[0].height;
    let service = Service::start(Arc::clone(&source), Arc::clone(&local), 0).await;
    let stale_clock = installed::recover_metadata_from_peers(
        Arc::clone(&local),
        NetworkId::Dev,
        fixture.params.clone(),
        Arc::clone(&service.registry),
    )
    .await
    .unwrap()
    .unwrap();
    let metadata_before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    assert!(stale_clock
        .project_token_indexes_at(
            &local,
            burst_types::Timestamp::new(expected.anchor().cutoff() - 1),
        )
        .is_err());
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        metadata_before
    );
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
    let projected = checked.project_token_indexes_at(&local, later).unwrap();
    assert_eq!(projected.economic_changes().len(), 5);
    assert!(!projected
        .economic_changes()
        .iter()
        .any(|record| record.block == burn));
    let successor = projected
        .economic_changes()
        .into_iter()
        .find(|record| record.block == successor)
        .unwrap();
    let account = successor
        .changes
        .iter()
        .find(|change| change.table == UndoTable::Accounts)
        .unwrap();
    for bytes in [&account.before, &account.after] {
        let image: burst_store::account::AccountInfo =
            bincode::deserialize(bytes.as_ref().unwrap()).unwrap();
        assert_eq!(image.confirmation_height, confirmation);
    }
    let rewind: Vec<_> = projected
        .economic_changes()
        .iter()
        .rev()
        .map(|record| record.block)
        .collect();
    projected.verify_unchanged().unwrap();
    // Test-only publication into a disposable fixture, never a running node.
    let staged = projected.stage_undo().unwrap();
    let runtime = staged.runtime();
    assert_eq!(runtime.cache_at, later);
    assert_eq!(
        runtime.anchor.hash(),
        local.meta_store().payment_epoch().unwrap()
    );
    let runtime_wallets = runtime.trst.save_wallets();
    let runtime_graph = runtime.trst.merger_graph.to_bytes();
    let runtime_frontiers = runtime.frontiers.clone();
    let runtime_counts = runtime.ledger_counts;
    staged.commit_for_test().unwrap();
    {
        let batch = local.tx_begin_write().unwrap();
        assert_eq!(batch.ledger_counts().unwrap(), runtime_counts);
        assert_eq!(
            crate::trst_storage::load(&batch, fixture.params.trst_expiry_secs)
                .unwrap()
                .save_wallets(),
            runtime_wallets
        );
        assert_eq!(
            crate::graph_storage::load(&batch).unwrap().to_bytes(),
            runtime_graph
        );
        for (address, head) in runtime_frontiers {
            assert_eq!(batch.get_account(&address).unwrap().head, head);
        }
    }
    local.env().force_sync().unwrap();
    assert_eq!(Arc::strong_count(&local), 1);
    drop(local);
    let local = Arc::new(LmdbStore::open(_dir.path(), 32, 32 << 20).unwrap());
    let service = Service::start(Arc::clone(&source), Arc::clone(&local), 0).await;
    let restarted = installed::recover_metadata_from_peers(
        Arc::clone(&local),
        NetworkId::Dev,
        fixture.params.clone(),
        Arc::clone(&service.registry),
    )
    .await
    .unwrap()
    .unwrap();
    service.stop().await;
    let repeated = restarted.project_token_indexes_at(&local, later).unwrap();
    let repeated_order: Vec<_> = repeated
        .economic_changes()
        .iter()
        .rev()
        .map(|record| record.block)
        .collect();
    assert_eq!(repeated_order, rewind);
    let repeated = repeated.stage_undo().unwrap();
    let repeated_records: Vec<_> = rewind
        .iter()
        .map(|hash| repeated.journal(hash).unwrap().unwrap())
        .collect();
    drop(repeated);
    let mut batch = local.tx_begin_write().unwrap();
    for expected in repeated_records {
        assert_eq!(batch.block_undo(&expected.block).unwrap(), Some(expected));
    }
    assert!(batch.block_undo(&burn).unwrap().is_none());
    let receiver = batch.get_account(&address(30)).unwrap();
    assert_eq!(receiver.trst_balance, 1);
    assert_eq!(receiver.expired_trst, 1);
    assert_eq!(receiver.revoked_trst, 0);
    let restored = crate::payment_transition::restore(&batch, &fixture.params).unwrap();
    let portfolio = restored.trst.get_portfolio(&address(30)).unwrap();
    assert_eq!(portfolio.cached_transferable, 0);
    assert_eq!(
        portfolio
            .tokens
            .iter()
            .map(|token| token.amount)
            .sum::<u128>(),
        1
    );
    let account = batch.get_account(&address(0)).unwrap();
    assert_eq!(account.confirmation_height, confirmation);
    assert_eq!(account.block_count, confirmation + 3);
    for hash in rewind {
        batch.rewind_block(&hash).unwrap();
    }
    let account = batch.get_account(&address(0)).unwrap();
    assert_eq!(account.head, burn);
    assert_eq!(account.confirmation_height, confirmation);
    assert_eq!(account.block_count, confirmation);
    assert!(batch.get_account(&address(30)).is_err());
    assert_eq!(
        batch
            .get_pending(&address(30), &burn.into_tx_hash())
            .unwrap()
            .amount,
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_recovery_allows_later_expiry_cache_but_rejects_changed_token_rights() {
    let mut fixture = Fixture::selection();
    let burn = fixture.push(0, BlockType::Burn, 1_595_434_040, |block| {
        block.link = destination(30);
        block.brn_balance += 1;
    });
    fixture.push(30, BlockType::Open, 1_595_434_041, |block| {
        block.link = burn;
        block.trst_balance = 1;
    });
    let (_dir, source, snapshot) = snapshot(&fixture);
    let epoch = snapshot.anchor().hash();
    let bytes = archive(&source, &fixture, epoch);
    let expected = snapshot.into_continuation();
    let (_dir, local) = cemented::sparse(&expected, &source);
    local
        .meta_store()
        .put_meta(installed::BASE_KEY, &bytes)
        .unwrap();
    for (key, value) in source
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"checkpoint_chain/v1/")
        .unwrap()
    {
        local
            .meta_store()
            .put_meta(std::str::from_utf8(&key).unwrap(), &value)
            .unwrap();
    }
    let mut batch = local.tx_begin_write().unwrap();
    let mut payment = crate::payment_transition::restore(&batch, &fixture.params).unwrap();
    assert_eq!(
        payment
            .trst
            .get_portfolio(&address(30))
            .unwrap()
            .tokens
            .len(),
        1
    );
    let later = burst_types::Timestamp::new(
        expected.anchor().cutoff() + fixture.params.trst_expiry_secs + 1,
    );
    payment.trst.flush_all_expired(later);
    assert_eq!(
        payment.trst.get_portfolio(&address(30)).unwrap().tokens[0].state,
        burst_types::TrstState::Expired
    );
    batch
        .put_meta(
            burst_trst::TrstEngine::meta_key(),
            &payment.trst.save_wallets(),
        )
        .unwrap();
    let mut account = batch.get_account(&address(30)).unwrap();
    crate::ledger_updater::update_token_counters(&mut account, &payment.trst, later).unwrap();
    batch.put_account_info(&account, false).unwrap();
    batch.commit().unwrap();
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
    checked
        .verify_unchanged(&local.tx_begin_write().unwrap())
        .unwrap();
    for case in 0..2 {
        let mut batch = local.tx_begin_write().unwrap();
        let mut altered = payment.trst.clone();
        let token = &mut altered.get_portfolio_mut(&address(30)).unwrap().tokens[0];
        if case == 0 {
            token.holder = address(31);
        } else {
            token.state = burst_types::TrstState::Revoked;
        }
        batch
            .put_meta(burst_trst::TrstEngine::meta_key(), &altered.save_wallets())
            .unwrap();
        assert!(
            checked.verify_unchanged(&batch).is_err(),
            "token rights case {case}"
        );
    }
    assert_eq!(
        local
            .account_store()
            .get_account(&address(30))
            .unwrap()
            .expired_trst,
        1
    );
    use burst_store::trst_index::TrstIndexStore;
    use burst_store_lmdb::block_undo::UndoTable;
    let index = local.trst_index_store();
    let token = burn.into_tx_hash();
    index.delete_token(&token).unwrap();
    let foreign = TxHash::new([77; 32]);
    index.put_origin_index(&foreign, &foreign).unwrap();
    let projected = checked.project_token_indexes(&local).unwrap();
    assert!(projected.economic_changes().is_empty());
    projected.verify_unchanged().unwrap();
    assert_eq!(
        projected
            .metadata()
            .recovered()
            .continuation
            .anchor()
            .hash(),
        epoch
    );
    let reverse = projected
        .transaction()
        .undo_value(UndoTable::TrstReverse, token.as_bytes())
        .unwrap()
        .unwrap();
    assert_eq!(&reverse[..32], token.as_bytes());
    assert_eq!(
        &reverse[32..],
        &1_595_434_040u64
            .saturating_add(fixture.params.trst_expiry_secs)
            .to_be_bytes()
    );
    assert!(projected
        .transaction()
        .undo_value(UndoTable::TrstReverse, foreign.as_bytes())
        .unwrap()
        .is_none());
    drop(projected);
    assert!(index.get_by_origin(&token).unwrap().is_empty());
    assert_eq!(index.get_by_origin(&foreign).unwrap(), vec![foreign]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_recovery_rechecks_economic_caches_without_repairing_them() {
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let expected =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    let (_dir, local) = cemented::sparse(&expected, &source);
    records(&local, &source, &fixture, &bytes);
    let source = Arc::new(source);
    let local = Arc::new(local);
    let service = Service::start(source, Arc::clone(&local), 0).await;
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
    for case in 0..11 {
        let mut batch = local.tx_begin_write().unwrap();
        match case {
            0 => batch.put_brn_meta(b"next_stake_id", b"bad").unwrap(),
            1 => batch
                .put_meta(burst_trst::TrstEngine::meta_key(), b"bad")
                .unwrap(),
            2 => batch.put_meta("merger_graph", b"bad").unwrap(),
            3 => {
                let pending = burst_store::pending::PendingInfo {
                    source: address(0),
                    amount: 1,
                    timestamp: burst_types::Timestamp::new(1),
                    provenance: Vec::new(),
                    return_credit: false,
                };
                batch
                    .put_pending(
                        &address(1),
                        &[77; 32],
                        &bincode::serialize(&pending).unwrap(),
                    )
                    .unwrap();
            }
            4 => {
                let mut params = fixture.params.clone();
                params.trst_expiry_secs += 1;
                batch
                    .put_brn_meta(b"protocol_params", &bincode::serialize(&params).unwrap())
                    .unwrap();
            }
            5 => batch
                .put_brn_meta(burst_verification::bootstrap::BootstrapPhase::KEY, b"bad")
                .unwrap(),
            6 => batch
                .put_meta(crate::endorsement_commit::SNAPSHOT_KEY, b"bad")
                .unwrap(),
            7 => batch.put_brn_meta(b"governance_engine", b"bad").unwrap(),
            8 => batch.put_meta("consti_engine", b"bad").unwrap(),
            9 => batch
                .put_meta(burst_governance::DelegationEngine::meta_key(), b"bad")
                .unwrap(),
            10 => batch
                .put_meta("verification_stake/foreign", b"bad")
                .unwrap(),
            _ => unreachable!(),
        }
        assert!(
            checked.verify_unchanged(&batch).is_err(),
            "cache case {case}"
        );
        // Abort only the test mutation. Recovery itself must not repair data.
        drop(batch);
        checked
            .verify_unchanged(&local.tx_begin_write().unwrap())
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_recovery_checks_provisional_chains_without_confirming_them() {
    let (mut fixture, _dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let epoch = certificate.checkpoint.hash();
    let expected = authenticate(&source, &fixture.params, epoch).into_continuation();
    let at = expected.anchor().cutoff() + 1;
    fixture.push(0, BlockType::ChangeRepresentative, at, |block| {
        block.payment_epoch = epoch;
        block.representative = address(3);
    });
    let good = fixture.blocks.last().unwrap().clone();
    let source = Arc::new(source);
    for case in 0..6 {
        let (_dir, local) = cemented::sparse(&expected, &source);
        records(&local, &source, &fixture, &bytes);
        let mut block = good.clone();
        match case {
            1 => {
                block.signature = burst_types::Signature([99; 64]);
            }
            2 => {
                block.params_hash = BlockHash::new([99; 32]);
                sign(0, &mut block);
            }
            3 => {
                block.payment_epoch = BlockHash::new([99; 32]);
                sign(0, &mut block);
            }
            4 => {
                block.trst_balance += 1;
                sign(0, &mut block);
            }
            _ => (),
        }
        let mut batch = local.tx_begin_write().unwrap();
        let mut account = batch.get_account(&block.account).unwrap();
        let confirmation = account.confirmation_height;
        account.head = block.hash;
        account.block_count += 1;
        account.representative = block.representative.clone();
        batch.put_frontier(&account.address, &account.head).unwrap();
        if case == 5 {
            account.representative = address(4);
        }
        batch
            .put_block_with_account(
                &block.hash,
                &bincode::serialize(&block).unwrap(),
                &block.account,
                account.block_count,
            )
            .unwrap();
        batch.put_account_info(&account, false).unwrap();
        batch.commit().unwrap();
        let local = Arc::new(local);
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let service = Service::start(Arc::clone(&source), Arc::clone(&local), 0).await;
        let result = installed::recover_metadata_from_peers(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&service.registry),
        )
        .await;
        service.stop().await;
        if case == 0 {
            let checked = result.unwrap().unwrap();
            assert_eq!(checked.cemented().tail().block_count(), 0);
            assert_eq!(checked.applied().tail().block_count(), 1);
            assert_eq!(
                checked
                    .applied()
                    .tail()
                    .blocks()
                    .next()
                    .unwrap()
                    .unwrap()
                    .hash,
                block.hash
            );
            assert_eq!(
                checked
                    .applied_account(&block.account)
                    .unwrap()
                    .confirmation_height,
                confirmation
            );
            checked
                .verify_unchanged(&local.tx_begin_write().unwrap())
                .unwrap();

            let recovered = checked.recovered();
            let continuation = &recovered.continuation;
            let original = continuation
                .state()
                .account(&block.account)
                .unwrap()
                .clone();
            let mut observed = Vec::new();
            let replayed = continuation
                .replay_applied_observed(
                    checked.applied(),
                    &recovered.selections,
                    &mut |applied, state| {
                        observed.push((
                            applied.map(|value| value.hash),
                            state.account(&block.account).unwrap().clone(),
                        ));
                        Ok(())
                    },
                )
                .unwrap();
            assert_eq!(observed.len(), 2);
            assert_eq!(observed[0].0, None);
            assert_eq!(observed[0].1.head, original.head);
            assert_eq!(observed[1].0, Some(block.hash));
            assert_eq!(observed[1].1.head, block.hash);
            assert_eq!(observed[1].1.representative, block.representative);
            assert_eq!(observed[1].1.confirmation_height, confirmation);
            assert_eq!(replayed.account(&block.account).unwrap().head, block.hash);

            // Either observer boundary can reject (for example, when its
            // retained-delta budget is exhausted) without changing the base.
            for fail_after_block in [false, true] {
                let mut calls = 0;
                let result = continuation.replay_applied_observed(
                    checked.applied(),
                    &recovered.selections,
                    &mut |applied, _| {
                        calls += 1;
                        if applied.is_some() == fail_after_block {
                            return Err(super::super::super::invalid("observer budget exhausted"));
                        }
                        Ok(())
                    },
                );
                assert!(result.is_err());
                assert_eq!(calls, if fail_after_block { 2 } else { 1 });
                assert_eq!(
                    bincode::serialize(continuation.state().account(&block.account).unwrap())
                        .unwrap(),
                    bincode::serialize(&original).unwrap()
                );
            }
            let projected = checked.project_token_indexes(&local).unwrap();
            let changes = projected.economic_changes();
            assert_eq!(changes.len(), 1);
            assert_eq!(changes[0].block, block.hash);
            let account_change = changes[0]
                .changes
                .iter()
                .find(|change| {
                    change.table == burst_store_lmdb::block_undo::UndoTable::Accounts
                        && change.key == block.account.as_str().as_bytes()
                })
                .unwrap();
            assert_eq!(
                account_change.before,
                Some(bincode::serialize(&original).unwrap())
            );
            assert_eq!(
                account_change.after,
                Some(bincode::serialize(&account).unwrap())
            );
            projected.verify_unchanged().unwrap();
            drop(projected);
        } else {
            assert!(result.is_err(), "provisional case {case}");
        }
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .get_account(&block.account)
                .unwrap()
                .confirmation_height,
            confirmation
        );
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

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_recovery_preserves_promises_and_authenticates_a_later_local_decision() {
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let epoch = manifest.checkpoint.parent;
    let bytes = archive(&source, &fixture, epoch);
    let expected = authenticate(&source, &fixture.params, epoch).into_continuation();
    let replay = CertifiedCheckpoint::replay_manifest(&source, &fixture.params, &manifest).unwrap();
    crate::checkpoint_agreement::apply(
        &source,
        expected.anchor(),
        &burst_crypto::keypair_from_seed(&[0; 32]).private,
        crate::checkpoint_agreement::Command::Decide(certificate.clone()),
        &[&replay],
        None,
    )
    .unwrap();
    let (_dir, local) = cemented::sparse(&expected, &source);
    records(&local, &source, &fixture, &bytes);
    for row in source
        .block_store()
        .indexed_blocks_txn(&source.tx_begin_read().unwrap(), 131_072, 64 << 20)
        .unwrap()
    {
        if row.height
            > expected
                .state()
                .account(&row.account)
                .map_or(0, |account| account.block_count)
        {
            local
                .meta_store()
                .put_meta(&format!("checkpoint_history/v1/{}", row.hash), &row.bytes)
                .unwrap();
        }
    }
    for (key, value) in source
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"checkpoint_round/")
        .unwrap()
    {
        local
            .meta_store()
            .put_meta(std::str::from_utf8(&key).unwrap(), &value)
            .unwrap();
    }
    let promise = format!(
        "consensus_final/v2/{epoch}/{}/{}/{}",
        address(0),
        address(0),
        BlockHash::ZERO
    );
    local.meta_store().put_meta(&promise, &[77; 32]).unwrap();
    for (key, value) in [
        (format!("consensus_sequence/v1/{}", address(0)), 12u64),
        (
            format!("consensus_time/v1/{epoch}/{}", address(0)),
            expected.anchor().cutoff(),
        ),
        (
            format!("consensus_governance/v1/{epoch}/{}", address(0)),
            expected.anchor().cutoff() + 10,
        ),
    ] {
        local
            .meta_store()
            .put_meta(&key, &value.to_be_bytes())
            .unwrap();
    }
    for (key, value) in source
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"snapshot_attestation/")
        .unwrap()
    {
        local
            .meta_store()
            .put_meta(std::str::from_utf8(&key).unwrap(), &value)
            .unwrap();
    }
    let fence = format!("consensus_fence/v2/{epoch}/{}", address(0));
    local
        .meta_store()
        .put_meta(&fence, epoch.as_bytes())
        .unwrap();
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let local = Arc::new(local);
    let service = Service::start(Arc::new(source), Arc::clone(&local), 0).await;
    let result = installed::recover_metadata_from_peers(
        Arc::clone(&local),
        NetworkId::Dev,
        fixture.params.clone(),
        Arc::clone(&service.registry),
    )
    .await;
    service.stop().await;
    let checked = result.unwrap().unwrap();
    assert_eq!(checked.recovered().continuation.anchor().hash(), epoch);
    compact::assert_same(checked.recovered().continuation.state(), expected.state());
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    checked
        .verify_unchanged(&local.tx_begin_write().unwrap())
        .unwrap();
    local.meta_store().put_meta(&promise, &[78; 32]).unwrap();
    assert!(checked
        .verify_unchanged(&local.tx_begin_write().unwrap())
        .is_err());
    local.meta_store().put_meta(&promise, &[77; 32]).unwrap();
    let later = format!("checkpoint_chain/v1/dev/{}/2", fixture.blocks[0].hash);
    {
        let mut batch = local.tx_begin_write().unwrap();
        batch
            .put_meta("block_undo/v2/changed", b"changed during recovery")
            .unwrap();
        assert!(checked.verify_unchanged(&batch).is_err());
    }
    local
        .meta_store()
        .put_meta(&later, b"changed accepted decision")
        .unwrap();
    assert!(checked
        .verify_unchanged(&local.tx_begin_write().unwrap())
        .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_recovery_rejects_damaged_or_foreign_signer_journals_without_repair() {
    let (fixture, _dir, source, manifest, _) = selection::setup();
    let epoch = manifest.checkpoint.parent;
    let bytes = archive(&source, &fixture, epoch);
    let expected = authenticate(&source, &fixture.params, epoch).into_continuation();
    let source = Arc::new(source);
    for case in 0..15 {
        let (_dir, local) = cemented::sparse(&expected, &source);
        records(&local, &source, &fixture, &bytes);
        let (key, value) = match case {
            0 => (
                format!(
                    "consensus_final/v2/{epoch}/{}/{}/{}",
                    address(0),
                    address(0),
                    BlockHash::ZERO
                ),
                vec![0; 32],
            ),
            1 => (
                format!(
                    "consensus_final/v2/{}/{}/{}/{}",
                    BlockHash::new([99; 32]),
                    address(0),
                    address(0),
                    BlockHash::ZERO
                ),
                vec![77; 32],
            ),
            2 => ("consensus_final/v1/legacy".into(), vec![77; 32]),
            3 => (
                format!("consensus_fence/v2/{epoch}/{}", address(0)),
                vec![77; 32],
            ),
            4 => (
                format!(
                    "checkpoint_round/v1/dev/{}/{}/1",
                    fixture.blocks[0].hash,
                    address(0)
                ),
                b"damaged".to_vec(),
            ),
            5 => ("checkpoint_round/v2/unknown".into(), b"damaged".to_vec()),
            6 => (
                burst_store::meta::closed_payment_epoch_key(&fixture.blocks[0].hash),
                vec![77; 64],
            ),
            7 => (format!("consensus_sequence/v1/{}", address(0)), vec![1]),
            8 => (
                format!("consensus_time/v1/{epoch}/{}", address(0)),
                1u64.to_be_bytes().to_vec(),
            ),
            9 => (
                format!("consensus_governance/v1/{epoch}/{}", address(0)),
                1u64.to_be_bytes().to_vec(),
            ),
            10 => (
                "snapshot_attestation/v1/damaged".into(),
                b"damaged".to_vec(),
            ),
            11 => ("checkpoint_vote/v1/legacy".into(), vec![77; 32]),
            12 => ("block_undo/v2/unknown".into(), vec![77; 32]),
            13 => ("block_undo/v1/short".into(), vec![77; 32]),
            _ => (
                format!("block_undo/v1/{}", "x".repeat(32)),
                b"damaged".to_vec(),
            ),
        };
        local.meta_store().put_meta(&key, &value).unwrap();
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let local = Arc::new(local);
        let service = Service::start(Arc::clone(&source), Arc::clone(&local), 0).await;
        let result = installed::recover_metadata_from_peers(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&service.registry),
        )
        .await;
        service.stop().await;
        assert!(result.is_err(), "case {case}");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_recovery_authenticates_retained_old_rows_and_pins_the_inventory() {
    let (fixture, _dir, source, manifest, _) = selection::setup();
    let epoch = manifest.checkpoint.parent;
    let bytes = archive(&source, &fixture, epoch);
    let expected = authenticate(&source, &fixture.params, epoch).into_continuation();
    let source = Arc::new(source);
    let rows = source
        .block_store()
        .indexed_blocks_txn(&source.tx_begin_read().unwrap(), 131_072, 64 << 20)
        .unwrap();
    let old = rows
        .iter()
        .find(|row| {
            expected
                .state()
                .account(&row.account)
                .is_some_and(|account| row.height + 1 < account.block_count)
        })
        .expect("sparse old ancestor");
    for case in 0..4 {
        let (_dir, local) = cemented::sparse(&expected, &source);
        records(&local, &source, &fixture, &bytes);
        let mut batch = local.tx_begin_write().unwrap();
        if case == 0 {
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
        } else {
            let mut body = old.bytes.clone();
            if case == 3 {
                let mut block: StateBlock = bincode::deserialize(&body).unwrap();
                block.signature = burst_types::Signature([99; 64]);
                body = bincode::serialize(&block).unwrap();
            }
            batch
                .put_block_with_account(
                    &old.hash,
                    &body,
                    &old.account,
                    old.height + u64::from(case == 2),
                )
                .unwrap();
        }
        batch.commit().unwrap();
        let local = Arc::new(local);
        let service = Service::start(Arc::clone(&source), Arc::clone(&local), 0).await;
        let result = installed::recover_metadata_from_peers(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&service.registry),
        )
        .await;
        let requests = service.stop().await;
        if case >= 2 {
            assert!(result.is_err(), "corrupt retained case {case}");
            continue;
        }
        let checked = result.unwrap().unwrap();
        assert_eq!(requests > 0, case == 1);
        checked
            .verify_unchanged(&local.tx_begin_write().unwrap())
            .unwrap();
        local
            .block_store()
            .put_block(&old.hash, b"changed after verification")
            .unwrap();
        assert!(checked
            .verify_unchanged(&local.tx_begin_write().unwrap())
            .is_err());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_recovery_targets_the_installed_epoch_not_the_accepted_tip() {
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let source = Arc::new(source);
    for epoch in [manifest.checkpoint.parent, certificate.checkpoint.hash()] {
        let expected = authenticate(&source, &fixture.params, epoch).into_continuation();
        let (dir, local) = cemented::sparse(&expected, &source);
        records(&local, &source, &fixture, &bytes);
        local.force_sync().unwrap();
        drop(local);
        let local = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let count = local.block_store().block_count().unwrap();
        let service = Service::start(Arc::clone(&source), Arc::clone(&local), 0).await;
        let result = installed::recover_metadata_from_peers(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&service.registry),
        )
        .await;
        let requests = service.stop().await;
        assert_eq!(requests > 0, epoch == certificate.checkpoint.hash());
        let checked = result.unwrap().unwrap();
        assert_eq!(checked.recovered().continuation.anchor().hash(), epoch);
        compact::assert_same(checked.recovered().continuation.state(), expected.state());
        assert_eq!(checked.cemented().tail().block_count(), 0);
        checked
            .verify_unchanged(&local.tx_begin_write().unwrap())
            .unwrap();
        assert_eq!(local.block_store().block_count().unwrap(), count);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_tip_reuses_authority_but_still_checks_signer_journals_and_head() {
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let parent =
        authenticate(&source, &fixture.params, manifest.checkpoint.parent).into_continuation();
    let replay = CertifiedCheckpoint::replay_manifest(&source, &fixture.params, &manifest).unwrap();
    crate::checkpoint_agreement::apply(
        &source,
        parent.anchor(),
        &burst_crypto::keypair_from_seed(&[0; 32]).private,
        crate::checkpoint_agreement::Command::Decide(certificate.clone()),
        &[&replay],
        None,
    )
    .unwrap();
    let epoch = certificate.checkpoint.hash();
    let expected = authenticate(&source, &fixture.params, epoch).into_continuation();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let (_local_dir, local) = cemented::sparse(&expected, &source);
    records(&local, &source, &fixture, &bytes);
    let journals = source
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"checkpoint_round/")
        .unwrap();
    assert!(!journals.is_empty());
    for (key, value) in &journals {
        local
            .meta_store()
            .put_meta(std::str::from_utf8(key).unwrap(), value)
            .unwrap();
    }
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let local = Arc::new(local);
    let service = Service::start(Arc::new(source), local.clone(), 0).await;
    let checked = installed::recover_metadata_from_peers(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(checked.recovered().continuation.anchor().hash(), epoch);
    compact::assert_same(checked.recovered().continuation.state(), expected.state());
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
    let head = format!("checkpoint_chain/v1/dev/{}/head", fixture.blocks[0].hash);
    let old_head = local.meta_store().get_meta(&head).unwrap();
    local
        .meta_store()
        .put_meta(&head, &(expected.anchor().height() + 1).to_be_bytes())
        .unwrap();
    assert!(checked
        .verify_unchanged(&local.tx_begin_write().unwrap())
        .is_err());
    local.meta_store().put_meta(&head, &old_head).unwrap();
    checked
        .verify_unchanged(&local.tx_begin_write().unwrap())
        .unwrap();
    let key = std::str::from_utf8(&journals[0].0).unwrap();
    local.meta_store().put_meta(key, b"damaged").unwrap();
    assert!(checked
        .verify_unchanged(&local.tx_begin_write().unwrap())
        .is_err());
    assert!(installed::recover_metadata_from_peers(
        local.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
    )
    .await
    .is_err());
    assert_eq!(local.meta_store().get_meta(key).unwrap(), b"damaged");
    assert_eq!(local.meta_store().payment_epoch().unwrap(), epoch);
    service.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn startup_cache_reuses_verified_history_and_advances_without_publishing() {
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let source = Arc::new(source);
    for epoch in [manifest.checkpoint.parent, certificate.checkpoint.hash()] {
        let expected = authenticate(&source, &fixture.params, epoch).into_continuation();
        let (_dir, local) = cemented::sparse(&expected, &source);
        records(&local, &source, &fixture, &bytes);
        let local = Arc::new(local);
        let service = Service::start(source.clone(), local.clone(), 0).await;
        let metadata = installed::recover_metadata_from_peers(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
        )
        .await
        .unwrap()
        .unwrap();
        let (runtime, cache) = metadata
            .project_token_indexes_at(&local, Timestamp::new(expected.anchor().cutoff()))
            .unwrap()
            .stage_undo()
            .unwrap()
            .commit_runtime()
            .unwrap();
        local.force_sync().unwrap();
        assert_eq!(runtime.anchor.hash(), epoch);
        let base = cache.base.clone();
        let before = service.requests.load(Ordering::SeqCst);
        let reused = cache
            .recover(base.clone(), local.clone(), service.registry.clone(), epoch)
            .await
            .unwrap()
            .unwrap();
        compact::assert_same(reused.continuation.state(), expected.state());
        assert_eq!(
            service.requests.load(Ordering::SeqCst),
            before,
            "Reusing startup history must not fetch its proofs again"
        );
        let advanced = installed::ReplayCache {
            base: base.clone(),
            recovered: reused,
        }
        .recover(
            base,
            local.clone(),
            service.registry.clone(),
            certificate.checkpoint.hash(),
        )
        .await
        .unwrap()
        .unwrap();
        let tip = authenticate(&source, &fixture.params, certificate.checkpoint.hash())
            .into_continuation();
        compact::assert_same(advanced.continuation.state(), tip.state());
        assert_eq!(
            local.meta_store().payment_epoch().unwrap(),
            epoch,
            "Cache preparation must not publish successor authority"
        );
        service.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn accepted_tip_cache_reuses_history_but_rechecks_base_and_records() {
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let older = archive(&source, &fixture, manifest.checkpoint.parent);
    let newer = archive(&source, &fixture, certificate.checkpoint.hash());
    let expected =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    let source = Arc::new(source);
    for change in 0..4 {
        let (_dir, local) = cemented::sparse(&expected, &source);
        records(&local, &source, &fixture, &older);
        let local = Arc::new(local);
        let service = Service::start(source.clone(), local.clone(), 0).await;
        let cached = installed::recover_current_tip_with_cache(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
            None,
        )
        .await
        .unwrap();
        let requests = service.requests.load(Ordering::SeqCst);
        let forked = cached.fork(local.env().path()).unwrap();
        drop(cached);
        match change {
            0 => local
                .meta_store()
                .put_meta("test/unrelated-revision", b"changed")
                .unwrap(),
            1 => local
                .meta_store()
                .put_meta(installed::BASE_KEY, &newer)
                .unwrap(),
            2 => local
                .meta_store()
                .put_meta(installed::BASE_KEY, b"corrupt")
                .unwrap(),
            3 => {
                let key = format!(
                    "checkpoint_chain/v1/dev/{}/{}",
                    fixture.blocks[0].hash, certificate.checkpoint.height
                );
                assert!(local.meta_store().get_meta(&key).is_ok());
                local.meta_store().put_meta(&key, b"corrupt").unwrap();
            }
            _ => unreachable!(),
        }
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let result = crate::checkpoint_sync::authority::authenticate_with_cache(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
            Some(forked),
        )
        .await;
        if change < 2 {
            let checked = result.unwrap().take_recovery().unwrap();
            compact::assert_same(checked.recovered.continuation.state(), expected.state());
            if change == 0 {
                assert_eq!(
                    service.requests.load(Ordering::SeqCst),
                    requests,
                    "An unrelated write must not fetch authenticated ancestry again"
                );
            }
        } else {
            assert!(
                result.is_err(),
                "corrupt input {change} must not be hidden by replay reuse"
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
        service.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn checkpoint_sync_reauthenticates_a_replacement_base_but_not_corruption() {
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let older = archive(&source, &fixture, manifest.checkpoint.parent);
    let newer = archive(&source, &fixture, certificate.checkpoint.hash());
    let expected =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    source
        .meta_store()
        .put_meta(installed::BASE_KEY, &newer)
        .unwrap();
    let source = Arc::new(source);
    for change in [7, 9, 2, 3, 4] {
        let (_dir, local) = cemented::sparse(&expected, &source);
        records(&local, &source, &fixture, &older);
        {
            local.meta_store().delete_meta(installed::BASE_KEY).unwrap();
            let first = format!("checkpoint_chain/v1/dev/{}/1", fixture.blocks[0].hash);
            local
                .meta_store()
                .put_meta(&first, &source.meta_store().get_meta(&first).unwrap())
                .unwrap();
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
            for block in &fixture.blocks {
                local
                    .meta_store()
                    .put_meta(
                        &format!("checkpoint_history/v1/{}", block.hash),
                        &bincode::serialize(block).unwrap(),
                    )
                    .unwrap();
            }
        }
        let local = Arc::new(local);
        let service = Service::start(source.clone(), local.clone(), change).await;
        let result = crate::checkpoint_sync::authority::authenticate(
            local.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
        )
        .await;
        assert!(
            service.stop().await > 0,
            "change={change}, error={:?}",
            result.as_ref().err()
        );
        assert_eq!(
            result.is_ok(),
            matches!(change, 7 | 9),
            "change={change}: {:?}",
            result.err()
        );
        if change == 7 {
            assert_eq!(
                local.meta_store().get_meta(installed::BASE_KEY).unwrap(),
                newer
            );
            assert_eq!(
                CertifiedCheckpoint::stored_tip(&local, NetworkId::Dev, &fixture.params).unwrap(),
                2
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn checkpoint_sync_authentication_checks_the_accepted_suffix_without_writes() {
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let epoch = manifest.checkpoint.parent;
    let bytes = archive(&source, &fixture, epoch);
    let expected =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    let source = Arc::new(source);
    for change in [0, 2, 3, 4] {
        let (_dir, local) = cemented::sparse(&expected, &source);
        records(&local, &source, &fixture, &bytes);
        local
            .meta_store()
            .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, epoch.as_bytes())
            .unwrap();
        let prefix = format!("checkpoint_chain/v1/dev/{}", fixture.blocks[0].hash);
        let damaged_key = match change {
            2 => Some(installed::BASE_KEY.to_owned()),
            3 => Some(format!("{prefix}/2")),
            4 => Some(format!("{prefix}/head")),
            _ => None,
        };
        if let Some(key) = damaged_key {
            local.meta_store().put_meta(&key, b"damaged").unwrap();
        }
        let local = Arc::new(local);
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let count = local.block_store().block_count().unwrap();
        let service = Service::start(Arc::clone(&source), Arc::clone(&local), 0).await;
        let result = crate::checkpoint_sync::authority::authenticate(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&service.registry),
        )
        .await;
        service.stop().await;
        match result {
            Ok(_) => assert_eq!(change, 0),
            Err(error) => {
                assert_ne!(change, 0, "{error}");
            }
        }
        assert_eq!(local.block_store().block_count().unwrap(), count);
        let after = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        assert_eq!(after, before, "case {change}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn checkpoint_sync_authentication_rejects_tip_below_snapshot_without_writes() {
    let (fixture, _dir, source, manifest, _) = selection::setup();
    let epoch = manifest.checkpoint.parent;
    let bytes = archive(&source, &fixture, epoch);
    let expected = authenticate(&source, &fixture.params, epoch).into_continuation();
    let (_dir, local) = cemented::sparse(&expected, &source);
    local
        .meta_store()
        .put_meta(installed::BASE_KEY, &bytes)
        .unwrap();
    let local = Arc::new(local);
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let service = Service::start(Arc::new(source), Arc::clone(&local), 0).await;
    let result = crate::checkpoint_sync::authority::authenticate(
        Arc::clone(&local),
        NetworkId::Dev,
        fixture.params,
        Arc::clone(&service.registry),
    )
    .await;
    assert_eq!(service.stop().await, 0);
    let error = result
        .err()
        .expect("missing accepted tip must not authenticate");
    assert!(error.to_string().contains("tip precedes"), "{error}");
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_recovery_rejects_epoch_base_or_record_changes_during_proof_delivery() {
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let expected =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    let source = Arc::new(source);
    for change in [1, 2, 3, 4] {
        let (_dir, local) = cemented::sparse(&expected, &source);
        records(&local, &source, &fixture, &bytes);
        let local = Arc::new(local);
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let service = Service::start(Arc::clone(&source), Arc::clone(&local), change).await;
        let result = installed::recover_metadata_from_peers(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&service.registry),
        )
        .await;
        assert!(service.stop().await > 0);
        assert!(result.is_err());
        let after = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let key = match change {
            1 => burst_store::meta::PAYMENT_EPOCH_KEY.to_owned(),
            2 => installed::BASE_KEY.to_owned(),
            _ => {
                let ending = if change == 3 {
                    b"/2".as_slice()
                } else {
                    b"/head".as_slice()
                };
                String::from_utf8(
                    after
                        .iter()
                        .find(|(key, _)| key.ends_with(ending))
                        .unwrap()
                        .0
                        .clone(),
                )
                .unwrap()
            }
        };
        let without_change = |rows: Vec<(Vec<u8>, Vec<u8>)>| {
            rows.into_iter()
                .filter(|(k, _)| k != key.as_bytes())
                .collect::<Vec<_>>()
        };
        assert_eq!(without_change(after), without_change(before));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_recovery_distinguishes_genesis_from_unprovable_installed_epochs() {
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let expected =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    let source = Arc::new(source);
    for case in 0..7 {
        let (_dir, local) = if case >= 5 {
            let directory = tempfile::tempdir().unwrap();
            let store = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
            (directory, store)
        } else {
            cemented::sparse(&expected, &source)
        };
        if (1..5).contains(&case) {
            records(&local, &source, &fixture, &bytes);
        }
        let epoch = match case {
            1 => vec![0; 31],
            2 => vec![0; 32],
            3 | 6 => fixture.blocks[0].hash.as_bytes().to_vec(),
            _ => BlockHash::new([44; 32]).as_bytes().to_vec(),
        };
        if case != 5 {
            local
                .meta_store()
                .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &epoch)
                .unwrap();
        }
        let local = Arc::new(local);
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let service = Service::start(Arc::clone(&source), Arc::clone(&local), 0).await;
        let result = installed::recover_metadata_from_peers(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&service.registry),
        )
        .await;
        assert_eq!(service.stop().await, 0);
        if case >= 5 {
            assert!(result.unwrap().is_none());
        } else {
            assert!(result.is_err(), "case {case}");
        }
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
