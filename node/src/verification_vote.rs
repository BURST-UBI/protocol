//! Admission checks for signed humanity votes, before any BRN is locked.

use burst_ledger::StateBlock;
use burst_types::ProtocolParams;
use burst_verification::VerificationOrchestrator;

pub(crate) fn availability_error(
    block_type: &burst_ledger::BlockType,
    network: burst_types::NetworkId,
) -> Option<String> {
    if !burst_verification::checkpoint_selection_enabled(network)
        && matches!(
            block_type,
            burst_ledger::BlockType::VerificationVote | burst_ledger::BlockType::Challenge
        )
    {
        Some(
            "certified verification is disabled for this build and network; no stake was locked"
                .into(),
        )
    } else {
        None
    }
}

pub(crate) fn validate(
    block: &StateBlock,
    previous_brn: u128,
    orchestrator: &VerificationOrchestrator,
    params: &ProtocolParams,
) -> Result<(), String> {
    if block.params_hash != params.params_hash() {
        return Err("verification vote uses different protocol parameters".into());
    }
    let encoded = block.transaction.as_bytes();
    if !(1..=3).contains(&encoded[0]) || encoded[1..].iter().any(|byte| *byte != 0) {
        return Err("invalid verification vote encoding".into());
    }
    let expected = if encoded[0] == 3 {
        0
    } else {
        params.verifier_stake_amount
    };
    if block.brn_balance.checked_sub(previous_brn) != Some(expected) {
        return Err(format!(
            "verification vote must lock exactly {expected} raw BRN"
        ));
    }
    let target = crate::ledger_bridge::extract_receiver_from_link(&block.link)
        .ok_or("verification vote has an invalid target")?;
    let selection_params = orchestrator
        .selection_request(&target)
        .ok_or("verification committee is not certified")?
        .params_hash();
    orchestrator
        .validate_certified_vote(
            &target,
            &block.account,
            burst_types::BlockHash::new(*block.origin.as_bytes()),
            selection_params,
            block.timestamp,
        )
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verification_test_support::{select, time};
    use burst_types::{BlockHash, TxHash, WalletAddress};
    use burst_wallet_core::transaction_builder::{
        build_state_block, build_verification_vote_tx, AccountState,
    };

    #[test]
    fn uncertified_verification_cannot_lock_vote_or_challenge_stakes() {
        use burst_ledger::BlockType;
        for kind in [BlockType::VerificationVote, BlockType::Challenge] {
            assert!(availability_error(&kind, burst_types::NetworkId::Live).is_some());
            assert_eq!(
                availability_error(&kind, burst_types::NetworkId::Test).is_none(),
                burst_verification::checkpoint_selection_enabled(burst_types::NetworkId::Test),
            );
        }
        for kind in [BlockType::Endorse, BlockType::Send, BlockType::Receive] {
            assert!(availability_error(&kind, burst_types::NetworkId::Live).is_none());
        }
    }

    fn address(seed: u8) -> WalletAddress {
        burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[seed; 32]).public)
    }

    fn fixture(vote: u8, stake: u128) -> (StateBlock, VerificationOrchestrator, ProtocolParams) {
        let params = ProtocolParams {
            endorsement_threshold: 1,
            endorsement_burn_amount: 100,
            num_verifiers: 2,
            verifier_stake_amount: 10,
            ..ProtocolParams::default()
        };
        let target = address(1);
        let voter = address(2);
        let mut orchestrator = VerificationOrchestrator::new();
        orchestrator
            .process_endorsement_at(&target, &address(3), 100, &params, time(100))
            .unwrap();
        let context = select(
            &mut orchestrator,
            &target,
            &[voter.clone(), address(4)],
            &params,
        );
        let transaction =
            build_verification_vote_tx(&voter, &target, context, vote, stake, time(1000)).unwrap();
        let state = AccountState { signing_grant: burst_types::BlockHash::ZERO,
            payment_epoch: burst_types::BlockHash::new([1; 32]),
            release_sources: Vec::new(),
            head: BlockHash::new([1; 32]),
            block_count: 1,
            representative: voter,
            brn_balance: 20,
            trst_balance: 0,
        };
        let block = build_state_block(
            &state,
            &burst_transactions::Transaction::VerificationVote(transaction),
            TxHash::ZERO,
            params.params_hash(),
        )
        .unwrap();
        (block, orchestrator, params)
    }

    #[test]
    fn current_vote_rules_do_not_reselect_the_certified_panel() {
        let (mut block, orchestrator, params) = fixture(1, 10);
        let mut governed = params.clone();
        governed.brn_rate += 1;
        governed.verifier_stake_amount = 17;
        block.params_hash = governed.params_hash();
        block.brn_balance = 37;
        assert!(validate(&block, 20, &orchestrator, &governed).is_ok());
        block.brn_balance = 30;
        assert!(validate(&block, 20, &orchestrator, &governed).is_err());
        block.brn_balance = 37;
        block.params_hash = params.params_hash();
        assert!(validate(&block, 20, &orchestrator, &governed).is_err());
    }

    #[test]
    fn selected_votes_require_exact_stakes_and_neither_requires_zero() {
        for (vote, stake) in [(1, 10), (2, 10), (3, 0)] {
            let (block, orchestrator, params) = fixture(vote, stake);
            assert!(validate(&block, 20, &orchestrator, &params).is_ok());
            assert!(orchestrator
                .get_state(&address(1))
                .unwrap()
                .votes
                .is_empty());
        }
        for (vote, stake) in [(1, 0), (1, 11), (2, 9), (3, 10)] {
            let (block, orchestrator, params) = fixture(vote, stake);
            assert!(validate(&block, 20, &orchestrator, &params).is_err());
        }
    }

    #[test]
    fn altered_context_parameters_and_pre_beacon_votes_are_rejected() {
        let (block, orchestrator, params) = fixture(1, 10);
        for context in [TxHash::ZERO, TxHash::new([99; 32])] {
            let mut altered = block.clone();
            altered.origin = context;
            assert!(validate(&altered, 20, &orchestrator, &params).is_err());
        }
        let mut altered = block.clone();
        altered.params_hash = BlockHash::new([99; 32]);
        assert!(validate(&altered, 20, &orchestrator, &params).is_err());
        altered = block;
        altered.timestamp = time(100);
        assert!(validate(&altered, 20, &orchestrator, &params).is_err());
        assert!(orchestrator
            .get_state(&address(1))
            .unwrap()
            .votes
            .is_empty());
    }

    #[test]
    fn restored_legacy_committee_is_not_treated_as_certified() {
        let (block, orchestrator, params) = fixture(1, 10);
        let snapshot = orchestrator.snapshot();
        let mut bytes = b"BURST-VERIFICATION\x03".to_vec();
        bytes.extend(
            bincode::serialize(&(
                snapshot.states,
                snapshot.active_challenges,
                snapshot.penalized_verifiers,
                (
                    std::collections::HashMap::<String, (u32, u32)>::new(),
                    5000u32,
                ),
                snapshot.prior_votes,
            ))
            .unwrap(),
        );
        let restored = VerificationOrchestrator::restore(
            burst_verification::OrchestratorSnapshot::decode(&bytes).unwrap(),
        );
        assert!(restored
            .get_state(&address(1))
            .unwrap()
            .selected_verifiers
            .contains(&address(2)));
        assert!(restored.selection_context(&address(1)).is_none());
        assert!(validate(&block, 20, &restored, &params).is_err());
    }

    #[test]
    fn malformed_unknown_and_duplicate_votes_are_rejected_without_mutating_state() {
        let (mut block, mut orchestrator, params) = fixture(1, 10);
        block.account = address(99);
        assert!(validate(&block, 20, &orchestrator, &params).is_err());
        block.account = address(2);
        for bytes in [[0; 32], [4; 32], [1; 32]] {
            block.transaction = TxHash::new(bytes);
            assert!(validate(&block, 20, &orchestrator, &params).is_err());
        }
        let mut bytes = [0; 32];
        bytes[0] = 1;
        block.transaction = TxHash::new(bytes);
        assert!(validate(&block, 21, &orchestrator, &params).is_err());
        orchestrator
            .process_certified_vote_at(
                &address(1),
                &address(2),
                burst_verification::Vote::Legitimate,
                BlockHash::new(*block.origin.as_bytes()),
                &params,
                block.timestamp,
            )
            .unwrap();
        assert!(validate(&block, 20, &orchestrator, &params).is_err());
        assert_eq!(orchestrator.get_state(&address(1)).unwrap().votes.len(), 1);
    }
}
