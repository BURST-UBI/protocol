use super::*;
use burst_verification::bootstrap::BootstrapPhase;

#[test]
fn approved_threshold_changes_close_bootstrap_atomically_without_reopening_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut state = candidate();
    let mut batch = store.tx_begin_write().unwrap();
    for seed in [0, 1] {
        let address =
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[seed; 32]).public);
        let mut account = burst_store::account::AccountInfo::unopened(address);
        account.state = burst_types::WalletState::Verified;
        batch.put_account_info(&account, false).unwrap();
    }
    crate::bootstrap_phase::initialize(&mut batch, &state.params).unwrap();
    batch.commit().unwrap();
    let block = block(BlockType::GovernanceActivation);
    let mut brn = BrnEngine::new();
    let mut trst = TrstEngine::new();
    for (seed, threshold) in [(1, 2), (2, 1000)] {
        let id = TxHash::new([seed; 32]);
        state
            .governance
            .submit_proposal(
                burst_governance::proposal::Proposal {
                    hash: id,
                    proposer: block.account.clone(),
                    content: ProposalContent::ParameterChange {
                        param: GovernableParam::BootstrapExitThreshold,
                        new_value: threshold,
                    },
                    phase: GovernancePhase::Proposal,
                    created_at: Timestamp::new(1),
                    endorsement_count: 0,
                    exploration_votes_yea: 0,
                    exploration_votes_nay: 0,
                    exploration_votes_abstain: 0,
                    promotion_votes_yea: 0,
                    promotion_votes_nay: 0,
                    promotion_votes_abstain: 0,
                    exploration_started_at: None,
                    cooldown_started_at: None,
                    promotion_started_at: None,
                    activation_at: Some(Timestamp::new(block.timestamp.as_secs() - 10)),
                    total_eligible_voters: 2,
                    round: 0,
                },
                u128::MAX,
                true,
                &state.params,
            )
            .unwrap();
        // This is an approved-proposal component fixture, not a tally test.
        state.governance.get_proposal_mut(&id).unwrap().phase = GovernancePhase::Activation;
        let proposal = state.governance.get_proposal(&id).unwrap();
        let hash = state
            .governance
            .preview_activation(proposal, &state.params)
            .unwrap()
            .params_hash();
        let economic = EconomicResult::GovernanceActivation {
            proposal_hash: id,
            new_params_hash: hash,
        };
        let prior_phase = crate::bootstrap_phase::require(&store).unwrap();
        for commit in [false, true] {
            let mut staged = state.clone();
            let mut batch = store.tx_begin_write().unwrap();
            staged
                .apply(&mut batch, &block, &economic, &mut brn, &mut trst)
                .unwrap();
            assert_eq!(
                crate::bootstrap_phase::read(&batch).unwrap(),
                BootstrapPhase::Complete
            );
            if commit {
                batch.commit().unwrap();
                state = staged;
            } else {
                drop(batch);
            }
            assert_eq!(
                crate::bootstrap_phase::require(&store).unwrap(),
                if commit {
                    BootstrapPhase::Complete
                } else {
                    prior_phase
                }
            );
        }
    }
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let params: ProtocolParams = bincode::deserialize(
        &store
            .brn_store()
            .get_meta(b"protocol_params")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(params.bootstrap_exit_threshold, 1000);
    assert_eq!(
        crate::bootstrap_phase::require(&store).unwrap(),
        BootstrapPhase::Complete
    );
}
