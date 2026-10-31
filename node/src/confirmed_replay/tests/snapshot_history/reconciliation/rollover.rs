use super::*;
use crate::{
    checkpoint_agreement::{CompactReplayCandidate, ReplayManifest},
    checkpoint_replay::{ReplayedCheckpoint, SigningClock},
    confirmed_replay::snapshot::installed,
};

mod electorate;

fn install(fixture: &Fixture, store: &LmdbStore, epoch: BlockHash, signers: &[u8]) {
    let export = SnapshotExport::prepare(store, NetworkId::Dev, &fixture.params, epoch).unwrap();
    let signatures: Vec<_> = signers
        .iter()
        .map(|seed| {
            export
                .attest(
                    store,
                    &burst_crypto::keypair_from_seed(&[*seed; 32]).private,
                )
                .unwrap()
        })
        .collect();
    let mut bytes = Vec::new();
    export.write(&signatures, &mut bytes).unwrap();
    let genesis = ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &fixture.params).unwrap();
    let snapshot =
        AuthenticatedSnapshot::read_in(&genesis, &mut bytes.as_slice(), store.env().path()).unwrap();
    let needs = snapshot.installation_needs(store).unwrap();
    let hashes: Vec<_> = needs
        .snapshot_heads
        .iter()
        .chain(&needs.cemented_heads)
        .map(|head| head.hash)
        .collect();
    let proofs = witnesses(&snapshot, fixture, store, &hashes);
    snapshot
        .reconcile_installation(
            store.tx_begin_write().unwrap(),
            NetworkId::Dev,
            &fixture.params,
            &proofs,
        )
        .unwrap()
        .project_ledger(&[])
        .unwrap()
        .project_economics()
        .unwrap()
        .commit_for_startup()
        .unwrap();
    store.force_sync().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn repeated_snapshot_rollover_replays_real_roster_changes_beyond_height_128() {
    let mut fixture = Fixture::new();
    fixture.params.bootstrap_exit_threshold = 2;
    fixture.blocks[0] = crate::genesis_state::block(NetworkId::Dev, &fixture.params);
    fixture.push(0, BlockType::Endorse, 100, |block| {
        block.brn_spent = 1;
        block.link = destination(1);
    });
    fixture.push(1, BlockType::Open, 101, |_| {});
    let (directory, mut store, _) = fixture.store();
    let first = PreparedRoster::prepare_next(&store, NetworkId::Dev, &fixture.params, 110)
        .unwrap()
        .unwrap();
    let mut epoch = first.manifest().checkpoint.hash();
    chain::accept_manifest(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certified_votes::certify(&first.manifest().checkpoint, &[0]),
        &ReplayManifest::Roster(first.manifest().clone()),
    )
    .unwrap();
    install(&fixture, &store, epoch, &[0, 1]);
    let first_manifest = SnapshotExport::prepare(&store, NetworkId::Dev, &fixture.params, epoch)
        .unwrap()
        .manifest()
        .clone();
    let prefix = format!("checkpoint_chain/v1/dev/{}/", fixture.blocks[0].hash);
    let mut historical_manifests = vec![first_manifest];
    for height in 2..=130 {
        let base = installed::load_base(&store, NetworkId::Dev, &fixture.params)
            .unwrap()
            .unwrap()
            .into_continuation();
        let at = base.anchor().cutoff() + 1;
        let representative = if height % 2 == 0 { 0 } else { 1 };
        fixture.push(1, BlockType::ChangeRepresentative, at, |block| {
            block.payment_epoch = epoch;
            block.representative = address(representative);
        });
        cemented::append(&store, fixture.blocks.last().unwrap(), true);
        let tail = cemented::finish(&base, &store, &store);
        let candidate = base
            .prepare_next_cemented_candidate(
                &store,
                &tail,
                &[],
                SigningClock {
                    now: at + 2,
                    error_bound: 0,
                },
            )
            .unwrap()
            .unwrap();
        assert!(matches!(&candidate, CompactReplayCandidate::Roster(_)));
        let manifest = candidate.input().manifest();
        assert_eq!(manifest.checkpoint().height, height);
        let signers: Vec<_> = (0..=1)
            .filter(|seed| base.anchor().roster().weight(&address(*seed)) > 0)
            .collect();
        let certificate = certified_votes::certify(manifest.checkpoint(), &signers);
        crate::checkpoint_store::compact::accept_candidate(
            &store,
            base.anchor(),
            candidate.input(),
            &certificate,
        )
        .unwrap();
        epoch = certificate.checkpoint.hash();
        let next_signers: &[u8] = if representative == 0 { &[0] } else { &[0, 1] };
        install(&fixture, &store, epoch, next_signers);
        if height == 129 {
            historical_manifests.push(
                SnapshotExport::prepare(&store, NetworkId::Dev, &fixture.params, epoch)
                    .unwrap()
                    .manifest()
                    .clone(),
            );
        }
        assert_eq!(store.meta_store().payment_epoch().unwrap(), epoch);
        assert_eq!(
            store
                .account_store()
                .get_account(&address(1))
                .unwrap()
                .representative,
            address(representative)
        );
        assert_eq!(
            store
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(prefix.as_bytes())
                .unwrap()
                .len(),
            1
        );
        if height % 32 == 0 || height == 130 {
            drop(store);
            store = LmdbStore::open(directory.path(), 32, 32 << 20).unwrap();
            let recovered = installed::load_base(&store, NetworkId::Dev, &fixture.params)
                .unwrap()
                .unwrap()
                .recover_stored_successors(&store, |_, _, _| panic!("no uninstalled successor"))
                .unwrap();
            assert_eq!(recovered.continuation.anchor().height(), height);
            assert_eq!(recovered.continuation.anchor().hash(), epoch);
            assert_eq!(recovered.continuation.anchor().roster().total_humans(), 2);
        }
    }
    assert!(chain::load(&store, NetworkId::Dev, &fixture.params).is_err());
    for manifest in historical_manifests {
        let historical =
            SnapshotExport::prepare(&store, NetworkId::Dev, &fixture.params, manifest.epoch)
                .unwrap();
        assert_eq!(historical.manifest(), &manifest);
        historical
            .attest(&store, &burst_crypto::keypair_from_seed(&[0; 32]).private)
            .unwrap();
    }
    for height in [1, 64, 128, 130] {
        assert!(CertifiedCheckpoint::export_record(
            &store,
            NetworkId::Dev,
            &fixture.params,
            height
        )
        .unwrap()
        .is_some());
    }
    let expected = installed::load_base(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .into_state();
    store.meta_store().delete_meta(installed::BASE_KEY).unwrap();
    store.force_sync().unwrap();
    drop(store);
    let store = std::sync::Arc::new(LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
    let service =
        super::super::installed_recovery::WitnessService::start(store.clone(), store.clone(), 0).await;
    let recovered = installed::recover_metadata_from_peers(
        store.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!recovered.recovered().uses_snapshot_archive());
    assert_eq!(recovered.recovered().continuation.anchor().height(), 130);
    assert_eq!(recovered.recovered().continuation.anchor().hash(), epoch);
    compact::assert_same(recovered.recovered().continuation.state(), &expected);
    recovered
        .verify_unchanged(&store.tx_begin_write().unwrap())
        .unwrap();
    service.stop().await;
}

pub(in crate::confirmed_replay::tests) async fn prepare_retired_rosters(
    mut observe: impl FnMut(u64, &LmdbStore, BlockHash),
) -> (Fixture, tempfile::TempDir, BlockHash) {
    use super::super::installed_recovery::WitnessService;
    use std::sync::Arc;

    let mut fixture = Fixture::new();
    fixture.params.bootstrap_exit_threshold = 2;
    fixture.blocks[0] = crate::genesis_state::block(NetworkId::Dev, &fixture.params);
    fixture.push(0, BlockType::Endorse, 100, |block| {
        block.brn_spent = 1;
        block.link = destination(1);
    });
    fixture.push(1, BlockType::Open, 101, |_| {});
    let (directory, store, _) = fixture.store();
    let first = PreparedRoster::prepare_next(&store, NetworkId::Dev, &fixture.params, 110)
        .unwrap()
        .unwrap();
    let mut epoch = first.manifest().checkpoint.hash();
    chain::accept_manifest(
        &store,
        NetworkId::Dev,
        &fixture.params,
        certified_votes::certify(&first.manifest().checkpoint, &[0]),
        &ReplayManifest::Roster(first.manifest().clone()),
    )
    .unwrap();
    PreparedInstallation::next(&store, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&store)
        .unwrap();
    let prefix = format!("checkpoint_chain/v1/dev/{}", fixture.blocks[0].hash);
    let archived = format!(
        "checkpoint_record_archive/v1/dev/{}/1",
        fixture.blocks[0].hash
    );
    let original = store.meta_store().get_meta(&format!("{prefix}/1")).unwrap();
    let store = Arc::new(store);
    let service = WitnessService::start(store.clone(), store.clone(), 0).await;
    let mut pending_burn = None;
    for height in 2..=130 {
        let recovered = installed::recover_metadata_from_peers(
            store.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
        )
        .await
        .unwrap()
        .unwrap();
        let base = &recovered.recovered().continuation;
        let at = base.anchor().cutoff() + 1;
        let payment = if let Some(burn) = pending_burn.take() {
            Some(fixture.push(1, BlockType::Receive, at, |block| {
                block.payment_epoch = epoch;
                block.link = burn;
                block.trst_balance += 1;
            }))
        } else if height % 32 == 0 {
            let burn = fixture.push(0, BlockType::Burn, at, |block| {
                block.payment_epoch = epoch;
                block.link = destination(1);
                block.brn_spent += 1;
            });
            pending_burn = Some(burn);
            Some(burn)
        } else {
            None
        };
        if payment.is_some() {
            // This fixture supplies finality directly. Use the real economic
            // transition so catch-up must reconstruct pending and received funds.
            let block = fixture.blocks.last().unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            crate::payment_transition::apply_elected(&mut batch, block, &fixture.params).unwrap();
            let account = batch.get_account(&block.account).unwrap();
            batch
                .advance_confirmation_height(&block.account, account.block_count)
                .unwrap();
            batch.commit().unwrap();
        }
        let representative = if height % 2 == 0 { 0 } else { 1 };
        fixture.push(1, BlockType::ChangeRepresentative, at + 1, |block| {
            block.payment_epoch = epoch;
            block.representative = address(representative);
        });
        cemented::append(&store, fixture.blocks.last().unwrap(), true);
        let tail = cemented::finish(base, &store, &store);
        {
            let mut batch = store.tx_begin_write().unwrap();
            let mut account = batch.get_account(&address(1)).unwrap();
            account.representative = address(representative);
            batch.put_frontier(&account.address, &account.head).unwrap();
            batch.put_account_info(&account, false).unwrap();
            crate::verification_commit::dependencies::bind_identity(
                &mut batch,
                fixture.blocks.last().unwrap(),
            )
            .unwrap();
            batch.commit().unwrap();
        }
        let candidate = base
            .prepare_next_cemented_candidate(
                &store,
                &tail,
                &[],
                SigningClock {
                    now: at + 2,
                    error_bound: 0,
                },
            )
            .unwrap()
            .unwrap();
        assert!(matches!(&candidate, CompactReplayCandidate::Roster(_)));
        let manifest = candidate.input().manifest();
        assert_eq!(manifest.checkpoint().height, height);
        let signers: Vec<_> = (0..=1)
            .filter(|seed| base.anchor().roster().weight(&address(*seed)) > 0)
            .collect();
        let certificate = certified_votes::certify(manifest.checkpoint(), &signers);
        if height == 129 {
            store
                .meta_store()
                .put_meta(&archived, b"conflicting archive")
                .unwrap();
            let before = store
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"")
                .unwrap();
            assert!(crate::checkpoint_store::compact::accept_candidate(
                &store,
                base.anchor(),
                candidate.input(),
                &certificate,
            )
            .is_err());
            assert_eq!(
                store
                    .tx_begin_write()
                    .unwrap()
                    .checkpoint_metadata(b"")
                    .unwrap(),
                before
            );
            store.meta_store().delete_meta(&archived).unwrap();
        }
        crate::checkpoint_store::compact::accept_candidate(
            &store,
            base.anchor(),
            candidate.input(),
            &certificate,
        )
        .unwrap();
        epoch = certificate.checkpoint.hash();
        // Acceptance changed the observed record set. Reload installed authority
        // before projecting the accepted successor, as the runtime does.
        drop(recovered);
        let recovered = installed::recover_metadata_from_peers(
            store.clone(),
            NetworkId::Dev,
            fixture.params.clone(),
            service.registry.clone(),
        )
        .await
        .unwrap()
        .unwrap();
        recovered
            .prepare_next_from_peers(store.clone(), service.registry.clone())
            .await
            .unwrap()
            .unwrap()
            .commit(&store)
            .unwrap();
        assert_eq!(store.meta_store().payment_epoch().unwrap(), epoch);
        if let Some(burn) = pending_burn {
            assert_eq!(
                store
                    .pending_store()
                    .get_pending(&address(1), &burn.into_tx_hash())
                    .unwrap()
                    .amount,
                1
            );
        }
        assert!(store.meta_store().get_meta(installed::BASE_KEY).is_err());
        assert_eq!(
            store
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(format!("{prefix}/").as_bytes())
                .unwrap()
                .len(),
            height.min(128) as usize + 1
        );
        observe(height, &store, epoch);
    }
    assert_eq!(store.meta_store().get_meta(&archived).unwrap(), original);
    assert!(pending_burn.is_none());
    assert_eq!(
        store
            .account_store()
            .get_account(&address(1))
            .unwrap()
            .trst_balance,
        4
    );
    service.stop().await;
    drop(store);
    (fixture, directory, epoch)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn automatic_record_retirement_advances_without_snapshots_and_recovers() {
    use super::super::installed_recovery::WitnessService;
    use std::sync::Arc;
    let (fixture, directory, epoch) = prepare_retired_rosters(|_, _, _| {}).await;
    let store = Arc::new(LmdbStore::open(directory.path(), 32, 32 << 20).unwrap());
    let service = WitnessService::start(store.clone(), store.clone(), 0).await;
    let recovered = installed::recover_metadata_from_peers(
        store.clone(),
        NetworkId::Dev,
        fixture.params.clone(),
        service.registry.clone(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(recovered.recovered().continuation.anchor().height(), 130);
    assert_eq!(recovered.recovered().continuation.anchor().hash(), epoch);
    assert!(!recovered.recovered().uses_snapshot_archive());
    recovered
        .verify_unchanged(&store.tx_begin_write().unwrap())
        .unwrap();
    for height in [1, 2, 128, 130] {
        assert!(CertifiedCheckpoint::export_record(
            &store,
            NetworkId::Dev,
            &fixture.params,
            height,
        )
        .unwrap()
        .is_some());
    }
    service.stop().await;
}
