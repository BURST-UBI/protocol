use super::*;

#[test]
fn governance_proposal_and_vote_economics() {
    let mut brn = BrnEngine::new();
    let mut trst = TrstEngine::with_expiry(86400 * 365);
    let now = Timestamp::new(5000);

    let proposer = make_address(202);
    let voter = make_address(203);

    // GovernanceProposal block
    let mut proposal_block = make_block(
        BlockType::GovernanceProposal,
        &proposer,
        BlockHash::new([1u8; 32]),
        &proposer,
        1000,
        500,
        BlockHash::ZERO,
        TxHash::ZERO,
        now.as_secs(),
    );
    proposal_block.transaction = TxHash::new([0xAA; 32]);
    proposal_block.hash = proposal_block.compute_hash();

    let result = burst_node::process_block_economics(
        &proposal_block,
        &mut brn,
        &mut trst,
        now,
        86400 * 365,
        1000,
    );
    match result {
        burst_node::EconomicResult::GovernanceProposal {
            proposer: p,
            proposal_hash,
            ..
        } => {
            assert_eq!(p, proposer);
            assert_eq!(proposal_hash, proposal_block.hash.into_tx_hash());
            assert_ne!(proposal_hash, proposal_block.transaction);
        }
        other => panic!("expected GovernanceProposal, got {:?}", other),
    }

    // GovernanceVote block (link = proposal hash, transaction[0] = vote value)
    let proposal_hash = proposal_block.hash.into_tx_hash();
    let mut vote_block = make_block(
        BlockType::GovernanceVote,
        &voter,
        BlockHash::new([2u8; 32]),
        &voter,
        1000,
        500,
        BlockHash::new(*proposal_hash.as_bytes()),
        TxHash::ZERO,
        now.as_secs() + 10,
    );
    vote_block.transaction = TxHash::new({
        let mut bytes = [0u8; 32];
        bytes[0] = 0; // Yea
        bytes
    });
    vote_block.hash = vote_block.compute_hash();

    let result = burst_node::process_block_economics(
        &vote_block,
        &mut brn,
        &mut trst,
        Timestamp::new(now.as_secs() + 10),
        86400 * 365,
        1000,
    );
    match result {
        burst_node::EconomicResult::GovernanceVote {
            voter: v,
            proposal_hash: ph,
            vote,
        } => {
            assert_eq!(v, voter);
            assert_eq!(ph, proposal_hash);
            assert_eq!(vote, burst_transactions::governance::GovernanceVote::Yea);
        }
        other => panic!("expected GovernanceVote, got {:?}", other),
    }
}
