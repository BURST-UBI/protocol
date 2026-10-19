use super::*;
use burst_governance::{content::MAX_AMENDMENT_CHARS, proposal::AmendmentOp, ProposalContent};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_proposal_fork_survives_pre_election_restart_and_replacement() {
    for proposal_wins in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = crate::NodeConfig {
            network: NetworkId::Dev,
            data_dir: dir.path().to_path_buf(),
            port: 0,
            enable_rpc: false,
            enable_upnp: false,
            enable_representative: false,
            bootstrap_peers: Vec::new(),
            ..crate::NodeConfig::default()
        };
        config.params.min_work_difficulty = 0;
        let mut node = crate::BurstNode::new(config.clone()).await.unwrap();
        node.initialize_genesis().unwrap();
        node.verification_fixture_clock = Some(Arc::new(|| None));
        let store = node.store.clone();
        let creator = crate::genesis_key::genesis_address(NetworkId::Dev);
        let genesis = store.account_store().get_account(&creator).unwrap().head;
        let content = ProposalContent::ConstitutionalDiff {
            title: "Equal rights".into(),
            operations: vec![AmendmentOp::AddArticle {
                title: "Humanity".into(),
                text: "🌍".repeat(MAX_AMENDMENT_CHARS),
            }],
        };
        let state = burst_wallet_core::transaction_builder::AccountState {
            signing_grant: burst_types::BlockHash::ZERO,
            head: genesis,
            block_count: 1,
            representative: creator.clone(),
            payment_epoch: store.meta_store().payment_epoch().unwrap(),
            brn_balance: 0,
            trst_balance: 0,
            release_sources: Vec::new(),
        };
        let (proposal, encoded) = burst_wallet_core::governance_content::build_signed_proposal(
            &state,
            &burst_crypto::keypair_from_seed(&[0; 32]),
            &content,
            config.params.params_hash(),
            Timestamp::now(),
        )
        .unwrap();
        assert!(encoded.bytes().len() > burst_types::MAX_P2P_MESSAGE_BYTES);
        let mut preference = proposal.clone();
        preference.block_type = BlockType::ChangeRepresentative;
        preference.link = BlockHash::ZERO;
        preference.origin = burst_types::TxHash::ZERO;
        preference.transaction = burst_types::TxHash::ZERO;
        sign(&mut preference, 0);
        let (loser, winner) = if proposal_wins {
            (preference, proposal.clone())
        } else {
            (proposal.clone(), preference)
        };
        node.governance_transfers
            .offer(&proposal, encoded.bytes())
            .unwrap();
        let queue = node.block_queue();
        let shutdown = node.shutdown.clone();
        let task = tokio::spawn(async move {
            node.start().await.unwrap();
            node.stop().await.unwrap();
        });
        let result = tokio::time::timeout(Duration::from_secs(45), async {
            assert!(queue.push(loser.clone()).await);
            until(|| store.block_store().exists(&loser.hash).unwrap()).await;
            assert!(queue.push(winner.clone()).await);
            until(|| {
                store
                    .meta_store()
                    .fork_candidate(&winner.hash)
                    .unwrap()
                    .is_some()
            })
            .await;
            assert_eq!(
                store
                    .meta_store()
                    .fork_content(&winner.hash)
                    .unwrap()
                    .as_deref(),
                proposal_wins.then_some(encoded.bytes())
            );
        })
        .await;
        shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(15), task)
            .await
            .unwrap()
            .unwrap();
        result.expect("full proposal admission failed");

        // No volatile transfer offer on restart. Cached bytes are not a quorum.
        let mut node = crate::BurstNode::new(config.clone()).await.unwrap();
        node.verification_fixture_clock = Some(Arc::new(|| None));
        let elections = node.active_elections.clone();
        let governance = node.governance.clone();
        let shutdown = node.shutdown.clone();
        let task = tokio::spawn(async move {
            node.start().await.unwrap();
            node.stop().await.unwrap();
        });
        let result = tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                if elections
                    .read()
                    .await
                    .candidate_root(&winner.hash)
                    .is_some()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert_eq!(
                store.account_store().get_account(&creator).unwrap().head,
                loser.hash
            );
            assert_eq!(
                governance
                    .lock()
                    .await
                    .get_proposal(&proposal.hash.into_tx_hash())
                    .is_some(),
                !proposal_wins
            );
            {
                let root = crate::ledger_updater::election_root(&winner);
                let mut active = elections.write().await;
                active.remove_root(&root);
                active.set_online_weight(1);
                active
                    .register_candidate(root, loser.hash, Timestamp::now())
                    .unwrap();
                active
                    .register_candidate(root, winner.hash, Timestamp::now())
                    .unwrap();
                assert!(active
                    .process_vote(&root, &creator, winner.hash, 1, true, Timestamp::now())
                    .unwrap()
                    .is_some());
            }
            until(|| {
                let account = store.account_store().get_account(&creator).unwrap();
                account.head == winner.hash && account.confirmation_height == 2
            })
            .await;
            assert!(!store.block_store().exists(&loser.hash).unwrap());
            let live = governance.lock().await;
            let restored = live.get_proposal(&proposal.hash.into_tx_hash());
            assert_eq!(restored.is_some(), proposal_wins);
            if let Some(restored) = restored {
                assert_eq!(restored.content, content);
            }
            if proposal_wins {
                let batch = store.tx_begin_write().unwrap();
                assert_eq!(
                    crate::governance_content::load(&batch, &proposal).unwrap(),
                    content
                );
            }
        })
        .await;
        shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(15), task)
            .await
            .unwrap()
            .unwrap();
        result.expect("full proposal fork recovery failed");
        crate::fork_resolution::prune_finalized(&store).unwrap();
        assert_eq!(
            store.meta_store().fork_candidate(&winner.hash).unwrap(),
            None
        );
        assert_eq!(store.meta_store().fork_content(&winner.hash).unwrap(), None);
        if proposal_wins {
            let batch = store.tx_begin_write().unwrap();
            assert_eq!(
                crate::governance_content::load(&batch, &proposal).unwrap(),
                content
            );
        }
    }
}
