use super::*;
use burst_governance::signing_delegation::{
    operation::DelegationOperation,
    view::{Direction, Query},
    Revocation,
};

pub(super) fn fixture() -> Fixture {
    crate::confirmed_replay::tests::governance::fixture_with_key_ballots(2).0
}

pub(super) async fn check_recovery(client: &NodeClient, fixture: &Fixture, at: u64) {
    let Some(source) = fixture.blocks.iter().find(|block| {
        block.block_type == BlockType::Delegate && !block.delegation_payload.is_empty()
    }) else {
        return;
    };
    let DelegationOperation::Grant(grant) =
        DelegationOperation::decode(&source.delegation_payload).unwrap()
    else {
        panic!("expected encrypted grant");
    };
    let query = Query {
        account: address(0).to_string(),
        direction: Direction::Incoming,
        scope: None,
        cursor: None,
    };
    let incoming = client.delegation_keys(&query).await.unwrap();
    assert!(incoming.enabled);
    assert_eq!(incoming.entries.len(), 1);
    assert!(incoming.entries[0].confirmed);
    assert_eq!(
        incoming.entries[0].grant_block,
        Some(source.hash.to_string())
    );
    incoming.entries[0]
        .grant
        .as_ref()
        .unwrap()
        .decrypt(&burst_crypto::keypair_from_seed(&[0; 32]).private)
        .unwrap();
    let owner = burst_crypto::keypair_from_seed(&[1; 32]);
    let revoke = Revocation::create(
        &owner,
        grant.scope,
        2,
        NetworkId::Dev,
        fixture.blocks[0].hash,
    )
    .unwrap();
    let state = client.signing_state(&address(1)).await.unwrap();
    let signed = burst_wallet_core::delegation::build_key_operation(
        &state,
        &owner,
        &DelegationOperation::Revoke(revoke),
        fixture.params.params_hash(),
        Timestamp::new(at),
    )
    .unwrap();
    let result = client
        .process(&serde_json::to_string(&signed).unwrap())
        .await
        .unwrap();
    assert!(result.accepted, "recovered owner cannot revoke: {result:?}");
    assert!(client
        .delegation_keys(&query)
        .await
        .unwrap()
        .entries
        .is_empty());
}
