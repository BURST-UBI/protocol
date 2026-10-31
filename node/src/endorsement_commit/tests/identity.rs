use super::*;

#[test]
fn creator_verified_wallet_open_confirms_its_endorsement_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let params = ProtocolParams::default();
    let genesis = crate::genesis_state::block(NetworkId::Dev, &params);
    let info = crate::genesis_state::account_to_write(None, genesis.account.clone(), genesis.hash)
        .unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch
        .put_block_with_account(
            &genesis.hash,
            &bincode::serialize(&genesis).unwrap(),
            &genesis.account,
            1,
        )
        .unwrap();
    batch.put_account_info(&info, false).unwrap();
    batch.put_frontier(&genesis.account, &genesis.hash).unwrap();
    batch
        .advance_confirmation_height(&genesis.account, 1)
        .unwrap();
    crate::confirmed_authority::ConfirmedAuthority::initialize(&mut batch, &info).unwrap();
    crate::bootstrap_phase::initialize(&mut batch, &params).unwrap();
    batch.commit().unwrap();
    let mut endorse = endorsement();
    endorse.previous = genesis.hash;
    endorse.params_hash = params.params_hash();
    endorse.hash = endorse.compute_hash();
    let target = crate::block_economics::wallet_from_link(&endorse.link).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    crate::ledger_updater::stage_block(&mut batch, &endorse, Some(&info), 0, None, None).unwrap();
    apply(
        &mut batch,
        &endorse,
        10,
        NetworkId::Dev,
        &params,
        &mut VerificationOrchestrator::new(),
        (&mut BrnEngine::new(), &mut TrstEngine::new()),
    )
    .unwrap();
    batch.commit().unwrap();
    let mut open = endorse.clone();
    open.block_type = BlockType::Open;
    open.account = target.clone();
    open.representative = target.clone();
    open.previous = BlockHash::ZERO;
    open.link = BlockHash::ZERO;
    open.brn_spent = 0;
    open.hash = open.compute_hash();
    let mut batch = store.tx_begin_write().unwrap();
    let before = batch.get_account(&target).unwrap();
    crate::ledger_updater::stage_block(&mut batch, &open, Some(&before), 0, None, None).unwrap();
    batch.commit().unwrap();
    assert_eq!(
        crate::rollback_plan::preflight(&store, endorse.hash)
            .unwrap()
            .blocks(),
        &[open.hash, endorse.hash]
    );
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    assert!(!crate::causal_cementation::handoff(&mut batch, open.hash).unwrap());
    assert!(!batch.confirmation_pending(&open.hash).unwrap());
    batch.commit().unwrap();
    store.meta_store().enqueue_confirmation(&open.hash).unwrap();
    let waiting = crate::causal_cementation::cement(&store, open.hash).unwrap();
    assert!(!waiting.complete);
    assert!(waiting.hashes.is_empty());
    assert_eq!(
        store
            .account_store()
            .get_account(&target)
            .unwrap()
            .confirmation_height,
        0
    );
    assert_eq!(
        store
            .account_store()
            .get_account(&genesis.account)
            .unwrap()
            .confirmation_height,
        1
    );
    assert_eq!(
        store.meta_store().pending_confirmations().unwrap(),
        vec![open.hash]
    );
    store
        .meta_store()
        .enqueue_confirmation(&endorse.hash)
        .unwrap();
    drop(store);
    let store = LmdbStore::open(dir.path(), 32, 16 << 20).unwrap();
    let confirmed = crate::causal_cementation::cement(&store, open.hash).unwrap();
    assert!(confirmed.complete);
    assert_eq!(confirmed.hashes, vec![endorse.hash, open.hash]);
    let authority = crate::confirmed_authority::ConfirmedAuthority::read(&store).unwrap();
    assert!(authority.accounts[&target.to_string()].state.is_verified());
}
