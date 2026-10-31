use crate::test_rpc::state;
use crate::{BurstNode, NodeConfig};
use burst_governance::engine::GovernanceRecord;
use burst_governance::{
    GovernableParam, GovernanceEngine, GovernancePhase, Proposal, ProposalContent,
};
use burst_rpc::RpcServer;
use burst_store::governance::GovernanceStore;
use burst_store::MetaStore;
use burst_types::{NetworkId, Timestamp, TxHash, WalletAddress};
use burst_wallet_core::NodeClient;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

fn proposal(n: u8) -> Proposal {
    Proposal {
        hash: TxHash::new([n; 32]),
        proposer: WalletAddress::new("brst_proposer"),
        content: ProposalContent::ParameterChange {
            param: GovernableParam::BrnRate,
            new_value: 1000,
        },
        phase: GovernancePhase::Proposal,
        created_at: Timestamp::new(100),
        endorsement_count: 0,
        exploration_votes_yea: 0,
        exploration_votes_nay: 0,
        exploration_votes_abstain: 0,
        promotion_votes_yea: 0,
        promotion_votes_nay: 0,
        promotion_votes_abstain: 0,
        exploration_started_at: None,
        cooldown_started_at: None,
        promotion_started_at: None,
        activation_at: None,
        total_eligible_voters: 1,
        round: 0,
    }
}

async fn install(node: &BurstNode, ids: &[u8]) {
    install_proposals(node, ids.iter().map(|n| proposal(*n)).collect()).await;
}

async fn install_proposals(node: &BurstNode, proposals: Vec<Proposal>) {
    for proposal in &proposals {
        node.store
            .governance_store()
            .put_proposal(&proposal.hash, &bincode::serialize(proposal).unwrap())
            .unwrap();
    }
    let engine = GovernanceEngine::from_records(
        proposals
            .into_iter()
            .flat_map(|p| {
                let timing =
                    burst_governance::GovernanceTiming::new(&node.config.params, &p.content);
                let hash = p.hash;
                [
                    Ok(GovernanceRecord::Proposal(Box::new(p))),
                    Ok(GovernanceRecord::Timing {
                        proposal: hash,
                        timing,
                    }),
                ]
            })
            .chain(std::iter::once(Ok(GovernanceRecord::PendingEffects {
                changes: vec![],
                amendments: vec![],
                evictions: vec![],
            }))),
    )
    .unwrap();
    let mut batch = node.store.tx_begin_write().unwrap();
    if batch.uses_governance_records().unwrap() {
        batch
            .replace_checkpoint_governance_records(&engine)
            .unwrap();
    } else {
        batch
            .put_brn_meta(b"governance_engine", &bincode::serialize(&engine).unwrap())
            .unwrap();
    }
    batch
        .put_meta(
            burst_governance::DelegationEngine::meta_key(),
            &burst_governance::DelegationEngine::default().save_state(),
        )
        .unwrap();
    batch
        .put_meta(
            "consti_engine",
            &bincode::serialize(&burst_consti::ConstiEngine::bootstrap()).unwrap(),
        )
        .unwrap();
    batch.commit().unwrap();
    *node.governance.lock().await = engine;
}

fn hashes(page: &Value) -> Vec<String> {
    page["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["hash"].as_str().unwrap().to_string())
        .collect()
}
fn expected(ids: &[u8]) -> Vec<String> {
    ids.iter()
        .map(|n| TxHash::new([*n; 32]).to_string())
        .collect()
}

#[tokio::test]
async fn governance_keyset_http_pages_match_live_and_lmdb_and_accept_legacy_cursors() {
    for live in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let node = BurstNode::new(NodeConfig {
            network: NetworkId::Dev,
            data_dir: directory.path().to_path_buf(),
            port: 0,
            enable_rpc: false,
            enable_upnp: false,
            enable_representative: false,
            bootstrap_peers: vec![],
            nts_sources: vec![],
            ..NodeConfig::default()
        })
        .await
        .unwrap();
        install(&node, &[2, 4, 6, 8]).await;
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let clock = Arc::new(std::sync::atomic::AtomicU64::new(101));
        let mut rpc_state = state(&node, live);
        let display_clock = clock.clone();
        rpc_state.display_now = Arc::new(move || {
            Timestamp::new(display_clock.load(std::sync::atomic::Ordering::SeqCst))
        });
        let rpc = RpcServer::new(port, Arc::new(rpc_state));
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            rpc.start(async {
                let _ = stopped.await;
            })
            .await
        });
        let client = NodeClient::new(format!("http://127.0.0.1:{port}")).unwrap();
        let outcome = async {
            // Live HTTP projection must not wait for engine publication locks.
            let held = if live { Some((node.governance.lock().await,
                node.delegation_engine.lock().await, node.consti_engine.lock().await)) } else { None };
            let first = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    assert!(!task.is_finished(), "RPC exited before readiness");
                    if let Ok(page) = client.rpc_call("governance_proposals", json!({"count":2})).await { break page; }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }).await.expect("RPC readiness");
            assert_eq!(hashes(&first), expected(&[2, 4]));
            let stats = client.rpc_call("merger_graph_stats", json!({})).await.unwrap();
            assert!(stats["total_merges"].is_u64());
            assert_eq!(client.rpc_call("merger_graph_stats", json!({})).await.unwrap(), stats);
            let activity_before = client.rpc_call("verification_activity", json!({})).await.unwrap();
            let activity_address = WalletAddress::new("brst_http_activity");
            let mut account = burst_store::account::AccountInfo::unopened(activity_address.clone());
            account.state = burst_types::WalletState::Verified;
            account.verified_at = Some(Timestamp::new(u64::MAX - 1));
            account.verifier_opted_in_at = Some(Timestamp::new(100));
            let mut batch = node.store.tx_begin_write().unwrap();
            batch.put_account_info(&account, false).unwrap();
            batch.commit().unwrap();
            node.verification_orchestrator.lock().await.initialize_genesis(&activity_address);
            let activity = client.rpc_call("verification_activity", json!({})).await.unwrap();
            assert_eq!(activity["total_verified"].as_u64().unwrap(),
                activity_before["total_verified"].as_u64().unwrap() + 1);
            assert_eq!(activity["verifier_pool_size"].as_u64().unwrap(),
                activity_before["verifier_pool_size"].as_u64().unwrap() + 1);
            assert_eq!(activity["recent_verifications"][0], json!({
                "account":activity_address.as_str(), "result":"verified",
                "timestamp":u64::MAX - 1, "verifier_count":0
            }));
            for field in ["active_verifications", "pending_endorsements", "active_challenges",
                "total_revoked", "recent_endorsements", "recent_challenges"] {
                assert_eq!(activity[field], activity_before[field], "activity field {field}");
            }

            let info = tokio::time::timeout(Duration::from_secs(10), client.rpc_call(
                "governance_proposal_info", json!({"hash": TxHash::new([2;32]).to_string()}),
            )).await.expect("proposal info must not wait for live engine locks").unwrap();
            assert_eq!(info["hash"], TxHash::new([2;32]).to_string());
            drop(held);
            let repeated = client.rpc_call("governance_proposal_info",
                json!({"hash": TxHash::new([2;32]).to_string()})).await.unwrap();
            assert_eq!(repeated, info);
            if live {
                let end = 100 + node.config.params.governance_proposal_duration_secs;
                for (now, phase) in [(end - 1, "Proposal"), (end, "Rejected"),
                    (end + 1, "Rejected"), (101, "Proposal")] {
                    clock.store(now, std::sync::atomic::Ordering::SeqCst);
                    let at_boundary = client.rpc_call("governance_proposal_info",
                        json!({"hash": TxHash::new([2;32]).to_string()})).await.unwrap();
                    assert_eq!(at_boundary["phase"], phase, "display boundary at {now}");
                }
            }
            let changed = [2, 4, 6, 8].into_iter().map(|id| {
                let mut p = proposal(id);
                if id == 2 { p.phase = GovernancePhase::Withdrawn; }
                p
            }).collect();
            install_proposals(&node, changed).await;
            let withdrawn = client.rpc_call("governance_proposal_info",
                json!({"hash": TxHash::new([2;32]).to_string()})).await.unwrap();
            assert_eq!(withdrawn["phase"], "Withdrawn", "same hash and time must see a new committed generation");
            if live {
                use burst_rpc::governance_snapshot::{Reader, Selection};
                let reader = crate::governance_read::GovernanceReader {
                    store: node.store.clone(), fallback_params: node.config.params.clone(),
                };
                let captured = reader.capture(Selection::Info(TxHash::new([2;32])), Timestamp::new(101)).unwrap();
                node.store.meta_store().put_meta("consti_engine", b"invalid").unwrap();
                node.store.meta_store().put_meta(burst_governance::DelegationEngine::meta_key(), b"invalid").unwrap();
                let missing = reader.capture(Selection::Info(TxHash::new([255;32])), Timestamp::new(101)).unwrap();
                assert!(missing.selected.is_empty() && missing.verified.is_empty());
                let empty = reader.capture(Selection::Page { after: Some(TxHash::new([255;32])), offset: 0, count: 1 }, Timestamp::new(101)).unwrap();
                assert!(empty.selected.is_empty() && empty.verified.is_empty());
                assert!(reader.capture(Selection::Info(TxHash::new([2;32])), Timestamp::new(101)).is_err());
                assert_eq!(captured.selected, vec![TxHash::new([2;32])]);
                assert!(captured.governance.get_proposal(&TxHash::new([2;32])).is_some());
                // The next install restores valid snapshots before more requests.
            }
            let boundary = first["cursor"].as_str().unwrap();
            assert_eq!(burst_rpc::pagination::decode_proposal_cursor(Some(boundary)).unwrap().0, Some(TxHash::new([4;32])));
            install(&node, &[1, 2, 4, 5, 6, 8]).await;
            if live {
                // Restore a state where the last-seen key has been removed.
                install(&node, &[1, 5, 6, 8]).await;
            }
            let next = client.rpc_call("governance_proposals", json!({"count":2,"cursor":boundary})).await.unwrap();
            assert_eq!(hashes(&next), expected(&[5,6]));
            let last = client.rpc_call("governance_proposals", json!({"count":2,"cursor":next["cursor"]})).await.unwrap();
            assert_eq!(hashes(&last), expected(&[8]));
            assert!(last.get("cursor").is_none());
            let legacy = client.rpc_call("governance_proposals", json!({"count":2,"cursor":burst_rpc::pagination::encode_cursor(2)})).await.unwrap();
            assert_eq!(hashes(&legacy), expected(if live { &[6,8] } else { &[4,5] }));
            if live { assert!(legacy.get("cursor").is_none()); }
            else { assert_eq!(burst_rpc::pagination::decode_proposal_cursor(legacy["cursor"].as_str()).unwrap().0, Some(TxHash::new([5;32]))); }
            let empty = client.rpc_call("governance_proposals", json!({"cursor":burst_rpc::pagination::encode_proposal_cursor(&TxHash::new([255;32]))})).await.unwrap();
            assert!(hashes(&empty).is_empty());
            assert!(empty.get("cursor").is_none());
            assert!(client.rpc_call("governance_proposals", json!({"cursor":"Z292MTpzaG9ydA=="})).await.is_err());
            let clamped = client.rpc_call("governance_proposals", json!({"count":0})).await.unwrap();
            assert_eq!(hashes(&clamped), expected(&[1]));
        }.await;
        let _ = stop.send(());
        tokio::time::timeout(Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        outcome
    }
}

#[test]
fn pinned_due_projection_reuses_delegation_and_keeps_captured_inputs() {
    use burst_rpc::governance_snapshot::{Reader, Selection};
    use burst_store::account::AccountInfo;
    use burst_types::{ProtocolParams, WalletState};
    let directory = tempfile::tempdir().unwrap();
    let store =
        Arc::new(burst_store_lmdb::LmdbStore::open(directory.path(), 32, 16 << 20).unwrap());
    let params = ProtocolParams::default();
    let mut p = proposal(2);
    p.phase = GovernancePhase::Exploration;
    p.exploration_started_at = Some(Timestamp::new(100));
    let hash = p.hash;
    let voter = WalletAddress::new("brst_snapshot_voter");
    let owner = WalletAddress::new("brst_snapshot_owner");
    let mut delegation = burst_governance::DelegationEngine::default();
    delegation.delegate(&owner, &voter).unwrap();
    let governance = GovernanceEngine::from_records([
        Ok(GovernanceRecord::Proposal(Box::new(p))),
        Ok(GovernanceRecord::Timing {
            proposal: hash,
            timing: burst_governance::GovernanceTiming {
                proposal_secs: 1,
                voting_secs: 1,
                buffer_secs: 1,
            },
        }),
        Ok(GovernanceRecord::VoteBucket {
            proposal: hash,
            promotion: false,
        }),
        Ok(GovernanceRecord::Vote {
            proposal: hash,
            promotion: false,
            voter: voter.clone(),
            vote: burst_transactions::governance::GovernanceVote::Yea,
        }),
        Ok(GovernanceRecord::PendingEffects {
            changes: vec![],
            amendments: vec![],
            evictions: vec![],
        }),
    ])
    .unwrap();
    let mut batch = store.tx_begin_write().unwrap();
    batch.initialize_governance_records(&governance).unwrap();
    for address in [&voter, &owner] {
        let mut account = AccountInfo::unopened(address.clone());
        account.state = WalletState::Verified;
        batch.put_account_info(&account, false).unwrap();
    }
    batch
        .put_meta(
            burst_governance::DelegationEngine::meta_key(),
            &delegation.save_state(),
        )
        .unwrap();
    batch
        .put_meta(
            "consti_engine",
            &bincode::serialize(&burst_consti::ConstiEngine::bootstrap()).unwrap(),
        )
        .unwrap();
    batch
        .put_brn_meta(b"protocol_params", &bincode::serialize(&params).unwrap())
        .unwrap();
    batch.commit().unwrap();
    let reader = crate::governance_read::GovernanceReader {
        store: store.clone(),
        fallback_params: params.clone(),
    };
    let now = Timestamp::new(102);
    let captured = reader.capture(Selection::Info(hash), now).unwrap();
    assert_eq!(captured.verified.len(), 2);
    let repeated = reader.capture(Selection::Info(hash), now).unwrap();
    assert!(captured
        .delegation
        .shares_storage_with(&repeated.delegation));
    assert!(Arc::ptr_eq(&captured.verified, &repeated.verified));
    let project = |snapshot: &burst_rpc::governance_snapshot::Snapshot| {
        snapshot.governance.project_proposal_at(
            snapshot.governance.get_proposal(&hash).unwrap(),
            snapshot.now,
            &snapshot.params,
            &snapshot.delegation,
            &snapshot.verified,
        )
    };
    let expected = project(&captured);
    assert_eq!(expected.exploration_votes_yea, 2);
    assert!(captured.projection_identity.is_some());
    let mut batch = store.tx_begin_write().unwrap();
    let mut account = batch.get_account(&owner).unwrap();
    account.trst_balance += 1;
    batch.put_account_info(&account, false).unwrap();
    batch.commit().unwrap();
    let after_payment = reader.capture(Selection::Info(hash), now).unwrap();
    assert_ne!(captured.generation, after_payment.generation);
    assert_eq!(
        captured.projection_identity,
        after_payment.projection_identity
    );
    assert_eq!(project(&after_payment), expected);

    let mut changed_params = params.clone();
    changed_params.brn_rate += 1;
    let mut batch = store.tx_begin_write().unwrap();
    for address in [&voter, &owner] {
        let mut account = batch.get_account(address).unwrap();
        account.state = WalletState::Unverified;
        batch.put_account_info(&account, false).unwrap();
    }
    batch
        .put_meta(
            burst_governance::DelegationEngine::meta_key(),
            &burst_governance::DelegationEngine::new(3).save_state(),
        )
        .unwrap();
    batch
        .put_brn_meta(
            b"protocol_params",
            &bincode::serialize(&changed_params).unwrap(),
        )
        .unwrap();
    batch.commit().unwrap();
    assert_eq!(project(&captured), expected);
    let next = reader.capture(Selection::Info(hash), now).unwrap();
    assert!(next.verified.is_empty());
    assert_ne!(captured.projection_identity, next.projection_identity);
    assert_eq!(next.params.brn_rate, changed_params.brn_rate);
    assert_eq!(captured.params.brn_rate, params.brn_rate);
    assert_eq!(next.delegation.max_depth(), 3);
    assert_eq!(project(&next).exploration_votes_yea, 0);
    let before_deadline = reader
        .capture(Selection::Info(hash), Timestamp::new(100))
        .unwrap();
    assert!(before_deadline.verified.is_empty());
    assert_eq!(
        project(&before_deadline).phase,
        GovernancePhase::Exploration
    );
}

/// Resource diagnostic for the existing production Reader/RPC wiring. The
/// fixture is derived query data, not a signed ledger or a release benchmark.
#[tokio::test]
#[ignore = "explicit capped-host repeated governance HTTP resource diagnostic"]
async fn repeated_governance_http_query_resources() {
    use burst_store::account::AccountInfo;
    use burst_types::WalletState;
    use std::time::Instant;
    let directory = tempfile::tempdir().unwrap();
    let node = BurstNode::new(NodeConfig {
        network: NetworkId::Dev,
        data_dir: directory.path().to_path_buf(),
        port: 0,
        enable_rpc: false,
        enable_upnp: false,
        enable_representative: false,
        bootstrap_peers: vec![],
        nts_sources: vec![],
        ..NodeConfig::default()
    })
    .await
    .unwrap();
    let mut proposals = Vec::new();
    for n in 1u32..=192 {
        let mut p = proposal(1);
        let mut hash = [0; 32];
        hash[..4].copy_from_slice(&n.to_be_bytes());
        p.hash = TxHash::new(hash);
        p.phase = GovernancePhase::Exploration;
        p.exploration_started_at = Some(Timestamp::new(100));
        proposals.push(p);
    }
    let hashes: Vec<_> = proposals.iter().map(|p| p.hash.to_string()).collect();
    install_proposals(&node, proposals).await;
    let mut batch = node.store.tx_begin_write().unwrap();
    for n in 0..10_000 {
        let mut account = AccountInfo::unopened(WalletAddress::new(format!("brst_query_{n:05}")));
        account.state = WalletState::Verified;
        batch.put_account_info(&account, false).unwrap();
    }
    batch.commit().unwrap();
    let now = 100
        + node.config.params.governance_exploration_duration_secs
        + node.config.params.governance_propagation_buffer_secs;
    let mut rpc_state = state(&node, true);
    rpc_state.display_now = Arc::new(move || Timestamp::new(now));
    // Isolate the shared read worker/cache from the independent IP throttle.
    rpc_state.rate_limiter = Arc::new(burst_rpc::RateLimiter::new(u32::MAX));
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let rpc = RpcServer::new(port, Arc::new(rpc_state));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        rpc.start(async {
            let _ = stopped.await;
        })
        .await
    });
    let client = NodeClient::new(format!("http://127.0.0.1:{port}")).unwrap();
    let memory = |stage: &str| {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let fields: Vec<_> = status
            .lines()
            .filter(|line| {
                line.starts_with("VmRSS:")
                    || line.starts_with("VmHWM:")
                    || line.starts_with("RssAnon:")
            })
            .collect();
        eprintln!(
            "QUERY_LOAD stage={stage} proposals=192 verified_fixture=10000 {}",
            fields.join(" ")
        );
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(!task.is_finished(), "RPC exited before readiness");
            if client
                .rpc_call("merger_graph_stats", json!({}))
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("RPC readiness");
    memory("before_queries");
    let mut initial_identity = None;
    let mut initial_verified = 0;
    for cycle in 0..3 {
        // One unrelated commit must retain projection identity; one eligibility
        // change must invalidate it. Repeated pages exceed the 128-entry cache.
        if cycle > 0 {
            let mut batch = node.store.tx_begin_write().unwrap();
            if cycle == 1 {
                batch
                    .put_meta("query-fixture-unrelated", b"changed")
                    .unwrap();
            } else {
                let address = WalletAddress::new("brst_query_00000");
                let mut account = batch.get_account(&address).unwrap();
                account.state = WalletState::Unverified;
                batch.put_account_info(&account, false).unwrap();
            }
            batch.commit().unwrap();
        }
        let start = Instant::now();
        let mut cursor = None;
        let mut seen = Vec::new();
        let mut page_latencies = Vec::new();
        loop {
            let request = Instant::now();
            let page = client
                .rpc_call("governance_proposals", json!({"count":16,"cursor":cursor}))
                .await
                .unwrap();
            page_latencies.push(request.elapsed().as_micros());
            for p in page["proposals"].as_array().unwrap() {
                assert_eq!(p["phase"], "Rejected");
                seen.push(p["hash"].as_str().unwrap().to_string());
            }
            cursor = page
                .get("cursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(seen, hashes);
        page_latencies.sort_unstable();
        eprintln!(
            "QUERY_LOAD cycle={cycle} pages={} elapsed_ms={} page_median_us={} page_max_us={}",
            page_latencies.len(),
            start.elapsed().as_millis(),
            page_latencies[page_latencies.len() / 2],
            page_latencies.last().unwrap()
        );
        let start = Instant::now();
        let expected = client
            .rpc_call("governance_proposal_info", json!({"hash":hashes[191]}))
            .await
            .unwrap();
        for _ in 0..128 {
            assert_eq!(
                client
                    .rpc_call("governance_proposal_info", json!({"hash":hashes[191]}))
                    .await
                    .unwrap(),
                expected
            );
        }
        eprintln!(
            "QUERY_LOAD cycle={cycle} repeated_info=128 elapsed_ms={}",
            start.elapsed().as_millis()
        );
        memory(&format!("after_cycle_{cycle}"));
        use burst_rpc::governance_snapshot::{Reader, Selection};
        let reader = crate::governance_read::GovernanceReader {
            store: node.store.clone(),
            fallback_params: node.config.params.clone(),
        };
        let snapshot = reader
            .capture(
                Selection::Info(TxHash::new({
                    let mut hash = [0; 32];
                    hash[..4].copy_from_slice(&192u32.to_be_bytes());
                    hash
                })),
                Timestamp::new(now),
            )
            .unwrap();
        assert!(snapshot.projection_identity.is_some());
        if cycle == 0 {
            initial_identity = snapshot.projection_identity;
            initial_verified = snapshot.verified.len();
            assert!(initial_verified >= 10_000);
        } else if cycle == 1 {
            assert_eq!(snapshot.projection_identity, initial_identity);
            assert_eq!(snapshot.verified.len(), initial_verified);
        } else {
            assert_ne!(snapshot.projection_identity, initial_identity);
            assert_eq!(snapshot.verified.len(), initial_verified - 1);
        }
    }
    // Invalidate the electorate once, then contend for the same nonqueued worker
    // through real HTTP. Rejections must be explicit, and capacity must recover.
    let mut batch = node.store.tx_begin_write().unwrap();
    let mut account = batch
        .get_account(&WalletAddress::new("brst_query_00001"))
        .unwrap();
    account.state = WalletState::Unverified;
    batch.put_account_info(&account, false).unwrap();
    batch.commit().unwrap();
    let client = Arc::new(client);
    let barrier = Arc::new(tokio::sync::Barrier::new(65));
    let mut requests = tokio::task::JoinSet::new();
    for index in 0..64 {
        let client = client.clone();
        let barrier = barrier.clone();
        requests.spawn(async move {
            barrier.wait().await;
            let started = Instant::now();
            let kind = index % 3;
            let result = match kind {
                0 => {
                    client
                        .rpc_call("governance_proposals", json!({"count":16}))
                        .await
                }
                1 => client.rpc_call("merger_graph_stats", json!({})).await,
                _ => client.rpc_call("verification_activity", json!({})).await,
            };
            (kind, result, started.elapsed().as_micros())
        });
    }
    let started = Instant::now();
    barrier.wait().await;
    let mut accepted = [0usize; 3];
    let mut rejected = [0usize; 3];
    let mut latencies = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(result) = requests.join_next().await {
            let (kind, result, elapsed) = result.unwrap();
            latencies.push(elapsed);
            match result {
                Ok(value) => {
                    accepted[kind] += 1;
                    match kind {
                        0 => {
                            let proposals = value["proposals"].as_array().unwrap();
                            assert_eq!(proposals.len(), 16);
                            for proposal in proposals {
                                assert_eq!(proposal["phase"], "Rejected");
                            }
                        }
                        1 => assert!(value["total_merges"].is_u64()),
                        _ => assert_eq!(
                            value["total_verified"].as_u64().unwrap(),
                            initial_verified as u64 - 2
                        ),
                    }
                }
                Err(burst_wallet_core::WalletError::Node(message))
                    if message == "node error: rate limited" =>
                {
                    rejected[kind] += 1;
                }
                Err(error) => panic!("unexpected concurrent query failure: {error}"),
            }
        }
    })
    .await
    .expect("bounded concurrent query burst");
    assert_eq!(
        accepted.iter().sum::<usize>() + rejected.iter().sum::<usize>(),
        64
    );
    assert!(accepted.iter().sum::<usize>() > 0);
    latencies.sort_unstable();
    eprintln!("QUERY_BURST requests=64 accepted={accepted:?} rejected={rejected:?} elapsed_ms={} median_us={} max_us={}",
        started.elapsed().as_millis(), latencies[latencies.len()/2], latencies.last().unwrap());
    memory("after_concurrent_burst");
    let recovered = client
        .rpc_call("governance_proposals", json!({"count":16}))
        .await
        .unwrap();
    assert_eq!(recovered["proposals"].as_array().unwrap().len(), 16);
    assert!(client
        .rpc_call("merger_graph_stats", json!({}))
        .await
        .unwrap()["total_merges"]
        .is_u64());
    assert_eq!(
        client
            .rpc_call("verification_activity", json!({}))
            .await
            .unwrap()["total_verified"]
            .as_u64()
            .unwrap(),
        initial_verified as u64 - 2
    );
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn telemetry_http_uses_bounded_worker_and_recovers_after_contention() {
    let directory = tempfile::tempdir().unwrap();
    let node = BurstNode::new(NodeConfig {
        network: NetworkId::Dev,
        data_dir: directory.path().to_path_buf(),
        port: 0,
        enable_rpc: false,
        enable_upnp: false,
        enable_representative: false,
        bootstrap_peers: vec![],
        nts_sources: vec![],
        ..NodeConfig::default()
    })
    .await
    .unwrap();
    let verified_address =
        burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[77; 32]).public);
    let mut batch = node.store.tx_begin_write().unwrap();
    for (name, status) in [
        (
            verified_address.as_str(),
            burst_types::WalletState::Verified,
        ),
        (
            "brst_telemetry_unverified",
            burst_types::WalletState::Unverified,
        ),
    ] {
        let mut account = burst_store::account::AccountInfo::unopened(WalletAddress::new(name));
        account.state = status;
        batch.put_account_info(&account, false).unwrap();
    }
    batch.commit().unwrap();
    // A stored burn is enough for this read-only origin inspection fixture.
    let mut origin = crate::genesis_state::block(
        NetworkId::Dev,
        &burst_types::ProtocolParams::burst_defaults(),
    );
    origin.block_type = burst_ledger::BlockType::Burn;
    origin.hash = origin.compute_hash();
    burst_store::block::BlockStore::put_block(
        &node.store.block_store(),
        &origin.hash,
        &bincode::serialize(&origin).unwrap(),
    )
    .unwrap();
    let origin_params = json!({"origin":origin.hash.to_string()});
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    struct ExclusionCallback;
    impl burst_rpc::BlockProcessorCallback for ExclusionCallback {
        fn process_block(&self, _: &[u8]) -> Result<burst_rpc::ProcessResult, String> {
            Err("read-only fixture".into())
        }
        fn payment_exclusion(
            &self,
            query: burst_types::payment_recovery::ExclusionQuery,
        ) -> Result<burst_types::payment_recovery::ExclusionReview, String> {
            if query.payment_epoch == burst_types::BlockHash::ZERO.to_string() {
                return Err("invalid fixture epoch".into());
            }
            Ok(burst_types::payment_recovery::ExclusionReview {
                account: query.account,
                previous: query.previous,
                payment_epoch: query.payment_epoch,
                installed_epoch: burst_types::BlockHash::new([45; 32]).to_string(),
                excluded: false,
            })
        }
    }
    let mut rpc_state = state(&node, false);
    rpc_state.block_processor = Arc::new(ExclusionCallback);
    rpc_state.rate_limiter = Arc::new(burst_rpc::RateLimiter::new(u32::MAX));
    let rpc_state = Arc::new(rpc_state);
    let rpc = RpcServer::new(port, rpc_state.clone());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        rpc.start(async {
            let _ = stopped.await;
        })
        .await
    });
    let client = NodeClient::new(format!("http://127.0.0.1:{port}")).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(!server.is_finished());
            if client.rpc_call("peers", json!({})).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("RPC readiness");
    let held = node.trst_engine.lock().await;
    let request = burst_rpc::handlers::handle_telemetry(json!({}), &rpc_state);
    tokio::pin!(request);
    // First poll acquires admission synchronously before waiting on the worker.
    // Hold publication so the worker cannot finish until overload is checked.
    tokio::select! {
        biased;
        result = &mut request => panic!("telemetry finished under publication lock: {result:?}"),
        _ = std::future::ready(()) => {}
    }
    let error = client
        .rpc_call("verification_activity", json!({}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("rate limited"), "{error}");
    let exclusion_params = json!({"account":verified_address.as_str(),
        "previous":burst_types::BlockHash::ZERO.to_string(),
        "payment_epoch":burst_types::BlockHash::new([44;32]).to_string()});
    let error = client
        .rpc_call("payment_exclusion", exclusion_params.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("rate limited"), "{error}");
    let error = client.rpc_call("telemetry", json!({})).await.unwrap_err();
    assert!(error.to_string().contains("rate limited"), "{error}");
    let error = client
        .rpc_call("trst_origins", origin_params.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("rate limited"), "{error}");
    let assignment_params = json!({"account":burst_crypto::derive_address(
        &burst_crypto::keypair_from_seed(&[77;32]).public).as_str()});
    let error = client
        .rpc_call("verifier_assignments", assignment_params.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("rate limited"), "{error}");
    for method in [
        "brn_components",
        "trst_portfolio",
        "consti",
        "verification_status",
        "delegation_info",
        "representatives",
        "account_pending",
        "pending",
    ] {
        let error = client
            .rpc_call(method, assignment_params.clone())
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("rate limited"),
            "{method}: {error}"
        );
    }
    let error = client
        .rpc_call(
            "account_pending",
            json!({"account":verified_address.as_str(),
        "hash":origin.hash.to_string()}),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("rate limited"), "{error}");
    assert!(client.rpc_call("peers", json!({})).await.is_ok());
    drop(held);
    tokio::time::timeout(Duration::from_secs(10), request)
        .await
        .unwrap()
        .unwrap();
    let exclusion = client
        .rpc_call("payment_exclusion", exclusion_params.clone())
        .await
        .unwrap();
    assert_eq!(exclusion["excluded"], json!(false));
    assert_eq!(
        exclusion["payment_epoch"],
        exclusion_params["payment_epoch"]
    );
    let mut invalid_exclusion = exclusion_params;
    invalid_exclusion["payment_epoch"] = json!(burst_types::BlockHash::ZERO.to_string());
    let error = client
        .rpc_call("payment_exclusion", invalid_exclusion)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("invalid fixture epoch"),
        "{error}"
    );
    let result = client.rpc_call("telemetry", json!({})).await.unwrap();
    let accounts = rpc_state.account_store.all_accounts().unwrap();
    assert_eq!(result["account_count"], json!(accounts.len()));
    assert_eq!(
        result["verified_count"],
        json!(accounts.iter().filter(|a| a.state.is_verified()).count())
    );
    assert_eq!(
        result["unverified_count"],
        json!(accounts
            .iter()
            .filter(|a| a.state == burst_types::WalletState::Unverified)
            .count())
    );
    assert_eq!(
        result["block_count"],
        json!(rpc_state.block_store.block_count().unwrap())
    );
    for field in [
        "trst_circulating",
        "trst_expired_total",
        "trst_revoked_total",
    ] {
        assert_eq!(result[field], "0");
    }
    assert!(client.rpc_call("telemetry", json!({})).await.is_ok());
    assert!(client
        .rpc_call("verification_activity", json!({}))
        .await
        .is_ok());
    for (method, field, expected) in [
        (
            "brn_components",
            "account",
            json!(verified_address.as_str()),
        ),
        ("trst_portfolio", "total_active", json!("0")),
        ("consti", "version", json!(0)),
        ("verification_status", "status", json!("verified")),
        ("delegation_info", "delegated_from", json!([])),
        ("representatives", "representatives", json!([])),
    ] {
        let result = client
            .rpc_call(method, assignment_params.clone())
            .await
            .unwrap();
        assert_eq!(result[field], expected, "{method}: {result}");
    }
    let origins = client
        .rpc_call("trst_origins", origin_params)
        .await
        .unwrap();
    assert_eq!(origins["complete"], true);
    assert_eq!(origins["origins"].as_array().unwrap().len(), 1);
    assert_eq!(origins["origins"][0]["hash"], origin.hash.to_string());
    assert_eq!(origins["origins"][0]["revoked"], false);
    assert_eq!(origins["origins"][0]["confirmed"], false);
    let assignments = client
        .rpc_call("verifier_assignments", assignment_params)
        .await
        .unwrap();
    assert_eq!(assignments["targets"], json!([]));
    assert!(assignments["cursor"].is_null());
    for (id, amount) in [(1, 9), (2, 15), (3, 9)] {
        burst_store::pending::PendingStore::put_pending(
            &node.store.pending_store(),
            &verified_address,
            &TxHash::new([id; 32]),
            &burst_store::pending::PendingInfo {
                source: verified_address.clone(),
                amount,
                timestamp: Timestamp::new(100),
                provenance: vec![],
                return_credit: true,
            },
        )
        .unwrap();
    }
    let first = client
        .rpc_call(
            "account_pending",
            json!({"account":verified_address.as_str(), "count":1}),
        )
        .await
        .unwrap();
    assert_eq!(
        first["pending"][0]["hash"],
        TxHash::new([2; 32]).to_string()
    );
    assert_eq!(first["pending"][0]["amount"], "15");
    assert_eq!(
        burst_rpc::pagination::decode_pending_cursor(first["cursor"].as_str()).unwrap(),
        (Some((15, TxHash::new([2; 32]))), 0)
    );
    let legacy = client
        .rpc_call(
            "account_pending",
            json!({"account":verified_address.as_str(),
        "count":1, "cursor":burst_rpc::pagination::encode_cursor(1)}),
        )
        .await
        .unwrap();
    assert_eq!(
        legacy["pending"][0]["hash"],
        TxHash::new([1; 32]).to_string()
    );
    burst_store::pending::PendingStore::delete_pending(
        &node.store.pending_store(),
        &verified_address,
        &TxHash::new([2; 32]),
    )
    .unwrap();
    burst_store::pending::PendingStore::put_pending(
        &node.store.pending_store(),
        &verified_address,
        &TxHash::new([5; 32]),
        &burst_store::pending::PendingInfo {
            source: verified_address.clone(),
            amount: 100,
            timestamp: Timestamp::new(100),
            provenance: vec![],
            return_credit: true,
        },
    )
    .unwrap();
    let second = client
        .rpc_call(
            "pending",
            json!({"account":verified_address.as_str(),
        "count":2, "cursor":first["cursor"]}),
        )
        .await
        .unwrap();
    assert_eq!(
        second["pending"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["hash"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            TxHash::new([1; 32]).to_string(),
            TxHash::new([3; 32]).to_string()
        ]
    );
    assert!(second["cursor"].is_null());
    let exact = client
        .rpc_call(
            "account_pending",
            json!({"account":verified_address.as_str(),
        "hash":TxHash::new([1;32]).to_string(), "threshold":"9"}),
        )
        .await
        .unwrap();
    assert_eq!(exact["pending"].as_array().unwrap().len(), 1);
    assert_eq!(exact["pending"][0]["return_credit"], true);
    for (id, threshold) in [(1, "10"), (4, "0")] {
        let empty = client
            .rpc_call(
                "account_pending",
                json!({"account":verified_address.as_str(),
            "hash":TxHash::new([id;32]).to_string(), "threshold":threshold}),
            )
            .await
            .unwrap();
        assert_eq!(empty["pending"], json!([]));
    }
    burst_store::pending::PendingStore::put_pending(
        &node.store.pending_store(),
        &verified_address,
        &TxHash::new([6; 32]),
        &burst_store::pending::PendingInfo {
            source: verified_address.clone(),
            amount: 23,
            timestamp: Timestamp::new(100),
            provenance: vec![],
            return_credit: false,
        },
    )
    .unwrap();
    let portfolio = client
        .rpc_call(
            "trst_portfolio",
            json!({"account":verified_address.as_str()}),
        )
        .await
        .unwrap();
    assert_eq!(portfolio["total_pending"], "23");
    assert_eq!(portfolio["pending_count"], 1);
    burst_store::pending::PendingStore::delete_pending(
        &node.store.pending_store(),
        &verified_address,
        &TxHash::new([6; 32]),
    )
    .unwrap();
    let portfolio = client
        .rpc_call(
            "trst_portfolio",
            json!({"account":verified_address.as_str()}),
        )
        .await
        .unwrap();
    assert_eq!(portfolio["total_pending"], "0");
    assert_eq!(portfolio["pending_count"], 0);
    let representatives: Vec<_> = (0..31)
        .map(|n| WalletAddress::new(format!("brst_page_rep{n:02}")))
        .collect();
    let expected = {
        let mut cache = rpc_state.rep_weight_cache.write().await;
        cache.rebuild_from_accounts(
            representatives
                .iter()
                .enumerate()
                .flat_map(|(n, address)| std::iter::repeat_n((true, address.clone()), n % 4 + 1)),
        );
        cache.set_evicted(std::collections::HashSet::from(
            [representatives[7].clone()],
        ));
        cache
            .all_weights()
            .into_iter()
            .map(|(a, w)| (a.to_string(), w))
            .collect::<std::collections::HashMap<_, _>>()
    };
    let first = client
        .rpc_call("representatives", json!({"count":7}))
        .await
        .unwrap();
    assert_eq!(
        client
            .rpc_call("representatives", json!({"count":7}))
            .await
            .unwrap(),
        first
    );
    let mut page = first;
    let mut seen = std::collections::HashMap::new();
    let mut prior_weight = u128::MAX;
    for _ in 0..5 {
        for row in page["representatives"].as_array().unwrap() {
            let weight: u128 = row["weight"].as_str().unwrap().parse().unwrap();
            assert!(weight <= prior_weight);
            prior_weight = weight;
            assert!(seen
                .insert(row["account"].as_str().unwrap().to_string(), weight)
                .is_none());
            assert_eq!(
                row["delegator_count"],
                json!(weight / burst_consensus::rep_weights::WEIGHT_PER_HUMAN)
            );
        }
        let Some(cursor) = page["cursor"].as_str() else {
            break;
        };
        page = client
            .rpc_call("representatives", json!({"count":7,"cursor":cursor}))
            .await
            .unwrap();
    }
    assert_eq!(
        seen, expected,
        "tied-weight pages must neither duplicate nor skip representatives"
    );
    let _ = stop.send(());
    server.await.unwrap().unwrap();
}
