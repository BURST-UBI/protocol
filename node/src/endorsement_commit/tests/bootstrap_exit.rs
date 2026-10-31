use super::*;
use burst_verification::bootstrap::BootstrapPhase;

#[test]
fn creator_endorsements_lose_special_authority_permanently_after_exit() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut params = ProtocolParams {
        bootstrap_exit_threshold: 2,
        endorsement_burn_amount: 10,
        ..ProtocolParams::default()
    };
    let genesis = crate::genesis_state::block(NetworkId::Dev, &params);
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .put_block_with_account(
            &genesis.hash,
            &bincode::serialize(&genesis).unwrap(),
            &genesis.account,
            1,
        )
        .unwrap();
    let info = crate::genesis_state::account_to_write(None, genesis.account.clone(), genesis.hash)
        .unwrap();
    batch.put_account_info(&info, false).unwrap();
    crate::bootstrap_phase::initialize(&mut batch, &params).unwrap();
    batch.commit().unwrap();

    let mut block = endorsement();
    block.previous = genesis.hash;
    block.params_hash = params.params_hash();
    block.hash = block.compute_hash();
    let target = crate::block_economics::wallet_from_link(&block.link).unwrap();
    for commit in [false, true] {
        let mut orchestrator = VerificationOrchestrator::new();
        let mut brn = BrnEngine::new();
        let mut trst = TrstEngine::new();
        let mut batch = store.tx_begin_write().unwrap();
        crate::ledger_updater::stage_block(&mut batch, &block, Some(&info), 0, None, None).unwrap();
        apply(
            &mut batch,
            &block,
            10,
            NetworkId::Dev,
            &params,
            &mut orchestrator,
            (&mut brn, &mut trst),
        )
        .unwrap();
        assert_eq!(
            crate::bootstrap_phase::read(&batch).unwrap(),
            BootstrapPhase::Complete
        );
        crate::ledger_updater::persist_economics(&mut batch, &brn, &trst).unwrap();
        if commit {
            batch.commit().unwrap();
        } else {
            drop(batch);
        }
        assert_eq!(store.account_store().get_account(&target).is_ok(), commit);
        assert_eq!(
            crate::bootstrap_phase::require(&store).unwrap(),
            if commit {
                BootstrapPhase::Complete
            } else {
                BootstrapPhase::Active
            }
        );
    }
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut orchestrator = VerificationOrchestrator::restore(
        burst_verification::OrchestratorSnapshot::decode(
            &store.meta_store().get_meta(SNAPSHOT_KEY).unwrap(),
        )
        .unwrap(),
    );
    let mut brn = BrnEngine::load_from_store(&store.brn_store()).unwrap();
    let mut trst = TrstEngine::new();
    let mut batch = store.tx_begin_write().unwrap();
    let mut removed = batch.get_account(&target).unwrap();
    removed.state = burst_types::WalletState::Revoked;
    batch.put_account_info(&removed, false).unwrap();
    assert_eq!(batch.verified_account_count().unwrap(), 1);
    batch.commit().unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    params.bootstrap_exit_threshold = 1000;
    let second_target = burst_crypto::keypair_from_seed(&[43; 32]);
    let second_address = burst_crypto::derive_address(&second_target.public);
    let mut second = block.clone();
    second.previous = block.hash;
    second.link = BlockHash::new(second_target.public.0);
    second.brn_spent += 10;
    second.timestamp = Timestamp::new(1235);
    second.params_hash = params.params_hash();
    second.hash = second.compute_hash();
    let previous = batch.get_account(&genesis.account).unwrap();
    crate::ledger_updater::stage_block(
        &mut batch,
        &second,
        Some(&previous),
        block.brn_spent,
        None,
        None,
    )
    .unwrap();
    apply(
        &mut batch,
        &second,
        10,
        NetworkId::Dev,
        &params,
        &mut orchestrator,
        (&mut brn, &mut trst),
    )
    .unwrap();
    assert!(batch.get_account(&second_address).is_err());
    assert_eq!(
        orchestrator.get_state(&second_address).unwrap().phase,
        burst_verification::state::VerificationPhase::Endorsing
    );
    batch.commit().unwrap();
    assert_eq!(
        crate::bootstrap_phase::require(&store).unwrap(),
        BootstrapPhase::Complete
    );
}
