use super::*;
use burst_store::account::AccountStore;

#[test]
fn compact_projection_uses_only_authenticated_heads_and_finality_witnesses() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    for used in [false, true] {
        let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
        let (_local_dir, local) = if used {
            let (dir, store, _) = fixture.store();
            (dir, store)
        } else {
            local_genesis(&fixture)
        };
        let before = local.block_store().block_count().unwrap();
        let needs = snapshot.installation_needs(&local).unwrap();
        let mut hashes: Vec<_> = needs.cemented_heads.iter().map(|head| head.hash).collect();
        hashes.extend(needs.confirmation_targets);
        let proofs = witnesses(&snapshot, &fixture, &source, &hashes);
        let heads: Vec<_> = needs.snapshot_heads.iter().map(|head| head.hash).collect();
        let bodies = witnesses(&snapshot, &fixture, &source, &heads);
        let checked = snapshot
            .reconcile_installation(
                local.tx_begin_write().unwrap(),
                NetworkId::Dev,
                &fixture.params,
                &proofs,
            )
            .unwrap();
        let projected = checked.project_ledger(&bodies).unwrap();
        assert_eq!(
            projected.snapshot().anchor().hash(),
            certificate.checkpoint.hash()
        );
        assert_eq!(projected.ledger_blocks(), fixture.blocks.len() as u64);
        if !used {
            assert!(projected.retained_blocks() < projected.ledger_blocks());
            assert_eq!(projected.retained_blocks() as usize, heads.len() + 1);
        } else {
            assert_eq!(projected.retained_blocks(), projected.ledger_blocks());
        }
        assert!(projected.retired_blocks().is_empty());
        // Readers and restart see the original ledger until the complete
        // economics/authority handoff exists. The public guard cannot commit.
        assert_eq!(local.block_store().block_count().unwrap(), before);
        drop(projected);
        assert_eq!(local.block_store().block_count().unwrap(), before);
        assert_eq!(
            local.meta_store().payment_epoch().unwrap(),
            fixture.blocks[0].hash
        );
    }
}

#[test]
fn compact_projection_rejects_missing_foreign_and_duplicate_bodies_without_writes() {
    let (fixture, _source_dir, source, _, certificate) = selection::setup();
    for case in 0..3 {
        let snapshot = authenticate(&source, &fixture.params, certificate.checkpoint.hash());
        let (_dir, local) = local_genesis(&fixture);
        let proofs = witnesses(&snapshot, &fixture, &source, &[fixture.blocks[0].hash]);
        let needs = snapshot.installation_needs(&local).unwrap();
        let hashes: Vec<_> = needs.snapshot_heads.iter().map(|head| head.hash).collect();
        let mut bodies = witnesses(&snapshot, &fixture, &source, &hashes);
        match case {
            0 => {
                bodies.pop();
            }
            1 => {
                let earlier = authenticate(&source, &fixture.params, certificate.checkpoint.parent);
                bodies = witnesses(&earlier, &fixture, &source, &hashes);
            }
            2 => bodies.extend(witnesses(&snapshot, &fixture, &source, &hashes[..1])),
            _ => unreachable!(),
        }
        let checked = snapshot
            .reconcile_installation(
                local.tx_begin_write().unwrap(),
                NetworkId::Dev,
                &fixture.params,
                &proofs,
            )
            .unwrap();
        assert!(checked.project_ledger(&bodies).is_err(), "case {case}");
        assert_eq!(local.block_store().block_count().unwrap(), 1);
        assert_eq!(
            local.account_store().get_account(&address(0)).unwrap().head,
            fixture.blocks[0].hash
        );
        assert_eq!(
            local.meta_store().payment_epoch().unwrap(),
            fixture.blocks[0].hash
        );
    }
}
