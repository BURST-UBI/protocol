//! Signed writer transitions and dependency closure, not network election evidence.
use super::*;
use burst_governance::key_delegation::{
    operation::DelegationOperation, scoped, DelegatedBallot, Grant, KeyScope, Revocation,
};
use burst_wallet_core::transaction_builder::*;

fn account(block: &StateBlock, height: u64) -> AccountState {
    AccountState {
        signing_grant: burst_types::BlockHash::ZERO,
        head: block.hash,
        block_count: height,
        representative: block.representative.clone(),
        payment_epoch: block.payment_epoch,
        brn_balance: block.brn_balance,
        trst_balance: block.trst_balance,
        release_sources: Vec::new(),
    }
}

#[test]
fn grant_fork_removes_dependent_ballot_in_both_key_scopes_after_reopen() {
    for (proposal_scope, revoked) in [(false, false), (false, true), (true, false), (true, true)] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut state = candidate();
        state.constitution = ConstiEngine::bootstrap();
        state.params.min_work_difficulty = 0;
        state.params.governance_proposal_endorsements = 0;
        state.params.governance_proposal_duration_secs = 1;
        state.params.governance_proposal_window_secs = 1;
        let params = state.params.clone();
        let mut brn = BrnEngine::with_rate(1, Timestamp::EPOCH);
        let mut trst = TrstEngine::new();
        let keys: Vec<_> = [0, 1]
            .map(|seed| burst_crypto::keypair_from_seed(&[seed; 32]))
            .into();
        let mut opens = Vec::new();
        {
            let mut batch = store.tx_begin_write().unwrap();
            crate::payment_epoch::initialize(&mut batch, BlockHash::new([1; 32])).unwrap();
            crate::bootstrap_phase::initialize(&mut batch, &params).unwrap();
            for key in &keys {
                let mut open = block(BlockType::Open);
                open.account = burst_crypto::derive_address(&key.public);
                open.representative = open.account.clone();
                open.link = BlockHash::ZERO;
                open.transaction = TxHash::ZERO;
                open.timestamp = Timestamp::EPOCH;
                open.params_hash = params.params_hash();
                open.hash = open.compute_hash();
                open = sign_state_block(open, &key.private);
                let mut info =
                    crate::genesis_state::account_to_write(None, open.account.clone(), open.hash)
                        .unwrap();
                info.confirmation_height = 1;
                batch.put_account_info(&info, false).unwrap();
                batch
                    .put_block_with_account(
                        &open.hash,
                        &bincode::serialize(&open).unwrap(),
                        &open.account,
                        1,
                    )
                    .unwrap();
                batch.put_frontier(&open.account, &open.hash).unwrap();
                brn.track_wallet(
                    open.account.clone(),
                    burst_brn::BrnWalletState::new(Timestamp::EPOCH),
                );
                opens.push(open);
            }
            state.persist(&mut batch).unwrap();
            crate::ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
            batch.commit().unwrap();
        }
        let proposal_tx = burst_transactions::Transaction::GovernanceProposal(
            build_governance_proposal_tx(
                &opens[0].account,
                burst_transactions::governance::ProposalContent::ParameterChange {
                    parameter: "brn_rate".into(),
                    current_value: params.brn_rate,
                    proposed_value: 2,
                },
                Timestamp::new(100),
            )
            .unwrap(),
        );
        let proposal = sign_state_block(
            build_state_block(
                &account(&opens[0], 1),
                &proposal_tx,
                TxHash::ZERO,
                params.params_hash(),
            )
            .unwrap(),
            &keys[0].private,
        );
        let scope = if proposal_scope {
            KeyScope::Proposal(proposal.hash.into_tx_hash())
        } else {
            KeyScope::Parameter(GovernableParam::BrnRate)
        };
        let grant = Grant::create(
            &keys[0],
            keys[1].public.clone(),
            scope.clone(),
            1,
            state.network,
            state.genesis,
        )
        .unwrap();
        let grant_block = burst_wallet_core::delegation::build_key_operation(
            &account(&proposal, 2),
            &keys[0],
            &DelegationOperation::Grant(grant.clone()),
            params.params_hash(),
            Timestamp::new(102),
        )
        .unwrap();
        let secondary = grant.decrypt(&keys[1].private).unwrap();
        let ballot = DelegatedBallot::sign(
            &grant,
            &secondary,
            proposal.hash.into_tx_hash(),
            burst_transactions::governance::GovernanceVote::Yea,
        )
        .unwrap();
        let ballot_block = burst_wallet_core::delegation::build_key_operation(
            &account(&opens[1], 1),
            &keys[1],
            &DelegationOperation::Ballot {
                ballot,
                grant_block: grant_block.hash,
            },
            params.params_hash(),
            Timestamp::new(104),
        )
        .unwrap();
        let revocation = burst_wallet_core::delegation::build_key_operation(
            &account(&grant_block, 3),
            &keys[0],
            &DelegationOperation::Revoke(
                Revocation::create(&keys[0], scope.clone(), 2, state.network, state.genesis)
                    .unwrap(),
            ),
            params.params_hash(),
            Timestamp::new(103),
        )
        .unwrap();
        let mut history = vec![&proposal, &grant_block];
        if revoked {
            history.push(&revocation);
        }
        history.push(&ballot_block);
        for block in history {
            crate::payment_transition::authenticate_signed_contents(block, &params).unwrap();
            let mut batch = store.tx_begin_write().unwrap();
            state =
                CandidateGovernance::restore(&batch, params.clone(), state.network, state.genesis)
                    .unwrap();
            let effect = crate::economic_candidate::stage(
                &mut batch,
                block,
                &params,
                &mut (&mut brn, &mut trst),
            )
            .unwrap();
            state
                .apply(&mut batch, block, &effect.effect, &mut brn, &mut trst)
                .unwrap();
            crate::ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
            batch.commit().unwrap();
        }
        assert!(state
            .governance
            .key_ballot(
                proposal.hash.into_tx_hash(),
                &opens[0].account,
                grant.id().unwrap()
            )
            .is_some());
        let scope_key =
            scoped::key(state.network, state.genesis, &opens[0].account, &scope).unwrap();
        let metadata = [(
            scope_key.clone(),
            store.meta_store().get_meta(&scope_key).unwrap(),
        )]
        .into();
        let people: Vec<_> = opens.iter().map(|open| open.account.clone()).collect();
        let projection = state
            .delegation_at_cut(proposal.hash.into_tx_hash(), &people, &metadata)
            .unwrap();
        assert_eq!(
            projection.key_ballot(&opens[0].account),
            Some((!revoked).then_some(burst_transactions::governance::GovernanceVote::Yea))
        );
        drop(store);
        store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut replacement = grant_block.clone();
        replacement.block_type = BlockType::ChangeRepresentative;
        replacement.link = BlockHash::ZERO;
        replacement.delegation_payload.clear();
        replacement.hash = replacement.compute_hash();
        replacement = sign_state_block(replacement, &keys[0].private);
        let plan = crate::rollback_plan::preflight(&store, grant_block.hash).unwrap();
        assert_eq!(plan.blocks().len(), 2 + usize::from(revoked));
        assert!(plan.blocks().contains(&ballot_block.hash));
        let before = store.brn_store().get_meta(b"governance_engine").unwrap();
        assert!(crate::rollback_plan::replace_with(
            &store,
            grant_block.hash,
            &replacement,
            |batch, block| {
                crate::payment_transition::apply(batch, block, &params, block.timestamp)?;
                Err::<(), _>(burst_store::StoreError::Backend(
                    "injected after grant replacement".into(),
                ))
            }
        )
        .is_err());
        assert_eq!(
            store.brn_store().get_meta(b"governance_engine").unwrap(),
            before
        );
        assert!(store.block_store().exists(&ballot_block.hash).unwrap());
        crate::rollback_plan::replace_with(
            &store,
            grant_block.hash,
            &replacement,
            |batch, block| crate::payment_transition::apply(batch, block, &params, block.timestamp),
        )
        .unwrap();
        drop(store);
        let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        assert!(!store.block_store().exists(&ballot_block.hash).unwrap());
        assert!(!store.block_store().exists(&grant_block.hash).unwrap());
        assert!(!store.block_store().exists(&revocation.hash).unwrap());
        assert!(store.block_store().exists(&proposal.hash).unwrap());
        assert_eq!(
            store
                .account_store()
                .get_account(&opens[1].account)
                .unwrap()
                .head,
            opens[1].hash
        );
        let batch = store.tx_begin_write().unwrap();
        let restored =
            CandidateGovernance::restore(&batch, params, state.network, state.genesis).unwrap();
        assert!(restored
            .governance
            .key_ballot(
                proposal.hash.into_tx_hash(),
                &opens[0].account,
                grant.id().unwrap()
            )
            .is_none());
        assert_eq!(
            restored
                .governance
                .get_proposal(&proposal.hash.into_tx_hash())
                .unwrap()
                .phase,
            GovernancePhase::Proposal
        );
        let scope_key =
            scoped::key(state.network, state.genesis, &opens[0].account, &scope).unwrap();
        assert!(batch.get_meta(&scope_key).unwrap().is_none());
    }
}
