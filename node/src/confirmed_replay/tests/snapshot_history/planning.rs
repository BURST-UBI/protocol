use super::*;

#[test]
fn snapshot_tail_plan_discovers_old_sources_and_finishes_only_with_authenticated_witnesses() {
    let mut fixture = Fixture::selection();
    let burn = fixture.push(0, BlockType::Burn, 1_595_434_040, |block| {
        block.link = destination(30);
        block.brn_balance += 1;
    });
    fixture.push(0, BlockType::ChangeRepresentative, 1_595_434_041, |_| {});
    let (_dir, source, snapshot) = snapshot(&fixture);
    let cutoff = snapshot.anchor().cutoff() + 10;
    fixture.push(30, BlockType::Open, cutoff, |block| {
        block.payment_epoch = snapshot.anchor().hash();
        block.link = burn;
        block.trst_balance = 1;
    });
    let full = fixture.history(cutoff);
    let (_dir, suffix) = suffix_store(std::slice::from_ref(fixture.blocks.last().unwrap()));
    let plan = || snapshot.plan_tail(&suffix, cutoff, full.heads()).unwrap();
    let required = plan().witness_hashes().to_vec();
    assert!(required.contains(&burn));
    assert!(required
        .windows(2)
        .all(|pair| pair[0].as_bytes() < pair[1].as_bytes()));
    assert!(!required.contains(&fixture.blocks.last().unwrap().hash));
    let proofs = || {
        compact::retained(&snapshot, &fixture, &source, &required)
            .into_iter()
            .filter(|proof| required.contains(&proof.block().hash))
            .collect::<Vec<_>>()
    };
    assert_eq!(proofs().len(), required.len());
    let mut missing = proofs();
    missing.retain(|proof| proof.block().hash != burn);
    assert!(plan().finish(missing).is_err());
    let mut duplicate = proofs();
    duplicate.push(proofs().pop().unwrap());
    assert!(plan().finish(duplicate).is_err());
    let other_fixture = Fixture::selection();
    let (_dir, other_source, other_snapshot) = super::snapshot(&other_fixture);
    let mut foreign = proofs();
    foreign.push(
        compact::retained(&other_snapshot, &other_fixture, &other_source, &[])
            .pop()
            .unwrap(),
    );
    assert!(plan().finish(foreign).is_err());
    let tail = plan().finish(proofs()).unwrap();
    assert_eq!(tail.commitment(), full.commitment());
    assert_eq!(tail.block_count(), 1);
    assert_eq!(tail.retained().len(), required.len());
    let legacy = snapshot
        .read_tail(&suffix, cutoff, full.heads(), proofs())
        .unwrap();
    assert_eq!(
        bincode::serialize(&tail.collected_blocks()).unwrap(),
        bincode::serialize(&legacy.collected_blocks()).unwrap()
    );
}

#[test]
fn snapshot_tail_plan_pins_signed_bytes_and_rejects_later_corruption_on_a_new_read() {
    let mut fixture = Fixture::selection();
    let (_dir, source, snapshot) = snapshot(&fixture);
    let cutoff = snapshot.anchor().cutoff() + 10;
    fixture.push(0, BlockType::ChangeRepresentative, cutoff, |block| {
        block.payment_epoch = snapshot.anchor().hash();
    });
    let full = fixture.history(cutoff);
    let block = fixture.blocks.last().unwrap();
    let (_dir, suffix) = suffix_store(std::slice::from_ref(block));
    let plan = snapshot.plan_tail(&suffix, cutoff, full.heads()).unwrap();
    let required: HashSet<_> = plan.witness_hashes().iter().copied().collect();
    assert!(required.contains(&block.previous));
    let proofs = compact::retained(&snapshot, &fixture, &source, plan.witness_hashes())
        .into_iter()
        .filter(|proof| required.contains(&proof.block().hash))
        .collect();
    suffix
        .meta_store()
        .put_meta(&format!("checkpoint_history/v1/{}", block.hash), b"corrupt")
        .unwrap();
    assert!(snapshot.plan_tail(&suffix, cutoff, full.heads()).is_err());
    let tail = plan.finish(proofs).unwrap();
    assert_eq!(
        bincode::serialize(&tail.blocks().next().unwrap().unwrap()).unwrap(),
        bincode::serialize(block).unwrap()
    );
    assert_eq!(tail.commitment(), full.commitment());
}
