use super::*;
use burst_governance::key_delegation::{
    operation::DelegationOperation,
    view::{Direction, Query},
    DelegatedBallot, Grant, KeyScope, Revocation,
};

pub(super) fn fixture() -> Fixture {
    let (mut fixture, proposals) =
        crate::confirmed_replay::tests::governance::fixture_with_cooldown(10);
    let legacy = fixture
        .blocks
        .iter()
        .position(|block| {
            block.block_type == BlockType::Delegate
                && block.account == address(1)
                && block.delegation_payload.is_empty()
        })
        .unwrap();
    let removed = fixture.blocks.remove(legacy);
    assert!(!fixture
        .blocks
        .iter()
        .any(|block| block.previous == removed.hash));
    let owner = burst_crypto::keypair_from_seed(&[1; 32]);
    let recipient = burst_crypto::keypair_from_seed(&[0; 32]);
    let grant = Grant::create(
        &owner,
        recipient.public.clone(),
        KeyScope::Parameter(burst_governance::GovernableParam::BrnRate),
        1,
        NetworkId::Dev,
        fixture.blocks[0].hash,
    )
    .unwrap();
    let payload = DelegationOperation::Grant(grant.clone()).encode().unwrap();
    let grant_block = fixture.push(1, BlockType::Delegate, 123, |block| {
        block.link = destination(0);
        block.delegation_payload = payload;
    });
    let secondary = grant.decrypt(&recipient.private).unwrap();
    for proposal in proposals {
        let ballot = DelegatedBallot::sign(
            &grant,
            &secondary,
            proposal,
            burst_transactions::governance::GovernanceVote::Yea,
        )
        .unwrap();
        let payload = DelegationOperation::Ballot {
            ballot,
            grant_block,
        }
        .encode()
        .unwrap();
        fixture.push(0, BlockType::GovernanceVote, 124, |block| {
            block.link = BlockHash::new(*proposal.as_bytes());
            block.transaction = TxHash::ZERO;
            block.delegation_payload = payload;
        });
    }
    fixture
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
