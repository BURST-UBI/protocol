use super::*;
use crate::{confirmed_replay::snapshot::installed, connection_registry::ConnectionRegistry};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::{mpsc, RwLock};

mod authority;
pub(in crate::confirmed_replay::tests) mod body_retirement;
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
        let service = WitnessService::start(Arc::clone(&source), Arc::clone(&local), 0).await;
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

pub(in crate::confirmed_replay::tests) struct WitnessService {
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
        let service = WitnessService::start(Arc::clone(&source), Arc::clone(&local), change).await;
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
        // Discovery authenticates confirmed history, not provisional balances.
        // Startup and publication check the full account inventory separately.
        if matches!(change, 0 | 6) {
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

impl WitnessService {
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

fn copy_installed_records(local: &LmdbStore, source: &LmdbStore, fixture: &Fixture, bytes: &[u8]) {
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
    identity_arrival_recovery(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_recovery_derives_typed_governance_journals_and_rewinds() {
    identity_arrival_recovery(true).await;
}

async fn identity_arrival_recovery(with_governance: bool) {
    for normalized_verification in [false, true] {
        for order in [[0, 1, 2], [1, 0, 2], [1, 2, 0]] {
            // Bootstrap identity arrival is the subject of this test. Its base
            // must be an authority cut, not a forbidden pre-bootstrap selection.
            let mut fixture = Fixture::selection_with(|params| {
                params.bootstrap_exit_threshold = 10;
                if with_governance {
                    params.governance_proposal_cost = 1;
                }
            });
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
            if normalized_verification {
                let mut batch = local.tx_begin_write().unwrap();
                batch.normalize_verification_records().unwrap();
                if with_governance {
                    batch.migrate_governance_records().unwrap();
                }
                batch.commit().unwrap();
            }
            let at = expected.anchor().cutoff() + 1;
            fixture.push(0, BlockType::Endorse, at, |block| {
                block.payment_epoch = expected.anchor().hash();
                block.brn_spent += 1;
                block.link = destination(30);
            });
            fixture.push(30, BlockType::Open, at + 1, |block| {
                block.payment_epoch = expected.anchor().hash();
            });
            fixture.push(30, BlockType::ChangeRepresentative, at + 2, |block| {
                block.payment_epoch = expected.anchor().hash();
                block.representative = address(0);
            });
            let mut order = order.to_vec();
            if with_governance {
                let content = burst_governance::ProposalContent::ParameterChange {
                    param: burst_governance::GovernableParam::BrnRate,
                    new_value: 2,
                };
                fixture.push(0, BlockType::GovernanceProposal, at + 3, |block| {
                    block.payment_epoch = expected.anchor().hash();
                    block.link = BlockHash::new(content.encode_compact().unwrap());
                });
                order.push(3);
            }
            let blocks = &fixture.blocks[fixture.blocks.len() - order.len()..];
            for &index in &order {
                let block = &blocks[index];
                let mut batch = local.tx_begin_write().unwrap();
                if block.block_type == BlockType::Endorse {
                    crate::payment_transition::authenticate_contents(block, &fixture.params)
                        .unwrap();
                    let previous = batch.get_account(&block.account).unwrap();
                    let prior: StateBlock =
                        bincode::deserialize(&batch.get_block(&block.previous).unwrap()).unwrap();
                    let mut engines =
                        crate::payment_transition::restore(&batch, &fixture.params).unwrap();
                    let effect = crate::economic_candidate::apply(
                        block,
                        crate::economic_candidate::EconomicContext {
                            account: Some(&previous),
                            previous_brn_spent: prior.brn_spent,
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
                        prior.brn_spent,
                        effect.pending.as_ref(),
                        effect.indices.as_ref(),
                    )
                    .unwrap();
                    let mut verification = crate::verification_storage::load(&batch).unwrap();
                    let crate::block_economics::EconomicResult::Endorse { burn_amount, .. } =
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
                    crate::ledger_updater::persist_economics(
                        &mut batch,
                        &engines.brn,
                        &engines.trst,
                    )
                    .unwrap();
                } else if block.block_type == BlockType::GovernanceProposal {
                    crate::payment_transition::authenticate_signed_contents(block, &fixture.params)
                        .unwrap();
                    let mut engines =
                        crate::payment_transition::restore(&batch, &fixture.params).unwrap();
                    let mut governance = crate::governance_commit::CandidateGovernance::restore(
                        &batch,
                        fixture.params.clone(),
                        NetworkId::Dev,
                        fixture.blocks[0].hash,
                    )
                    .unwrap();
                    let economic = crate::economic_candidate::stage(
                        &mut batch,
                        block,
                        &fixture.params,
                        &mut (&mut engines.brn, &mut engines.trst),
                    )
                    .unwrap();
                    governance
                        .apply(
                            &mut batch,
                            block,
                            &economic.effect,
                            &mut engines.brn,
                            &mut engines.trst,
                        )
                        .unwrap();
                    crate::ledger_updater::persist_economics(
                        &mut batch,
                        &engines.brn,
                        &engines.trst,
                    )
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
            let service = WitnessService::start(Arc::clone(&source), Arc::clone(&local), 0).await;
            assert!(
                crate::checkpoint_store::chain::load(&local, NetworkId::Dev, &fixture.params)
                    .is_err()
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
            let cache_at =
                burst_types::Timestamp::new(checked.recovered().continuation.anchor().cutoff());
            let prepared = checked.prepare_token_indexes_at(&local, cache_at).unwrap();
            let mut roots = std::collections::HashSet::new();
            prepared
                .visit_rollback_body_roots(local.env().path(), |hash| {
                    roots.insert(hash);
                    Ok(())
                })
                .unwrap();
            prepared
                .visit_replayed_changes(|record| {
                    assert!(roots.contains(&record.block));
                    Ok(())
                })
                .unwrap();
            let projected = prepared.stage(&local).unwrap();
            let hashes: Vec<_> = projected
                .economic_changes()
                .iter()
                .map(|record| record.block)
                .collect();
            let identity_hashes: Vec<_> = hashes
                .iter()
                .copied()
                .filter(|hash| !with_governance || *hash != blocks[3].hash)
                .collect();
            assert_eq!(
                identity_hashes,
                order
                    .iter()
                    .copied()
                    .filter(|index| *index < 3)
                    .map(|index| blocks[index].hash)
                    .collect::<Vec<_>>()
            );
            if with_governance {
                // Independent account operations use causal/hash order, not a
                // total arrival order. Preserve the exact inventory and edges.
                assert_eq!(hashes.len(), 4);
                assert_eq!(
                    hashes
                        .iter()
                        .copied()
                        .collect::<std::collections::HashSet<_>>(),
                    blocks.iter().map(|block| block.hash).collect()
                );
                let position = |hash| hashes.iter().position(|entry| *entry == hash).unwrap();
                assert!(position(blocks[0].hash) < position(blocks[3].hash));
            }
            if with_governance && normalized_verification {
                let records = projected.economic_changes();
                let proposal = records
                    .iter()
                    .find(|record| record.block == blocks[3].hash)
                    .unwrap();
                assert!(proposal.governance.is_some());
                assert!(proposal
                    .governance
                    .as_ref()
                    .unwrap()
                    .changes()
                    .iter()
                    .all(
                        |row| row.table == burst_store_lmdb::block_undo::UndoTable::Meta
                            && row.key.starts_with(b"governance_state/")
                    ));
            }
            let staged = projected.stage_undo().unwrap();
            for block in blocks {
                let journal = staged.journal(&block.hash).unwrap().unwrap();
                if block.block_type == BlockType::GovernanceProposal {
                    assert_eq!(
                        journal
                            .changes
                            .iter()
                            .any(|row| row.key.starts_with(b"governance_revision/")),
                        normalized_verification
                    );
                    assert_eq!(
                        journal
                            .changes
                            .iter()
                            .any(|row| row.key == b"governance_engine"),
                        !normalized_verification
                    );
                }
                if block.block_type == BlockType::Endorse {
                    let physical = journal.changes.iter().any(|row| {
                        row.table == burst_store_lmdb::block_undo::UndoTable::Meta
                            && row.key.starts_with(b"verification_state/")
                    });
                    let aggregate = journal.changes.iter().any(|row| {
                        row.table == burst_store_lmdb::block_undo::UndoTable::Meta
                            && row.key == b"verification_orchestrator"
                    });
                    // Full authenticated installation normalizes verification
                    // before rebuilding provisional journals in either fixture.
                    assert!(physical);
                    assert!(!aggregate);
                }
            }
            staged.commit_for_test().unwrap();
            let service = WitnessService::start(source, Arc::clone(&local), 0).await;
            let mut recovered = installed::recover_metadata_from_peers(
                Arc::clone(&local),
                NetworkId::Dev,
                fixture.params.clone(),
                Arc::clone(&service.registry),
            )
            .await
            .unwrap()
            .unwrap();
            if with_governance && !normalized_verification && order == vec![0, 1, 2, 3] {
                let (before_metadata, before_aggregate, before_journals) = {
                    let batch = local.tx_begin_write().unwrap();
                    (
                        batch.checkpoint_metadata(b"").unwrap(),
                        batch
                            .undo_value(
                                burst_store_lmdb::block_undo::UndoTable::BrnMeta,
                                b"governance_engine",
                            )
                            .unwrap(),
                        blocks
                            .iter()
                            .map(|block| {
                                bincode::serialize(&batch.block_undo(&block.hash).unwrap()).unwrap()
                            })
                            .collect::<Vec<_>>(),
                    )
                };
                let projected = recovered
                    .project_token_indexes_with_stable_holdings(&local, cache_at)
                    .unwrap();
                let proposal = projected
                    .economic_changes()
                    .into_iter()
                    .find(|row| row.block == blocks[3].hash)
                    .unwrap();
                assert!(
                    proposal.governance.is_some(),
                    "migration must derive keyed undo directly"
                );
                assert!(proposal
                    .changes
                    .iter()
                    .all(|row| row.key != b"governance_engine"));
                let aborted = projected.stage_undo().unwrap();
                let journal = aborted.journal(&blocks[3].hash).unwrap().unwrap();
                assert!(journal
                    .changes
                    .iter()
                    .any(|row| row.key.starts_with(b"governance_revision/")));
                assert!(journal
                    .changes
                    .iter()
                    .all(|row| row.key != b"governance_engine"));
                drop(aborted);
                {
                    let batch = local.tx_begin_write().unwrap();
                    assert!(!batch.uses_governance_records().unwrap());
                    assert_eq!(batch.checkpoint_metadata(b"").unwrap(), before_metadata);
                    assert_eq!(
                        batch
                            .undo_value(
                                burst_store_lmdb::block_undo::UndoTable::BrnMeta,
                                b"governance_engine"
                            )
                            .unwrap(),
                        before_aggregate
                    );
                    assert_eq!(
                        blocks
                            .iter()
                            .map(|block| bincode::serialize(
                                &batch.block_undo(&block.hash).unwrap()
                            )
                            .unwrap())
                            .collect::<Vec<_>>(),
                        before_journals
                    );
                }
                recovered = installed::recover_metadata_from_peers(
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
            assert!(!local
                .tx_begin_write()
                .unwrap()
                .uses_stable_trst_records()
                .unwrap());
            // Unauthenticated time must not influence recovered expiry caches.
            node.clock_sync.lock().await.offset_ms = i64::MAX;
            node.publish_recovered(recovered).await.unwrap();
            assert!(local
                .tx_begin_write()
                .unwrap()
                .uses_stable_trst_records()
                .unwrap());
            {
                let batch = local.tx_begin_write().unwrap();
                assert!(batch.uses_governance_records().unwrap());
                assert!(batch
                    .undo_value_ref(
                        burst_store_lmdb::block_undo::UndoTable::BrnMeta,
                        b"governance_engine"
                    )
                    .unwrap()
                    .is_none());
                if with_governance {
                    let journal = batch.block_undo(&blocks[3].hash).unwrap().unwrap();
                    assert!(journal
                        .changes
                        .iter()
                        .any(|row| row.key.starts_with(b"governance_revision/")));
                    assert!(journal
                        .changes
                        .iter()
                        .all(|row| row.key != b"governance_engine"));
                }
            }
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
            let persisted = crate::verification_storage::load(&local.tx_begin_write().unwrap())
                .unwrap()
                .snapshot();
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
            if with_governance {
                assert!(crate::governance_storage::load(&batch)
                    .unwrap()
                    .unwrap()
                    .get_proposal(&blocks[3].hash.into_tx_hash())
                    .is_none());
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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_recovery_derives_payment_deltas_without_trusting_local_undo() {
    payment_delta_recovery(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_recovery_derives_row_payment_deltas_without_trusting_local_undo() {
    payment_delta_recovery(true).await;
}

async fn payment_delta_recovery(uses_row_storage: bool) {
    use burst_store_lmdb::block_undo::UndoTable;
    let (mut fixture, _dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    let expected =
        authenticate(&source, &fixture.params, certificate.checkpoint.hash()).into_continuation();
    let (_dir, local) = cemented::sparse(&expected, &source);
    copy_installed_records(&local, &source, &fixture, &bytes);
    assert!(!local
        .tx_begin_write()
        .unwrap()
        .uses_brn_wallet_records()
        .unwrap());
    if uses_row_storage {
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
        block.brn_spent += 1;
    });
    let received = fixture.push(30, BlockType::Open, at + 1, |block| {
        block.payment_epoch = expected.anchor().hash();
        block.link = burn;
        block.trst_balance = 1;
    });
    let returned_burn = fixture.push(0, BlockType::Burn, at + 2, |block| {
        block.payment_epoch = expected.anchor().hash();
        block.link = destination(30);
        block.brn_spent += 1;
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
    service.stop().await;
    let before_preparation = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let prepared = checked
        .prepare_token_indexes_at(
            &local,
            burst_types::Timestamp::new(expected.anchor().cutoff()),
        )
        .unwrap();
    let mut replayed_blocks = std::collections::HashSet::new();
    prepared
        .visit_replayed_changes(|record| {
            assert!(replayed_blocks.insert(record.block));
            // This would deadlock if preparation retained the publication writer.
            let batch = local.tx_begin_write()?;
            assert!(batch.get_account(&record.account)?.block_count >= record.height);
            Ok(())
        })
        .unwrap();
    assert_eq!(replayed_blocks.len(), 5);
    assert_eq!(
        replayed_blocks,
        recorded.iter().map(|record| record.block).collect()
    );
    let mut rollback_roots = std::collections::HashSet::new();
    prepared
        .visit_rollback_body_roots(local.env().path(), |hash| {
            assert!(rollback_roots.insert(hash));
            Ok(())
        })
        .unwrap();
    assert!(replayed_blocks
        .iter()
        .all(|hash| rollback_roots.contains(hash)));
    assert!(rollback_roots.contains(&blocks[0].previous));
    let mut callbacks = 0;
    assert!(prepared
        .visit_replayed_changes(|_| {
            callbacks += 1;
            Err(burst_store::StoreError::Corruption(
                "retention inspection stopped".into(),
            ))
        })
        .is_err());
    assert_eq!(callbacks, 1);
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before_preparation
    );
    let projected = prepared.stage(&local).unwrap();
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
        let mut expected_wallets = Vec::new();
        for change in expected
            .changes
            .iter()
            .filter(|row| row.table == UndoTable::BrnWallets)
        {
            use burst_brn::{wallet_storage, BrnWalletState};
            let address =
                burst_types::WalletAddress::new(std::str::from_utf8(&change.key).unwrap());
            let before = change
                .before
                .as_deref()
                .map(BrnWalletState::from_persisted_bytes)
                .transpose()
                .unwrap();
            let after = change
                .after
                .as_deref()
                .map(BrnWalletState::from_persisted_bytes)
                .transpose()
                .unwrap();
            let prefix = wallet_storage::row_prefix(&address).unwrap();
            wallet_storage::visit_delta(
                before.as_ref(),
                after.as_ref(),
                |relative, before, after| {
                    let (table, key) = if relative == wallet_storage::HEADER {
                        (UndoTable::BrnWallets, change.key.clone())
                    } else {
                        (UndoTable::BrnMeta, [prefix.as_slice(), relative].concat())
                    };
                    expected_wallets.push(burst_store_lmdb::block_undo::UndoChange {
                        table,
                        key,
                        before,
                        after,
                    });
                    Ok(())
                },
            )
            .unwrap();
        }
        expected_wallets.sort_by(|a, b| (a.table, &a.key).cmp(&(b.table, &b.key)));
        let actual_wallets: Vec<_> = actual
            .changes
            .iter()
            .filter(|row| {
                row.table == UndoTable::BrnWallets
                    || row.table == UndoTable::BrnMeta
                        && row.key.starts_with(burst_brn::wallet_storage::ROW_PREFIX)
            })
            .cloned()
            .collect();
        assert_eq!(
            actual_wallets, expected_wallets,
            "wallet records must match canonical live undo economics"
        );
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
    assert!(!local
        .tx_begin_write()
        .unwrap()
        .uses_brn_wallet_records()
        .unwrap());
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
    let service = WitnessService::start(Arc::clone(&source), Arc::clone(&local), 0).await;
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
    {
        let view = local.tx_begin_read().unwrap();
        let resolved = checked
            .resolve_body(&local, &view, &burn)
            .unwrap()
            .expect("cemented burn");
        assert_eq!(resolved.origin(), installed::BodyOrigin::CementedSuffix);
        assert_eq!(resolved.block().hash, burn);
        let resolved = checked
            .resolve_body(&local, &view, &successor)
            .unwrap()
            .expect("provisional successor");
        assert_eq!(resolved.origin(), installed::BodyOrigin::ProvisionalSuffix);
    }
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
    assert!(local
        .tx_begin_write()
        .unwrap()
        .uses_brn_wallet_records()
        .unwrap());
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
    let service = WitnessService::start(Arc::clone(&source), Arc::clone(&local), 0).await;
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
async fn recovered_trst_pages_bind_authority_and_reject_changed_economics() {
    let mut fixture = Fixture::selection();
    let burn = fixture.push(0, BlockType::Burn, 1_595_434_040, |block| {
        block.link = destination(30);
        block.brn_spent += 1;
    });
    fixture.push(30, BlockType::Open, 1_595_434_041, |block| {
        block.link = burn;
        block.trst_balance = 1;
    });
    let (_source_dir, source, snapshot) = snapshot(&fixture);
    let epoch = snapshot.anchor().hash();
    let bytes = archive(&source, &fixture, epoch);
    let expected = snapshot.into_continuation();
    let (_local_dir, local) = cemented::sparse(&expected, &source);
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
    let local = Arc::new(local);
    let service = WitnessService::start(Arc::new(source), Arc::clone(&local), 0).await;
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
    assert!(
        checked.trst_references(&local).is_err(),
        "aggregate layout unsupported"
    );
    // This fully certified fixture has no provisional journals to convert.
    let mut batch = local.tx_begin_write().unwrap();
    let payment = crate::payment_transition::restore(&batch, &fixture.params).unwrap();
    crate::trst_storage::stage_replayed_stable(&mut batch, &payment.trst).unwrap();
    crate::graph_storage::stage_replayed(&mut batch, &payment.trst.merger_graph).unwrap();
    batch.begin_brn_wallet_records_projection().unwrap();
    for (owner, state) in payment.brn.wallets.iter() {
        batch.put_brn_wallet_state(owner, state).unwrap();
    }
    batch.normalize_verification_records().unwrap();
    batch.migrate_governance_records().unwrap();
    batch
        .put_brn_rate_history(&payment.brn.rate_history)
        .unwrap();
    let original_constitution = batch.get_meta("consti_engine").unwrap().unwrap();
    let decoded_constitution: burst_consti::ConstiEngine =
        bincode::deserialize(&original_constitution).unwrap();
    let original_schedule = batch.read_confirmed_governance().unwrap();
    batch
        .write_confirmed_governance(original_schedule.clone())
        .unwrap();
    let expected_delegation = batch.checkpoint_metadata(b"key_delegation/").unwrap().len();
    let expected_governance = batch
        .checkpoint_metadata(b"governance_state/")
        .unwrap()
        .len()
        + batch
            .checkpoint_metadata(b"governance_endorsement/")
            .unwrap()
            .len();
    let expected_verification = batch
        .checkpoint_metadata(b"verification_state/")
        .unwrap()
        .len();
    assert!(expected_verification > 1);
    let expected_receipts: usize = crate::verification_commit::retention::PREFIXES
        .iter()
        .map(|prefix| batch.checkpoint_metadata(prefix.as_bytes()).unwrap().len())
        .sum();
    batch.commit().unwrap();
    {
        let bodies = checked.body_requirements(&local).unwrap();
        assert!(bodies.has_root(fixture.blocks[0].hash).unwrap());
        assert!(bodies.has_root(burn).unwrap());
        let mut found_burn = false;
        bodies
            .visit_resolved_roots(|hash, resolved| {
                if hash == burn {
                    let resolved = resolved.expect("checked burn witness");
                    assert_eq!(resolved.block().hash, hash);
                    assert_eq!(resolved.origin(), installed::BodyOrigin::SnapshotPrefix);
                    assert_eq!(
                        resolved.context(),
                        checked.recovered().continuation.history_context()
                    );
                    found_burn = true;
                }
                Ok(())
            })
            .unwrap();
        assert!(found_burn);
        let mut roots = std::collections::HashSet::new();
        bodies
            .visit_roots(|hash| {
                assert!(roots.insert(hash));
                Ok(())
            })
            .unwrap();
        assert!(roots.len() >= 2);
        bodies
            .check_unchanged_cut(&local.tx_begin_write().unwrap())
            .unwrap();
        local
            .meta_store()
            .put_meta("body_plan_stale_cut", &[1])
            .unwrap();
        assert!(bodies
            .check_unchanged_cut(&local.tx_begin_write().unwrap())
            .is_err());
        assert!(bodies.close(Default::default()).is_err());
    }
    local
        .meta_store()
        .delete_meta("body_plan_stale_cut")
        .unwrap();
    let mut reader = checked.trst_references(&local).unwrap();
    {
        use crate::confirmed_replay::snapshot::SelectionSource;
        let (base, steps) = reader.signer_authority();
        let mut authority = base.clone();
        for step in steps {
            authority = step.unwrap().authenticate(&authority).unwrap();
        }
        assert_eq!(authority.hash(), epoch);
        assert_eq!(
            reader.signer_selections().len(),
            checked.recovered().selections.len()
        );
        for selection in reader.signer_selections().iter_selections() {
            selection.unwrap();
        }
    }

    assert_eq!(reader.confirmed_governance_schedule(), &original_schedule);
    assert_eq!(reader.confirmed_governance_schedule().epoch, epoch);
    assert!(std::ptr::eq(
        reader.confirmed_governance_schedule(),
        reader.confirmed_governance_schedule()
    ));
    let original_rates: Vec<_> = payment
        .brn
        .rate_history
        .segments()
        .iter()
        .cloned()
        .collect();
    assert!(!original_rates.is_empty());
    let mut rate_iter = reader.brn_rate_segments();
    let mut observed_rates: Vec<_> = rate_iter.by_ref().take(1).cloned().collect();
    observed_rates.extend(rate_iter.cloned());
    assert_eq!(observed_rates, original_rates);
    assert_eq!(reader.constitution().unwrap(), &decoded_constitution);
    assert!(!reader
        .constitution()
        .unwrap()
        .get_constitution()
        .articles
        .is_empty());
    let legacy_roots: Vec<_> = reader
        .legacy_delegation_references()
        .unwrap()
        .map(|r| format!("{r:?}"))
        .collect();
    assert_eq!(
        reader.metadata().recovered().continuation.anchor().hash(),
        epoch
    );
    let mut brn_reader = reader.brn_wallet_references().unwrap();
    let original_brn = payment.brn.get_wallet(&address(0)).unwrap();
    // A writer can proceed while pages remain pinned to authenticated old rows.
    let mut batch = local.tx_begin_write().unwrap();
    let mut altered = payment.trst.clone();
    altered.get_portfolio_mut(&address(30)).unwrap().tokens[0].origin_wallet = address(31);
    crate::trst_storage::stage_replayed_stable(&mut batch, &altered).unwrap();
    let mut altered_brn = original_brn.clone();
    altered_brn.verified_at = burst_types::Timestamp::new(42);
    batch
        .put_brn_wallet_state(&address(0), &altered_brn)
        .unwrap();
    batch
        .put_meta("verification_receipt/v1/forged", b"invalid")
        .unwrap();
    batch
        .put_meta("verification_state/v3/forged", b"invalid")
        .unwrap();
    batch
        .put_meta("governance_state/v2/forged", b"invalid")
        .unwrap();
    batch
        .put_meta("key_delegation/v3/forged", b"invalid")
        .unwrap();
    batch
        .put_meta("governance_endorsement/forged", b"invalid")
        .unwrap();
    batch
        .put_meta("confirmed_governance/v4/forged", b"invalid")
        .unwrap();
    let legacy_key = burst_governance::DelegationEngine::meta_key();
    let original_legacy = batch.get_meta(legacy_key).unwrap().unwrap();
    batch.put_meta(legacy_key, b"invalid").unwrap();
    batch.put_meta("consti_engine", b"invalid").unwrap();
    let rate_key = burst_brn::rate_storage::row_key(0);
    let original_rate = burst_brn::rate_storage::row_value(&original_rates[0]);
    let mut changed_rate = original_rates[0].clone();
    changed_rate.rate = changed_rate.rate.checked_add(1).unwrap();
    let changed_rate = burst_brn::rate_storage::row_value(&changed_rate);
    batch.put_brn_meta(&rate_key, &changed_rate).unwrap();
    batch.commit().unwrap();
    assert_eq!(
        reader.brn_rate_segments().cloned().collect::<Vec<_>>(),
        original_rates
    );
    assert_eq!(reader.confirmed_governance_schedule(), &original_schedule);
    assert_eq!(reader.constitution().unwrap(), &decoded_constitution);
    let mut brn_found = false;
    let mut brn_done = false;
    for _ in 0..100 {
        let page = brn_reader
            .visit_next_page(1, 1 << 20, |reference| {
                use burst_brn::retention::Reference;
                brn_found |= reference == Reference::Boundary(original_brn.verified_at);
                assert_ne!(
                    reference,
                    Reference::Boundary(burst_types::Timestamp::new(42))
                );
                Ok(())
            })
            .unwrap();
        assert!(page.rows <= 1);
        if page.done {
            brn_done = true;
            break;
        }
    }
    assert!(brn_done && brn_found);
    drop(brn_reader);
    assert_eq!(
        legacy_roots,
        reader
            .legacy_delegation_references()
            .unwrap()
            .map(|r| format!("{r:?}"))
            .collect::<Vec<_>>()
    );
    let mut delegation_reader = reader.delegation_references().unwrap();
    let mut delegation_rows = 0;
    let mut delegation_done = false;
    for _ in 0..expected_delegation + 1 {
        let page = delegation_reader
            .visit_next_page(1, 4096, |_| Ok(()))
            .unwrap();
        assert!(page.rows <= 1 && page.bytes <= 4096);
        delegation_rows += page.rows;
        if page.done {
            delegation_done = true;
            break;
        }
    }
    assert!(delegation_done);
    assert_eq!(delegation_rows, expected_delegation);
    drop(delegation_reader);
    let mut governance_reader = reader.governance_references().unwrap();
    let mut governance_rows = 0;
    let mut governance_done = false;
    for _ in 0..expected_governance + 2 {
        let page = governance_reader
            .visit_next_page(1, 1 << 20, |_| Ok(()))
            .unwrap();
        assert!(page.rows <= 1 && page.bytes <= 1 << 20);
        governance_rows += page.rows;
        if page.done {
            governance_done = true;
            break;
        }
    }
    assert!(governance_done);
    assert_eq!(governance_rows, expected_governance);
    drop(governance_reader);
    let mut verification_reader = reader.verification_references().unwrap();
    let mut verification_rows = 0;
    let mut verification_done = false;
    let mut verification_target = false;
    for _ in 0..expected_verification + 1 {
        let page = verification_reader
            .visit_next_page(1, 1 << 20, |reference| {
                verification_target |= reference
                    == burst_verification::orchestrator::records::retention::Reference::Account(
                        &address(0),
                    );
                Ok(())
            })
            .unwrap();
        assert!(page.rows <= 1 && page.bytes <= 1 << 20);
        verification_rows += page.rows;
        if page.done {
            verification_done = true;
            break;
        }
    }
    assert!(verification_done && verification_target);
    assert_eq!(verification_rows, expected_verification);
    drop(verification_reader);
    let mut receipt_reader = reader.receipt_references();
    let mut receipt_rows = 0;
    let mut receipts_done = false;
    for _ in 0..(expected_receipts + crate::verification_commit::retention::PREFIXES.len() + 1) {
        let page = receipt_reader
            .visit_next_page(1, 1 << 20, |_| Ok(()))
            .unwrap();
        receipt_rows += page.rows;
        if page.done {
            receipts_done = true;
            break;
        }
    }
    assert!(receipts_done);
    assert_eq!(receipt_rows, expected_receipts);
    drop(receipt_reader);
    let mut found = false;
    let mut done = false;
    for _ in 0..100 {
        let page = reader
            .visit_next_page(1, 1 << 20, |reference| {
                found |= reference == burst_types::TrstReference::Token(burn.into_tx_hash());
                Ok(())
            })
            .unwrap();
        assert!(page.rows <= 1);
        if page.done {
            done = true;
            break;
        }
    }
    assert!(done && found);
    drop(reader);
    assert!(
        checked.trst_references(&local).is_err(),
        "combined changed rights reject acquisition"
    );
    let forged_keys = [
        "verification_receipt/v1/forged",
        "verification_state/v3/forged",
        "governance_state/v2/forged",
        "key_delegation/v3/forged",
        "governance_endorsement/forged",
        "confirmed_governance/v4/forged",
    ];
    let mut batch = local.tx_begin_write().unwrap();
    crate::trst_storage::stage_replayed_stable(&mut batch, &payment.trst).unwrap();
    batch
        .put_brn_wallet_state(&address(0), original_brn)
        .unwrap();
    batch.put_brn_meta(&rate_key, &original_rate).unwrap();
    batch.put_meta(legacy_key, &original_legacy).unwrap();
    batch
        .put_meta("consti_engine", &original_constitution)
        .unwrap();
    for key in forged_keys {
        batch.delete_meta(key).unwrap();
    }
    batch.commit().unwrap();
    drop(checked.trst_references(&local).unwrap());

    // Each rejection starts from a checked clean baseline. Simultaneous
    // corruptions cannot mask a missing check for another dependency family.
    for key in forged_keys {
        local.meta_store().put_meta(key, b"invalid").unwrap();
        assert!(
            checked.trst_references(&local).is_err(),
            "independent rejection: {key}"
        );
        local.meta_store().delete_meta(key).unwrap();
        drop(checked.trst_references(&local).unwrap());
    }
    // Structurally valid but inconsistent with authenticated constitutional history.
    let changed_constitution = burst_consti::ConstiEngine::new();
    local
        .meta_store()
        .put_meta(
            "consti_engine",
            &bincode::serialize(&changed_constitution).unwrap(),
        )
        .unwrap();
    assert!(
        checked.trst_references(&local).is_err(),
        "changed constitution alone"
    );
    local
        .meta_store()
        .put_meta("consti_engine", &original_constitution)
        .unwrap();
    drop(checked.trst_references(&local).unwrap());
    local.meta_store().put_meta(legacy_key, b"invalid").unwrap();
    assert!(
        checked.trst_references(&local).is_err(),
        "legacy delegation alone"
    );
    local
        .meta_store()
        .put_meta(legacy_key, &original_legacy)
        .unwrap();
    drop(checked.trst_references(&local).unwrap());
    for trst in [true, false] {
        let mut batch = local.tx_begin_write().unwrap();
        if trst {
            crate::trst_storage::stage_replayed_stable(&mut batch, &altered).unwrap();
        } else {
            batch
                .put_brn_wallet_state(&address(0), &altered_brn)
                .unwrap();
        }
        batch.commit().unwrap();
        assert!(
            checked.trst_references(&local).is_err(),
            "independent changed wallet, trst={trst}"
        );
        let mut batch = local.tx_begin_write().unwrap();
        if trst {
            crate::trst_storage::stage_replayed_stable(&mut batch, &payment.trst).unwrap();
        } else {
            batch
                .put_brn_wallet_state(&address(0), original_brn)
                .unwrap();
        }
        batch.commit().unwrap();
        drop(checked.trst_references(&local).unwrap());
    }
    // A validly encoded different rate, a missing row and mixed local formats
    // must each fail before exposing an authenticated iterator.
    for corruption in 0..3 {
        let mut batch = local.tx_begin_write().unwrap();
        match corruption {
            0 => batch.put_brn_meta(&rate_key, &changed_rate).unwrap(),
            1 => batch.delete_brn_meta(&rate_key).unwrap(),
            _ => batch
                .put_brn_meta(
                    b"rate_history",
                    &bincode::serialize(&payment.brn.rate_history).unwrap(),
                )
                .unwrap(),
        }
        batch.commit().unwrap();
        assert!(
            checked.trst_references(&local).is_err(),
            "independent rate corruption {corruption}"
        );
        let mut batch = local.tx_begin_write().unwrap();
        batch.put_brn_meta(&rate_key, &original_rate).unwrap();
        batch.delete_brn_meta(b"rate_history").unwrap();
        batch.commit().unwrap();
        let valid = checked.trst_references(&local).unwrap();
        assert_eq!(
            valid.brn_rate_segments().cloned().collect::<Vec<_>>(),
            original_rates
        );
    }
    let mut fabricated = original_schedule.clone();
    fabricated.proposals.insert(
        [9; 32],
        burst_store_lmdb::confirmed_governance::Schedule {
            proposer: address(0),
            created_at: 1_595_434_042,
            timing: burst_governance::GovernanceTiming {
                proposal_secs: 10,
                voting_secs: 20,
                buffer_secs: 1,
            },
            endorsements: 1,
            last_endorsement: Some(1_595_434_043),
            withdrawn_at: None,
            exploration_started_at: None,
        },
    );
    let mut batch = local.tx_begin_write().unwrap();
    batch
        .write_confirmed_governance(fabricated.clone())
        .unwrap();
    batch.commit().unwrap();
    // The physical manifest and derived fence are consistent; history is not.
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .read_confirmed_governance()
            .unwrap(),
        fabricated
    );
    assert!(
        checked.trst_references(&local).is_err(),
        "fabricated schedule alone"
    );
    let mut batch = local.tx_begin_write().unwrap();
    batch
        .write_confirmed_governance(original_schedule.clone())
        .unwrap();
    batch.commit().unwrap();
    assert_eq!(
        checked
            .trst_references(&local)
            .unwrap()
            .confirmed_governance_schedule(),
        &original_schedule
    );
    for key in [
        "payment_epoch/v2",
        "payment_epoch/v2/closed/test",
        "payment_epoch/v1/unknown",
    ] {
        local
            .meta_store()
            .put_meta(key, b"unknown epoch record")
            .unwrap();
        assert!(
            checked.trst_references(&local).is_err(),
            "unknown payment namespace: {key}"
        );
        local.meta_store().delete_meta(key).unwrap();
        drop(checked.trst_references(&local).unwrap());
    }
    let revision_key = format!(
        "governance_revision/v1/{}",
        burst_types::TxHash::new([66; 32])
    );
    let pinned_rollback = checked.trst_references(&local).unwrap();
    let mut rollback = pinned_rollback.rollback_records().unwrap();
    local
        .meta_store()
        .put_meta(&revision_key, &1u128.to_be_bytes())
        .unwrap();
    // This certified fixture has no provisional journals or local counters.
    // Later writes cannot enter its checked old cut; nonempty local images are
    // covered separately by the physical reader and typed recovery fixtures.
    let page = rollback
        .visit_next_page(1, 4096, |_| {
            panic!("later local rollback row leaked into pinned cut")
        })
        .unwrap();
    assert!(page.done && page.rows == 0);
    drop(rollback);
    drop(pinned_rollback);
    assert!(
        checked.trst_references(&local).is_err(),
        "local rollback revision changed after capture"
    );
    local.meta_store().delete_meta(&revision_key).unwrap();
    drop(checked.trst_references(&local).unwrap());
    local
        .meta_store()
        .put_meta(burst_store::meta::PAYMENT_EPOCH_KEY, &[99; 32])
        .unwrap();
    assert!(
        checked.trst_references(&local).is_err(),
        "changed authority cannot acquire a checked reader"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_recovery_allows_later_expiry_cache_but_rejects_changed_token_rights() {
    let mut fixture = Fixture::selection();
    let burn = fixture.push(0, BlockType::Burn, 1_595_434_040, |block| {
        block.link = destination(30);
        block.brn_spent += 1;
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
    let service = WitnessService::start(Arc::new(source), Arc::clone(&local), 0).await;
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
    let staged = projected.stage_undo().unwrap();
    assert_eq!(staged.runtime().anchor.hash(), epoch);
    assert_eq!(
        staged
            .runtime()
            .trst
            .get_portfolio(&address(30))
            .unwrap()
            .tokens
            .len(),
        1
    );
    drop(staged);
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
    copy_installed_records(&local, &source, &fixture, &bytes);
    let source = Arc::new(source);
    let local = Arc::new(local);
    let service = WitnessService::start(source, Arc::clone(&local), 0).await;
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
            6 => {
                if batch.uses_verification_records().unwrap() {
                    let (key, _) = batch
                        .checkpoint_metadata(b"verification_state/")
                        .unwrap()
                        .into_iter()
                        .next()
                        .expect("nonempty keyed verification state");
                    batch
                        .put_meta(std::str::from_utf8(&key).unwrap(), b"bad")
                        .unwrap();
                } else {
                    batch
                        .put_meta(crate::endorsement_commit::SNAPSHOT_KEY, b"bad")
                        .unwrap();
                }
            }
            7 => {
                if batch.uses_governance_records().unwrap() {
                    let (key, _) = batch
                        .checkpoint_metadata(b"governance_state/")
                        .unwrap()
                        .into_iter()
                        .next()
                        .expect("nonempty keyed governance state");
                    batch
                        .put_meta(std::str::from_utf8(&key).unwrap(), b"bad")
                        .unwrap();
                } else {
                    batch.put_brn_meta(b"governance_engine", b"bad").unwrap();
                }
            }
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
    // Keyed comparisons must also see uncommitted writer changes, in both
    // verification formats, without weakening inventory or corruption checks.
    for normalized in [false, true] {
        for prefix in ["verification_state/", "governance_state/"] {
            for damage in 0..3 {
                let mut batch = local.tx_begin_write().unwrap();
                batch.migrate_verification_records().unwrap();
                if normalized {
                    batch.normalize_verification_records().unwrap();
                }
                batch.migrate_governance_records().unwrap();
                checked.verify_unchanged(&batch).unwrap();
                let (key, _) = batch
                    .checkpoint_metadata(prefix.as_bytes())
                    .unwrap()
                    .into_iter()
                    .next()
                    .expect("nonempty keyed engine");
                let key = String::from_utf8(key).unwrap();
                match damage {
                    0 => batch.put_meta(&key, b"bad").unwrap(),
                    1 => batch.delete_meta(&key).unwrap(),
                    2 => batch
                        .put_meta(&format!("{prefix}unexpected"), b"bad")
                        .unwrap(),
                    _ => unreachable!(),
                }
                let before = batch.checkpoint_metadata(prefix.as_bytes()).unwrap();
                assert!(
                    checked.verify_unchanged(&batch).is_err(),
                    "keyed cache normalized={normalized} prefix={prefix} damage={damage}"
                );
                assert_eq!(
                    batch.checkpoint_metadata(prefix.as_bytes()).unwrap(),
                    before
                );
                drop(batch);
                checked
                    .verify_unchanged(&local.tx_begin_write().unwrap())
                    .unwrap();
            }
        }
    }
    // Preparation releases the writer; a later edit must still reject staging.
    let prepared = checked
        .prepare_token_indexes_at(
            &local,
            burst_types::Timestamp::new(expected.anchor().cutoff()),
        )
        .unwrap();
    local
        .meta_store()
        .put_meta("consti_engine", b"changed after preparation")
        .unwrap();
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    assert!(prepared.stage(&local).is_err());
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap(),
        before
    );
}

pub(in crate::confirmed_replay::tests) fn normalize_retention_fixture(
    local: &LmdbStore,
    params: &ProtocolParams,
) {
    let mut batch = local.tx_begin_write().unwrap();
    let payment = crate::payment_transition::restore(&batch, params).unwrap();
    crate::trst_storage::stage_replayed_stable(&mut batch, &payment.trst).unwrap();
    crate::graph_storage::stage_replayed(&mut batch, &payment.trst.merger_graph).unwrap();
    batch.begin_brn_wallet_records_projection().unwrap();
    for (owner, state) in payment.brn.wallets.iter() {
        batch.put_brn_wallet_state(owner, state).unwrap();
    }
    batch.normalize_verification_records().unwrap();
    batch.migrate_governance_records().unwrap();
    batch.commit().unwrap();
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
    for (case, retain_prefix) in (0..6).map(|case| (case, false)).chain([(0, true)]) {
        let (_dir, local) = cemented::sparse(&expected, &source);
        copy_installed_records(&local, &source, &fixture, &bytes);
        if retain_prefix {
            // This case retains the complete prefix and its height indexes;
            // the other cases use sparse storage.
            let mut batch = local.tx_begin_write().unwrap();
            for body in fixture.blocks.iter().filter(|body| body.hash != good.hash) {
                let height = source
                    .block_store()
                    .height_of_block(&body.hash)
                    .unwrap()
                    .unwrap();
                batch
                    .put_block_with_account(
                        &body.hash,
                        &source.block_store().get_block(&body.hash).unwrap(),
                        &body.account,
                        height,
                    )
                    .unwrap();
            }
            batch.commit().unwrap();
        }
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
        // Normalize the unchanged economic base so the same checked cut can
        // exercise required-body collection. This fixture has no undo journals.
        normalize_retention_fixture(&local, &fixture.params);
        let local = Arc::new(local);
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let service = WitnessService::start(Arc::clone(&source), Arc::clone(&local), 0).await;
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
            {
                let bodies = checked.body_requirements(&local).unwrap();
                assert!(bodies.has_root(block.hash).unwrap());
                for dependency in block.dependencies() {
                    assert!(bodies.has_root(dependency).unwrap());
                }
                bodies
                    .check_unchanged_cut(&local.tx_begin_write().unwrap())
                    .unwrap();
                assert_eq!(
                    local
                        .tx_begin_write()
                        .unwrap()
                        .get_account(&block.account)
                        .unwrap()
                        .confirmation_height,
                    confirmation
                );
            }
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
            for reject_before in [false, true] {
                let mut boundaries = Vec::new();
                let result = continuation.replay_applied_recorded_transitions(
                    checked.applied(),
                    &recovered.selections,
                    &[],
                    false,
                    &mut |before, applied, state| {
                        if let Some(applied) = applied {
                            assert_eq!(applied.hash, block.hash);
                            boundaries.push(before);
                            assert_eq!(
                                state.account(&block.account).unwrap().head,
                                if before { original.head } else { block.hash }
                            );
                            if before == reject_before {
                                return Err(super::super::super::invalid(
                                    "transition observer rejected",
                                ));
                            }
                        }
                        Ok(())
                    },
                );
                assert!(result.is_err());
                assert_eq!(
                    boundaries,
                    if reject_before {
                        vec![true]
                    } else {
                        vec![true, false]
                    }
                );
                assert_eq!(
                    continuation.state().account(&block.account).unwrap().head,
                    original.head
                );
            }
            let cache_at =
                burst_types::Timestamp::new(checked.recovered().continuation.anchor().cutoff());
            let prepared = checked.prepare_token_indexes_at(&local, cache_at).unwrap();
            {
                let roots = prepared.body_requirements(&local).unwrap();
                assert!(roots.has_root(block.hash).unwrap());
                assert!(roots.has_root(block.previous).unwrap());
                let mut found = false;
                roots
                    .visit_resolved_roots(|hash, resolved| {
                        if hash == block.hash {
                            let resolved = resolved.expect("replayed provisional body");
                            assert_eq!(resolved.origin(), installed::BodyOrigin::ProvisionalSuffix);
                            assert_eq!(resolved.block().hash, block.hash);
                            found = true;
                        }
                        Ok(())
                    })
                    .unwrap();
                assert!(found);
                roots
                    .check_unchanged_cut(&local.tx_begin_write().unwrap())
                    .unwrap();
                let mut missing = Vec::new();
                roots
                    .visit_resolved_roots(|hash, body| {
                        if body.is_none() {
                            missing.push(hash);
                        }
                        Ok(())
                    })
                    .unwrap();
                if retain_prefix {
                    assert!(missing.is_empty());
                    let disconnected = Arc::new(RwLock::new(ConnectionRegistry::new()));
                    let closed = roots
                        .close_from_peers(Default::default(), &disconnected)
                        .await
                        .unwrap();
                    assert!(closed.has_body(block.hash).unwrap());
                    assert!(closed.has_body(block.previous).unwrap());
                    let mut count = 0;
                    closed
                        .visit_bodies(|_| {
                            count += 1;
                            Ok(())
                        })
                        .unwrap();
                    assert_eq!(count, closed.work().bodies);
                    assert!(closed.work().bytes > 0);
                    closed
                        .check_unchanged_cut(&local.tx_begin_write().unwrap())
                        .unwrap();
                } else {
                    assert!(!missing.is_empty());
                    let error = roots
                        .close(Default::default())
                        .err()
                        .expect("sparse roots cannot close");
                    assert_eq!(
                        format!("{error:?}"),
                        format!("Unresolved({:?})", missing[0])
                    );
                    // A transport that returns no proofs must not manufacture
                    // a complete result or silently drop missing obligations.
                    let incomplete = Arc::new(RwLock::new(ConnectionRegistry::new()));
                    let (sender, mut receiver) =
                        mpsc::channel::<crate::checkpoint_sync::witness::Command>(1);
                    incomplete.write().await.set_witness_requests(sender);
                    let worker = tokio::spawn(async move {
                        let mut requests = 0;
                        while let Some(command) = receiver.recv().await {
                            requests += 1;
                            let _ = command.result.send(Ok(Vec::new()));
                        }
                        requests
                    });
                    assert!(prepared
                        .body_requirements(&local)
                        .unwrap()
                        .close_from_peers(Default::default(), &incomplete)
                        .await
                        .is_err());
                    drop(incomplete);
                    assert!(worker.await.unwrap() > 0);
                    let count_before = local.block_store().block_count().unwrap();
                    let service =
                        WitnessService::start(Arc::clone(&source), Arc::clone(&local), 0).await;
                    let closed = prepared
                        .body_requirements(&local)
                        .unwrap()
                        .close_from_peers(Default::default(), &service.registry)
                        .await
                        .unwrap();
                    assert!(service.stop().await > 0);
                    // Fetched bytes must remain accessible after transport stops.
                    for hash in missing {
                        let body = closed
                            .body(hash)
                            .unwrap()
                            .expect("retained checked witness");
                        assert_eq!(
                            bincode::serialize(&body).unwrap(),
                            source.block_store().get_block(&hash).unwrap()
                        );
                    }
                    let mut closure_bytes = 0u64;
                    closed
                        .visit_bodies(|hash| {
                            closure_bytes +=
                                bincode::serialized_size(&closed.body(hash)?.unwrap()).unwrap();
                            Ok(())
                        })
                        .unwrap();
                    assert_eq!(
                        closed.work().bytes,
                        closure_bytes,
                        "local and recovered bodies share one closure byte budget"
                    );
                    assert!(closed.body(BlockHash::new([99; 32])).unwrap().is_none());
                    closed
                        .check_unchanged_cut(&local.tx_begin_write().unwrap())
                        .unwrap();
                    local
                        .meta_store()
                        .put_meta("closure_recovery_stale_cut", &[1])
                        .unwrap();
                    assert!(closed
                        .check_unchanged_cut(&local.tx_begin_write().unwrap())
                        .is_err());
                    drop(closed);
                    // block_count opens a read transaction; release the plan's
                    // pinned reader first to respect LMDB's per-thread slot.
                    assert_eq!(local.block_store().block_count().unwrap(), count_before);
                    local
                        .meta_store()
                        .delete_meta("closure_recovery_stale_cut")
                        .unwrap();
                }
            }
            let projected = prepared.stage(&local).unwrap();
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
    copy_installed_records(&local, &source, &fixture, &bytes);
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
    // Retention reads require normalized storage. This installed-only fixture
    // has no provisional undo to convert; use the existing replay projection.
    {
        let mut batch = local.tx_begin_write().unwrap();
        assert!(batch
            .checkpoint_metadata_entries(b"block_undo/")
            .unwrap()
            .next()
            .is_none());
        let payment = crate::payment_transition::restore(&batch, &fixture.params).unwrap();
        crate::trst_storage::stage_replayed_stable(&mut batch, &payment.trst).unwrap();
        crate::graph_storage::stage_replayed(&mut batch, &payment.trst.merger_graph).unwrap();
        batch.commit().unwrap();
    }
    // Already-known historical work still needs its queue identity retained.
    local
        .meta_store()
        .enqueue_confirmation(&fixture.blocks[0].hash)
        .unwrap();
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"")
        .unwrap();
    let local = Arc::new(local);
    let service = WitnessService::start(Arc::new(source), Arc::clone(&local), 0).await;
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
    {
        use crate::confirmed_replay::snapshot::SelectionSource;
        let retained = checked.trst_references(&local).unwrap();
        let (base, steps) = retained.signer_authority();
        let mut authority = base.clone();
        for step in steps {
            authority = step.unwrap().authenticate(&authority).unwrap();
        }
        assert_eq!(authority.hash(), certificate.checkpoint.hash());
        assert!(authority.height() > checked.recovered().continuation.anchor().height());
        let mut selected = None;
        for selection in retained.signer_selections().iter_selections() {
            selected = Some(selection.unwrap().certificate().checkpoint.hash());
        }
        assert_eq!(selected, Some(certificate.checkpoint.hash()));
        let mut account_work = retained.account_work().unwrap();
        let mut accounts = 0;
        let mut pending = Vec::new();
        loop {
            let page = account_work
                .visit_next_page(1, 4096, |row| {
                    match row {
                        burst_store_lmdb::retention::AccountWorkRecord::Account(account) => {
                            assert_eq!(
                                account,
                                expected.state().account(&account.address).unwrap()
                            );
                            accounts += 1;
                        }
                        burst_store_lmdb::retention::AccountWorkRecord::PendingConfirmation(
                            hash,
                        ) => pending.push(hash),
                    }
                    Ok(())
                })
                .unwrap();
            if page.done {
                break;
            }
        }
        assert!(accounts > 0);
        assert_eq!(pending, vec![fixture.blocks[0].hash]);
        let mut signer_records = retained.signer_records();
        let mut observed = [false; 5];
        loop {
            use installed::{PaymentPromise, SignerRecord};
            let page = signer_records
                .visit_next_page(1, 4096, |_, record| {
                    match record {
                        SignerRecord::Payment(PaymentPromise::Final {
                            epoch: value,
                            signer,
                            account,
                            root,
                            block,
                        }) => {
                            assert_eq!(value, epoch);
                            assert_eq!(signer, address(0).as_str());
                            assert_eq!(account, address(0).as_str());
                            assert_eq!(root, BlockHash::ZERO);
                            assert_eq!(block, BlockHash::new([77; 32]));
                            observed[0] = true;
                        }
                        SignerRecord::Payment(PaymentPromise::Fence { epoch: value, .. }) => {
                            assert_eq!(value, epoch);
                            observed[1] = true;
                        }
                        SignerRecord::Time { value, .. } => {
                            assert_eq!(value, expected.anchor().cutoff());
                            observed[2] = true;
                        }
                        SignerRecord::Governance { value, .. } => {
                            assert_eq!(value, expected.anchor().cutoff() + 10);
                            observed[3] = true;
                        }
                        SignerRecord::Sequence { value, .. } => {
                            assert_eq!(value, 12);
                            observed[4] = true;
                        }
                        _ => (),
                    }
                    Ok(())
                })
                .unwrap();
            if page.done {
                break;
            }
        }
        assert!(observed.into_iter().all(|found| found));
        let mut rounds = retained.checkpoint_rounds();
        let mut rows = 0;
        loop {
            let page = rounds
                .visit_next_page(1, 64 << 20, |row| {
                    assert_eq!(row.height, certificate.checkpoint.height);
                    assert_eq!(
                        row.state.decision().unwrap().checkpoint.hash(),
                        certificate.checkpoint.hash()
                    );
                    assert!(row
                        .manifests
                        .unwrap()
                        .iter()
                        .any(|manifest| manifest.checkpoint().hash()
                            == certificate.checkpoint.hash()));
                    rows += 1;
                    Ok(())
                })
                .unwrap();
            if page.done {
                break;
            }
        }
        assert_eq!(rows, 1);

        retained
            .check_unchanged_cut(&local.tx_begin_write().unwrap())
            .unwrap();
        let sequence_key = format!("consensus_sequence/v1/{}", address(0));
        local
            .meta_store()
            .put_meta(&sequence_key, &13u64.to_be_bytes())
            .unwrap();
        let batch = local.tx_begin_write().unwrap();
        // Recovery explicitly permits a higher sequence; publication of this
        // older pinned cut must still reject it without restoring the previous sequence.
        checked.verify_unchanged(&batch).unwrap();
        assert!(retained.check_unchanged_cut(&batch).is_err());
        assert_eq!(
            batch.get_meta(&sequence_key).unwrap(),
            Some(13u64.to_be_bytes().to_vec())
        );
        drop(batch);
        assert_eq!(
            retained.metadata().recovered().continuation.anchor().hash(),
            epoch
        );
    }
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
    for case in 0..18 {
        let (_dir, local) = cemented::sparse(&expected, &source);
        copy_installed_records(&local, &source, &fixture, &bytes);
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
            15..=17 => {
                let genesis = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
                    NetworkId::Dev,
                    &fixture.params,
                )
                .unwrap()
                .hash();
                let (from, to) = match case {
                    15 => (genesis, genesis),
                    16 => (genesis, BlockHash::new([99; 32])),
                    _ => (epoch, genesis),
                };
                let mut value = to.as_bytes().to_vec();
                value.extend_from_slice(from.as_bytes());
                (burst_store::meta::closed_payment_epoch_key(&from), value)
            }
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
        let service = WitnessService::start(Arc::clone(&source), Arc::clone(&local), 0).await;
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
    static DURABLE_STOP: std::sync::Mutex<
        Option<(&'static str, Arc<crate::shutdown::ShutdownController>)>,
    > = std::sync::Mutex::new(None);
    static MUTATE: std::sync::Mutex<
        Option<(&'static str, Arc<LmdbStore>, BlockHash, Option<Vec<u8>>)>,
    > = std::sync::Mutex::new(None);
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
    for case in 0..12 {
        let (_dir, local) = cemented::sparse(&expected, &source);
        copy_installed_records(&local, &source, &fixture, &bytes);
        let mut batch = local.tx_begin_write().unwrap();
        if case == 0 || case == 7 || case >= 9 {
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
                    if case == 4 {
                        expected.state().account(&old.account).unwrap().block_count + 1
                    } else {
                        old.height + u64::from(case == 2)
                    },
                )
                .unwrap();
        }
        batch.commit().unwrap();
        let local = Arc::new(local);
        let service = WitnessService::start(Arc::clone(&source), Arc::clone(&local), 0).await;
        let map_bytes = 16 << 20;
        let limit = crate::confirmed_replay::scratch_file::ScratchLimit::acquire_reserved(
            local.env().path(),
            64 << 20,
            crate::confirmed_replay::snapshot::history::DurableHistoryInputs::reservation(
                map_bytes,
            )
            .unwrap(),
        )
        .unwrap();
        let shutdown = Arc::new(crate::shutdown::ShutdownController::new());
        if case <= 1 {
            let stage = if case == 0 {
                "private-recovery:local-history-page"
            } else {
                "private-recovery:durable-history-page"
            };
            *DURABLE_STOP.lock().unwrap() = Some((stage, shutdown.clone()));
            service.registry.write().await.recovery_probe = Some(|stage| {
                let stop = DURABLE_STOP.lock().unwrap();
                if let Some((expected, shutdown)) = stop.as_ref() {
                    if stage == *expected {
                        shutdown.shutdown();
                    }
                }
            });
        }

        if (5..9).contains(&case) || case >= 10 {
            let changed = if case >= 10 {
                let head = expected.state().account(&old.account).unwrap().head;
                rows.iter().find(|row| row.hash == head).unwrap()
            } else {
                old
            };
            let mut block: StateBlock = bincode::deserialize(&changed.bytes).unwrap();
            if case == 11 {
                block.signature = burst_types::Signature([99; 64]);
            } else {
                block.work = block.work.wrapping_add(1);
            }
            assert_eq!(
                block.compute_hash(),
                changed.hash,
                "unsigned mutation keeps the signed hash"
            );
            *MUTATE.lock().unwrap() = Some((
                if case >= 10 {
                    "private-recovery:confirmed-witnesses"
                } else if case == 8 {
                    "private-recovery:retained-inventory-page"
                } else {
                    "private-recovery:retained-planned"
                },
                local.clone(),
                changed.hash,
                if case == 6 {
                    None
                } else {
                    Some(bincode::serialize(&block).unwrap())
                },
            ));
            service.registry.write().await.recovery_probe = Some(|stage| {
                let mut mutation = MUTATE.lock().unwrap();
                if mutation.as_ref().is_some_and(|(at, _, _, _)| stage == *at) {
                    let (_, store, hash, bytes) = mutation.take().unwrap();
                    if let Some(bytes) = bytes {
                        store.block_store().put_block(&hash, &bytes).unwrap();
                    } else {
                        store.block_store().delete_block(&hash).unwrap();
                    }
                }
            });
        }

        let result = installed::recover_metadata_with_durable_inputs(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            if case == 9 {
                Arc::new(RwLock::new(ConnectionRegistry::new()))
            } else {
                Arc::clone(&service.registry)
            },
            shutdown.clone(),
            Some((limit.clone(), map_bytes)),
        )
        .await
        .map(|outcome| match outcome {
            installed::StartupRecovery::Ready(result) => result,
            installed::StartupRecovery::Cancelled if case <= 1 => None,
            installed::StartupRecovery::Cancelled => panic!("unexpected recovery cancellation"),
        });
        let requests = service.stop().await;
        if (2..9).contains(&case) || case >= 10 {
            assert!(result.is_err(), "corrupt retained case {case}");
            if case >= 10 {
                assert!(
                    MUTATE.lock().unwrap().is_none(),
                    "mutation must follow tail authentication"
                );
                assert!(result
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("retained head differs from authenticated proof"));
            } else if case == 4 {
                assert!(result
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("retained block exceeds local account height"));
            } else if (5..9).contains(&case) {
                assert!(
                    MUTATE.lock().unwrap().is_none(),
                    "mutation must follow inventory capture"
                );
                if case == 8 {
                    assert!(result
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("retained inventory changed"));
                } else if case != 6 {
                    assert!(result
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("retained payload differs from observation"));
                }
            }
            continue;
        }
        assert_eq!(requests > 0, case == 1);
        let checked = if case <= 1 {
            assert!(shutdown.is_requested());
            assert!(
                result.unwrap().is_none(),
                "cancelled preparation cannot be published"
            );
            *DURABLE_STOP.lock().unwrap() = None;
            if case == 1 {
                assert!(local
                    .env()
                    .path()
                    .join(crate::confirmed_replay::scratch_file::PERSISTENT_INPUTS_DIRECTORY)
                    .join("data.mdb")
                    .exists());
            }
            let service = WitnessService::start(Arc::clone(&source), Arc::clone(&local), 0).await;
            let checked = match installed::recover_metadata_with_durable_inputs(
                local.clone(),
                NetworkId::Dev,
                fixture.params.clone(),
                service.registry.clone(),
                Arc::new(crate::shutdown::ShutdownController::new()),
                Some((limit.clone(), map_bytes)),
            )
            .await
            .unwrap()
            {
                installed::StartupRecovery::Ready(Some(checked)) => checked,
                _ => panic!("durable retained recovery did not complete"),
            };
            assert_eq!(
                service.stop().await,
                0,
                "resumed local or cached ancestry must not request peers"
            );
            checked
        } else {
            result.unwrap().unwrap()
        };
        checked
            .verify_unchanged(&local.tx_begin_write().unwrap())
            .unwrap();
        let archived = checked
            .applied()
            .tail()
            .archive()
            .input_block(&old.hash)
            .unwrap()
            .is_some()
            || checked
                .cemented()
                .tail()
                .archive()
                .input_block(&old.hash)
                .unwrap()
                .is_some();
        let view = local.tx_begin_read().unwrap();
        let resolved = checked
            .resolve_body(&local, &view, &old.hash)
            .unwrap()
            .expect("authenticated old row");
        assert_eq!(resolved.origin(), installed::BodyOrigin::SnapshotPrefix);
        assert_eq!(
            resolved.context(),
            checked.recovered().continuation.history_context()
        );
        assert_eq!(bincode::serialize(resolved.block()).unwrap(), old.bytes);
        local
            .block_store()
            .put_block(&old.hash, b"changed after verification")
            .unwrap();
        assert_eq!(
            bincode::serialize(
                checked
                    .resolve_body(&local, &view, &old.hash)
                    .unwrap()
                    .unwrap()
                    .block()
            )
            .unwrap(),
            old.bytes
        );
        drop(view);
        let unknown = BlockHash::new([99; 32]);
        local
            .block_store()
            .put_block(&unknown, b"unverified raw body")
            .unwrap();
        let view = local.tx_begin_read().unwrap();
        if archived {
            // Immutable, independently checked archive bytes remain usable;
            // the live-cut guard below must still reject publication.
            assert_eq!(
                bincode::serialize(
                    checked
                        .resolve_body(&local, &view, &old.hash)
                        .unwrap()
                        .unwrap()
                        .block()
                )
                .unwrap(),
                old.bytes
            );
        } else {
            assert!(checked.resolve_body(&local, &view, &old.hash).is_err());
        }
        assert!(checked
            .resolve_body(&local, &view, &unknown)
            .unwrap()
            .is_none());
        drop(view);
        assert!(checked
            .verify_unchanged(&local.tx_begin_write().unwrap())
            .is_err());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn installed_recovery_targets_the_installed_epoch_not_the_accepted_tip() {
    let (fixture, _dir, source, manifest, certificate) = selection::setup();
    let bytes = archive(&source, &fixture, manifest.checkpoint.parent);
    // The successor changes checkpoint/selection state, not account history.
    // Prove this prerequisite before asserting that recovery needs no peer I/O.
    let base =
        authenticate(&source, &fixture.params, manifest.checkpoint.parent).into_continuation();
    for head in &manifest.heads {
        let account = base.state().account(&head.account).unwrap();
        assert_eq!(account.head, head.hash);
        assert_eq!(account.block_count, head.height);
    }

    let source = Arc::new(source);
    for epoch in [manifest.checkpoint.parent, certificate.checkpoint.hash()] {
        let expected = authenticate(&source, &fixture.params, epoch).into_continuation();
        let (dir, local) = cemented::sparse(&expected, &source);
        copy_installed_records(&local, &source, &fixture, &bytes);
        local.force_sync().unwrap();
        drop(local);
        let local = Arc::new(LmdbStore::open(dir.path(), 32, 32 << 20).unwrap());
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let count = local.block_store().block_count().unwrap();
        let service = WitnessService::start(Arc::clone(&source), Arc::clone(&local), 0).await;
        let result = installed::recover_metadata_from_peers(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&service.registry),
        )
        .await;
        let requests = service.stop().await;
        assert_eq!(
            requests, 0,
            "unchanged account heads are covered by the snapshot"
        );
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
    copy_installed_records(&local, &source, &fixture, &bytes);
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
        copy_installed_records(&local, &source, &fixture, &bytes);
        let local = Arc::new(local);
        let service = WitnessService::start(source.clone(), local.clone(), 0).await;
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
    static CACHE_HITS: AtomicUsize = AtomicUsize::new(0);
    for change in 0..6 {
        let (_dir, local) = cemented::sparse(&expected, &source);
        copy_installed_records(&local, &source, &fixture, &older);
        let local = Arc::new(local);
        let service = WitnessService::start(source.clone(), local.clone(), 0).await;
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
            4 | 5 => {}
            _ => unreachable!(),
        }
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        CACHE_HITS.store(0, Ordering::SeqCst);
        service.registry.write().await.recovery_probe = Some(|stage| {
            if stage == "tip-recovery:cached-base-verified" {
                CACHE_HITS.fetch_add(1, Ordering::SeqCst);
            }
        });
        let mut initial = fixture.params.clone();
        if change == 4 {
            initial.bootstrap_exit_threshold += 1;
        }
        let network = if change == 5 {
            NetworkId::Test
        } else {
            NetworkId::Dev
        };
        let result = installed::recover_current_tip_until_shutdown(
            local.clone(),
            network,
            initial,
            service.registry.clone(),
            Some(forked),
            Arc::new(crate::shutdown::ShutdownController::new()),
        )
        .await;
        if change < 2 {
            let checked = result.unwrap().unwrap();
            compact::assert_same(checked.recovered.continuation.state(), expected.state());
            assert_eq!(CACHE_HITS.load(Ordering::SeqCst), usize::from(change == 0));
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
                "corrupt or mismatched input {change} must not be hidden by replay reuse"
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
        copy_installed_records(&local, &source, &fixture, &older);
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
        let service = WitnessService::start(source.clone(), local.clone(), change).await;
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
        copy_installed_records(&local, &source, &fixture, &bytes);
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
        let service = WitnessService::start(Arc::clone(&source), Arc::clone(&local), 0).await;
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
    let service = WitnessService::start(Arc::new(source), Arc::clone(&local), 0).await;
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
        .expect("sparse old ancestor needs a membership proof");
    for change in [0, 1, 2, 3, 4] {
        let (_dir, local) = cemented::sparse(&expected, &source);
        copy_installed_records(&local, &source, &fixture, &bytes);
        // Head-only inventory can be authenticated without requesting old history.
        // Retain an old non-head row so the mutation hook must deliver a proof.
        let mut batch = local.tx_begin_write().unwrap();
        batch
            .put_block_with_account(&old.hash, &old.bytes, &old.account, old.height)
            .unwrap();
        batch.commit().unwrap();
        let local = Arc::new(local);
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"")
            .unwrap();
        let service = WitnessService::start(Arc::clone(&source), Arc::clone(&local), change).await;
        let result = installed::recover_metadata_from_peers(
            Arc::clone(&local),
            NetworkId::Dev,
            fixture.params.clone(),
            Arc::clone(&service.registry),
        )
        .await;
        assert!(
            service.stop().await > 0,
            "change={change}, error={:?}",
            result.as_ref().err()
        );
        if change == 0 {
            result
                .unwrap()
                .unwrap()
                .verify_unchanged(&local.tx_begin_write().unwrap())
                .unwrap();
            assert_eq!(
                local
                    .tx_begin_write()
                    .unwrap()
                    .checkpoint_metadata(b"")
                    .unwrap(),
                before
            );
            continue;
        }
        assert!(
            result.is_err(),
            "change={change} returned publishable recovery"
        );
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
            copy_installed_records(&local, &source, &fixture, &bytes);
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
        let service = WitnessService::start(Arc::clone(&source), Arc::clone(&local), 0).await;
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
