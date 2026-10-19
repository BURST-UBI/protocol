use super::*;
use crate::{checkpoint_store::chain, payment_authority::PaymentAuthority};
use burst_store::{
    account::AccountStore, block::BlockStore, meta::MetaStore, pending::PendingStore,
};

mod conflicts;
mod forks;
mod key_ballots;
mod keys;
mod native_return;
mod partitioned_expiry;
mod verification_fork;

pub(super) async fn installed(
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    height: usize,
    params: &ProtocolParams,
) -> BlockHash {
    let result = tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let mut epochs = Vec::new();
            for store in stores {
                let events = chain::load(store, NetworkId::Dev, params).unwrap();
                if let Some(event) = events.get(height - 1) {
                    let epoch = event.certificate().checkpoint.hash();
                    if PaymentAuthority::read(store)
                        .unwrap()
                        .is_some_and(|authority| authority.epoch == epoch)
                    {
                        epochs.push(epoch);
                    }
                }
            }
            if epochs.len() == stores.len() && epochs.iter().all(|epoch| *epoch == epochs[0]) {
                return epochs[0];
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    if result.is_err() {
        for (index, store) in stores.iter().enumerate() {
            let events = chain::load(store, NetworkId::Dev, params).unwrap();
            let authority = PaymentAuthority::read(store)
                .unwrap()
                .map(|value| value.epoch);
            let anchor = chain::next_anchor(&events, NetworkId::Dev, params).unwrap();
            let representative = burst_crypto::derive_address(
                &burst_crypto::keypair_from_seed(&[index as u8; 32]).public,
            );
            let state =
                crate::checkpoint_agreement::read_state(store, &anchor, &representative).unwrap();
            eprintln!(
                "node {index}: wanted height {height}, retained {}, authority {authority:?}, agreement {:?}",
                events.len()
                ,state.as_ref().map(|state| (state.round(), state.step(), state.prevote(), state.precommit(), state.decision().is_some()))
            );
        }
    }
    result.expect("certified governance epoch did not install")
}

pub(super) async fn stop(
    workers: &mut Vec<(
        Arc<crate::shutdown::ShutdownController>,
        tokio::task::JoinHandle<()>,
    )>,
) {
    for (shutdown, _) in workers.iter() {
        shutdown.shutdown();
    }
    for (_, task) in workers.drain(..) {
        tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn automatic_governance_counts_activates_and_recovers_across_three_human_nodes() {
    governance_journey(false, false, Scenario::DelayedRecovery).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expiry_extension_at_pending_deadline_survives_three_human_nodes_and_fresh_catchup() {
    governance_journey(true, false, Scenario::Idle).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revived_expiry_return_is_claimed_once_and_spent_across_restart_and_fresh_catchup() {
    governance_journey(true, true, Scenario::Idle).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the native wallet, Vite, WebKitWebDriver and libfaketime"]
async fn desktop_expiry_return_is_claimed_spent_and_recovers_across_certified_nodes() {
    governance_journey(true, true, Scenario::NativeReturn).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partitioned_expiry_count_and_activation_wait_for_quorum_then_recover() {
    governance_journey(true, true, Scenario::ExpiryPartition).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn certified_activation_fences_postcut_traffic_while_precut_payments_finish() {
    governance_journey(false, false, Scenario::Activation).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn confirmed_support_fences_counting_while_precut_payments_finish_and_nodes_recover() {
    governance_journey(false, false, Scenario::Counting).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unvoted_proposals_release_promised_limits_and_recover_across_three_human_nodes() {
    for scenario in [Scenario::Rejected, Scenario::Withdrawn] {
        governance_journey(false, false, scenario).await;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    VerificationFork,
    Idle,
    DelayedRecovery,
    Counting,
    Activation,
    Rejected,
    Withdrawn,
    Timing,
    Conflicts,
    Keys,
    PartitionedKeys,
    KeyBallots,
    PartitionedKeyBallots,
    NativeReturn,
    ExpiryPartition,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn secondary_ballots_apply_scopes_revocations_and_transitive_counting_through_recovery() {
    governance_journey(false, false, Scenario::KeyBallots).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partitioned_secondary_ballots_and_counting_wait_for_quorum_then_recover() {
    governance_journey(false, false, Scenario::PartitionedKeyBallots).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scoped_key_journal_preserves_revocation_through_certified_recovery() {
    governance_journey(false, false, Scenario::Keys).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn partitioned_key_revocation_waits_for_quorum_and_recovers() {
    governance_journey(false, false, Scenario::PartitionedKeys).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conflicting_constitutional_diffs_do_not_stall_certified_activation_or_recovery() {
    governance_journey(false, false, Scenario::Conflicts).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timing_change_preserves_existing_windows_and_new_proposals_use_new_rules() {
    governance_journey(false, false, Scenario::Timing).await;
}

async fn supported_rate_proposal(
    clients: &[NodeClient],
    proposer: &KeyPair,
    supporter: &KeyPair,
    params: &ProtocolParams,
    at: u64,
    new_rate: u128,
) -> StateBlock {
    let owner = burst_crypto::derive_address(&proposer.public);
    let tx = Transaction::GovernanceProposal(
        build_governance_proposal_tx(
            &owner,
            burst_transactions::governance::ProposalContent::ParameterChange {
                parameter: "brn_rate".into(),
                current_value: params.brn_rate,
                proposed_value: new_rate,
            },
            Timestamp::new(at),
        )
        .unwrap(),
    );
    let proposal = signed_block(&clients[0], proposer, tx, params).await;
    submit(clients, &proposal).await;
    let owner = burst_crypto::derive_address(&supporter.public);
    let tx = Transaction::ChangeRepresentative(
        build_change_rep_tx(&owner, &owner, Timestamp::new(at)).unwrap(),
    );
    let mut support = signed_block(&clients[0], supporter, tx, params).await;
    support.block_type = BlockType::GovernanceEndorse;
    support.link = proposal.hash;
    support.brn_balance += params.governance_proposal_cost;
    support.hash = support.compute_hash();
    submit(clients, &sign_state_block(support, &supporter.private)).await;
    proposal
}

async fn governance_journey(extend_expiry: bool, reclaim_return: bool, traffic: Scenario) {
    let terminal = matches!(traffic, Scenario::Rejected | Scenario::Withdrawn);
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let _permit = crate::verification_test_support::NETWORK_FIXTURE
        .acquire()
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let base = if extend_expiry {
        Timestamp::now().as_secs()
    } else {
        time(10).as_secs()
    };
    let clock = Arc::new(AtomicU64::new(base + 10));
    let params = ProtocolParams {
        brn_rate: BRN_UNIT,
        min_work_difficulty: 0,
        bootstrap_exit_threshold: if traffic == Scenario::VerificationFork {
            3
        } else {
            4
        },
        endorsement_burn_amount: 1,
        new_wallet_spending_limit: 0,
        new_wallet_tx_limit_per_day: 0,
        trst_expiry_secs: if extend_expiry { 31 } else { u64::MAX },
        governance_proposal_cost: 1,
        governance_proposal_duration_secs: 10,
        governance_exploration_duration_secs: 20,
        governance_cooldown_duration_secs: 10,
        governance_propagation_buffer_secs: 1,
        governance_proposal_endorsements: if traffic == Scenario::Rejected { 2 } else { 1 },
        governance_supermajority_bps: 6000,
        governance_quorum_bps: 10000,
        ..ProtocolParams::burst_defaults()
    };
    let ports = [port(), port(), port()];
    let mut configs = Vec::new();
    let mut clients = Vec::new();
    let mut stores = Vec::new();
    let mut workers = Vec::new();
    let mut registries = Vec::new();
    let mut signers = Vec::new();
    let mut elections = Vec::new();
    for index in 0..3 {
        let key_file = directory.path().join(format!("representative-{index}.key"));
        std::fs::write(&key_file, hex::encode([index as u8; 32])).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let config = NodeConfig {
            network: NetworkId::Dev,
            data_dir: directory.path().join(format!("node-{index}")),
            port: ports[index],
            rpc_port: port(),
            enable_upnp: false,
            enable_faucet: false,
            nts_sources: vec![],
            representative_key_file: Some(key_file),
            params: params.clone(),
            bootstrap_peers: ports
                .iter()
                .enumerate()
                .filter(|(peer, _)| *peer != index)
                .map(|(_, port)| format!("127.0.0.1:{port}"))
                .collect(),
            ..NodeConfig::default()
        };
        clients.push(NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap());
        configs.push(config.clone());
        let mut node = fixture_node(config, &clock, "http://127.0.0.1:1").await;
        registries.push(node.connection_registry.clone());
        signers.push(node.vote_generator.clone());
        elections.push(node.active_elections.clone());
        stores.push(node.store.clone());
        workers.push((
            node.shutdown.clone(),
            tokio::spawn(async move {
                node.start().await.unwrap();
                node.stop().await.unwrap();
            }),
        ));
    }
    let journey_stores = stores.clone();
    let journey_clock = clock.clone();
    let journey_params = params.clone();
    let mut journey = tokio::spawn(async move {
        let params = journey_params;
        let stores = journey_stores;
        let clock = journey_clock;
        for client in &clients {
            loop {
                if client
                    .rpc_call("peers", json!({}))
                    .await
                    .is_ok_and(|value| value["count"].as_u64().unwrap_or(0) >= 2)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let keys: Vec<_> = (0..3)
            .map(|seed| burst_crypto::keypair_from_seed(&[seed; 32]))
            .collect();
        let addresses: Vec<_> = keys
            .iter()
            .map(|key| burst_crypto::derive_address(&key.public))
            .collect();
        let now = Timestamp::new(base + 10);
        for index in 1..3 {
            let tx = Transaction::Endorse(
                build_endorse_tx(&addresses[0], &addresses[index], 1, now).unwrap(),
            );
            submit(
                &clients,
                &signed_block(&clients[0], &keys[0], tx, &params).await,
            )
            .await;
            let tx =
                Transaction::Burn(build_burn_tx(&addresses[0], &addresses[index], 1, now).unwrap());
            let source = signed_block(&clients[0], &keys[0], tx, &params).await;
            submit(&clients, &source).await;
            let tx = Transaction::Receive(
                build_receive_tx(&addresses[index], source.hash.into_tx_hash(), 1, now).unwrap(),
            );
            submit(
                &clients,
                &signed_block(&clients[0], &keys[index], tx, &params).await,
            )
            .await;
        }
        clock.store(base + 20, Ordering::SeqCst);
        installed(&stores, 1, &params).await;
        for store in &stores {
            assert_eq!(
                PaymentAuthority::read(store)
                    .unwrap()
                    .unwrap()
                    .roster
                    .total_humans(),
                3
            );
        }
        if traffic == Scenario::VerificationFork {
            verification_fork::replace(
                &clients,
                &stores,
                &registries,
                (&signers, &elections),
                &keys,
                &params,
                base + 20,
            )
            .await;
        }
        let now = Timestamp::new(base + 20);
        let tx =
            Transaction::Delegate(build_delegate_tx(&addresses[1], &addresses[0], now).unwrap());
        submit(
            &clients,
            &signed_block(&clients[0], &keys[1], tx, &params).await,
        )
        .await;
        let tx = Transaction::GovernanceProposal(
            build_governance_proposal_tx(
                &addresses[0],
                burst_transactions::governance::ProposalContent::ParameterChange {
                    parameter: if extend_expiry {
                        "trst_expiry_secs"
                    } else if traffic == Scenario::Timing {
                        "governance_exploration_duration_secs"
                    } else {
                        "brn_rate"
                    }
                    .into(),
                    current_value: if extend_expiry {
                        31
                    } else if traffic == Scenario::Timing {
                        20
                    } else {
                        BRN_UNIT
                    },
                    proposed_value: if extend_expiry {
                        1000
                    } else if traffic == Scenario::Timing {
                        0
                    } else {
                        2 * BRN_UNIT
                    },
                },
                now,
            )
            .unwrap(),
        );
        let proposal = signed_block(&clients[0], &keys[0], tx, &params).await;
        submit(&clients, &proposal).await;
        let tx = Transaction::ChangeRepresentative(
            build_change_rep_tx(&addresses[0], &addresses[0], now).unwrap(),
        );
        let mut support = signed_block(&clients[0], &keys[0], tx, &params).await;
        support.block_type = BlockType::GovernanceEndorse;
        support.link = proposal.hash;
        support.brn_balance += 1;
        support.hash = support.compute_hash();
        support = sign_state_block(support, &keys[0].private);
        submit(&clients, &support).await;
        if matches!(traffic, Scenario::Keys | Scenario::PartitionedKeys) {
            keys::register(&clients, &keys, &params, proposal.hash, base + 20).await;
        }
        if traffic == Scenario::Withdrawn {
            clock.store(base + 25, Ordering::SeqCst);
            let tx = Transaction::ChangeRepresentative(
                build_change_rep_tx(&addresses[0], &addresses[0], Timestamp::new(base + 25))
                    .unwrap(),
            );
            let mut withdrawal = signed_block(&clients[0], &keys[0], tx, &params).await;
            withdrawal.block_type = BlockType::GovernanceWithdraw;
            withdrawal.link = proposal.hash;
            withdrawal.hash = withdrawal.compute_hash();
            withdrawal = sign_state_block(withdrawal, &keys[0].private);
            submit(&clients, &withdrawal).await;
        }
        clock.store(base + 31, Ordering::SeqCst);
        let pending_burn = if extend_expiry {
            let tx = Transaction::Burn(
                build_burn_tx(
                    &addresses[0],
                    &addresses[1],
                    7,
                    Timestamp::new(base + if reclaim_return { 20 } else { 31 }),
                )
                .unwrap(),
            );
            let burn = signed_block(&clients[0], &keys[0], tx, &params).await;
            submit(&clients, &burn).await;
            Some(burn.hash)
        } else {
            None
        };
        if !terminal {
            let mut votes = vec![
                (0, burst_transactions::governance::GovernanceVote::Yea),
                (2, burst_transactions::governance::GovernanceVote::Nay),
            ];
            if matches!(traffic, Scenario::Keys | Scenario::PartitionedKeys) {
                // A scoped grant overrides blanket delegation even without a
                // secondary ballot. This journal fixture uses the owner's
                // direct vote; secondary counting has its own journey.
                votes.push((1, burst_transactions::governance::GovernanceVote::Yea));
            }
            for (index, choice) in votes {
                let tx = Transaction::GovernanceVote(
                    build_governance_vote_tx(
                        &addresses[index],
                        proposal.hash.into_tx_hash(),
                        choice,
                        Timestamp::new(base + 31),
                    )
                    .unwrap(),
                );
                submit(
                    &clients,
                    &signed_block(&clients[0], &keys[index], tx, &params).await,
                )
                .await;
            }
        }
        let overlapping = if traffic == Scenario::Timing {
            clock.store(base + 50, Ordering::SeqCst);
            Some(
                supported_rate_proposal(
                    &clients,
                    &keys[1],
                    &keys[0],
                    &params,
                    base + 50,
                    3 * BRN_UNIT,
                )
                .await,
            )
        } else {
            None
        };
        let counting_tail = if traffic == Scenario::Counting || terminal {
            let tx = Transaction::Burn(
                build_burn_tx(&addresses[2], &addresses[0], 1, Timestamp::new(base + 52)).unwrap(),
            );
            let late = signed_block(&clients[0], &keys[2], tx, &params).await;
            assert!(
                clients[0]
                    .process(&serde_json::to_string(&late).unwrap())
                    .await
                    .unwrap()
                    .accepted
            );
            for second in [32, 41, 49] {
                clock.store(base + second, Ordering::SeqCst);
                let tx = Transaction::Burn(
                    build_burn_tx(
                        &addresses[1],
                        &addresses[0],
                        1,
                        Timestamp::new(base + second),
                    )
                    .unwrap(),
                );
                submit(
                    &clients,
                    &signed_block(&clients[0], &keys[1], tx, &params).await,
                )
                .await;
            }
            for (index, store) in stores.iter().enumerate() {
                if let Some(height) = store.block_store().height_of_block(&late.hash).unwrap() {
                    assert!(
                        store
                            .account_store()
                            .get_account(&late.account)
                            .unwrap()
                            .confirmation_height
                            < height
                    );
                }
                let key = format!(
                    "consensus_final/v2/{}/{}/{}/{}",
                    late.payment_epoch, addresses[index], late.account, late.previous
                );
                assert!(matches!(
                    store.meta_store().get_meta(&key),
                    Err(burst_store::StoreError::NotFound(_))
                ));
            }
            Some(late)
        } else {
            None
        };
        if traffic == Scenario::ExpiryPartition {
            partitioned_expiry::advance(
                &registries,
                &stores,
                &clock,
                base + 53,
                &addresses,
                pending_burn.unwrap().into_tx_hash(),
            )
            .await;
        } else {
            clock.store(base + 53, Ordering::SeqCst);
        }
        let counted_epoch = installed(&stores, 2, &params).await;
        if !terminal {
            for client in &clients {
                let counted = client
                    .rpc_call(
                        "governance_proposal_info",
                        json!({"hash":proposal.hash.to_string()}),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    counted["phase"], "Cooldown",
                    "unexpected certified count: {counted}"
                );
                assert_eq!(counted["votes_yea"], 2);
                assert_eq!(counted["votes_nay"], 1);
            }
        }
        if let Some(late) = counting_tail {
            for store in &stores {
                assert_ne!(
                    store
                        .account_store()
                        .get_account(&late.account)
                        .unwrap()
                        .head,
                    late.hash
                );
            }
        }
        if terminal {
            let tx = Transaction::Burn(
                build_burn_tx(&addresses[1], &addresses[0], 1, Timestamp::new(base + 53)).unwrap(),
            );
            let payment = signed_block(&clients[0], &keys[1], tx, &params).await;
            assert_eq!(payment.payment_epoch, counted_epoch);
            submit(&clients, &payment).await;
            return (proposal.hash, payment, counted_epoch, params, Vec::new());
        }
        let tx = Transaction::RevokeDelegation(
            build_revoke_delegation_tx(&addresses[1], Timestamp::new(base + 53)).unwrap(),
        );
        submit(
            &clients,
            &signed_block(&clients[0], &keys[1], tx, &params).await,
        )
        .await;
        let late = if traffic == Scenario::Activation {
            let tx = Transaction::Burn(
                build_burn_tx(&addresses[2], &addresses[0], 1, Timestamp::new(base + 63)).unwrap(),
            );
            let late = signed_block(&clients[0], &keys[2], tx, &params).await;
            assert!(
                clients[0]
                    .process(&serde_json::to_string(&late).unwrap())
                    .await
                    .unwrap()
                    .accepted
            );
            for second in [54, 57, 61] {
                clock.store(base + second, Ordering::SeqCst);
                let tx = Transaction::Burn(
                    build_burn_tx(
                        &addresses[1],
                        &addresses[0],
                        1,
                        Timestamp::new(base + second),
                    )
                    .unwrap(),
                );
                submit(
                    &clients,
                    &signed_block(&clients[0], &keys[1], tx, &params).await,
                )
                .await;
            }
            for (index, store) in stores.iter().enumerate() {
                if let Some(height) = store.block_store().height_of_block(&late.hash).unwrap() {
                    assert!(
                        store
                            .account_store()
                            .get_account(&late.account)
                            .unwrap()
                            .confirmation_height
                            < height
                    );
                }
                let key = format!(
                    "consensus_final/v2/{}/{}/{}/{}",
                    late.payment_epoch, addresses[index], late.account, late.previous
                );
                assert!(matches!(
                    store.meta_store().get_meta(&key),
                    Err(burst_store::StoreError::NotFound(_))
                ));
            }
            Some(late)
        } else {
            None
        };
        if traffic == Scenario::PartitionedKeys {
            keys::partitioned_revoke(
                &clients,
                &keys,
                &params,
                proposal.hash,
                base + 53,
                &registries,
                &stores,
            )
            .await;
        }
        if matches!(traffic, Scenario::Keys | Scenario::PartitionedKeys) {
            keys::rotate(
                &clients,
                &keys,
                &params,
                proposal.hash,
                base + 53,
                traffic == Scenario::PartitionedKeys,
            )
            .await;
        }
        if let Some(overlapping) = &overlapping {
            clock.store(base + 61, Ordering::SeqCst);
            let tx = Transaction::GovernanceVote(
                build_governance_vote_tx(
                    &addresses[0],
                    overlapping.hash.into_tx_hash(),
                    burst_transactions::governance::GovernanceVote::Yea,
                    Timestamp::new(base + 61),
                )
                .unwrap(),
            );
            submit(
                &clients,
                &signed_block(&clients[0], &keys[0], tx, &params).await,
            )
            .await;
        }
        if traffic == Scenario::ExpiryPartition {
            partitioned_expiry::advance(
                &registries,
                &stores,
                &clock,
                base + 64,
                &addresses,
                pending_burn.unwrap().into_tx_hash(),
            )
            .await;
        } else {
            clock.store(base + 64, Ordering::SeqCst);
        }
        let mut epoch = installed(&stores, 3, &params).await;
        if let Some(late) = late {
            for store in &stores {
                assert_ne!(
                    store
                        .account_store()
                        .get_account(&late.account)
                        .unwrap()
                        .head,
                    late.hash
                );
            }
        }
        let mut governed = params.clone();
        if extend_expiry {
            governed.trst_expiry_secs = 1000;
        } else if traffic == Scenario::Timing {
            governed.governance_exploration_duration_secs = 0;
        } else {
            governed.brn_rate = 2 * BRN_UNIT;
        }
        let claimant = if reclaim_return { 0 } else { 1 };
        let tx = if let Some(burn) = pending_burn {
            for store in &stores {
                assert!(
                    store
                        .pending_store()
                        .get_pending(&addresses[claimant], &burn.into_tx_hash())
                        .is_ok_and(|entry| entry.return_credit == reclaim_return),
                    "expiry must preserve the correct owner's claim"
                );
            }
            Transaction::Receive(
                build_receive_tx(
                    &addresses[claimant],
                    burn.into_tx_hash(),
                    7,
                    Timestamp::new(base + 64),
                )
                .unwrap(),
            )
        } else {
            Transaction::Burn(
                build_burn_tx(&addresses[1], &addresses[0], 1, Timestamp::new(base + 64)).unwrap(),
            )
        };
        let stale = signed_block(&clients[0], &keys[claimant], tx.clone(), &params).await;
        assert!(
            !clients[0]
                .process(&serde_json::to_string(&stale).unwrap())
                .await
                .unwrap()
                .accepted
        );
        let mut payment = signed_block(&clients[0], &keys[claimant], tx.clone(), &governed).await;
        if traffic == Scenario::NativeReturn {
            payment.origin = pending_burn.unwrap().into_tx_hash();
            payment.hash = payment.compute_hash();
            payment = sign_state_block(payment, &keys[claimant].private);
        }
        assert_eq!(payment.payment_epoch, epoch);
        if reclaim_return {
            let burn = pending_burn.unwrap().to_string();
            let receipt = clients[0]
                .account_pending_source(addresses[0].as_str(), &burn)
                .await
                .unwrap()
                .unwrap();
            assert!(receipt.return_credit);
            assert_eq!(receipt.amount, "7");
            assert!(clients[0]
                .reject_signed(&keys[0], &burn)
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("receive the credit"));
            let result = if traffic == Scenario::NativeReturn {
                native_return::claim(&clients[0], &burn, Timestamp::new(base + 64)).await
            } else {
                clients[0]
                    .submit_signed(&keys[0], &tx, TxHash::ZERO)
                    .await
                    .unwrap()
            };
            assert_eq!(result.hash, payment.hash.to_string());
            for client in &clients {
                client
                    .wait_confirmed(&result.hash, Duration::from_secs(25))
                    .await
                    .unwrap();
            }
        } else {
            submit(&clients, &payment).await;
        }
        if reclaim_return {
            let burn = pending_burn.unwrap();
            let duplicate = signed_block(&clients[0], &keys[0], tx, &governed).await;
            assert!(
                !clients[0]
                    .process(&serde_json::to_string(&duplicate).unwrap())
                    .await
                    .unwrap()
                    .accepted
            );
            let tx = Transaction::Send(
                build_send_tx(
                    &addresses[0],
                    &addresses[1],
                    7,
                    payment.hash.into_tx_hash(),
                    burn.into_tx_hash(),
                    Timestamp::new(base + 64),
                )
                .unwrap(),
            );
            let send = signed_block(&clients[0], &keys[0], tx, &governed).await;
            submit(&clients, &send).await;
            let tx = Transaction::Receive(
                build_receive_tx(
                    &addresses[1],
                    send.hash.into_tx_hash(),
                    7,
                    Timestamp::new(base + 64),
                )
                .unwrap(),
            );
            payment = signed_block(&clients[0], &keys[1], tx, &governed).await;
            submit(&clients, &payment).await;
            for store in &stores {
                assert!(store
                    .pending_store()
                    .get_pending(&addresses[0], &burn.into_tx_hash())
                    .is_err());
                assert_eq!(
                    store
                        .account_store()
                        .get_account(&addresses[0])
                        .unwrap()
                        .trst_balance,
                    0
                );
            }
        }
        for store in &stores {
            let events = chain::load(store, NetworkId::Dev, &params).unwrap();
            assert!(matches!(events[1], chain::CertifiedEvent::Governance(_)));
            assert!(matches!(events[2], chain::CertifiedEvent::Governance(_)));
            assert_eq!(events[1].certificate().checkpoint.cutoff, base + 51);
            assert_eq!(events[2].certificate().checkpoint.cutoff, base + 62);
            assert_eq!(events[2].certificate().signatures.len(), 3);
        }
        let mut extra_proposals = Vec::new();
        if matches!(traffic, Scenario::Keys | Scenario::PartitionedKeys) {
            keys::check(&stores, &params, proposal.hash);
        }
        if let Some(overlapping) = overlapping {
            let view = clients[0]
                .rpc_call(
                    "governance_proposal_info",
                    json!({"hash": overlapping.hash.to_string()}),
                )
                .await
                .unwrap();
            assert_eq!(view["phase_deadline"], base + 80);
            clock.store(base + 65, Ordering::SeqCst);
            let shorter = supported_rate_proposal(
                &clients,
                &keys[2],
                &keys[0],
                &governed,
                base + 65,
                4 * BRN_UNIT,
            )
            .await;
            let view = clients[0]
                .rpc_call(
                    "governance_proposal_info",
                    json!({"hash": shorter.hash.to_string()}),
                )
                .await
                .unwrap();
            assert_eq!(view["phase_deadline"], base + 75);
            clock.store(base + 70, Ordering::SeqCst);
            for (index, choice) in [
                (1, burst_transactions::governance::GovernanceVote::Yea),
                (2, burst_transactions::governance::GovernanceVote::Nay),
            ] {
                let tx = Transaction::GovernanceVote(
                    build_governance_vote_tx(
                        &addresses[index],
                        overlapping.hash.into_tx_hash(),
                        choice,
                        Timestamp::new(base + 70),
                    )
                    .unwrap(),
                );
                submit(
                    &clients,
                    &signed_block(&clients[0], &keys[index], tx, &governed).await,
                )
                .await;
            }
            for (second, height, cutoff) in [(78, 4, 76), (83, 5, 81), (94, 6, 92)] {
                clock.store(base + second, Ordering::SeqCst);
                epoch = installed(&stores, height, &params).await;
                for store in &stores {
                    assert_eq!(
                        chain::load(store, NetworkId::Dev, &params).unwrap()[height - 1]
                            .certificate()
                            .checkpoint
                            .cutoff,
                        base + cutoff
                    );
                }
            }
            governed.brn_rate = 3 * BRN_UNIT;
            let tx = Transaction::Burn(
                build_burn_tx(&addresses[1], &addresses[0], 1, Timestamp::new(base + 94)).unwrap(),
            );
            payment = signed_block(&clients[0], &keys[1], tx, &governed).await;
            submit(&clients, &payment).await;
            extra_proposals.push((overlapping.hash, "Activated"));
            extra_proposals.push((shorter.hash, "Rejected"));
        }
        if traffic == Scenario::Conflicts {
            (payment, epoch, extra_proposals) =
                conflicts::activate(&clients, &stores, &clock, &keys, (&params, &governed), base)
                    .await;
        }
        if matches!(
            traffic,
            Scenario::KeyBallots | Scenario::PartitionedKeyBallots
        ) {
            (payment, epoch, extra_proposals) = key_ballots::activate(
                &clients,
                &stores,
                &clock,
                &keys,
                (&params, &mut governed),
                base,
                (
                    forks::Network {
                        registries: &registries,
                        signers: &signers,
                        elections: &elections,
                    },
                    traffic == Scenario::PartitionedKeyBallots,
                ),
            )
            .await;
        }
        (proposal.hash, payment, epoch, governed, extra_proposals)
    });
    let result = tokio::time::timeout(
        Duration::from_secs(
            if matches!(
                traffic,
                Scenario::Conflicts | Scenario::KeyBallots | Scenario::PartitionedKeyBallots
            ) {
                180
            } else {
                150
            },
        ),
        &mut journey,
    )
    .await;
    if result.is_err() {
        journey.abort();
        let _ = journey.await;
    }
    stop(&mut workers).await;
    let (proposal, payment, epoch, governed, extra_proposals) =
        result.expect("governance journey timed out").unwrap();
    if traffic == Scenario::DelayedRecovery {
        let bytes = stores[0].block_store().get_block(&proposal).unwrap();
        let proposed: StateBlock = bincode::deserialize(&bytes).unwrap();
        let recovery_at =
            payment.timestamp.as_secs() + crate::block_processor::PAST_TIMESTAMP_TOLERANCE_SECS + 1;
        assert!(recovery_at > clock.load(Ordering::SeqCst));
        assert!(
            recovery_at - proposed.timestamp.as_secs()
                > crate::block_processor::PAST_TIMESTAMP_TOLERANCE_SECS
        );
        // Both certified history and the final-voted post-checkpoint payment
        // are older than the live window when the fresh observer joins.
        clock.store(recovery_at, Ordering::SeqCst);
    }
    drop(stores);

    let mut observer = configs[0].clone();
    observer.data_dir = directory.path().join("fresh-observer");
    observer.port = port();
    observer.rpc_port = port();
    observer.enable_representative = false;
    observer.representative_key_file = None;
    observer.bootstrap_peers = ports
        .iter()
        .map(|port| format!("127.0.0.1:{port}"))
        .collect();
    let observer_restart = observer.clone();
    configs.push(observer);
    let delayed_sources: Vec<_> = if traffic == Scenario::DelayedRecovery {
        configs.drain(1..3).collect()
    } else {
        Vec::new()
    };
    let mut clients = Vec::new();
    let mut stores = Vec::new();
    let mut recovery_registries = Vec::new();
    for config in configs {
        clients.push(NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap());
        let mut node = fixture_node(config, &clock, "http://127.0.0.1:1").await;
        stores.push(node.store.clone());
        recovery_registries.push(node.connection_registry.clone());
        workers.push((
            node.shutdown.clone(),
            tokio::spawn(async move {
                node.start().await.unwrap();
                node.stop().await.unwrap();
            }),
        ));
    }
    if traffic == Scenario::DelayedRecovery {
        for attempt in 0..2 {
            assert_eq!(installed(&stores, 3, &params).await, epoch);
            let observer = stores.last().unwrap();
            let proof = recovery_registries
                .last()
                .unwrap()
                .read()
                .await
                .historical_finality
                .clone();
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    assert!(!observer.block_store().exists(&payment.hash).unwrap());
                    if proof.collected_weight(&payment.hash) == Some(1) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .expect("observer never received the available representative's final vote");
            let until = tokio::time::Instant::now() + Duration::from_secs(3);
            while tokio::time::Instant::now() < until {
                assert!(!proof.contains(&payment.hash));
                assert!(!observer.block_store().exists(&payment.hash).unwrap());
                assert_eq!(
                    observer
                        .account_store()
                        .get_account(&payment.account)
                        .unwrap()
                        .head,
                    payment.previous
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            if attempt == 0 {
                stop(&mut vec![workers.pop().unwrap()]).await;
                drop(proof);
                stores.pop();
                recovery_registries.pop();
                let mut node =
                    fixture_node(observer_restart.clone(), &clock, "http://127.0.0.1:1").await;
                let restarted_proof = node
                    .connection_registry
                    .read()
                    .await
                    .historical_finality
                    .clone();
                assert_eq!(restarted_proof.collected_weight(&payment.hash), None);
                assert!(!restarted_proof.contains(&payment.hash));
                assert!(!node.store.block_store().exists(&payment.hash).unwrap());
                stores.push(node.store.clone());
                recovery_registries.push(node.connection_registry.clone());
                workers.push((
                    node.shutdown.clone(),
                    tokio::spawn(async move {
                        node.start().await.unwrap();
                        node.stop().await.unwrap();
                    }),
                ));
            }
        }
        eprintln!("old payment remained unapplied with one human final vote; restoring the other representatives");
        for config in delayed_sources {
            clients.push(NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap());
            let mut node = fixture_node(config, &clock, "http://127.0.0.1:1").await;
            stores.push(node.store.clone());
            recovery_registries.push(node.connection_registry.clone());
            workers.push((
                node.shutdown.clone(),
                tokio::spawn(async move {
                    node.start().await.unwrap();
                    node.stop().await.unwrap();
                }),
            ));
        }
    }
    let mut recovery = tokio::spawn(async move {
        assert_eq!(
            installed(
                &stores,
                if terminal {
                    2
                } else if traffic == Scenario::Timing {
                    6
                } else if matches!(
                    traffic,
                    Scenario::Conflicts | Scenario::KeyBallots | Scenario::PartitionedKeyBallots
                ) {
                    5
                } else {
                    3
                },
                &params
            )
            .await,
            epoch
        );
        for (index, client) in clients.iter().enumerate() {
            if traffic == Scenario::VerificationFork {
                let snapshot = burst_verification::OrchestratorSnapshot::decode(
                    &stores[index]
                        .meta_store()
                        .get_meta(crate::endorsement_commit::SNAPSHOT_KEY)
                        .unwrap(),
                )
                .unwrap();
                let verification = burst_verification::VerificationOrchestrator::restore(snapshot);
                let target =
                    burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[9; 32]).public);
                assert_eq!(
                    verification.get_state(&target).unwrap().endorsements.len(),
                    1
                );
            }
            let confirmed = client
                .wait_confirmed(&payment.hash.to_string(), Duration::from_secs(30))
                .await;
            if let Err(error) = confirmed {
                for (peer, client) in clients.iter().enumerate() {
                    eprintln!(
                        "recovery node {peer} peers {:?}; payment {:?}",
                        client.rpc_call("peers", json!({})).await,
                        client
                            .rpc_call("block_info", json!({"hash":payment.hash.to_string()}))
                            .await
                    );
                }
                for (peer, registry) in recovery_registries.iter().enumerate() {
                    let proof = registry.read().await.historical_finality.clone();
                    eprintln!(
                        "recovery node {peer} historical weight {:?}, authorized {}",
                        proof.collected_weight(&payment.hash),
                        proof.contains(&payment.hash)
                    );
                }
                panic!("payment did not confirm on recovery node {index}: {error}");
            }
            let view = client
                .rpc_call(
                    "governance_proposal_info",
                    json!({"hash":proposal.to_string()}),
                )
                .await
                .unwrap();
            assert_eq!(
                view["phase"],
                match traffic {
                    Scenario::Rejected => "Rejected",
                    Scenario::Withdrawn => "Withdrawn",
                    _ => "Activated",
                }
            );
            assert_eq!(view["votes_yea"], if terminal { 0 } else { 2 });
            assert_eq!(view["votes_nay"], if terminal { 0 } else { 1 });
            let current = client.rpc_call("params_hash", json!({})).await.unwrap();
            assert_eq!(current["params_hash"], governed.params_hash().to_string());
            for (id, phase) in &extra_proposals {
                let view = client
                    .rpc_call("governance_proposal_info", json!({"hash": id.to_string()}))
                    .await
                    .unwrap();
                assert_eq!(view["phase"], *phase);
                if *phase == "Conflicted" {
                    assert_eq!(
                        view["activation_conflict"],
                        json!({"ArticleAlreadyRepealed": 1})
                    );
                    assert_eq!(view["votes_yea"], 3);
                }
            }
            if traffic == Scenario::Conflicts {
                conflicts::check_document(client).await;
            }
        }
        if extend_expiry {
            for store in &stores {
                let account = store.account_store().get_account(&payment.account).unwrap();
                assert_eq!(account.trst_balance, 8);
                assert_eq!(account.expired_trst, 0);
                assert!(store
                    .pending_store()
                    .get_pending(&payment.account, &payment.link.into_tx_hash())
                    .is_err());
            }
        }
        if matches!(traffic, Scenario::Keys | Scenario::PartitionedKeys) {
            keys::check(&stores, &params, proposal);
        }
        if matches!(
            traffic,
            Scenario::KeyBallots | Scenario::PartitionedKeyBallots
        ) {
            key_ballots::check(&clients, &extra_proposals).await;
            key_ballots::check_records(&stores, &params, &extra_proposals);
        }
    });
    let recovered = tokio::time::timeout(Duration::from_secs(75), &mut recovery).await;
    if recovered.is_err() {
        recovery.abort();
        let _ = recovery.await;
    }
    stop(&mut workers).await;
    recovered
        .expect("certified governance did not recover on the fresh node")
        .unwrap();
}
