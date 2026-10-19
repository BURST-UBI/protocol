use super::*;
use burst_governance::{
    key_delegation::{operation::DelegationOperation, scoped, Grant, KeyScope, Revocation},
    GovernableParam,
};

fn owner() -> WalletAddress {
    burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[1; 32]).public)
}
fn genesis(params: &ProtocolParams) -> BlockHash {
    crate::genesis_state::block(NetworkId::Dev, params).hash
}

async fn block(
    clients: &[NodeClient],
    keys: &[KeyPair],
    params: &ProtocolParams,
    operation: &DelegationOperation,
    at: u64,
) -> StateBlock {
    let state = clients[0].signing_state(&owner()).await.unwrap();
    burst_wallet_core::delegation::build_key_operation(
        &state,
        &keys[1],
        operation,
        params.params_hash(),
        Timestamp::new(at),
    )
    .unwrap()
}

async fn apply(
    clients: &[NodeClient],
    keys: &[KeyPair],
    params: &ProtocolParams,
    operation: &DelegationOperation,
    at: u64,
) {
    submit(clients, &block(clients, keys, params, operation, at).await).await;
}

pub(super) async fn register(
    clients: &[NodeClient],
    keys: &[KeyPair],
    params: &ProtocolParams,
    proposal: BlockHash,
    at: u64,
) {
    assert_eq!(
        clients[0].rpc_call("params_hash", json!({})).await.unwrap()["delegation_key_journal"],
        true
    );
    for scope in [
        KeyScope::Parameter(GovernableParam::BrnRate),
        KeyScope::Proposal(proposal.into_tx_hash()),
    ] {
        let grant = Grant::create(
            &keys[1],
            keys[2].public.clone(),
            scope,
            1,
            NetworkId::Dev,
            genesis(params),
        )
        .unwrap();
        assert!(grant.decrypt(&keys[0].private).is_err());
        let secondary = grant.decrypt(&keys[2].private).unwrap();
        let operation = DelegationOperation::Grant(grant);
        let signed = block(clients, keys, params, &operation, at).await;
        let mut forged = signed.clone();
        forged.signature = burst_crypto::sign_message(forged.hash.as_bytes(), &secondary);
        assert!(
            !clients[0]
                .process(&serde_json::to_string(&forged).unwrap())
                .await
                .unwrap()
                .accepted
        );
        submit(clients, &signed).await;
    }
    let foreign = Grant::create(
        &keys[1],
        keys[2].public.clone(),
        KeyScope::Parameter(GovernableParam::BrnRate),
        2,
        NetworkId::Test,
        genesis(params),
    )
    .unwrap();
    let bad = block(
        clients,
        keys,
        params,
        &DelegationOperation::Grant(foreign),
        at,
    )
    .await;
    let before = clients[0].signing_state(&owner()).await.unwrap().head;
    assert!(
        !clients[0]
            .process(&serde_json::to_string(&bad).unwrap())
            .await
            .unwrap()
            .accepted
    );
    assert_eq!(
        clients[0].signing_state(&owner()).await.unwrap().head,
        before
    );
}

pub(super) async fn rotate(
    clients: &[NodeClient],
    keys: &[KeyPair],
    params: &ProtocolParams,
    proposal: BlockHash,
    at: u64,
    proposal_already_revoked: bool,
) {
    let specific = KeyScope::Proposal(proposal.into_tx_hash());
    let parameter = KeyScope::Parameter(GovernableParam::BrnRate);
    for scope in [specific.clone(), parameter.clone()] {
        if proposal_already_revoked && scope == specific {
            continue;
        }
        let revoke =
            Revocation::create(&keys[1], scope, 2, NetworkId::Dev, genesis(params)).unwrap();
        apply(
            clients,
            keys,
            params,
            &DelegationOperation::Revoke(revoke),
            at,
        )
        .await;
    }
    let stale = Grant::create(
        &keys[1],
        keys[2].public.clone(),
        parameter.clone(),
        1,
        NetworkId::Dev,
        genesis(params),
    )
    .unwrap();
    let rejected = block(
        clients,
        keys,
        params,
        &DelegationOperation::Grant(stale),
        at,
    )
    .await;
    assert!(
        !clients[0]
            .process(&serde_json::to_string(&rejected).unwrap())
            .await
            .unwrap()
            .accepted
    );
    let replacement = Grant::create(
        &keys[1],
        keys[2].public.clone(),
        parameter,
        3,
        NetworkId::Dev,
        genesis(params),
    )
    .unwrap();
    apply(
        clients,
        keys,
        params,
        &DelegationOperation::Grant(replacement),
        at,
    )
    .await;
}

pub(super) async fn partitioned_revoke(
    clients: &[NodeClient],
    keys: &[KeyPair],
    params: &ProtocolParams,
    proposal: BlockHash,
    at: u64,
    registries: &[partition::Registry],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
) {
    let scope = KeyScope::Proposal(proposal.into_tx_hash());
    let revoke =
        Revocation::create(&keys[1], scope.clone(), 2, NetworkId::Dev, genesis(params)).unwrap();
    let signed = block(
        clients,
        keys,
        params,
        &DelegationOperation::Revoke(revoke),
        at,
    )
    .await;
    let before: Vec<_> = stores
        .iter()
        .map(|store| {
            (
                store
                    .account_store()
                    .get_account(&owner())
                    .unwrap()
                    .confirmation_height,
                store.meta_store().payment_epoch().unwrap(),
            )
        })
        .collect();
    let partition = partition::Partition::split(registries, 2).await;
    let result = clients[0]
        .process(&serde_json::to_string(&signed).unwrap())
        .await
        .unwrap();
    assert!(result.accepted, "revocation admission failed: {result:?}");
    let until = tokio::time::Instant::now() + Duration::from_secs(6);
    while tokio::time::Instant::now() < until {
        for (index, store) in stores.iter().enumerate() {
            let account = store.account_store().get_account(&owner()).unwrap();
            assert_eq!(
                account.confirmation_height, before[index].0,
                "node {index} cemented revocation without quorum"
            );
            assert_eq!(store.meta_store().payment_epoch().unwrap(), before[index].1);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    partition.heal().await;
    for client in clients {
        client
            .wait_confirmed(&signed.hash.to_string(), Duration::from_secs(25))
            .await
            .unwrap();
    }
    let key = scoped::key(NetworkId::Dev, genesis(params), &owner(), &scope).unwrap();
    for store in stores {
        let bytes = store.meta_store().get_meta(&key).unwrap();
        let authority =
            scoped::restore(NetworkId::Dev, genesis(params), &owner(), &scope, &bytes).unwrap();
        assert_eq!(authority.generation(&owner(), &scope), 2);
        assert!(scoped::grant(&authority, &owner(), &scope).is_none());
    }
}

pub(super) fn check(
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    params: &ProtocolParams,
    proposal: BlockHash,
) {
    let mut snapshots = Vec::new();
    for store in stores {
        let mut snapshot = Vec::new();
        for (scope, generation) in [
            (KeyScope::Proposal(proposal.into_tx_hash()), 2),
            (KeyScope::Parameter(GovernableParam::BrnRate), 3),
        ] {
            let key = scoped::key(NetworkId::Dev, genesis(params), &owner(), &scope).unwrap();
            let bytes = store.meta_store().get_meta(&key).unwrap();
            let authority =
                scoped::restore(NetworkId::Dev, genesis(params), &owner(), &scope, &bytes).unwrap();
            assert_eq!(authority.generation(&owner(), &scope), generation);
            if generation == 2 {
                assert!(scoped::grant(&authority, &owner(), &scope).is_none());
            } else {
                scoped::grant(&authority, &owner(), &scope)
                    .unwrap()
                    .decrypt(&burst_crypto::keypair_from_seed(&[2; 32]).private)
                    .unwrap();
            }
            snapshot.extend(bytes);
        }
        snapshots.push(snapshot);
    }
    assert!(snapshots.windows(2).all(|pair| pair[0] == pair[1]));
}
