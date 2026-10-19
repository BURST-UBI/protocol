use super::*;
use crate::{WireMessage, WireVote};

pub(super) struct Network<'a> {
    pub registries: &'a [partition::Registry],
    pub signers: &'a [Arc<tokio::sync::Mutex<burst_consensus::VoteGenerator>>],
    pub elections: &'a [Arc<tokio::sync::RwLock<burst_consensus::ActiveElections>>],
}

pub(super) async fn replace(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    network: &Network<'_>,
    keys: &[KeyPair],
    (loser, winner): (&StateBlock, &StateBlock),
) {
    assert_eq!(loser.account, winner.account);
    assert_eq!(loser.previous, winner.previous);
    for signer in network.signers {
        signer.lock().await.set_representative(false);
    }
    assert!(
        clients[0]
            .process(&serde_json::to_string(loser).unwrap())
            .await
            .unwrap()
            .accepted
    );
    for store in stores {
        loop {
            if store.block_store().exists(&loser.hash).unwrap() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    let result = clients[1]
        .process(&serde_json::to_string(winner).unwrap())
        .await
        .unwrap();
    assert!(!result.accepted);
    assert!(result
        .detail
        .unwrap_or_default()
        .to_lowercase()
        .contains("fork"));
    for active in network.elections {
        loop {
            if active.read().await.candidate_root(&winner.hash).is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    let root = crate::ledger_updater::election_root(winner);
    for (index, key) in keys.iter().enumerate() {
        let mut vote = WireVote {
            voter: burst_crypto::derive_address(&key.public),
            block_hashes: vec![winner.hash],
            is_final: true,
            timestamp: Timestamp::now().as_secs() * 1000,
            sequence: 20_000,
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
            &key.private,
        );
        let bytes = WireMessage::Vote(vote).encode().unwrap();
        for registry in network.registries {
            let registry = registry.read().await;
            for peer in registry.peer_ids() {
                assert!(registry.send(peer, bytes.clone()));
            }
        }
        if index < 2 {
            for (store, active) in stores.iter().zip(network.elections) {
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        if active
                            .read()
                            .await
                            .get_election(&root)
                            .is_some_and(|election| {
                                election.final_tally.get(&winner.hash)
                                    == Some(
                                        &((index + 1) as u128
                                            * burst_consensus::rep_weights::WEIGHT_PER_HUMAN),
                                    )
                            })
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                })
                .await
                .expect("signed subquorum ballot vote was not received");
                let account = store.account_store().get_account(&winner.account).unwrap();
                assert_eq!(account.head, loser.hash);
                assert!(account.confirmation_height < account.block_count);
            }
        }
    }
    for (client, store) in clients.iter().zip(stores) {
        client
            .wait_confirmed(&winner.hash.to_string(), Duration::from_secs(20))
            .await
            .unwrap();
        assert!(!store.block_store().exists(&loser.hash).unwrap());
        assert_eq!(
            store
                .account_store()
                .get_account(&winner.account)
                .unwrap()
                .head,
            winner.hash
        );
    }
    for signer in network.signers {
        signer.lock().await.set_representative(true);
    }
}
