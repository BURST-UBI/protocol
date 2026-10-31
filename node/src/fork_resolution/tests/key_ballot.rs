use super::*;
use burst_governance::signing_delegation::{
    operation::DelegationOperation, DelegatedBallot, Grant, KeyScope,
};
use burst_wallet_core::transaction_builder::*;

fn account(block: &StateBlock, height: u64) -> AccountState {
    AccountState {
        signing_grant: burst_types::BlockHash::ZERO,
        head: block.hash,
        block_count: height,
        representative: block.representative.clone(),
        payment_epoch: block.payment_epoch,
        brn_spent: block.brn_spent,
        trst_balance: block.trst_balance,
        release_sources: Vec::new(),
    }
}

async fn authorize(
    elections: &[Arc<RwLock<ActiveElections>>],
    registries: &[Arc<RwLock<crate::ConnectionRegistry>>],
    block: &StateBlock,
    voter: &burst_types::WalletAddress,
    sequence: u64,
) {
    if !registries.is_empty() {
        for elections in elections {
            tokio::time::timeout(Duration::from_secs(10), async {
                while elections.read().await.candidate_root(&block.hash).is_none() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("network candidate was not registered");
        }
        let mut vote = crate::WireVote {
            voter: voter.clone(),
            block_hashes: vec![block.hash],
            is_final: true,
            timestamp: Timestamp::now().as_secs() * 1000,
            sequence,
            signature: burst_types::Signature([0; 64]),
        };
        vote.signature = burst_crypto::sign_message(
            &burst_consensus::vote_signing_payload(
                &vote.voter,
                vote.is_final,
                vote.timestamp,
                vote.sequence,
                &vote.block_hashes,
            ),
            &burst_crypto::keypair_from_seed(&[0; 32]).private,
        );
        let bytes = crate::WireMessage::Vote(vote).encode().unwrap();
        for registry in registries {
            let registry = registry.read().await;
            for peer in registry.peer_ids() {
                assert!(registry.send(peer, bytes.clone()));
            }
        }
        return;
    }
    let root = crate::ledger_updater::election_root(block);
    let mut active = elections[0].write().await;
    active.remove_root(&root);
    active.set_online_weight(1);
    active
        .register_candidate(root, block.hash, Timestamp::now())
        .unwrap();
    assert!(active
        .process_vote(&root, voter, block.hash, 1, true, Timestamp::now())
        .unwrap()
        .is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn key_ballot_worker_removes_dependency_and_accepts_fresh_ballot_after_restart() {
    journey(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_network_key_ballot_fork_removes_dependency_and_recovers() {
    journey(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_network_ballot_fork_keeps_elected_choice_after_restart() {
    journey(true, true).await;
}

async fn journey(wire: bool, ballot_fork: bool) {
    for proposal_scope in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::NodeConfig {
            network: NetworkId::Dev,
            data_dir: dir.path().join("0"),
            port: 0,
            enable_rpc: false,
            enable_upnp: false,
            enable_representative: false,
            bootstrap_peers: Vec::new(),
            ..crate::NodeConfig::default()
        };
        config.params.min_work_difficulty = 0;
        config.params.endorsement_burn_amount = 1;
        config.params.governance_proposal_endorsements = 0;
        config.params.governance_proposal_duration_secs = 1;
        config.params.governance_proposal_window_secs = 1;
        let ports = [super::network::port(), super::network::port()];
        assert_ne!(ports[0], ports[1]);
        if wire {
            config.port = ports[0];
            config.bootstrap_peers = vec![format!("127.0.0.1:{}", ports[1])];
        }
        let mut node = crate::BurstNode::new(config.clone()).await.unwrap();
        node.initialize_genesis().unwrap();
        node.verification_fixture_clock = Some(Arc::new(|| None));
        let store = node.store.clone();
        let queue = node.block_queue();
        let mut elections = vec![node.active_elections.clone()];
        let governance = node.governance.clone();
        let shutdown = node.shutdown.clone();
        let mut stores = vec![store.clone()];
        let mut governances = vec![governance.clone()];
        let mut configs = vec![config.clone()];
        let mut tasks = Vec::new();
        let mut registries = Vec::new();
        if wire {
            registries.push(node.connection_registry.clone());
            let mut peer_config = config.clone();
            peer_config.data_dir = dir.path().join("1");
            peer_config.port = ports[1];
            peer_config.bootstrap_peers = vec![format!("127.0.0.1:{}", ports[0])];
            let mut peer = crate::BurstNode::new(peer_config.clone()).await.unwrap();
            peer.initialize_genesis().unwrap();
            peer.verification_fixture_clock = Some(Arc::new(|| None));
            stores.push(peer.store.clone());
            governances.push(peer.governance.clone());
            elections.push(peer.active_elections.clone());
            registries.push(peer.connection_registry.clone());
            configs.push(peer_config);
            tasks.push((
                peer.shutdown.clone(),
                tokio::spawn(async move {
                    peer.start_with_services(burst_daemon::start_public_services)
                        .await
                        .unwrap();
                    peer.stop().await.unwrap();
                }),
            ));
        }
        let keys = [0, 1].map(|seed| burst_crypto::keypair_from_seed(&[seed; 32]));
        let owner = burst_crypto::derive_address(&keys[0].public);
        let recipient = burst_crypto::derive_address(&keys[1].public);
        let genesis = store.account_store().get_account(&owner).unwrap().head;
        let base: StateBlock =
            bincode::deserialize(&store.block_store().get_block(&genesis).unwrap()).unwrap();
        let now = Timestamp::now();
        let mut endorse = base.clone();
        endorse.previous = genesis;
        endorse.payment_epoch = store.meta_store().payment_epoch().unwrap();
        endorse.block_type = BlockType::Endorse;
        endorse.timestamp = now;
        endorse.brn_spent = 1;
        endorse.link = BlockHash::new(keys[1].public.0);
        sign(&mut endorse, 0);
        let mut open = base.clone();
        open.payment_epoch = endorse.payment_epoch;
        open.account = recipient.clone();
        open.representative = recipient.clone();
        open.timestamp = now;
        sign(&mut open, 1);
        let proposal_tx = burst_transactions::Transaction::GovernanceProposal(
            build_governance_proposal_tx(
                &owner,
                burst_transactions::governance::ProposalContent::ParameterChange {
                    parameter: "brn_rate".into(),
                    current_value: config.params.brn_rate,
                    proposed_value: 2,
                },
                now,
            )
            .unwrap(),
        );
        let proposal = sign_state_block(
            build_state_block(
                &account(&endorse, 2),
                &proposal_tx,
                burst_types::TxHash::ZERO,
                config.params.params_hash(),
            )
            .unwrap(),
            &keys[0].private,
        );
        let scope = if proposal_scope {
            KeyScope::Proposal(proposal.hash.into_tx_hash())
        } else {
            KeyScope::Parameter(burst_governance::GovernableParam::BrnRate)
        };
        let grant = Grant::create(
            &keys[0],
            keys[1].public.clone(),
            scope,
            1,
            NetworkId::Dev,
            genesis,
        )
        .unwrap();
        let grant_block = burst_wallet_core::delegation::build_key_operation(
            &account(&proposal, 3),
            &keys[0],
            &DelegationOperation::Grant(grant.clone()),
            config.params.params_hash(),
            Timestamp::new(now.as_secs() + 1),
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
            &account(&open, 1),
            &keys[1],
            &DelegationOperation::Ballot {
                ballot: ballot.clone(),
                grant_block: grant_block.hash,
            },
            config.params.params_hash(),
            Timestamp::new(now.as_secs() + 2),
        )
        .unwrap();
        let mut winner = grant_block.clone();
        winner.block_type = BlockType::ChangeRepresentative;
        winner.link = BlockHash::ZERO;
        winner.delegation_payload.clear();
        sign(&mut winner, 0);
        if ballot_fork {
            let nay = DelegatedBallot::sign(
                &grant,
                &secondary,
                proposal.hash.into_tx_hash(),
                burst_transactions::governance::GovernanceVote::Nay,
            )
            .unwrap();
            winner = burst_wallet_core::delegation::build_key_operation(
                &account(&open, 1),
                &keys[1],
                &DelegationOperation::Ballot {
                    ballot: nay,
                    grant_block: grant_block.hash,
                },
                config.params.params_hash(),
                ballot_block.timestamp,
            )
            .unwrap();
        }
        tasks.push((
            shutdown,
            tokio::spawn(async move {
                node.start_with_services(burst_daemon::start_public_services)
                    .await
                    .unwrap();
                node.stop().await.unwrap();
            }),
        ));
        let result = tokio::time::timeout(Duration::from_secs(60), async {
            for registry in &registries {
                while registry.read().await.peer_ids().is_empty() {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
            for (index, (block, height)) in [(&endorse, 2), (&open, 1), (&proposal, 3)]
                .into_iter()
                .enumerate()
            {
                assert!(queue.push(block.clone()).await);
                for store in &stores {
                    until(|| store.block_store().exists(&block.hash).unwrap()).await;
                }
                authorize(
                    &elections,
                    &registries,
                    block,
                    &owner,
                    10_000 + index as u64,
                )
                .await;
                for store in &stores {
                    until(|| {
                        store
                            .account_store()
                            .get_account(&block.account)
                            .unwrap()
                            .confirmation_height
                            >= height
                    })
                    .await;
                }
            }
            for block in [&grant_block, &ballot_block] {
                assert!(queue.push(block.clone()).await);
                for store in &stores {
                    until(|| store.block_store().exists(&block.hash).unwrap()).await;
                }
            }
            if ballot_fork {
                authorize(&elections, &registries, &grant_block, &owner, 10_003).await;
                for store in &stores {
                    until(|| {
                        store
                            .account_store()
                            .get_account(&owner)
                            .unwrap()
                            .confirmation_height
                            == 4
                    })
                    .await;
                }
            }
            assert!(governance
                .lock()
                .await
                .key_ballot(proposal.hash.into_tx_hash(), &owner, grant.id().unwrap())
                .is_some());
            assert!(queue.push(winner.clone()).await);
            for store in &stores {
                until(|| {
                    store
                        .meta_store()
                        .fork_candidate(&winner.hash)
                        .unwrap()
                        .is_some()
                })
                .await;
            }
            authorize(&elections, &registries, &winner, &owner, 10_004).await;
            for (store, governance) in stores.iter().zip(&governances) {
                until(|| {
                    store
                        .account_store()
                        .get_account(&winner.account)
                        .unwrap()
                        .confirmation_height
                        == if ballot_fork { 2 } else { 4 }
                })
                .await;
                assert!(!store.block_store().exists(&ballot_block.hash).unwrap());
                assert_eq!(
                    store.account_store().get_account(&recipient).unwrap().head,
                    if ballot_fork { winner.hash } else { open.hash }
                );
                assert_eq!(
                    governance.lock().await.key_ballot(
                        proposal.hash.into_tx_hash(),
                        &owner,
                        grant.id().unwrap()
                    ),
                    ballot_fork.then_some(burst_transactions::governance::GovernanceVote::Nay)
                );
            }
        })
        .await;
        for (shutdown, _) in &tasks {
            shutdown.shutdown();
        }
        for (_, task) in tasks {
            tokio::time::timeout(Duration::from_secs(15), task)
                .await
                .unwrap()
                .unwrap();
        }
        result.expect("live ballot fork failed");

        for (config, store) in configs.into_iter().zip(stores) {
            let mut restarted = crate::BurstNode::new(config.clone()).await.unwrap();
            restarted.verification_fixture_clock = Some(Arc::new(|| None));
            let queue = restarted.block_queue();
            let governance = restarted.governance.clone();
            let shutdown = restarted.shutdown.clone();
            let task = tokio::spawn(async move {
                restarted
                    .start_with_services(burst_daemon::start_public_services)
                    .await
                    .unwrap();
                restarted.stop().await.unwrap();
            });
            let result = tokio::time::timeout(Duration::from_secs(30), async {
                if ballot_fork {
                    use burst_governance::signing_delegation::{scoped, Revocation};
                    let revoke = burst_wallet_core::delegation::build_key_operation(
                        &account(&grant_block, 4),
                        &keys[0],
                        &DelegationOperation::Revoke(
                            Revocation::create(
                                &keys[0],
                                grant.scope.clone(),
                                2,
                                NetworkId::Dev,
                                genesis,
                            )
                            .unwrap(),
                        ),
                        config.params.params_hash(),
                        Timestamp::new(now.as_secs() + 3),
                    )
                    .unwrap();
                    assert!(queue.push(ballot_block.clone()).await);
                    assert!(queue.push(revoke.clone()).await);
                    until(|| store.block_store().exists(&revoke.hash).unwrap()).await;
                    let mut preference = winner.clone();
                    preference.previous = winner.hash;
                    preference.block_type = BlockType::ChangeRepresentative;
                    preference.link = BlockHash::ZERO;
                    preference.transaction = burst_types::TxHash::ZERO;
                    preference.delegation_payload.clear();
                    preference.timestamp = Timestamp::new(now.as_secs() + 4);
                    sign(&mut preference, 1);
                    assert!(queue.push(preference.clone()).await);
                    until(|| {
                        store.account_store().get_account(&recipient).unwrap().head
                            == preference.hash
                    })
                    .await;
                    assert!(!store.block_store().exists(&ballot_block.hash).unwrap());
                    assert_eq!(
                        governance.lock().await.key_ballot(
                            proposal.hash.into_tx_hash(),
                            &owner,
                            grant.id().unwrap()
                        ),
                        Some(burst_transactions::governance::GovernanceVote::Nay)
                    );
                    let batch = store.tx_begin_write().unwrap();
                    let state = crate::governance_commit::CandidateGovernance::restore(
                        &batch,
                        config.params.clone(),
                        NetworkId::Dev,
                        genesis,
                    )
                    .unwrap();
                    let key = scoped::key(NetworkId::Dev, genesis, &owner, &grant.scope).unwrap();
                    let metadata = [(key.clone(), batch.get_meta(&key).unwrap().unwrap())].into();
                    let projection = state
                        .delegation_at_cut(
                            proposal.hash.into_tx_hash(),
                            &batch.verified_accounts().unwrap(),
                            &metadata,
                        )
                        .unwrap();
                    assert_eq!(projection.key_ballot(&owner), Some(None));
                    return;
                }
                let fresh_grant = burst_wallet_core::delegation::build_key_operation(
                    &account(&winner, 4),
                    &keys[0],
                    &DelegationOperation::Grant(grant.clone()),
                    config.params.params_hash(),
                    Timestamp::new(now.as_secs() + 3),
                )
                .unwrap();
                let fresh_ballot = burst_wallet_core::delegation::build_key_operation(
                    &account(&open, 1),
                    &keys[1],
                    &DelegationOperation::Ballot {
                        ballot: ballot.clone(),
                        grant_block: fresh_grant.hash,
                    },
                    config.params.params_hash(),
                    Timestamp::new(now.as_secs() + 4),
                )
                .unwrap();
                for block in [&fresh_grant, &fresh_ballot] {
                    assert!(queue.push(block.clone()).await);
                    until(|| store.block_store().exists(&block.hash).unwrap()).await;
                }
                assert_eq!(
                    governance.lock().await.key_ballot(
                        proposal.hash.into_tx_hash(),
                        &owner,
                        grant.id().unwrap()
                    ),
                    Some(burst_transactions::governance::GovernanceVote::Yea)
                );
                assert_eq!(
                    store.account_store().get_account(&recipient).unwrap().head,
                    fresh_ballot.hash
                );
            })
            .await;
            shutdown.shutdown();
            tokio::time::timeout(Duration::from_secs(15), task)
                .await
                .unwrap()
                .unwrap();
            result.expect("ballot fork restart failed");
        }
    }
}
