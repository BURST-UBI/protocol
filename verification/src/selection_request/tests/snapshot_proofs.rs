use super::*;

#[test]
fn selection_snapshot_is_independent_of_the_valid_agreement_round() {
    let (pending, _, request) = pending();
    let (parent, first) = certificate(&request);
    let mut second = first.clone();
    second.round = 3;
    second.signatures[0].signature = sign_message(
        &burst_consensus::checkpoint_round::Ballot::precommit(&second.checkpoint, second.round)
            .signing_payload(),
        &keypair_from_seed(&[4; 32]).private,
    );
    let mut left = pending.clone();
    let mut right = pending;
    let context = left
        .install_selection(&address(99), &parent, &first, &output())
        .unwrap();
    assert_eq!(
        right
            .install_selection(&address(99), &parent, &second, &output())
            .unwrap(),
        context
    );
    let left_bytes = left.snapshot().encode().unwrap();
    let right_bytes = right.snapshot().encode().unwrap();
    crate::orchestrator::snapshot_tests::assert_legacy_selection_migration(&left, &first);
    crate::orchestrator::snapshot_tests::assert_legacy_selection_migration(&right, &second);
    assert!(
        left_bytes == right_bytes,
        "equivalent certified decisions must have identical persisted execution state"
    );
    let mut restored =
        VerificationOrchestrator::restore(OrchestratorSnapshot::decode(&left_bytes).unwrap());
    assert_eq!(restored.selection_context(&address(99)), Some(context));
    restored
        .install_selection(&address(99), &parent, &second, &output())
        .unwrap();
    assert_eq!(restored.snapshot().encode().unwrap(), left_bytes);
    second.signatures.clear();
    assert!(restored
        .install_selection(&address(99), &parent, &second, &output())
        .is_err());
    assert_eq!(restored.snapshot().encode().unwrap(), left_bytes);
}
