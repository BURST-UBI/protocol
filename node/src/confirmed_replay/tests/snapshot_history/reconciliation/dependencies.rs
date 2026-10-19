use super::*;
use burst_store::account::AccountStore;

#[test]
fn snapshot_dependency_materialization_preserves_economics_and_rejects_foreign_proofs() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
    let (_dir, local) = local_genesis(&fixture);
    let heads = snapshot.installation_needs(&local).unwrap().snapshot_heads;
    let mut needed: Vec<_> = heads.iter().map(|head| head.hash).collect();
    needed.push(fixture.blocks[0].hash);
    needed.sort();
    needed.dedup();
    let old = fixture
        .blocks
        .iter()
        .find(|block| !needed.contains(&block.hash))
        .unwrap();
    let proof = witnesses(&snapshot, &fixture, &source, &[old.hash]);
    let context = proof[0].context();
    let head_proofs = witnesses(&snapshot, &fixture, &source, &needed);
    snapshot
        .reconcile_installation(
            local.tx_begin_write().unwrap(),
            NetworkId::Dev,
            &fixture.params,
            &head_proofs,
        )
        .unwrap()
        .project_ledger(&[])
        .unwrap()
        .project_economics()
        .unwrap()
        .commit_for_startup()
        .unwrap();
    assert!(!local.block_store().exists(&old.hash).unwrap());
    let before = local.account_store().get_account(&old.account).unwrap();
    let epoch = local.meta_store().payment_epoch().unwrap();
    let mut foreign = context;
    foreign.state_hash = BlockHash::new([99; 32]);
    {
        let mut batch = local.tx_begin_write().unwrap();
        assert!(
            crate::historical_dependencies::install(&mut batch, foreign, &[old.hash], &proof)
                .is_err()
        );
    }
    assert!(!local.block_store().exists(&old.hash).unwrap());
    {
        let mut batch = local.tx_begin_write().unwrap();
        assert!(
            crate::historical_dependencies::install(&mut batch, context, &[old.hash], &[]).is_err()
        );
    }
    {
        let mut batch = local.tx_begin_write().unwrap();
        assert_eq!(
            crate::historical_dependencies::install(&mut batch, context, &[old.hash], &proof)
                .unwrap(),
            1
        );
        batch.commit().unwrap();
    }
    assert!(local.block_store().exists(&old.hash).unwrap());
    assert_eq!(local.meta_store().payment_epoch().unwrap(), epoch);
    assert_eq!(
        bincode::serialize(&before).unwrap(),
        bincode::serialize(&local.account_store().get_account(&old.account).unwrap()).unwrap()
    );
    let mut batch = local.tx_begin_write().unwrap();
    assert_eq!(
        crate::historical_dependencies::install(&mut batch, context, &[old.hash], &proof).unwrap(),
        0
    );
    let wrong = BlockHash::new([98; 32]);
    batch
        .put_block_with_account(
            &wrong,
            b"conflicting index",
            &old.account,
            proof[0].height(),
        )
        .unwrap();
    assert!(
        crate::historical_dependencies::install(&mut batch, context, &[old.hash], &proof).is_err()
    );
}
