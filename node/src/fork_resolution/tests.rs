//! Running worker/publication checks. Election authorization is injected here;
//! signed-network quorum and Byzantine fork qualification remain separate gates.
use super::*;
use burst_ledger::BlockType;
use burst_store::{account::AccountStore, block::BlockStore, meta::MetaStore};
use std::time::Duration;

mod network;
mod governance;
mod full_content;
mod key_ballot;

fn sign(block: &mut StateBlock, seed: u8) {
    block.hash = block.compute_hash();
    block.signature = burst_crypto::sign_message(
        block.hash.as_bytes(),
        &burst_crypto::keypair_from_seed(&[seed; 32]).private,
    );
}

async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("verification fork worker did not make progress");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verification_fork_worker_replaces_endorsements_and_recovers() {
    for endorsement_wins in [false, true] {
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
        config.params.endorsement_burn_amount = 1;
        let mut node = crate::BurstNode::new(config.clone()).await.unwrap();
        node.initialize_genesis().unwrap();
        // Enable the test-only certified path, without issuing timed checkpoints.
        node.verification_fixture_clock = Some(Arc::new(|| None));
        let store = node.store.clone();
        let queue = node.block_queue();
        let verification = node.verification_orchestrator.clone();
        let elections = node.active_elections.clone();
        let shutdown = node.shutdown.clone();
        let creator = crate::genesis_key::genesis_address(NetworkId::Dev);
        let genesis = store.account_store().get_account(&creator).unwrap().head;
        let base: StateBlock =
            bincode::deserialize(&store.block_store().get_block(&genesis).unwrap()).unwrap();
        let target_key = burst_crypto::keypair_from_seed(&[9; 32]);
        let target = burst_crypto::derive_address(&target_key.public);
        let mut endorse = base.clone();
        endorse.block_type = BlockType::Endorse;
        endorse.previous = genesis;
        endorse.payment_epoch = store.meta_store().payment_epoch().unwrap();
        endorse.timestamp = Timestamp::now();
        endorse.brn_balance = 1;
        endorse.link = BlockHash::new(target_key.public.0);
        sign(&mut endorse, 0);
        let mut preference = endorse.clone();
        preference.block_type = BlockType::ChangeRepresentative;
        preference.link = BlockHash::ZERO;
        preference.brn_balance = 0;
        sign(&mut preference, 0);
        let (losing, winner) = if endorsement_wins {
            (preference, endorse)
        } else {
            (endorse, preference)
        };
        let task = tokio::spawn(async move {
            node.start().await.unwrap();
            node.stop().await.unwrap();
        });
        let result = tokio::time::timeout(Duration::from_secs(60), async {
            assert!(queue.push(losing.clone()).await);
            until(|| store.block_store().exists(&losing.hash).unwrap()).await;
            let dependent = if !endorsement_wins {
                let mut open = base.clone();
                open.payment_epoch = losing.payment_epoch;
                open.account = target.clone();
                open.representative = target.clone();
                open.timestamp = Timestamp::now();
                sign(&mut open, 9);
                assert!(queue.push(open.clone()).await);
                until(|| store.block_store().exists(&open.hash).unwrap()).await;
                Some(open.hash)
            } else {
                None
            };
            let before = verification.lock().await.snapshot().encode().unwrap();
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
                verification.lock().await.snapshot().encode().unwrap(),
                before,
                "candidate evaluation must not publish verification state"
            );
            assert!(store.block_store().exists(&losing.hash).unwrap());
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
                store.account_store().get_account(&creator).unwrap().head == winner.hash
                    && store
                        .account_store()
                        .get_account(&creator)
                        .unwrap()
                        .confirmation_height
                        == 2
            })
            .await;
            assert!(!store.block_store().exists(&losing.hash).unwrap());
            if let Some(dependent) = dependent {
                assert!(!store.block_store().exists(&dependent).unwrap());
            }
            let live = verification.lock().await;
            assert_eq!(live.get_state(&target).is_some(), endorsement_wins);
            assert_eq!(
                live.snapshot().encode().unwrap(),
                store
                    .meta_store()
                    .get_meta(crate::endorsement_commit::SNAPSHOT_KEY)
                    .unwrap()
            );
            assert_eq!(
                store.account_store().get_account(&target).is_ok(),
                endorsement_wins
            );
        })
        .await;
        shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .unwrap()
            .unwrap();
        result.expect("verification fork replacement timed out");

        let mut restarted = crate::BurstNode::new(config).await.unwrap();
        restarted.verification_fixture_clock = Some(Arc::new(|| None));
        let queue = restarted.block_queue();
        let verification = restarted.verification_orchestrator.clone();
        let shutdown = restarted.shutdown.clone();
        let task = tokio::spawn(async move {
            restarted.start().await.unwrap();
            restarted.stop().await.unwrap();
        });
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            let mut followup = winner.clone();
            followup.previous = winner.hash;
            followup.timestamp = Timestamp::now();
            followup.block_type = if endorsement_wins {
                BlockType::ChangeRepresentative
            } else {
                BlockType::Endorse
            };
            followup.link = if endorsement_wins {
                BlockHash::ZERO
            } else {
                BlockHash::new(target_key.public.0)
            };
            followup.brn_balance = 1;
            sign(&mut followup, 0);
            assert!(queue.push(followup.clone()).await);
            until(|| store.account_store().get_account(&creator).unwrap().head == followup.hash)
                .await;
            assert!(verification.lock().await.get_state(&target).is_some());
            assert!(store
                .account_store()
                .get_account(&target)
                .unwrap()
                .state
                .is_verified());
        })
        .await;
        shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .unwrap()
            .unwrap();
        result.expect("verification fork restart failed");
    }
}
