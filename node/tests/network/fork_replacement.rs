//! Live worker integration. Votes enter at the already-authenticated election
//! boundary; this fixture does not claim to test wire signatures or Byzantine quorum.
use burst_ledger::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
use burst_node::{BurstNode, NodeConfig};
use burst_store::{
    account::AccountStore, block::BlockStore, meta::MetaStore, pending::PendingStore,
};
use burst_types::{BlockHash, NetworkId, ProtocolParams, Signature, Timestamp, TxHash};
use std::time::Duration;

#[allow(clippy::too_many_arguments)]
fn block(
    seed: u8,
    kind: BlockType,
    previous: BlockHash,
    link: BlockHash,
    brn_spent: u128,
    trst_balance: u128,
    params: &ProtocolParams,
    payment_epoch: BlockHash,
) -> StateBlock {
    let keys = burst_crypto::keypair_from_seed(&[seed; 32]);
    let account = burst_crypto::derive_address(&keys.public);
    let mut block = StateBlock {
        signing_grant: burst_types::BlockHash::ZERO,
        delegation_payload: Vec::new(),
        version: CURRENT_BLOCK_VERSION,
        payment_epoch,
        block_type: kind,
        previous,
        link,
        representative: account.clone(),
        account,
        brn_spent,
        trst_balance,
        origin: TxHash::ZERO,
        transaction: TxHash::ZERO,
        timestamp: Timestamp::now(),
        params_hash: params.params_hash(),
        target_head: burst_types::BlockHash::ZERO,
        merge_sources: vec![],
        release_sources: Vec::new(),
        work: 0,
        signature: Signature([0; 64]),
        hash: BlockHash::ZERO,
    };
    block.hash = block.compute_hash();
    block.signature = burst_crypto::sign_message(block.hash.as_bytes(), &keys.private);
    block
}

async fn wait_until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("node did not reach expected state");
}

// Election authorization is injected at the same boundary as the other worker
// tests in this file. This is not a signed-network quorum fixture.
async fn elect(
    elections: &tokio::sync::RwLock<burst_consensus::ActiveElections>,
    creator: &burst_types::WalletAddress,
    winner: &StateBlock,
    competitor: Option<BlockHash>,
) {
    let root = burst_node::ledger_updater::election_root(winner);
    let mut active = elections.write().await;
    active.remove_root(&root);
    active.set_online_weight(1);
    if let Some(competitor) = competitor {
        active
            .register_candidate(root, competitor, Timestamp::now())
            .unwrap();
    }
    active
        .register_candidate(root, winner.hash, Timestamp::now())
        .unwrap();
    let second = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[3; 32]).public);
    let mut confirmed = false;
    for voter in [creator, &second] {
        if active.has_authorized_voter(voter)
            && active
                .process_vote(&root, voter, winner.hash, 1, true, Timestamp::now())
                .unwrap()
                .is_some()
        {
            confirmed = true;
            break;
        }
    }
    assert!(confirmed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_replacement_preserves_an_independent_cemented_payment_through_restart() {
    disjoint_payment_fork(true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn consecutive_forks_do_not_resurrect_balances_from_retained_undo_journals() {
    disjoint_payment_fork(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn merge_fork_preserves_a_cemented_consumer_of_the_same_burn_origin() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = NodeConfig {
        network: NetworkId::Dev,
        data_dir: directory.path().to_path_buf(),
        port: 0,
        enable_rpc: false,
        enable_upnp: false,
        enable_representative: false,
        bootstrap_peers: vec![],
        ..NodeConfig::default()
    };
    config.params.min_work_difficulty = 0;
    let mut node = BurstNode::new(config.clone()).await.unwrap();
    let store = node.store.clone();
    let queue = node.block_queue();
    let trst = node.trst_engine.clone();
    let elections = node.active_elections.clone();
    let shutdown = node.shutdown.clone();
    let task = tokio::spawn(async move {
        node.start_with_services(burst_daemon::start_public_services).await.unwrap();
        node.stop().await.unwrap();
    });
    let address =
        |seed| burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[seed; 32]).public);
    let creator = address(0);
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        wait_until(|| store.meta_store().payment_epoch().is_ok()).await;
        let genesis = store.account_store().get_account(&creator).unwrap().head;
        let epoch = store.meta_store().payment_epoch().unwrap();
        let signed = |seed, kind, previous, link, brn_spent, trst_balance| {
            block(seed, kind, previous, link, brn_spent, trst_balance, &config.params, epoch)
        };
        let link = |seed| BlockHash::new(burst_crypto::keypair_from_seed(&[seed; 32]).public.0);
        let resign = |block: &mut StateBlock, seed| {
            block.hash = block.compute_hash();
            block.signature = burst_crypto::sign_message(
                block.hash.as_bytes(),
                &burst_crypto::keypair_from_seed(&[seed; 32]).private,
            );
        };
        let funding = signed(0, BlockType::Burn, genesis, link(0), 200, 0);
        let received = signed(0, BlockType::Receive, funding.hash, funding.hash, 200, 200);
        let mut send1 = signed(0, BlockType::Send, received.hash, link(3), 200, 160);
        send1.origin = funding.hash.into_tx_hash();
        resign(&mut send1, 0);
        let mut send2 = signed(0, BlockType::Send, send1.hash, link(3), 200, 120);
        send2.origin = funding.hash.into_tx_hash();
        resign(&mut send2, 0);
        let other1 = signed(3, BlockType::Open, BlockHash::ZERO, send1.hash, 0, 40);
        let other2 = signed(3, BlockType::Receive, other1.hash, send2.hash, 0, 80);
        let mut self_send = signed(0, BlockType::Send, send2.hash, link(0), 200, 60);
        self_send.origin = funding.hash.into_tx_hash();
        resign(&mut self_send, 0);
        let self_receive = signed(
            0,
            BlockType::Receive,
            self_send.hash,
            self_send.hash,
            200,
            120,
        );
        let mut endorse = signed(0, BlockType::Endorse, self_receive.hash, link(3), 201, 120);
        endorse.target_head = other2.hash;
        resign(&mut endorse, 0);
        for block in [
            &funding,
            &received,
            &send1,
            &send2,
            &other1,
            &other2,
            &self_send,
            &self_receive,
            &endorse,
        ] {
            assert!(queue.push(block.clone()).await);
            wait_until(|| store.block_store().exists(&block.hash).unwrap()).await;
            elect(&elections, &creator, block, None).await;
            wait_until(|| {
                let account = store.account_store().get_account(&block.account).unwrap();
                let batch = store.tx_begin_write().unwrap();
                account.confirmation_height
                    >= batch.block_height(&block.account, &block.hash).unwrap()
            })
            .await;
        }
        let mut losing = signed(0, BlockType::Merge, endorse.hash, BlockHash::ZERO, 201, 120);
        losing.merge_sources = vec![
            received.hash.into_tx_hash(),
            self_receive.hash.into_tx_hash(),
        ];
        resign(&mut losing, 0);
        assert!(queue.push(losing.clone()).await);
        wait_until(|| store.block_store().exists(&losing.hash).unwrap()).await;
        let mut independent = signed(3, BlockType::Merge, other2.hash, BlockHash::ZERO, 0, 80);
        independent.merge_sources = vec![other1.hash.into_tx_hash(), other2.hash.into_tx_hash()];
        resign(&mut independent, 3);
        assert!(queue.push(independent.clone()).await);
        wait_until(|| store.block_store().exists(&independent.hash).unwrap()).await;
        elect(&elections, &creator, &independent, None).await;
        wait_until(|| {
            store
                .account_store()
                .get_account(&address(3))
                .unwrap()
                .confirmation_height
                == 3
        })
        .await;
        let winner = signed(
            0,
            BlockType::ChangeRepresentative,
            endorse.hash,
            link(3),
            201,
            120,
        );
        assert!(queue.push(winner.clone()).await);
        wait_until(|| {
            store
                .meta_store()
                .fork_candidate(&winner.hash)
                .unwrap()
                .is_some()
        })
        .await;
        elect(&elections, &creator, &winner, Some(losing.hash)).await;
        wait_until(|| store.account_store().get_account(&creator).unwrap().head == winner.hash).await;
        assert!(!store.block_store().exists(&losing.hash).unwrap());
        assert!(store.block_store().exists(&independent.hash).unwrap());
        let engine = trst.lock().await;
        assert!(!engine
            .merger_graph
            .contains_merge(&losing.hash.into_tx_hash()));
        assert!(engine
            .merger_graph
            .contains_merge(&independent.hash.into_tx_hash()));
        assert_eq!(engine.transferable_balance_snapshot(&creator), Some(120));
        assert_eq!(engine.get_portfolio(&creator).unwrap().tokens.len(), 2);
        assert_eq!(engine.transferable_balance_snapshot(&address(3)), Some(80));
        assert_eq!(engine.get_portfolio(&address(3)).unwrap().tokens.len(), 1);
        let mut revoked = engine.clone();
        revoked.revoke_by_originator(&creator);
        assert_eq!(revoked.transferable_balance_snapshot(&creator), Some(0));
        assert_eq!(revoked.transferable_balance_snapshot(&address(3)), Some(0));
        (engine.save_wallets(), engine.merger_graph.to_bytes())
    })
    .await;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .unwrap()
        .unwrap();
    let expected = result.expect("shared-origin fork timed out");
    let mut restarted = BurstNode::new(config).await.unwrap();
    let engine = restarted.trst_engine.clone();
    let shutdown = restarted.shutdown.clone();
    let task = tokio::spawn(async move {
        restarted.start_with_services(burst_daemon::start_public_services).await.unwrap();
        restarted.stop().await.unwrap();
    });
    let restored = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let restored = engine.lock().await;
            if restored.get_portfolio(&creator).is_some() {
                break restored.clone();
            }
            drop(restored);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(15), task)
        .await
        .unwrap()
        .unwrap();
    let restored = restored.expect("shared-origin fork restart timed out");
    assert_eq!(restored.save_wallets(), expected.0);
    assert_eq!(restored.merger_graph.to_bytes(), expected.1);
}

async fn disjoint_payment_fork(cement_independent: bool) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = NodeConfig {
        network: NetworkId::Dev,
        data_dir: dir.path().to_path_buf(),
        port: 0,
        enable_rpc: false,
        enable_upnp: false,
        enable_representative: false,
        bootstrap_peers: vec![],
        ..NodeConfig::default()
    };
    config.params.min_work_difficulty = 0;
    let mut node = BurstNode::new(config.clone()).await.unwrap();
    let store = node.store.clone();
    let frontier = node.frontier.clone();
    let queue = node.block_queue();
    let trst = node.trst_engine.clone();
    let elections = node.active_elections.clone();
    let shutdown = node.shutdown.clone();
    let task = tokio::spawn(async move {
        node.start_with_services(burst_daemon::start_public_services).await.unwrap();
        node.stop().await.unwrap();
    });
    let address =
        |seed| burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[seed; 32]).public);
    let creator = address(0);
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        wait_until(|| store.meta_store().payment_epoch().is_ok()).await;
        let genesis = store.account_store().get_account(&creator).unwrap().head;
        let epoch = store.meta_store().payment_epoch().unwrap();
        let signed = |seed, kind, previous, link, brn_spent, trst_balance| {
            block(seed, kind, previous, link, brn_spent, trst_balance, &config.params, epoch)
        };
        let link = |seed| BlockHash::new(burst_crypto::keypair_from_seed(&[seed; 32]).public.0);
        let funding = signed(0, BlockType::Burn, genesis, link(3), 100, 0);
        assert!(queue.push(funding.clone()).await);
        wait_until(|| store.block_store().exists(&funding.hash).unwrap()).await;
        let independent = signed(3, BlockType::Open, BlockHash::ZERO, funding.hash, 0, 100);
        assert!(queue.push(independent.clone()).await);
        wait_until(|| store.block_store().exists(&independent.hash).unwrap()).await;
        let mut endorsement = signed(0, BlockType::Endorse, funding.hash, link(3), 101, 0);
        endorsement.target_head = independent.hash;
        endorsement.hash = endorsement.compute_hash();
        endorsement.signature = burst_crypto::sign_message(
            endorsement.hash.as_bytes(),
            &burst_crypto::keypair_from_seed(&[0; 32]).private,
        );
        assert!(queue.push(endorsement.clone()).await);
        wait_until(|| store.block_store().exists(&endorsement.hash).unwrap()).await;
        assert!(store
            .account_store()
            .get_account(&address(3))
            .unwrap()
            .state
            .is_verified());

        let losing = signed(0, BlockType::Burn, endorsement.hash, link(1), 141, 0);
        assert!(queue.push(losing.clone()).await);
        wait_until(|| store.block_store().exists(&losing.hash).unwrap()).await;
        let receive = signed(1, BlockType::Open, BlockHash::ZERO, losing.hash, 0, 40);
        assert!(queue.push(receive.clone()).await);
        wait_until(|| store.block_store().exists(&receive.hash).unwrap()).await;

        let mut send = signed(3, BlockType::Send, independent.hash, link(4), 0, 70);
        send.origin = funding.hash.into_tx_hash();
        send.hash = send.compute_hash();
        send.signature = burst_crypto::sign_message(
            send.hash.as_bytes(),
            &burst_crypto::keypair_from_seed(&[3; 32]).private,
        );
        assert!(queue.push(send.clone()).await);
        wait_until(|| store.block_store().exists(&send.hash).unwrap()).await;
        let received = signed(4, BlockType::Open, BlockHash::ZERO, send.hash, 0, 30);
        assert!(queue.push(received.clone()).await);
        wait_until(|| store.block_store().exists(&received.hash).unwrap()).await;
        let mut final_blocks = vec![&funding, &independent, &endorsement];
        if cement_independent {
            final_blocks.extend([&send, &received]);
        }
        for block in final_blocks {
            elect(&elections, &creator, block, None).await;
            wait_until(|| {
                let account = store.account_store().get_account(&block.account).unwrap();
                let batch = store.tx_begin_write().unwrap();
                account.confirmation_height
                    >= batch.block_height(&block.account, &block.hash).unwrap()
            })
            .await;
        }
        let mut retained: Vec<_> = [
            funding.hash,
            independent.hash,
            endorsement.hash,
            send.hash,
            received.hash,
        ]
        .into_iter()
        .map(|hash| (hash, store.block_store().get_block(&hash).unwrap()))
        .collect();
        let winner = signed(0, BlockType::Burn, endorsement.hash, link(2), 171, 0);
        assert!(queue.push(winner.clone()).await);
        wait_until(|| {
            store
                .meta_store()
                .fork_candidate(&winner.hash)
                .unwrap()
                .is_some()
        })
        .await;
        elect(&elections, &creator, &winner, Some(losing.hash)).await;
        wait_until(|| store.account_store().get_account(&creator).unwrap().head == winner.hash).await;
        assert!(!store.block_store().exists(&losing.hash).unwrap());
        assert!(!store.block_store().exists(&receive.hash).unwrap());
        for (hash, bytes) in &retained {
            assert_eq!(&store.block_store().get_block(hash).unwrap(), bytes);
        }
        {
            let engine = trst.lock().await;
            assert_eq!(engine.transferable_balance_snapshot(&address(3)), Some(70));
            assert_eq!(engine.transferable_balance_snapshot(&address(4)), Some(30));
            assert!(engine.get_portfolio(&address(1)).is_none());
        }
        let replacement_send = if !cement_independent {
            // These journals were recorded while the first losing balance
            // existed. Reusing them must not bring that removed wallet back.
            let mut replacement = send.clone();
            replacement.link = link(5);
            replacement.trst_balance = 80;
            replacement.hash = replacement.compute_hash();
            replacement.signature = burst_crypto::sign_message(
                replacement.hash.as_bytes(),
                &burst_crypto::keypair_from_seed(&[3; 32]).private,
            );
            assert!(queue.push(replacement.clone()).await);
            wait_until(|| {
                store
                    .meta_store()
                    .fork_candidate(&replacement.hash)
                    .unwrap()
                    .is_some()
            })
            .await;
            elect(&elections, &creator, &replacement, Some(send.hash)).await;
            wait_until(|| {
                store.account_store().get_account(&address(3)).unwrap().head == replacement.hash
            })
            .await;
            assert!(!store.block_store().exists(&send.hash).unwrap());
            assert!(!store.block_store().exists(&received.hash).unwrap());
            assert!(!store.account_store().exists(&address(1)).unwrap());
            assert!(!store.account_store().exists(&address(4)).unwrap());
            retained.retain(|(hash, _)| *hash != send.hash && *hash != received.hash);
            retained.push((
                replacement.hash,
                store.block_store().get_block(&replacement.hash).unwrap(),
            ));
            Some(replacement)
        } else {
            None
        };
        {
            let frontier = frontier.read().await;
            assert_eq!(frontier.get_head(&creator), Some(&winner.hash));
            assert_eq!(
                frontier.get_head(&address(3)),
                Some(&store.account_store().get_account(&address(3)).unwrap().head)
            );
            assert!(frontier.get_head(&address(1)).is_none());
        }
        (winner, retained, replacement_send)
    })
    .await;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();
    let (winner, retained, replacement_send) =
        result.expect("disjoint payment fork replacement timed out");
    let mut node = BurstNode::new(config.clone()).await.unwrap();
    let queue = node.block_queue();
    let trst = node.trst_engine.clone();
    let shutdown = node.shutdown.clone();
    let task = tokio::spawn(async move {
        node.start_with_services(burst_daemon::start_public_services).await.unwrap();
        node.stop().await.unwrap();
    });
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let receive = block(
            2,
            BlockType::Open,
            BlockHash::ZERO,
            winner.hash,
            0,
            70,
            &config.params,
            store.meta_store().payment_epoch().unwrap(),
        );
        assert!(queue.push(receive.clone()).await);
        wait_until(|| store.block_store().exists(&receive.hash).unwrap()).await;
        if let Some(send) = replacement_send {
            let receive = block(
                5,
                BlockType::Open,
                BlockHash::ZERO,
                send.hash,
                0,
                20,
                &config.params,
                store.meta_store().payment_epoch().unwrap(),
            );
            assert!(queue.push(receive.clone()).await);
            wait_until(|| store.block_store().exists(&receive.hash).unwrap()).await;
        }
        for (hash, bytes) in retained {
            assert_eq!(store.block_store().get_block(&hash).unwrap(), bytes);
        }
        let engine = trst.lock().await;
        for (seed, amount) in if cement_independent {
            [(2, 70), (3, 70), (4, 30)]
        } else {
            [(2, 70), (3, 80), (5, 20)]
        } {
            assert_eq!(
                engine.transferable_balance_snapshot(&address(seed)),
                Some(amount)
            );
        }
        assert!(engine.get_portfolio(&address(1)).is_none());
        assert_eq!(
            store
                .account_store()
                .get_account(&address(3))
                .unwrap()
                .confirmation_height,
            2
        );
        if cement_independent {
            assert_eq!(
                store
                    .account_store()
                    .get_account(&address(4))
                    .unwrap()
                    .confirmation_height,
                1
            );
        } else {
            assert!(!store.account_store().exists(&address(4)).unwrap());
            assert!(engine.get_portfolio(&address(4)).is_none());
        }
    })
    .await;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();
    result.expect("disjoint payment recovery timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verifier_preference_forks_replace_descendants_and_survive_restart() {
    for winner_kind in [BlockType::VerifierOptIn, BlockType::VerifierOptOut] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = NodeConfig {
            network: NetworkId::Dev,
            data_dir: dir.path().to_path_buf(),
            port: 0,
            enable_rpc: false,
            enable_upnp: false,
            enable_representative: false,
            bootstrap_peers: vec![],
            ..NodeConfig::default()
        };
        config.params.min_work_difficulty = 0;
        let mut node = BurstNode::new(config.clone()).await.unwrap();
        let store = node.store.clone();
        let queue = node.block_queue();
        let elections = node.active_elections.clone();
        let shutdown = node.shutdown.clone();
        let task = tokio::spawn(async move {
            node.start_with_services(burst_daemon::start_public_services).await.unwrap();
            node.stop().await.unwrap();
        });
        let creator = burst_node::genesis_key::genesis_address(NetworkId::Dev);
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            wait_until(|| store.meta_store().payment_epoch().is_ok()).await;
            let genesis = store.account_store().get_account(&creator).unwrap().head;
            let epoch = store.meta_store().payment_epoch().unwrap();
            let losing_kind = if winner_kind == BlockType::VerifierOptIn {
                BlockType::VerifierOptOut
            } else {
                BlockType::VerifierOptIn
            };
            let losing = block(
                0,
                losing_kind,
                genesis,
                BlockHash::ZERO,
                0,
                0,
                &config.params,
                epoch,
            );
            assert!(queue.push(losing.clone()).await);
            wait_until(|| store.block_store().exists(&losing.hash).unwrap()).await;
            let descendant = block(
                0,
                BlockType::ChangeRepresentative,
                losing.hash,
                BlockHash::ZERO,
                0,
                0,
                &config.params,
                epoch,
            );
            assert!(queue.push(descendant.clone()).await);
            wait_until(|| store.block_store().exists(&descendant.hash).unwrap()).await;
            let winner = block(
                0,
                winner_kind.clone(),
                genesis,
                BlockHash::ZERO,
                0,
                0,
                &config.params,
                epoch,
            );
            assert!(queue.push(winner.clone()).await);
            wait_until(|| {
                store
                    .meta_store()
                    .fork_candidate(&winner.hash)
                    .unwrap()
                    .is_some()
            })
            .await;
            elect(&elections, &creator, &winner, Some(losing.hash)).await;
            wait_until(|| {
                let account = store.account_store().get_account(&creator).unwrap();
                account.head == winner.hash && account.confirmation_height == 2
            })
            .await;
            assert!(!store.block_store().exists(&losing.hash).unwrap());
            assert!(!store.block_store().exists(&descendant.hash).unwrap());
            winner
        })
        .await;
        shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .unwrap()
            .unwrap();
        let winner = result.expect("preference fork replacement timed out");
        let mut node = BurstNode::new(config.clone()).await.unwrap();
        let queue = node.block_queue();
        let shutdown = node.shutdown.clone();
        let task = tokio::spawn(async move {
            node.start_with_services(burst_daemon::start_public_services).await.unwrap();
            node.stop().await.unwrap();
        });
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let followup = block(
                0,
                BlockType::ChangeRepresentative,
                winner.hash,
                BlockHash::ZERO,
                0,
                0,
                &config.params,
                store.meta_store().payment_epoch().unwrap(),
            );
            assert!(queue.push(followup.clone()).await);
            wait_until(|| store.account_store().get_account(&creator).unwrap().head == followup.hash)
                .await;
            let account = store.account_store().get_account(&creator).unwrap();
            assert_eq!(
                account.verifier_opted_in_at,
                (winner_kind == BlockType::VerifierOptIn).then_some(winner.timestamp)
            );
            assert_eq!(account.confirmation_height, 2);
        })
        .await;
        shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .unwrap()
            .unwrap();
        result.expect("preference recovery failed");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn restart_readmits_cached_fork_but_does_not_invent_a_quorum() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = NodeConfig {
        network: NetworkId::Dev,
        data_dir: dir.path().to_path_buf(),
        port: 0,
        enable_rpc: false,
        enable_upnp: false,
        enable_representative: false,
        bootstrap_peers: vec![],
        ..NodeConfig::default()
    };
    config.params.min_work_difficulty = 0;
    let mut node = BurstNode::new(config.clone()).await.unwrap();
    let store = node.store.clone();
    let queue = node.block_queue();
    let shutdown = node.shutdown.clone();
    let task = tokio::spawn(async move {
        node.start_with_services(burst_daemon::start_public_services).await.unwrap();
        node.stop().await.unwrap();
    });
    let creator = burst_node::genesis_key::genesis_address(NetworkId::Dev);
    wait_until(|| store.account_store().exists(&creator).unwrap()).await;
    let genesis = store.account_store().get_account(&creator).unwrap().head;
    let destination = BlockHash::new(burst_crypto::keypair_from_seed(&[3; 32]).public.0);
    let first = block(
        0,
        BlockType::Burn,
        genesis,
        destination,
        40,
        0,
        &config.params,
        store.meta_store().payment_epoch().unwrap(),
    );
    assert!(queue.push(first.clone()).await);
    wait_until(|| store.block_store().exists(&first.hash).unwrap()).await;
    let fork = block(
        0,
        BlockType::Burn,
        genesis,
        destination,
        70,
        0,
        &config.params,
        store.meta_store().payment_epoch().unwrap(),
    );
    assert!(queue.push(fork.clone()).await);
    wait_until(|| {
        store
            .meta_store()
            .fork_candidate(&fork.hash)
            .unwrap()
            .is_some()
    })
    .await;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();

    let mut node = BurstNode::new(config).await.unwrap();
    let elections = node.active_elections.clone();
    let brn = node.brn_engine.clone();
    let shutdown = node.shutdown.clone();
    let task = tokio::spawn(async move {
        node.start_with_services(burst_daemon::start_public_services).await.unwrap();
        node.stop().await.unwrap();
    });
    let root = burst_node::ledger_updater::election_root(&fork);
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if elections.read().await.candidate_root(&fork.hash) == Some(root) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(elections.read().await.confirmed_elections().is_empty());
        assert_eq!(
            store.account_store().get_account(&creator).unwrap().head,
            first.hash
        );
        assert!(!store.block_store().exists(&fork.hash).unwrap());
        assert_eq!(
            brn.lock().await.get_wallet(&creator).unwrap().total_burned,
            40
        );
    })
    .await;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();
    result.expect("cached fork was not readmitted after restart");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn elected_burn_replaces_received_branch_and_restart_can_receive_the_winner() {
    use burst_wallet_core::{offline::load_payment_with_ancestry, NodeClient};
    let _ = tracing_subscriber::fmt()
        .with_env_filter("burst_node=debug")
        .with_test_writer()
        .try_init();
    let dir = tempfile::tempdir().unwrap();
    let drafts = tempfile::tempdir().unwrap();
    let draft_path = drafts.path().join("offline.json");
    let native_report = drafts.path().join("native-fork.json");
    let native_enabled = std::env::var("BURST_NATIVE_FORK").as_deref() == Ok("1");
    let mut native_child = None;
    let rpc_port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut config = NodeConfig {
        network: NetworkId::Dev,
        data_dir: dir.path().to_path_buf(),
        port: 0,
        enable_rpc: true,
        rpc_port,
        enable_upnp: false,
        enable_representative: false,
        bootstrap_peers: vec![],
        ..NodeConfig::default()
    };
    config.params.min_work_difficulty = 0;
    let mut client = NodeClient::new(format!("http://127.0.0.1:{rpc_port}")).unwrap();
    let mut node = BurstNode::new(config.clone()).await.unwrap();
    let store = node.store.clone();
    let brn = node.brn_engine.clone();
    let trst = node.trst_engine.clone();
    let frontier = node.frontier.clone();
    let weights = node.rep_weights.clone();
    let counts = node.ledger_cache.clone();
    let elections = node.active_elections.clone();
    let queue = node.block_queue();
    let shutdown = node.shutdown.clone();
    let task = tokio::spawn(async move {
        node.start_with_services(burst_daemon::start_public_services).await.unwrap();
        node.stop().await.unwrap();
    });
    let creator = burst_node::genesis_key::genesis_address(NetworkId::Dev);
    let receiver = burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[1; 32]).public);
    let destination =
        burst_crypto::derive_address(&burst_crypto::keypair_from_seed(&[2; 32]).public);
    let result = tokio::time::timeout(
        Duration::from_secs(if native_enabled { 180 } else { 60 }),
        async {
            wait_until(|| {
                let batch = store.tx_begin_write().unwrap();
                batch.ledger_counts().unwrap().0 == 1 && batch.payment_epoch().is_ok()
            })
            .await;
            let mut base = store.account_store().get_account(&creator).unwrap().head;
            let mut grant = BlockHash::ZERO;
            if native_enabled {
                let runner = std::env::var_os("BURST_NATIVE_RUNNER")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| {
                        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                            .join("../../wallet/tests/native-session.mjs")
                    });
                native_child = Some(
                    tokio::process::Command::new("node")
                        .arg(runner)
                        .arg("--embedded")
                        .arg("--offline-fork-fixture")
                        .env("NATIVE_FIXTURE_RPC_URL", client.node_url())
                        .env("NATIVE_RESULT_PATH", &native_report)
                        .kill_on_drop(true)
                        .spawn()
                        .unwrap(),
                );
                // Enroll before creating the losing branch. Confirming enrollment
                // afterward would also cement its predecessor and defeat this test.
                tokio::time::timeout(Duration::from_secs(90), async {
                    while store.account_store().get_account(&creator).unwrap().head == base {
                        assert!(
                            native_child.as_mut().unwrap().try_wait().unwrap().is_none(),
                            "native enrollment exited early"
                        );
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                })
                .await
                .expect("native enrollment was not submitted");
                let handoff: StateBlock = bincode::deserialize(
                    &store
                        .block_store()
                        .get_block(&store.account_store().get_account(&creator).unwrap().head)
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(handoff.block_type, BlockType::ChangeSigner);
                assert_eq!(handoff.previous, base);
                elect(&elections, &creator, &handoff, None).await;
                wait_until(|| {
                    store
                        .account_store()
                        .get_account(&creator)
                        .unwrap()
                        .confirmation_height
                        == 2
                })
                .await;
                let enrolled = native_report.with_file_name("native-fork.json.enrolled");
                wait_until(|| enrolled.exists()).await;
                grant = handoff.hash;
                base = handoff.hash;
                client = client.clone().with_signing_grant(grant);
            }
            let creator_block = |mut value: StateBlock| {
                value.signing_grant = grant;
                value.hash = value.compute_hash();
                value.signature = burst_crypto::sign_message(
                    value.hash.as_bytes(),
                    &burst_crypto::keypair_from_seed(&[0; 32]).private,
                );
                value
            };
            let first = creator_block(block(
                0,
                BlockType::Burn,
                base,
                BlockHash::new(burst_crypto::decode_address(receiver.as_str()).unwrap()),
                40,
                0,
                &config.params,
                store.meta_store().payment_epoch().unwrap(),
            ));
            elections.write().await.set_online_weight(1);
            assert!(queue.try_push(first.clone()));
            wait_until(|| store.block_store().exists(&first.hash).unwrap()).await;
            let context = client.prepare_offline_payment(&creator).await.unwrap();
            let keys = burst_crypto::keypair_from_seed(&[0; 32]);
            let tx = burst_transactions::Transaction::Burn(
                burst_wallet_core::transaction_builder::build_burn_tx(
                    &creator,
                    &destination,
                    1,
                    Timestamp::now(),
                )
                .unwrap(),
            );
            let draft = context.sign(&keys, &tx, TxHash::ZERO).unwrap();
            context.save_payment(&draft_path, &draft).unwrap();
            let (_, ancestry) = load_payment_with_ancestry(&draft_path).unwrap();
            assert_eq!(ancestry.len(), 1);
            assert_eq!(ancestry[0].hash, first.hash);
            if native_enabled {
                assert_eq!(
                    store
                        .account_store()
                        .get_account(&creator)
                        .unwrap()
                        .confirmation_height,
                    2
                );
                std::fs::write(
                    native_report.with_file_name("native-fork.json.branch-ready"),
                    b"true",
                )
                .unwrap();
                let prepared = native_report.with_file_name("native-fork.json.prepared");
                tokio::time::timeout(Duration::from_secs(90), async {
                    while !prepared.exists() {
                        assert!(
                            native_child.as_mut().unwrap().try_wait().unwrap().is_none(),
                            "native fork preparation exited before saving"
                        );
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                })
                .await
                .expect("native fork preparation timed out");
            }
            let receive = block(
                1,
                BlockType::Open,
                BlockHash::ZERO,
                first.hash,
                0,
                40,
                &config.params,
                store.meta_store().payment_epoch().unwrap(),
            );
            assert!(queue.try_push(receive.clone()));
            wait_until(|| store.block_store().exists(&receive.hash).unwrap()).await;
            assert_eq!(
                trst.lock().await.transferable_balance_snapshot(&receiver),
                Some(40)
            );

            let mut invalid_signature = creator_block(block(
                0,
                BlockType::Burn,
                base,
                BlockHash::new(burst_crypto::decode_address(destination.as_str()).unwrap()),
                69,
                0,
                &config.params,
                store.meta_store().payment_epoch().unwrap(),
            ));
            invalid_signature.signature = Signature([0; 64]);
            assert!(queue.try_push(invalid_signature.clone()));
            let mut invalid_balance = creator_block(block(
                0,
                BlockType::Burn,
                base,
                BlockHash::new(burst_crypto::decode_address(destination.as_str()).unwrap()),
                68,
                1,
                &config.params,
                store.meta_store().payment_epoch().unwrap(),
            ));
            invalid_balance.hash = invalid_balance.compute_hash();
            invalid_balance.signature = burst_crypto::sign_message(
                invalid_balance.hash.as_bytes(),
                &burst_crypto::keypair_from_seed(&[0; 32]).private,
            );
            assert!(queue.try_push(invalid_balance.clone()));
            let mut winner = creator_block(block(
                0,
                BlockType::Burn,
                base,
                BlockHash::new(burst_crypto::decode_address(destination.as_str()).unwrap()),
                70,
                0,
                &config.params,
                store.meta_store().payment_epoch().unwrap(),
            ));
            // The worker uses PoW priority, not FIFO. Make the valid candidate a
            // barrier after both invalid submissions, without weakening admission.
            let priority = burst_node::priority_queue::work_difficulty;
            while priority(&winner.work_root(), winner.work)
                >= priority(&invalid_signature.work_root(), invalid_signature.work)
                    .min(priority(&invalid_balance.work_root(), invalid_balance.work))
            {
                winner.work += 1;
            }
            assert!(queue.try_push(winner.clone()));
            wait_until(|| {
                store
                    .meta_store()
                    .fork_candidate(&winner.hash)
                    .unwrap()
                    .is_some()
            })
            .await;
            for invalid in [invalid_signature, invalid_balance] {
                assert!(store
                    .meta_store()
                    .fork_candidate(&invalid.hash)
                    .unwrap()
                    .is_none());
                assert!(!store.block_store().exists(&invalid.hash).unwrap());
            }
            assert!(
                !store.block_store().exists(&winner.hash).unwrap(),
                "admission must not install a fork"
            );
            assert_eq!(
                brn.lock().await.get_wallet(&creator).unwrap().total_burned,
                40
            );
            assert_eq!(
                trst.lock().await.transferable_balance_snapshot(&receiver),
                Some(40)
            );

            // Quorum creation is explicit in this worker-integration fixture.
            let root = burst_node::ledger_updater::election_root(&winner);
            {
                let mut active = elections.write().await;
                active.remove_root(&root);
                active.set_online_weight(1);
                active
                    .register_candidate(root, first.hash, Timestamp::now())
                    .unwrap();
                active
                    .register_candidate(root, winner.hash, Timestamp::now())
                    .unwrap();
                assert!(active
                    .process_vote(&root, &creator, winner.hash, 1, true, Timestamp::now())
                    .unwrap()
                    .is_some());
            }
            wait_until(|| {
                store.account_store().get_account(&creator).unwrap().head == winner.hash
                    && store
                        .account_store()
                        .get_account(&creator)
                        .unwrap()
                        .confirmation_height
                        == if native_enabled { 3 } else { 2 }
            })
            .await;
            assert!(!store.block_store().exists(&first.hash).unwrap());
            assert!(!store.block_store().exists(&receive.hash).unwrap());
            assert!(!store.account_store().exists(&receiver).unwrap());
            assert!(trst.lock().await.get_portfolio(&receiver).is_none());
            assert_eq!(
                brn.lock().await.get_wallet(&creator).unwrap().total_burned,
                70
            );
            assert_eq!(frontier.read().await.get_head(&creator), Some(&winner.hash));
            assert!(frontier.read().await.get_head(&receiver).is_none());
            assert_eq!(weights.read().await.total_weight(), 10_000);
            assert_eq!(
                (
                    counts.block_count(),
                    counts.account_count(),
                    counts.pending_count()
                ),
                (if native_enabled { 3 } else { 2 }, 1, 1)
            );
            let pending = store
                .pending_store()
                .get_pending(&destination, &winner.hash.into_tx_hash())
                .unwrap();
            assert_eq!(pending.amount, 70);
            assert_eq!(pending.provenance[0].origin, winner.hash.into_tx_hash());
            assert!(store
                .pending_store()
                .get_pending(&receiver, &first.hash.into_tx_hash())
                .is_err());
            winner
        },
    )
    .await;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();
    let winner = result.expect("live fork replacement timed out");

    let mut restarted = BurstNode::new(config.clone()).await.unwrap();
    let queue = restarted.block_queue();
    let restored = restarted.trst_engine.clone();
    let shutdown = restarted.shutdown.clone();
    let task = tokio::spawn(async move {
        restarted.start_with_services(burst_daemon::start_public_services).await.unwrap();
        restarted.stop().await.unwrap();
    });
    let mut late_conflict = block(
        0,
        BlockType::Burn,
        winner.previous,
        winner.link,
        71,
        0,
        &config.params,
        store.meta_store().payment_epoch().unwrap(),
    );
    late_conflict.signing_grant = winner.signing_grant;
    late_conflict.hash = late_conflict.compute_hash();
    late_conflict.signature = burst_crypto::sign_message(
        late_conflict.hash.as_bytes(),
        &burst_crypto::keypair_from_seed(&[0; 32]).private,
    );
    assert!(queue.push(late_conflict.clone()).await);
    let mut receive = block(
        2,
        BlockType::Open,
        BlockHash::ZERO,
        winner.hash,
        0,
        70,
        &config.params,
        store.meta_store().payment_epoch().unwrap(),
    );
    while burst_node::work_difficulty(&receive.work_root(), receive.work)
        >= burst_node::work_difficulty(&late_conflict.work_root(), late_conflict.work)
    {
        receive.work += 1;
    }
    assert!(queue.try_push(receive.clone()));
    let result = tokio::time::timeout(
        Duration::from_secs(if native_enabled { 90 } else { 30 }),
        async {
            wait_until(|| store.block_store().exists(&receive.hash).unwrap()).await;
            assert!(store
                .meta_store()
                .fork_candidate(&late_conflict.hash)
                .unwrap()
                .is_none());
            assert!(!store.block_store().exists(&late_conflict.hash).unwrap());
            assert_eq!(
                store.account_store().get_account(&creator).unwrap().head,
                winner.hash
            );
            assert_eq!(
                restored
                    .lock()
                    .await
                    .transferable_balance_snapshot(&destination),
                Some(70)
            );
            assert!(restored.lock().await.get_portfolio(&receiver).is_none());
            let bytes = std::fs::read(&draft_path).unwrap();
            let (draft, ancestry) = load_payment_with_ancestry(&draft_path).unwrap();
            assert!(!store.block_store().exists(&draft.previous).unwrap());
            assert!(client.confirmed_offline_successor(&draft).await.is_err());
            assert_eq!(
                client
                    .confirmed_offline_resolution(&draft, &ancestry)
                    .await
                    .unwrap(),
                Some(winner.hash)
            );
            assert_eq!(std::fs::read(&draft_path).unwrap(), bytes);
            assert!(!store.block_store().exists(&draft.hash).unwrap());
            if let Some(child) = native_child.as_mut() {
                let staged = native_report.with_file_name("native-fork.json.ready.tmp");
                std::fs::write(
                    &staged,
                    serde_json::to_vec(&serde_json::json!({"winner":winner.hash.to_string()}))
                        .unwrap(),
                )
                .unwrap();
                std::fs::rename(
                    staged,
                    native_report.with_file_name("native-fork.json.ready"),
                )
                .unwrap();
                assert!(
                    child.wait().await.unwrap().success(),
                    "native fork recovery failed"
                );
                let report: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&native_report).unwrap()).unwrap();
                assert_eq!(report["resolved"], true);
                assert_eq!(report["winner"], winner.hash.to_string());
            }
        },
    )
    .await;
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(20), task)
        .await
        .unwrap()
        .unwrap();
    result.expect("post-restart receive failed");
}
