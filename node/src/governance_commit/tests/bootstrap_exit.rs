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
        let prior_phase = crate::bootstrap_phase::require(&store).unwrap();
        for commit in [false, true] {
            let mut staged = state.clone();
            let mut batch = store.tx_begin_write().unwrap();
            staged
                .activate_approved(&mut batch, id, &hash, block.timestamp, &mut brn, &mut trst)
                .unwrap();
            batch.put_brn_meta(b"protocol_params", &bincode::serialize(&staged.params).unwrap()).unwrap();
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

#[test]
fn expiry_activation_overflow_aborts_without_publishing_params_or_portfolios() {
    for retained in [false, true] { expiry_activation_overflow_case(retained); }
}

fn expiry_activation_overflow_case(retained: bool) {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut state = candidate();
    state.params.trst_expiry_secs = 10;
    let block = block(BlockType::GovernanceActivation);
    let id = TxHash::new([71; 32]);
    state.governance.submit_proposal(burst_governance::proposal::Proposal {
        hash: id, proposer: block.account.clone(),
        content: ProposalContent::ParameterChange {
            param: GovernableParam::TrstExpirySecs, new_value: 1000,
        },
        phase: GovernancePhase::Proposal, created_at: Timestamp::new(1),
        endorsement_count: 0, exploration_votes_yea: 0, exploration_votes_nay: 0,
        exploration_votes_abstain: 0, promotion_votes_yea: 0, promotion_votes_nay: 0,
        promotion_votes_abstain: 0, exploration_started_at: None,
        cooldown_started_at: None, promotion_started_at: None,
        activation_at: Some(Timestamp::new(90)), total_eligible_voters: 2, round: 0,
    }, u128::MAX, true, &state.params).unwrap();
    state.governance.get_proposal_mut(&id).unwrap().phase = GovernancePhase::Activation;
    let hash = state.governance.preview_activation(
        state.governance.get_proposal(&id).unwrap(), &state.params).unwrap().params_hash();
    let mut trst = TrstEngine::with_expiry(10);
    for (n, amount) in [(1, u128::MAX), (2, 1)] {
        let mut token = trst.mint(TxHash::new([n; 32]), block.account.clone(), amount,
            block.account.clone(), Timestamp::new(1)).unwrap();
        token.state = burst_types::TrstState::Expired;
        trst.track_token(token);
    }
    if retained {
        let mut checked = burst_trst::engine::RestoredTrstState::from_engine(trst).unwrap();
        checked.retain_portfolio_indexes().unwrap();
        trst = checked.into_engine();
    }
    let original = trst.save_wallets();
    let prior_params = bincode::serialize(&state.params).unwrap();
    {
        let mut batch = store.tx_begin_write().unwrap();
        batch.put_brn_meta(b"protocol_params", &prior_params).unwrap();
        crate::bootstrap_phase::initialize(&mut batch, &state.params).unwrap();
        batch.commit().unwrap();
    }
    let mut staged = state.clone();
    let mut batch = store.tx_begin_write().unwrap();
    let error = staged.activate_approved(&mut batch, id, &hash, block.timestamp, &mut BrnEngine::new(), &mut trst).unwrap_err();
    assert!(error.contains("TRST holding balance overflow"), "{error}");
    drop(batch);
    assert_eq!(trst.save_wallets(), original);
    assert_eq!(trst.expiry_secs, 10);
    assert_eq!(state.params.trst_expiry_secs, 10);
    assert_eq!(state.governance.get_proposal(&id).unwrap().phase, GovernancePhase::Activation);
    drop(store);
    let reopened = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    assert_eq!(reopened.brn_store().get_meta(b"protocol_params").unwrap().unwrap(), prior_params);
}

#[test]
fn creator_veto_is_authenticated_by_identity_and_one_way_bootstrap_state() {
    for (count, complete, signer, accepted) in [
        (49, false, 0, true), (50, false, 0, false),
        (1, true, 0, false), (1, false, 1, false),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
        let mut state = candidate();
        state.params.bootstrap_exit_threshold = 1000;
        let mut batch = store.tx_begin_write().unwrap();
        for seed in 0..count {
            let keys = burst_crypto::keypair_from_seed(&[seed; 32]);
            let mut account = burst_store::account::AccountInfo::unopened(burst_crypto::derive_address(&keys.public));
            account.state = burst_types::WalletState::Verified;
            batch.put_account_info(&account, false).unwrap();
        }
        batch.put_brn_meta(BootstrapPhase::KEY, &if complete {
            BootstrapPhase::Complete
        } else { BootstrapPhase::Active }.encoding()).unwrap();
        let mut veto = block(BlockType::GovernanceVeto);
        veto.account = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[signer; 32]).public);
        let id = TxHash::new([88; 32]);
        veto.link = BlockHash::new(*id.as_bytes());
        let proposal = burst_governance::Proposal {
            hash: id, proposer: veto.account.clone(), phase: GovernancePhase::Proposal,
            content: ProposalContent::ParameterChange { param: GovernableParam::BrnRate, new_value: 2 },
            created_at: Timestamp::new(1), endorsement_count: 0, total_eligible_voters: 1,
            exploration_votes_yea: 0, exploration_votes_nay: 0, exploration_votes_abstain: 0,
            promotion_votes_yea: 0, promotion_votes_nay: 0, promotion_votes_abstain: 0,
            exploration_started_at: None, cooldown_started_at: Some(Timestamp::new(2)),
            promotion_started_at: None, activation_at: Some(Timestamp::new(veto.timestamp.as_secs()+1)), round: 0,
        };
        state.governance.submit_proposal(proposal, u128::MAX, true, &state.params).unwrap();
        state.governance.get_proposal_mut(&id).unwrap().phase = GovernancePhase::Cooldown;
        let result = state.apply(&mut batch, &veto, &EconomicResult::NoEconomicEffect,
            &mut BrnEngine::new(), &mut TrstEngine::new());
        assert_eq!(result.is_ok(), accepted, "count={count}, complete={complete}, signer={signer}: {result:?}");
        assert_eq!(state.governance.get_proposal(&id).unwrap().phase,
            if accepted { GovernancePhase::Vetoed } else { GovernancePhase::Cooldown });
    }
}
