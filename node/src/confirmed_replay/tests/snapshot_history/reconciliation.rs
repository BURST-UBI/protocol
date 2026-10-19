use super::*;
use crate::{
    checkpoint_agreement::{self, Command, ReplayInput},
    checkpoint_roster::PreparedRoster,
    checkpoint_store::chain,
    confirmed_replay::{installation::PreparedInstallation, snapshot::history::HistoricalBlock},
};
use burst_store::{block::BlockStore, meta::MetaStore};

mod dependencies;
mod economics;
mod guards;
mod projection;
mod record_retirement;
pub(in crate::confirmed_replay::tests) mod rollover;

fn local_genesis(fixture: &Fixture) -> (tempfile::TempDir, LmdbStore) {
    let minimal = Fixture {
        params: fixture.params.clone(),
        blocks: vec![fixture.blocks[0].clone()],
    };
    let (dir, store, _) = minimal.store();
    (dir, store)
}

fn witnesses(
    snapshot: &AuthenticatedSnapshot,
    fixture: &Fixture,
    source: &LmdbStore,
    needed: &[BlockHash],
) -> Vec<HistoricalBlock> {
    let mut missing: HashSet<_> = needed.iter().copied().collect();
    let accounts: HashSet<_> = fixture
        .blocks
        .iter()
        .filter(|block| missing.contains(&block.hash))
        .map(|block| block.account.clone())
        .collect();
    let mut retained = Vec::new();
    for account in accounts {
        let mut cursor = snapshot.history_cursor(&account).unwrap().unwrap();
        while let Some(next) = cursor.next() {
            for witness in cursor
                .accept(&read_chunk(source, next.hash, MAX_WITNESS_BLOCKS).unwrap())
                .unwrap()
            {
                if missing.remove(&witness.block().hash) {
                    retained.push(witness);
                }
            }
        }
    }
    assert!(missing.is_empty());
    retained
}

#[test]
fn snapshot_reconciliation_checks_a_fresh_node_without_importing_the_prefix() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
    let proofs = witnesses(&snapshot, &fixture, &source, &[fixture.blocks[0].hash]);
    assert_eq!(proofs.len(), 1);
    let (_dir, local) = local_genesis(&fixture);
    let needs = snapshot.installation_needs(&local).unwrap();
    assert_eq!(needs.cemented_heads.len(), 1);
    assert_eq!(needs.cemented_heads[0].hash, fixture.blocks[0].hash);
    assert!(needs.confirmation_targets.is_empty());
    let checked = snapshot
        .reconcile_installation(
            local.tx_begin_write().unwrap(),
            NetworkId::Dev,
            &fixture.params,
            &proofs,
        )
        .unwrap();
    assert_eq!(checked.local_epoch().unwrap(), fixture.blocks[0].hash);
    assert_eq!(
        checked.snapshot().anchor().hash(),
        certificate.checkpoint.hash()
    );
    assert_eq!(checked.confirmed_accounts(), 1);
    assert!(checked.confirmation_targets().is_empty());
    assert!(checked.retained_promises().is_empty());
    drop(checked);
    assert_eq!(local.block_store().block_count().unwrap(), 1);
    assert_eq!(
        local.meta_store().payment_epoch().unwrap(),
        fixture.blocks[0].hash
    );
}

#[test]
fn snapshot_reconciliation_preserves_a_used_nodes_installed_authority() {
    let (fixture, dir, local, _, certificate) = selection::setup();
    let snapshot = authenticate(&local, &fixture.params, certificate.checkpoint.hash());
    let installed = PreparedInstallation::next(&local, NetworkId::Dev, &fixture.params)
        .unwrap()
        .unwrap()
        .commit(&local)
        .unwrap();
    assert_eq!(installed.anchor.height(), 1);
    let epoch = installed.anchor.hash();
    let count = local.block_store().block_count().unwrap();
    let needs = snapshot.installation_needs(&local).unwrap();
    assert!(needs.cemented_heads.is_empty());
    assert!(needs.confirmation_targets.is_empty());
    let checked = snapshot
        .reconcile_installation(
            local.tx_begin_write().unwrap(),
            NetworkId::Dev,
            &fixture.params,
            &[],
        )
        .unwrap();
    assert_eq!(checked.local_epoch().unwrap(), epoch);
    assert!(checked.confirmed_accounts() > 1);
    drop(checked);
    drop(local);
    let local = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    assert_eq!(local.meta_store().payment_epoch().unwrap(), epoch);
    assert_eq!(local.block_store().block_count().unwrap(), count);
}

#[test]
fn snapshot_reconciliation_retains_minority_promises_and_pending_confirmation_work() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
    let genesis = fixture.blocks[0].hash;
    let proofs = witnesses(&snapshot, &fixture, &source, &[genesis]);
    let (dir, local) = local_genesis(&fixture);
    let minority = BlockHash::new([98; 32]);
    local
        .meta_store()
        .reserve_consensus_vote(
            &genesis,
            &address(0),
            &address(0),
            &genesis,
            &minority,
            true,
            0,
        )
        .unwrap();
    let mut batch = local.tx_begin_write().unwrap();
    batch
        .seal_consensus_after_certificate(&address(0), &genesis)
        .unwrap();
    batch.commit().unwrap();
    local.meta_store().enqueue_confirmation(&genesis).unwrap();
    let before = local
        .tx_begin_write()
        .unwrap()
        .checkpoint_metadata(b"consensus_")
        .unwrap();
    let checked = snapshot
        .reconcile_installation(
            local.tx_begin_write().unwrap(),
            NetworkId::Dev,
            &fixture.params,
            &proofs,
        )
        .unwrap();
    assert_eq!(checked.retained_promises(), &[minority]);
    assert_eq!(checked.confirmation_targets(), &[genesis]);
    drop(checked);
    drop(local);
    let local = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    assert_eq!(
        local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"consensus_")
            .unwrap(),
        before
    );
    assert_eq!(
        local.meta_store().pending_confirmations().unwrap(),
        vec![genesis]
    );
}

#[test]
fn snapshot_reconciliation_rejects_missing_foreign_and_conflicting_finality_proofs() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    for case in 0..5 {
        let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
        let mut proofs = witnesses(&snapshot, &fixture, &source, &[fixture.blocks[0].hash]);
        let (_dir, local) = local_genesis(&fixture);
        match case {
            0 => proofs.clear(),
            1 => {
                let earlier = authenticate(&source, &fixture.params, certificate.checkpoint.parent);
                proofs = witnesses(&earlier, &fixture, &source, &[fixture.blocks[0].hash]);
            }
            2 => local
                .meta_store()
                .enqueue_confirmation(&BlockHash::new([99; 32]))
                .unwrap(),
            3 => {
                // This valid signed local branch differs from the imported prefix.
                let mut fork = fixture
                    .blocks
                    .iter()
                    .find(|block| block.previous == fixture.blocks[0].hash)
                    .unwrap()
                    .clone();
                fork.representative = address(99);
                sign(0, &mut fork);
                let mut batch = local.tx_begin_write().unwrap();
                batch
                    .put_block_with_account(
                        &fork.hash,
                        &bincode::serialize(&fork).unwrap(),
                        &fork.account,
                        2,
                    )
                    .unwrap();
                let mut account = batch.get_account(&fork.account).unwrap();
                account.head = fork.hash;
                account.block_count = 2;
                account.confirmation_height = 2;
                batch.put_account_info(&account, false).unwrap();
                batch.commit().unwrap();
                let canonical = fixture
                    .blocks
                    .iter()
                    .find(|block| block.previous == fixture.blocks[0].hash)
                    .unwrap();
                proofs = witnesses(
                    &snapshot,
                    &fixture,
                    &source,
                    &[fixture.blocks[0].hash, canonical.hash],
                );
            }
            _ => proofs.extend(witnesses(
                &snapshot,
                &fixture,
                &source,
                &[fixture.blocks[0].hash],
            )),
        }
        let count = local.block_store().block_count().unwrap();
        assert!(
            snapshot
                .reconcile_installation(
                    local.tx_begin_write().unwrap(),
                    NetworkId::Dev,
                    &fixture.params,
                    &proofs
                )
                .is_err(),
            "case {case}"
        );
        assert_eq!(local.block_store().block_count().unwrap(), count);
        assert_eq!(
            local.meta_store().payment_epoch().unwrap(),
            fixture.blocks[0].hash
        );
    }
}

#[test]
fn snapshot_reconciliation_owns_the_writer_and_aborts_dirty_inputs() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
    let proofs = witnesses(&snapshot, &fixture, &source, &[fixture.blocks[0].hash]);
    let (_dir, local) = local_genesis(&fixture);
    let mut dirty = local.tx_begin_write().unwrap();
    dirty
        .put_meta("snapshot-test/uncommitted", b"discard")
        .unwrap();
    assert!(snapshot
        .reconcile_installation(dirty, NetworkId::Dev, &fixture.params, &proofs)
        .is_err());
    assert!(local
        .tx_begin_write()
        .unwrap()
        .get_meta("snapshot-test/uncommitted")
        .unwrap()
        .is_none());

    let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
    let local = std::sync::Arc::new(local);
    assert!(snapshot
        .installation_needs(&local)
        .unwrap()
        .confirmation_targets
        .is_empty());
    let checked = snapshot
        .reconcile_installation(
            local.tx_begin_write().unwrap(),
            NetworkId::Dev,
            &fixture.params,
            &proofs,
        )
        .unwrap();
    let worker_store = local.clone();
    let (started, ready) = std::sync::mpsc::channel();
    let (finished, completed) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        started.send(()).unwrap();
        worker_store
            .meta_store()
            .enqueue_confirmation(&BlockHash::new([99; 32]))
            .unwrap();
        finished.send(()).unwrap();
    });
    ready
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(matches!(
        completed.recv_timeout(std::time::Duration::from_millis(50)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    drop(checked);
    completed
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    worker.join().unwrap();
    let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
    assert_eq!(
        snapshot
            .installation_needs(&local)
            .unwrap()
            .confirmation_targets,
        vec![BlockHash::new([99; 32])]
    );
    assert!(snapshot
        .reconcile_installation(
            local.tx_begin_write().unwrap(),
            NetworkId::Dev,
            &fixture.params,
            &proofs
        )
        .is_err());
}

#[test]
fn snapshot_reconciliation_respects_decisions_not_yet_in_the_chain() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    for conflicting in [false, true] {
        let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
        let (_dir, local, _) = fixture.store();
        let roster = PreparedRoster::prepare_next(
            &local,
            NetworkId::Dev,
            &fixture.params,
            certificate.checkpoint.cutoff - if conflicting { 2 } else { 1 },
        )
        .unwrap()
        .unwrap();
        let parent = crate::checkpoint_replay::ReplayedCheckpoint::genesis_anchor(
            NetworkId::Dev,
            &fixture.params,
        )
        .unwrap();
        checkpoint_agreement::apply_candidates(
            &local,
            &parent,
            &burst_crypto::keypair_from_seed(&[0; 32]).private,
            Command::Decide(certified_votes::certify(
                &roster.manifest().checkpoint,
                &[0],
            )),
            &[ReplayInput::Roster(&roster)],
            None,
        )
        .unwrap();
        assert!(chain::load(&local, NetworkId::Dev, &fixture.params)
            .unwrap()
            .is_empty());
        let before = local
            .tx_begin_write()
            .unwrap()
            .checkpoint_metadata(b"checkpoint_round/")
            .unwrap();
        let result = snapshot.reconcile_installation(
            local.tx_begin_write().unwrap(),
            NetworkId::Dev,
            &fixture.params,
            &[],
        );
        assert_eq!(result.is_err(), conflicting);
        drop(result);
        assert_eq!(
            local
                .tx_begin_write()
                .unwrap()
                .checkpoint_metadata(b"checkpoint_round/")
                .unwrap(),
            before
        );
    }
}
