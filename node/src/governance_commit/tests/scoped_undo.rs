//! Transactional governance rollback, not election or runtime-publication evidence.
use super::*;
use burst_wallet_core::transaction_builder::*;

#[test]
fn signed_proposal_undo_preserves_an_independent_proposal_across_abort_and_reopen() {
    scoped_undo_case(BlockType::GovernanceProposal);
}

#[test]
fn signed_delegation_undo_preserves_an_independent_owner_across_abort_and_reopen() {
    scoped_undo_case(BlockType::Delegate);
}

#[test]
fn signed_revocation_undo_preserves_an_independent_owner_across_abort_and_reopen() {
    scoped_undo_case(BlockType::RevokeDelegation);
}

fn scoped_undo_case(kind: BlockType) {
    let dir = tempfile::tempdir().unwrap();
    let mut store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let mut state = candidate();
    state.constitution = ConstiEngine::bootstrap();
    let target = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[2; 32]).public);
    if kind == BlockType::RevokeDelegation {
        for seed in [0, 1] {
            state
                .delegation
                .delegate(
                    &burst_crypto::derive_address(
                        &burst_crypto::keypair_from_seed(&[seed; 32]).public,
                    ),
                    &target,
                )
                .unwrap();
        }
    }
    let baseline_delegation: burst_governance::DelegationSnapshot =
        bincode::deserialize(&state.delegation.save_state()).unwrap();
    state.params.min_work_difficulty = 0;
    let params = state.params.clone();
    let mut brn = BrnEngine::with_rate(1, Timestamp::EPOCH);
    let mut trst = TrstEngine::new();
    let mut openings = Vec::new();
    {
        let mut batch = store.tx_begin_write().unwrap();
        crate::payment_epoch::initialize(&mut batch, BlockHash::new([1; 32])).unwrap();
        crate::bootstrap_phase::initialize(&mut batch, &params).unwrap();
        for seed in [0, 1] {
            let key = burst_crypto::keypair_from_seed(&[seed; 32]);
            let mut open = block(BlockType::Open);
            open.account = burst_crypto::derive_address(&key.public);
            open.representative = open.account.clone();
            open.link = BlockHash::ZERO;
            open.transaction = TxHash::ZERO;
            open.timestamp = Timestamp::EPOCH;
            open.params_hash = params.params_hash();
            open.hash = open.compute_hash();
            open = sign_state_block(open, &key.private);
            let mut account =
                crate::genesis_state::account_to_write(None, open.account.clone(), open.hash)
                    .unwrap();
            account.confirmation_height = 1;
            batch.put_account_info(&account, false).unwrap();
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
            openings.push(open);
        }
        state.persist(&mut batch).unwrap();
        crate::ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
        batch.commit().unwrap();
    }
    let mut proposals = Vec::new();
    for (seed, open) in openings.iter().enumerate() {
        let account = AccountState {
            signing_grant: burst_types::BlockHash::ZERO,
            payment_epoch: open.payment_epoch,
            release_sources: Vec::new(),
            head: open.hash,
            block_count: 1,
            representative: open.account.clone(),
            brn_balance: 0,
            trst_balance: 0,
        };
        let tx = if kind == BlockType::Delegate {
            burst_transactions::Transaction::Delegate(
                build_delegate_tx(&open.account, &target, Timestamp::new(100)).unwrap(),
            )
        } else if kind == BlockType::RevokeDelegation {
            burst_transactions::Transaction::RevokeDelegation(
                build_revoke_delegation_tx(&open.account, Timestamp::new(100)).unwrap(),
            )
        } else {
            burst_transactions::Transaction::GovernanceProposal(
                build_governance_proposal_tx(
                    &open.account,
                    burst_transactions::governance::ProposalContent::ParameterChange {
                        parameter: "brn_rate".into(),
                        current_value: params.brn_rate,
                        proposed_value: 2,
                    },
                    Timestamp::new(100),
                )
                .unwrap(),
            )
        };
        let proposal = sign_state_block(
            build_state_block(&account, &tx, TxHash::ZERO, params.params_hash()).unwrap(),
            &burst_crypto::keypair_from_seed(&[seed as u8; 32]).private,
        );
        crate::payment_transition::authenticate_signed_contents(&proposal, &params).unwrap();
        let mut batch = store.tx_begin_write().unwrap();
        state = CandidateGovernance::restore(&batch, params.clone(), state.network, state.genesis)
            .unwrap();
        let economic = crate::economic_candidate::stage(
            &mut batch,
            &proposal,
            &params,
            &mut (&mut brn, &mut trst),
        )
        .unwrap();
        state
            .apply(&mut batch, &proposal, &economic.effect, &mut brn, &mut trst)
            .unwrap();
        crate::ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
        batch.commit().unwrap();
        proposals.push(proposal);
    }
    for (index, losing) in proposals.iter().enumerate() {
        drop(store);
        store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
        let mut replacement = losing.clone();
        replacement.block_type = BlockType::ChangeRepresentative;
        replacement.link = BlockHash::ZERO;
        replacement.origin = TxHash::ZERO;
        replacement.transaction = TxHash::ZERO;
        replacement.hash = replacement.compute_hash();
        replacement = sign_state_block(
            replacement,
            &burst_crypto::keypair_from_seed(&[index as u8; 32]).private,
        );
        let original = store.brn_store().get_meta(b"governance_engine").unwrap();
        let original_delegation = store
            .meta_store()
            .get_meta(DelegationEngine::meta_key())
            .unwrap();
        let original_constitution = store.meta_store().get_meta("consti_engine").unwrap();
        assert!(crate::rollback_plan::replace_with(
            &store,
            losing.hash,
            &replacement,
            |batch, block| {
                crate::payment_transition::apply(batch, block, &params, block.timestamp)?;
                Err::<(), _>(burst_store::StoreError::Backend(
                    "injected after replacement staging".into(),
                ))
            }
        )
        .is_err());
        assert_eq!(
            store.brn_store().get_meta(b"governance_engine").unwrap(),
            original
        );
        assert_eq!(
            store
                .meta_store()
                .get_meta(DelegationEngine::meta_key())
                .unwrap(),
            original_delegation
        );
        assert_eq!(
            store.meta_store().get_meta("consti_engine").unwrap(),
            original_constitution
        );
        assert!(store.block_store().exists(&losing.hash).unwrap());
        assert!(!store.block_store().exists(&replacement.hash).unwrap());
        crate::rollback_plan::replace_with(&store, losing.hash, &replacement, |batch, block| {
            crate::payment_transition::apply(batch, block, &params, block.timestamp)
        })
        .unwrap();
        let batch = store.tx_begin_write().unwrap();
        let restored =
            CandidateGovernance::restore(&batch, params.clone(), state.network, state.genesis)
                .unwrap();
        assert!(restored
            .governance
            .get_proposal(&proposals[0].hash.into_tx_hash())
            .is_none());
        assert_eq!(
            restored
                .governance
                .get_proposal(&proposals[1].hash.into_tx_hash())
                .is_some(),
            kind == BlockType::GovernanceProposal && index == 0
        );
        assert_eq!(restored.constitution, ConstiEngine::bootstrap());
        if kind != BlockType::GovernanceProposal {
            for (owner, open) in openings.iter().enumerate() {
                let reverted = owner <= index;
                let delegated = if kind == BlockType::Delegate {
                    !reverted
                } else {
                    reverted
                };
                assert_eq!(
                    restored.delegation.resolve(&open.account),
                    Some(if delegated {
                        target.clone()
                    } else {
                        open.account.clone()
                    })
                );
            }
        }
        drop(batch);
    }
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 32 << 20).unwrap();
    let batch = store.tx_begin_write().unwrap();
    let restored =
        CandidateGovernance::restore(&batch, params.clone(), state.network, state.genesis).unwrap();
    let delegation: burst_governance::DelegationSnapshot =
        bincode::deserialize(&restored.delegation.save_state()).unwrap();
    assert_eq!(delegation, baseline_delegation);
    assert_eq!(
        CandidateGovernance::restore(&batch, params, state.network, state.genesis)
            .unwrap()
            .governance
            .all_proposals()
            .count(),
        0
    );
}
