use super::*;
use burst_governance::signing_delegation::{
    operation::DelegationOperation, scoped, Grant, KeyScope, Revocation,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delegation_fork_worker_publishes_both_directions_and_recovers() {
    delegation_fork(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn encrypted_key_fork_worker_replaces_recovers_and_revokes() {
    delegation_fork(true).await;
}

pub(super) fn scope() -> KeyScope {
    KeyScope::Parameter(burst_governance::GovernableParam::BrnRate)
}

pub(super) fn saved_grant(
    store: &LmdbStore,
    genesis: BlockHash,
    owner: &burst_types::WalletAddress,
) -> Option<BlockHash> {
    let key = scoped::key(NetworkId::Dev, genesis, owner, &scope()).unwrap();
    let bytes = match store.meta_store().get_meta(&key) {
        Ok(bytes) => bytes,
        Err(StoreError::NotFound(_)) => return None,
        Err(error) => panic!("cannot read delegation scope: {error}"),
    };
    let authority = scoped::restore(NetworkId::Dev, genesis, owner, &scope(), &bytes).unwrap();
    scoped::grant(&authority, owner, &scope()).map(|grant| grant.id().unwrap())
}

async fn delegation_fork(encrypted: bool) {
    for delegation_wins in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut config = crate::NodeConfig {
            network: NetworkId::Dev,
            data_dir: directory.path().to_path_buf(),
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
        let queue = node.block_queue();
        let delegation = node.delegation_engine.clone();
        let elections = node.active_elections.clone();
        let shutdown = node.shutdown.clone();
        let creator = crate::genesis_key::genesis_address(NetworkId::Dev);
        let genesis = store.account_store().get_account(&creator).unwrap().head;
        let mut delegate: StateBlock =
            bincode::deserialize(&store.block_store().get_block(&genesis).unwrap()).unwrap();
        let target_key = burst_crypto::keypair_from_seed(&[9; 32]);
        let target = burst_crypto::derive_address(&target_key.public);
        delegate.block_type = BlockType::Delegate;
        delegate.previous = genesis;
        delegate.payment_epoch = store.meta_store().payment_epoch().unwrap();
        delegate.timestamp = Timestamp::now();
        delegate.link = BlockHash::new(target_key.public.0);
        let grant = Grant::create(
            &burst_crypto::keypair_from_seed(&[0; 32]),
            target_key.public.clone(),
            scope(),
            1,
            NetworkId::Dev,
            genesis,
        )
        .unwrap();
        if encrypted {
            grant.decrypt(&target_key.private).unwrap();
            assert!(grant
                .decrypt(&burst_crypto::keypair_from_seed(&[8; 32]).private)
                .is_err());
            delegate.delegation_payload =
                DelegationOperation::Grant(grant.clone()).encode().unwrap();
        }
        sign(&mut delegate, 0);
        let mut preference = delegate.clone();
        preference.block_type = BlockType::ChangeRepresentative;
        preference.link = BlockHash::ZERO;
        preference.delegation_payload.clear();
        sign(&mut preference, 0);
        let (losing, winner) = if delegation_wins {
            (preference, delegate)
        } else {
            (delegate, preference)
        };
        let task = tokio::spawn(async move {
            node.start_with_services(burst_daemon::start_public_services).await.unwrap();
            node.stop().await.unwrap();
        });
        let result = tokio::time::timeout(Duration::from_secs(60), async {
            assert!(queue.push(losing.clone()).await);
            until(|| store.block_store().exists(&losing.hash).unwrap()).await;
            let before = delegation.lock().await.save_state();
            assert!(queue.push(winner.clone()).await);
            until(|| {
                store
                    .meta_store()
                    .fork_candidate(&winner.hash)
                    .unwrap()
                    .is_some()
            })
            .await;
            assert_eq!(delegation.lock().await.save_state(), before);
            {
                let root = crate::ledger_updater::election_root(&winner);
                let mut active = elections.write().await;
                active.remove_root(&root);
                active.set_online_weight(1);
                active
                    .register_candidate(root, losing.hash, Timestamp::now())
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
            assert!(!store.block_store().exists(&losing.hash).unwrap());
            assert_eq!(
                delegation.lock().await.get_delegate(&creator),
                (delegation_wins && !encrypted).then_some(&target)
            );
            if encrypted {
                assert_eq!(
                    saved_grant(&store, genesis, &creator),
                    delegation_wins.then_some(grant.id().unwrap())
                );
            }
        })
        .await;
        shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .unwrap()
            .unwrap();
        result.expect("delegation fork replacement timed out");

        let mut restarted = crate::BurstNode::new(config).await.unwrap();
        restarted.verification_fixture_clock = Some(Arc::new(|| None));
        let queue = restarted.block_queue();
        let delegation = restarted.delegation_engine.clone();
        let shutdown = restarted.shutdown.clone();
        let task = tokio::spawn(async move {
            restarted.start_with_services(burst_daemon::start_public_services).await.unwrap();
            restarted.stop().await.unwrap();
        });
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            let mut followup = winner.clone();
            followup.previous = winner.hash;
            followup.timestamp = Timestamp::now();
            followup.block_type = if delegation_wins {
                BlockType::RevokeDelegation
            } else {
                BlockType::Delegate
            };
            followup.link = if delegation_wins {
                BlockHash::ZERO
            } else {
                BlockHash::new(target_key.public.0)
            };
            if encrypted {
                assert_eq!(
                    saved_grant(&store, genesis, &creator),
                    delegation_wins.then_some(grant.id().unwrap())
                );
                followup.delegation_payload = if delegation_wins {
                    DelegationOperation::Revoke(
                        Revocation::create(
                            &burst_crypto::keypair_from_seed(&[0; 32]),
                            scope(),
                            2,
                            NetworkId::Dev,
                            genesis,
                        )
                        .unwrap(),
                    )
                } else {
                    DelegationOperation::Grant(grant.clone())
                }
                .encode()
                .unwrap();
            }
            sign(&mut followup, 0);
            assert!(queue.push(followup.clone()).await);
            until(|| store.account_store().get_account(&creator).unwrap().head == followup.hash)
                .await;
            assert_eq!(
                delegation.lock().await.get_delegate(&creator),
                (!delegation_wins && !encrypted).then_some(&target)
            );
            if encrypted {
                assert_eq!(
                    saved_grant(&store, genesis, &creator),
                    (!delegation_wins).then_some(grant.id().unwrap())
                );
                if delegation_wins {
                    let key = scoped::key(NetworkId::Dev, genesis, &creator, &scope()).unwrap();
                    let bytes = store.meta_store().get_meta(&key).unwrap();
                    let mut authority =
                        scoped::restore(NetworkId::Dev, genesis, &creator, &scope(), &bytes)
                            .unwrap();
                    assert_eq!(authority.generation(&creator, &scope()), 2);
                    assert!(authority.apply_grant(grant.clone()).is_err());
                }
            }
        })
        .await;
        shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .unwrap()
            .unwrap();
        result.expect("delegation fork recovery timed out");
    }
}
