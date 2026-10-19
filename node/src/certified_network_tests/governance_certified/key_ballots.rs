use super::*;
use burst_governance::{
    key_delegation::{
        operation::DelegationOperation, scoped, DelegatedBallot, Grant, KeyScope, Revocation,
    },
    GovernableParam,
};
use burst_transactions::governance::GovernanceVote;

mod partitioned;

async fn operation(
    clients: &[NodeClient],
    key: &KeyPair,
    params: &ProtocolParams,
    value: &DelegationOperation,
    at: u64,
) -> StateBlock {
    let state = clients[0]
        .signing_state(&burst_crypto::derive_address(&key.public))
        .await
        .unwrap();
    burst_wallet_core::delegation::build_key_operation(
        &state,
        key,
        value,
        params.params_hash(),
        Timestamp::new(at),
    )
    .unwrap()
}

pub(super) async fn check(clients: &[NodeClient], outcomes: &[(BlockHash, &'static str)]) {
    for client in clients {
        use burst_governance::key_delegation::view::{Direction, Query, ScopeChoice};
        let person = |n| {
            burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[n; 32]).public)
                .to_string()
        };
        let query = |n, direction| Query {
            account: person(n),
            direction,
            scope: None,
            cursor: None,
        };
        let incoming = client
            .delegation_keys(&query(0, Direction::Incoming))
            .await
            .unwrap();
        assert!(incoming.enabled);
        assert!(
            incoming.entries.is_empty(),
            "revoked grants must leave the recipient index"
        );
        let incoming = client
            .delegation_keys(&query(2, Direction::Incoming))
            .await
            .unwrap();
        assert_eq!(incoming.entries.len(), 1);
        assert_eq!(incoming.entries[0].owner, person(1));
        assert!(incoming.entries[0].confirmed);
        let outgoing = client
            .delegation_keys(&query(2, Direction::Outgoing))
            .await
            .unwrap();
        assert_eq!(outgoing.entries.len(), 3);
        assert!(outgoing
            .entries
            .iter()
            .all(|entry| entry.confirmed && entry.grant.is_none()));
        let mut exact = query(2, Direction::Outgoing);
        exact.scope = Some(ScopeChoice::Parameter("brn_rate".into()));
        let exact = client.delegation_keys(&exact).await.unwrap();
        assert_eq!(exact.entries.len(), 1);
        assert_eq!(exact.entries[0].generation, "2");
        for (index, (proposal, phase)) in outcomes.iter().enumerate() {
            let view = client
                .rpc_call(
                    "governance_proposal_info",
                    json!({"hash": proposal.to_string()}),
                )
                .await
                .unwrap();
            assert_eq!(view["phase"], *phase, "{view}");
            assert_eq!(view["votes_yea"], if index == 0 { 2 } else { 1 }, "{view}");
            assert_eq!(view["votes_nay"], if index == 3 { 2 } else { 1 }, "{view}");
        }
    }
}

pub(super) fn check_records(
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    initial: &ProtocolParams,
    outcomes: &[(BlockHash, &'static str)],
) {
    let genesis = crate::genesis_state::block(NetworkId::Dev, initial).hash;
    let mut snapshots = Vec::new();
    for store in stores {
        let mut snapshot = Vec::new();
        for (person, scope, generation, active) in [
            (1, KeyScope::Parameter(GovernableParam::BrnRate), 1, true),
            (2, KeyScope::Parameter(GovernableParam::BrnRate), 2, false),
            (
                2,
                KeyScope::Proposal(outcomes[1].0.into_tx_hash()),
                1,
                false,
            ),
            (
                2,
                KeyScope::Proposal(outcomes[2].0.into_tx_hash()),
                1,
                false,
            ),
        ] {
            let owner = burst_crypto::derive_address(
                &burst_crypto::keypair_from_seed(&[person; 32]).public,
            );
            let key = scoped::key(NetworkId::Dev, genesis, &owner, &scope).unwrap();
            let bytes = store.meta_store().get_meta(&key).unwrap();
            let authority =
                scoped::restore(NetworkId::Dev, genesis, &owner, &scope, &bytes).unwrap();
            assert_eq!(authority.generation(&owner, &scope), generation);
            assert_eq!(scoped::grant(&authority, &owner, &scope).is_some(), active);
            snapshot.extend(bytes);
        }
        snapshots.push(snapshot);
    }
    assert!(snapshots.windows(2).all(|pair| pair[0] == pair[1]));
}

pub(super) async fn activate(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    clock: &AtomicU64,
    keys: &[KeyPair],
    rules: (&ProtocolParams, &mut ProtocolParams),
    base: u64,
    (network, partitioned): (forks::Network<'_>, bool),
) -> (StateBlock, BlockHash, Vec<(BlockHash, &'static str)>) {
    let registries = partitioned.then_some(network.registries);
    let (initial, governed) = rules;
    let genesis = crate::genesis_state::block(NetworkId::Dev, initial).hash;
    let mut proposals = Vec::new();
    for index in 0..4 {
        proposals.push(
            supported_rate_proposal(
                clients,
                &keys[0],
                &keys[0],
                governed,
                base + 64,
                (3 + index) * BRN_UNIT,
            )
            .await
            .hash,
        );
    }
    let mut downstream = None;
    // A -> B -> C, using encrypted parameter grants. Only C receives B's key.
    for (owner, recipient) in [(1, 2), (2, 0)] {
        let grant = Grant::create(
            &keys[owner],
            keys[recipient].public.clone(),
            KeyScope::Parameter(GovernableParam::BrnRate),
            1,
            NetworkId::Dev,
            genesis,
        )
        .unwrap();
        let source = operation(
            clients,
            &keys[owner],
            governed,
            &DelegationOperation::Grant(grant.clone()),
            base + 64,
        )
        .await;
        submit(clients, &source).await;
        if owner == 2 {
            downstream = Some((grant, source.hash));
        }
    }
    let (grant, grant_block) = downstream.unwrap();
    for client in clients {
        use burst_governance::key_delegation::view::{Direction, Query};
        let found = client
            .delegation_keys(&Query {
                account: burst_crypto::derive_address(&keys[0].public).to_string(),
                direction: Direction::Incoming,
                scope: None,
                cursor: None,
            })
            .await
            .unwrap();
        assert_eq!(found.entries.len(), 1);
        assert_eq!(
            found.entries[0].grant_block.as_deref(),
            Some(grant_block.to_string().as_str())
        );
        found.entries[0]
            .grant
            .as_ref()
            .unwrap()
            .decrypt(&keys[0].private)
            .unwrap();
    }
    let secondary = grant.decrypt(&keys[0].private).unwrap();
    clock.store(base + 74, Ordering::SeqCst);
    let before = Revocation::create(
        &keys[2],
        KeyScope::Proposal(proposals[1].into_tx_hash()),
        1,
        NetworkId::Dev,
        genesis,
    )
    .unwrap();
    submit(
        clients,
        &operation(
            clients,
            &keys[2],
            governed,
            &DelegationOperation::Revoke(before),
            base + 74,
        )
        .await,
    )
    .await;
    clock.store(base + 75, Ordering::SeqCst);
    for (index, proposal) in proposals.iter().enumerate() {
        let ballot = DelegatedBallot::sign(
            &grant,
            &secondary,
            proposal.into_tx_hash(),
            GovernanceVote::Yea,
        )
        .unwrap();
        let value = DelegationOperation::Ballot {
            ballot,
            grant_block,
        };
        let signed = operation(clients, &keys[0], governed, &value, base + 75).await;
        if index == 0 {
            let wrong_relayer = operation(clients, &keys[1], governed, &value, base + 75).await;
            assert!(
                !clients[0]
                    .process(&serde_json::to_string(&wrong_relayer).unwrap())
                    .await
                    .unwrap()
                    .accepted
            );
            let DelegationOperation::Ballot {
                ballot,
                grant_block,
            } = &value
            else {
                unreachable!()
            };
            let mut forged = ballot.clone();
            forged.signature.0[0] ^= 1;
            let bad_signature = operation(
                clients,
                &keys[0],
                governed,
                &DelegationOperation::Ballot {
                    ballot: forged,
                    grant_block: *grant_block,
                },
                base + 75,
            )
            .await;
            assert!(
                !clients[0]
                    .process(&serde_json::to_string(&bad_signature).unwrap())
                    .await
                    .unwrap()
                    .accepted
            );
            let wrong_dependency = operation(
                clients,
                &keys[0],
                governed,
                &DelegationOperation::Ballot {
                    ballot: ballot.clone(),
                    grant_block: *proposal,
                },
                base + 75,
            )
            .await;
            assert!(
                !clients[0]
                    .process(&serde_json::to_string(&wrong_dependency).unwrap())
                    .await
                    .unwrap()
                    .accepted
            );
        }
        if let Some(registries) = registries.filter(|_| index == 0) {
            partitioned::ballot(clients, stores, registries, &signed, None).await;
        } else if let Some(registries) = registries.filter(|_| index == 2) {
            let revoke = Revocation::create(
                &keys[2],
                KeyScope::Proposal(proposal.into_tx_hash()),
                1,
                NetworkId::Dev,
                genesis,
            )
            .unwrap();
            let revoke = operation(
                clients,
                &keys[2],
                governed,
                &DelegationOperation::Revoke(revoke),
                base + 75,
            )
            .await;
            partitioned::ballot(clients, stores, registries, &signed, Some(&revoke)).await;
        } else if index == 0 {
            let losing = DelegationOperation::Ballot {
                ballot: DelegatedBallot::sign(
                    &grant,
                    &secondary,
                    proposal.into_tx_hash(),
                    GovernanceVote::Nay,
                )
                .unwrap(),
                grant_block,
            };
            let losing = operation(clients, &keys[0], governed, &losing, base + 75).await;
            forks::replace(clients, stores, &network, keys, (&losing, &signed)).await;
        } else {
            submit(clients, &signed).await;
        }
        if index == 0 {
            let duplicate = operation(clients, &keys[0], governed, &value, base + 75).await;
            assert!(
                !clients[0]
                    .process(&serde_json::to_string(&duplicate).unwrap())
                    .await
                    .unwrap()
                    .accepted
            );
        }
        for (person, vote) in [(0, GovernanceVote::Yea), (2, GovernanceVote::Nay)] {
            let address = burst_crypto::derive_address(&keys[person].public);
            let tx = Transaction::GovernanceVote(
                build_governance_vote_tx(
                    &address,
                    proposal.into_tx_hash(),
                    vote,
                    Timestamp::new(base + 75),
                )
                .unwrap(),
            );
            submit(
                clients,
                &signed_block(&clients[0], &keys[person], tx, governed).await,
            )
            .await;
        }
    }
    clock.store(base + 76, Ordering::SeqCst);
    if registries.is_none() {
        let after = Revocation::create(
            &keys[2],
            KeyScope::Proposal(proposals[2].into_tx_hash()),
            1,
            NetworkId::Dev,
            genesis,
        )
        .unwrap();
        submit(
            clients,
            &operation(
                clients,
                &keys[2],
                governed,
                &DelegationOperation::Revoke(after),
                base + 76,
            )
            .await,
        )
        .await;
    }
    let tx = Transaction::GovernanceVote(
        build_governance_vote_tx(
            &burst_crypto::derive_address(&keys[1].public),
            proposals[3].into_tx_hash(),
            GovernanceVote::Nay,
            Timestamp::new(base + 76),
        )
        .unwrap(),
    );
    submit(
        clients,
        &signed_block(&clients[0], &keys[1], tx, governed).await,
    )
    .await;
    let revoke = Revocation::create(
        &keys[2],
        KeyScope::Parameter(GovernableParam::BrnRate),
        2,
        NetworkId::Dev,
        genesis,
    )
    .unwrap();
    let revoke = DelegationOperation::Revoke(revoke);
    let late = if let Some(registries) = registries {
        let late = operation(clients, &keys[2], governed, &revoke, base + 97).await;
        partitioned::count(clients, stores, registries, clock, base + 97, &late).await;
        Some(late)
    } else {
        clock.store(base + 97, Ordering::SeqCst);
        None
    };
    installed(stores, 4, initial).await;
    if let Some(late) = late {
        for store in stores {
            assert!(
                !store.block_store().exists(&late.hash).unwrap(),
                "old-epoch boundary revocation survived the certified count"
            );
        }
    }
    submit(
        clients,
        &operation(clients, &keys[2], governed, &revoke, base + 97).await,
    )
    .await;
    clock.store(base + 108, Ordering::SeqCst);
    let epoch = installed(stores, 5, initial).await;
    let outcomes: Vec<_> = proposals
        .into_iter()
        .enumerate()
        .map(|(index, hash)| (hash, if index == 0 { "Activated" } else { "Rejected" }))
        .collect();
    check(clients, &outcomes).await;
    check_records(stores, initial, &outcomes);
    governed.brn_rate = 3 * BRN_UNIT;
    let tx = Transaction::Burn(
        build_burn_tx(
            &burst_crypto::derive_address(&keys[1].public),
            &burst_crypto::derive_address(&keys[0].public),
            1,
            Timestamp::new(base + 108),
        )
        .unwrap(),
    );
    let payment = signed_block(&clients[0], &keys[1], tx, governed).await;
    assert_eq!(payment.payment_epoch, epoch);
    submit(clients, &payment).await;
    (payment, epoch, outcomes)
}
