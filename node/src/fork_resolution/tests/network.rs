//! Signed TCP votes with the genesis human roster. Multi-human partitions remain
//! a separate acceptance boundary; these tests never inject election outcomes.
use super::*;
use crate::{BurstNode, NodeConfig, WireMessage, WireVote};
use burst_types::Signature;

pub(super) fn port() -> u16 {
    std::net::TcpListener::bind("0.0.0.0:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_network_vote_installs_verification_fork_and_recovers() {
    signed_fork_and_recovery(BlockType::Endorse, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_network_vote_installs_delegation_fork_and_recovers() {
    signed_fork_and_recovery(BlockType::Delegate, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_network_vote_installs_full_proposal_fork_and_recovers() {
    signed_fork_and_recovery(BlockType::GovernanceProposal, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_network_vote_installs_encrypted_key_fork_and_recovers() {
    signed_fork_and_recovery(BlockType::Delegate, true).await;
}

async fn signed_fork_and_recovery(kind: BlockType, encrypted: bool) {
    use super::governance::{saved_grant, scope};
    use burst_governance::signing_delegation::{operation::DelegationOperation, Grant, Revocation};
    for candidate_wins in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let ports = [port(), port()];
        assert_ne!(ports[0], ports[1]);
        let mut nodes = Vec::new();
        let mut configs = Vec::new();
        let mut stores = Vec::new();
        let mut queues = Vec::new();
        let mut registries = Vec::new();
        let mut elections = Vec::new();
        let mut verifications = Vec::new();
        let mut delegations = Vec::new();
        let mut governances = Vec::new();
        let mut transfers = Vec::new();
        for index in 0..2 {
            let mut config = NodeConfig {
                network: NetworkId::Dev,
                data_dir: directory.path().join(index.to_string()),
                port: ports[index],
                enable_rpc: false,
                enable_upnp: false,
                enable_representative: false,
                bootstrap_peers: vec![format!("127.0.0.1:{}", ports[1 - index])],
                ..NodeConfig::default()
            };
            config.params.min_work_difficulty = 0;
            config.params.endorsement_burn_amount = 1;
            let mut node = BurstNode::new(config.clone()).await.unwrap();
            node.initialize_genesis().unwrap();
            node.verification_fixture_clock = Some(Arc::new(|| None));
            configs.push(config);
            stores.push(node.store.clone());
            queues.push(node.block_queue());
            registries.push(node.connection_registry.clone());
            elections.push(node.active_elections.clone());
            verifications.push(node.verification_orchestrator.clone());
            delegations.push(node.delegation_engine.clone());
            governances.push(node.governance.clone());
            transfers.push(node.governance_transfers.clone());
            let shutdown = node.shutdown.clone();
            nodes.push((
                shutdown,
                tokio::spawn(async move {
                    node.start_with_services(burst_daemon::start_public_services).await.unwrap();
                    node.stop().await.unwrap();
                }),
            ));
        }
        let creator_keys = burst_crypto::keypair_from_seed(&[0; 32]);
        let creator = burst_crypto::derive_address(&creator_keys.public);
        let target_keys = burst_crypto::keypair_from_seed(&[9; 32]);
        let target = burst_crypto::derive_address(&target_keys.public);
        let genesis = stores[0]
            .account_store()
            .get_account(&creator)
            .unwrap()
            .head;
        let mut candidate: StateBlock =
            bincode::deserialize(&stores[0].block_store().get_block(&genesis).unwrap()).unwrap();
        candidate.previous = genesis;
        candidate.payment_epoch = stores[0].meta_store().payment_epoch().unwrap();
        candidate.block_type = kind.clone();
        candidate.timestamp = Timestamp::now();
        candidate.brn_spent = u128::from(kind == BlockType::Endorse);
        candidate.link = BlockHash::new(target_keys.public.0);
        let grant = encrypted.then(|| {
            Grant::create(
                &creator_keys,
                target_keys.public.clone(),
                scope(),
                1,
                NetworkId::Dev,
                genesis,
            )
            .unwrap()
        });
        if let Some(grant) = &grant {
            candidate.delegation_payload =
                DelegationOperation::Grant(grant.clone()).encode().unwrap();
        }
        let content = (kind == BlockType::GovernanceProposal).then(|| {
            burst_governance::content::EncodedContent::new(
                &burst_governance::ProposalContent::ConstitutionalDiff {
                    title: "Equal rights".into(),
                    operations: vec![burst_governance::proposal::AmendmentOp::AddArticle {
                        title: "Humanity".into(),
                        text: "🌍".repeat(burst_governance::content::MAX_AMENDMENT_CHARS),
                    }],
                },
            )
            .unwrap()
        });
        if let Some(content) = &content {
            assert!(content.bytes().len() > burst_types::MAX_P2P_MESSAGE_BYTES);
            (candidate.link, candidate.origin, candidate.transaction) =
                content.manifest().block_fields().unwrap();
        }
        sign(&mut candidate, 0);
        let proposal = candidate.clone();
        let mut preference = candidate.clone();
        preference.block_type = BlockType::ChangeRepresentative;
        preference.brn_spent = 0;
        preference.link = BlockHash::ZERO;
        preference.origin = burst_types::TxHash::ZERO;
        preference.transaction = burst_types::TxHash::ZERO;
        preference.delegation_payload.clear();
        sign(&mut preference, 0);
        let (loser, winner) = if candidate_wins {
            (preference, candidate)
        } else {
            (candidate, preference)
        };
        let result = tokio::time::timeout(Duration::from_secs(90), async {
            for registry in &registries {
                loop {
                    if !registry.read().await.peer_ids().is_empty() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
            if let Some(content) = content.as_ref().filter(|_| !candidate_wins) {
                transfers[0].offer(&proposal, content.bytes()).unwrap();
            }
            assert!(queues[0].push(loser.clone()).await);
            for store in &stores {
                until(|| store.block_store().exists(&loser.hash).unwrap()).await;
            }
            if let Some(content) = content.as_ref().filter(|_| candidate_wins) {
                transfers[1].offer(&proposal, content.bytes()).unwrap();
            }
            assert!(queues[1].push(winner.clone()).await);
            for active in &elections {
                loop {
                    if active.read().await.candidate_root(&winner.hash).is_some() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
            let mut vote = WireVote {
                voter: creator.clone(),
                block_hashes: vec![winner.hash],
                is_final: true,
                timestamp: Timestamp::now().as_secs() * 1000,
                sequence: 10_000,
                signature: Signature([0; 64]),
            };
            for authenticated in [false, true] {
                if authenticated {
                    vote.signature = burst_crypto::sign_message(
                        &burst_consensus::vote_signing_payload(
                            &vote.voter,
                            vote.is_final,
                            vote.timestamp,
                            vote.sequence,
                            &vote.block_hashes,
                        ),
                        &creator_keys.private,
                    );
                }
                let bytes = WireMessage::Vote(vote.clone()).encode().unwrap();
                for registry in &registries {
                    let registry = registry.read().await;
                    for peer in registry.peer_ids() {
                        assert!(registry.send(peer, bytes.clone()));
                    }
                }
                if !authenticated {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    for store in &stores {
                        let account = store.account_store().get_account(&creator).unwrap();
                        assert_eq!(account.head, loser.hash);
                        assert_eq!(account.confirmation_height, 1);
                    }
                }
            }
            for (store, verification) in stores.iter().zip(&verifications) {
                until(|| {
                    let account = store.account_store().get_account(&creator).unwrap();
                    account.head == winner.hash && account.confirmation_height == 2
                })
                .await;
                assert!(!store.block_store().exists(&loser.hash).unwrap());
                assert_eq!(
                    verification.lock().await.get_state(&target).is_some(),
                    candidate_wins && kind == BlockType::Endorse
                );
                assert_eq!(
                    verification.lock().await.snapshot().encode().unwrap(),
                    crate::verification_storage::load(&store.tx_begin_write().unwrap())
                        .unwrap().snapshot().encode().unwrap()
                );
                assert!(queues[0].push(loser.clone()).await);
            }
            for delegation in &delegations {
                assert_eq!(
                    delegation.lock().await.get_delegate(&creator),
                    (candidate_wins && kind == BlockType::Delegate && !encrypted)
                        .then_some(&target)
                );
            }
            if let Some(content) = &content {
                for governance in &governances {
                    let live = governance.lock().await;
                    let saved = live.get_proposal(&proposal.hash.into_tx_hash());
                    assert_eq!(saved.is_some(), candidate_wins);
                    if let Some(saved) = saved {
                        assert_eq!(
                            saved.content,
                            content.manifest().decode_verified(content.bytes()).unwrap()
                        );
                    }
                }
            }
            if let Some(grant) = &grant {
                for store in &stores {
                    assert_eq!(
                        saved_grant(store, genesis, &creator),
                        candidate_wins.then_some(grant.id().unwrap())
                    );
                }
            }
        })
        .await;
        for (shutdown, _) in &nodes {
            shutdown.shutdown();
        }
        for (_, task) in nodes {
            tokio::time::timeout(Duration::from_secs(15), task)
                .await
                .unwrap()
                .unwrap();
        }
        result.expect("signed verification fork did not converge");
        for config in configs {
            let mut restarted = BurstNode::new(config).await.unwrap();
            restarted.verification_fixture_clock = Some(Arc::new(|| None));
            let store = restarted.store.clone();
            let queue = restarted.block_queue();
            let verification = restarted.verification_orchestrator.clone();
            let delegation = restarted.delegation_engine.clone();
            let governance = restarted.governance.clone();
            let transfers = restarted.governance_transfers.clone();
            let shutdown = restarted.shutdown.clone();
            let task = tokio::spawn(async move {
                restarted.start_with_services(burst_daemon::start_public_services).await.unwrap();
                restarted.stop().await.unwrap();
            });
            let checked = tokio::time::timeout(Duration::from_secs(20), async {
                until(|| store.account_store().get_account(&creator).unwrap().head == winner.hash)
                    .await;
                loop {
                    let live = verification.lock().await.snapshot().encode().unwrap();
                    if live
                        == crate::verification_storage::load(&store.tx_begin_write().unwrap())
                        .unwrap().snapshot().encode().unwrap()
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                assert_eq!(
                    verification.lock().await.get_state(&target).is_some(),
                    candidate_wins && kind == BlockType::Endorse
                );
                assert_eq!(
                    delegation.lock().await.get_delegate(&creator),
                    (candidate_wins && kind == BlockType::Delegate && !encrypted)
                        .then_some(&target)
                );
                let mut followup = winner.clone();
                followup.previous = winner.hash;
                followup.timestamp = Timestamp::now();
                followup.brn_spent = u128::from(kind == BlockType::Endorse);
                followup.block_type = if candidate_wins {
                    if kind == BlockType::Delegate {
                        BlockType::RevokeDelegation
                    } else {
                        BlockType::ChangeRepresentative
                    }
                } else {
                    kind.clone()
                };
                followup.link = if candidate_wins {
                    BlockHash::ZERO
                } else {
                    BlockHash::new(target_keys.public.0)
                };
                sign(&mut followup, 0);
                if let Some(grant) = &grant {
                    assert_eq!(
                        saved_grant(&store, genesis, &creator),
                        candidate_wins.then_some(grant.id().unwrap())
                    );
                    followup.delegation_payload = if candidate_wins {
                        DelegationOperation::Revoke(
                            Revocation::create(&creator_keys, scope(), 2, NetworkId::Dev, genesis)
                                .unwrap(),
                        )
                    } else {
                        DelegationOperation::Grant(grant.clone())
                    }
                    .encode()
                    .unwrap();
                    sign(&mut followup, 0);
                }
                if let Some(content) = &content {
                    let live = governance.lock().await;
                    let saved = live.get_proposal(&proposal.hash.into_tx_hash());
                    assert_eq!(saved.is_some(), candidate_wins);
                    if let Some(saved) = saved {
                        assert_eq!(
                            saved.content,
                            content.manifest().decode_verified(content.bytes()).unwrap()
                        );
                    }
                    drop(live);
                    followup.origin = burst_types::TxHash::ZERO;
                    followup.transaction = burst_types::TxHash::ZERO;
                    if candidate_wins {
                        followup.block_type = BlockType::GovernanceWithdraw;
                        followup.link = proposal.hash;
                    } else {
                        (followup.link, followup.origin, followup.transaction) =
                            content.manifest().block_fields().unwrap();
                    }
                    sign(&mut followup, 0);
                    if !candidate_wins {
                        transfers.offer(&followup, content.bytes()).unwrap();
                    }
                }
                assert!(queue.push(followup.clone()).await);
                until(|| {
                    store.account_store().get_account(&creator).unwrap().head == followup.hash
                })
                .await;
                if let Some(grant) = &grant {
                    assert_eq!(
                        saved_grant(&store, genesis, &creator),
                        (!candidate_wins).then_some(grant.id().unwrap())
                    );
                    use burst_governance::signing_delegation::{
                        scoped,
                        view::{index_key, Direction},
                    };
                    let scope_key =
                        scoped::key(NetworkId::Dev, genesis, &creator, &scope()).unwrap();
                    let outgoing = index_key(Direction::Outgoing, &creator, &scope_key).unwrap();
                    let incoming = index_key(Direction::Incoming, &target, &scope_key).unwrap();
                    assert_eq!(
                        store.meta_store().get_meta(&outgoing).unwrap(),
                        followup.hash.as_bytes()
                    );
                    assert_eq!(
                        store.meta_store().get_meta(&incoming).unwrap(),
                        if candidate_wins {
                            BlockHash::ZERO
                        } else {
                            followup.hash
                        }
                        .as_bytes()
                    );
                }
                if content.is_some() {
                    let live = governance.lock().await;
                    let id = if candidate_wins {
                        proposal.hash
                    } else {
                        followup.hash
                    };
                    let saved = live.get_proposal(&id.into_tx_hash()).unwrap();
                    assert_eq!(
                        saved.phase,
                        if candidate_wins {
                            burst_governance::GovernancePhase::Withdrawn
                        } else {
                            burst_governance::GovernancePhase::Proposal
                        }
                    );
                }
                assert_eq!(
                    verification.lock().await.get_state(&target).is_some(),
                    kind == BlockType::Endorse
                );
                assert_eq!(
                    delegation.lock().await.get_delegate(&creator),
                    (!candidate_wins && kind == BlockType::Delegate && !encrypted)
                        .then_some(&target)
                );
            })
            .await;
            shutdown.shutdown();
            tokio::time::timeout(Duration::from_secs(15), task)
                .await
                .unwrap()
                .unwrap();
            checked.expect("verification fork restart did not restore state");
        }
    }
}
