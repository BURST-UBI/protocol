use super::*;
use crate::{
    checkpoint_deadline::PreparedDeadline, checkpoint_governance::PreparedGovernance,
    checkpoint_outcome::PreparedChallengeOutcome, checkpoint_replay::ReplayedCheckpoint,
};
use burst_consensus::snapshot::SnapshotAuthorityStep;

#[test]
fn unchanged_roster_recovery_cut_uses_separate_domain_and_old_quorum() {
    let fixture = Fixture::new();
    let state = fixture.replay(1).unwrap();
    let anchor = ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &fixture.params).unwrap();
    let root = BlockHash::new([9; 32]);
    let build = |cutoff| {
        crate::checkpoint_roster::PreparedRoster::manifest_for_cut(
            &anchor,
            cutoff,
            root,
            &[],
            &state,
        )
    };
    assert!(build(anchor.cutoff() + 59).is_err());
    let (manifest, roster) = build(anchor.cutoff() + 60).unwrap();
    assert_eq!(roster, *anchor.roster());
    assert_eq!(
        manifest.checkpoint.selection_root,
        crate::checkpoint_recovery::commitment()
    );
    assert_ne!(
        manifest.checkpoint.selection_root,
        crate::checkpoint_roster::commitment()
    );
    let certificate = certified_votes::certify(&manifest.checkpoint, &[0]);
    anchor.verify_certificate(&certificate).unwrap();
    let next = anchor.advance(&certificate, roster).unwrap();
    assert_ne!(next.hash(), anchor.hash());
    assert_eq!(next.roster(), anchor.roster());
}

#[test]
fn compact_manifests_cross_height_128_and_reject_wrong_parent_state() {
    let fixture = Fixture::new();
    let mut state = fixture.replay(1).unwrap();
    let mut anchor = ReplayedCheckpoint::genesis_anchor(NetworkId::Dev, &fixture.params).unwrap();
    let root = BlockHash::new([9; 32]);
    let proposal = TxHash::new([9; 32]);

    // Exercise manifest construction and certificate authority, not deadline
    // execution. Production callers must first derive effects by compact replay.
    for height in 1..=129 {
        state.payment_epoch = anchor.hash();
        let (manifest, roster) = PreparedDeadline::manifest_for_cut(
            &anchor,
            anchor.cutoff() + 1,
            root,
            &[],
            &state,
            root,
        )
        .unwrap();
        assert_eq!(manifest.checkpoint.height, height);
        let certificate = certified_votes::certify(&manifest.checkpoint, &[0]);
        anchor = SnapshotAuthorityStep {
            certificate,
            roster: roster.humans().to_vec(),
        }
        .authenticate(&anchor)
        .unwrap();
    }

    // The state still names height 128 while authority has advanced to 129.
    assert!(PreparedDeadline::manifest_for_cut(&anchor, 130, root, &[], &state, root).is_err());
    assert!(PreparedGovernance::manifest_for_cut(
        &anchor,
        130,
        root,
        &[],
        vec![proposal],
        root,
        &state
    )
    .is_err());
    assert!(PreparedChallengeOutcome::manifest_for_cut(
        &anchor,
        130,
        root,
        &[],
        &address(1),
        &state,
        root
    )
    .is_err());

    state.payment_epoch = anchor.hash();
    let (governance, _) =
        PreparedGovernance::manifest_for_cut(&anchor, 130, root, &[], vec![proposal], root, &state)
            .unwrap();
    let (outcome, _) = PreparedChallengeOutcome::manifest_for_cut(
        &anchor,
        130,
        root,
        &[],
        &address(1),
        &state,
        root,
    )
    .unwrap();
    assert_eq!(governance.checkpoint.height, 130);
    assert_eq!(outcome.checkpoint.height, 130);
    anchor.validate_successor(&governance.checkpoint).unwrap();
    anchor.validate_successor(&outcome.checkpoint).unwrap();
}
