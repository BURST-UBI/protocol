use super::*;

#[test]
fn election_lifecycle_vote_and_confirm() {
    use burst_consensus::Election;

    let block_hash = BlockHash::new([0xAA; 32]);
    let rep1 = make_address(226);
    let rep2 = make_address(227);
    let rep3 = make_address(228);

    // online_weight=1000 → confirmation_threshold = 1000 * 6700 / 10000 = 670
    let online_weight = 1000u128;
    let mut election = Election::new(block_hash, online_weight, Timestamp::new(1000));

    assert!(!election.is_confirmed());

    // Soft votes cannot confirm this election, even above its threshold.
    election.vote(&rep1, block_hash, 700, false, Timestamp::new(1001));
    election.try_confirm(Timestamp::new(1001));
    assert!(
        !election.is_confirmed(),
        "soft (non-final) votes must not cement a block"
    );

    // Final votes must reach the 670 threshold.
    // rep1 upgrades to final 300 — final 300 < 670
    election.vote(&rep1, block_hash, 300, true, Timestamp::new(1002));
    election.try_confirm(Timestamp::new(1002));
    assert!(!election.is_confirmed());

    // rep2 final 200 — final 500 < 670
    election.vote(&rep2, block_hash, 200, true, Timestamp::new(1003));
    election.try_confirm(Timestamp::new(1003));
    assert!(!election.is_confirmed());

    // rep3 final 200 — final 700 ≥ 670 → confirms
    election.vote(&rep3, block_hash, 200, true, Timestamp::new(1004));
    let status = election.try_confirm(Timestamp::new(1004));
    assert!(election.is_confirmed());
    assert!(status.is_some());
    let status = status.unwrap();
    assert_eq!(status.winner, block_hash);
    assert_eq!(status.final_tally, 700);
}
