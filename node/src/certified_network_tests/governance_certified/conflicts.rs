use super::*;
use burst_governance::{proposal::AmendmentOp, ProposalContent};

pub(super) async fn check_document(client: &NodeClient) {
    let document = client.rpc_call("consti", json!({})).await.unwrap();
    assert_eq!(document["version"], 2);
    let articles = document["articles"].as_array().unwrap();
    assert_eq!(
        articles.len(),
        3,
        "the conflicted patch must not leak its addition"
    );
    assert!(articles.iter().all(|article| article["id"] != "1"));
    assert_eq!(
        articles
            .iter()
            .find(|article| article["id"] == "2")
            .unwrap()["text"],
        "Independent update"
    );
}

pub(super) async fn activate(
    clients: &[NodeClient],
    stores: &[Arc<burst_store_lmdb::LmdbStore>],
    clock: &AtomicU64,
    keys: &[KeyPair],
    rules: (&ProtocolParams, &ProtocolParams),
    base: u64,
) -> (StateBlock, BlockHash, Vec<(BlockHash, &'static str)>) {
    let (initial, governed) = rules;
    let owner = burst_crypto::derive_address(&keys[0].public);
    let mut proposals = Vec::new();
    for index in 0..3 {
        let operations = if index < 2 {
            vec![
                AmendmentOp::AddArticle {
                    title: format!("Patch {index}"),
                    text: "New article".into(),
                },
                AmendmentOp::RepealArticle { article_number: 1 },
            ]
        } else {
            vec![AmendmentOp::ModifyArticle {
                article_number: 2,
                new_text: "Independent update".into(),
            }]
        };
        let content = ProposalContent::ConstitutionalDiff {
            title: format!("Independent amendment {index}"),
            operations,
        };
        let state = clients[0].signing_state(&owner).await.unwrap();
        let (proposal, body) = burst_wallet_core::governance_content::build_signed_proposal(
            &state,
            &keys[0],
            &content,
            governed.params_hash(),
            Timestamp::new(base + 64),
        )
        .unwrap();
        let response = clients[0].rpc_call("process", json!({
            "block": serde_json::to_string(&proposal).unwrap(), "proposal_content": hex::encode(body.bytes()),
        })).await.unwrap();
        assert_eq!(response["accepted"], true, "{response}");
        for client in clients {
            client
                .wait_confirmed(&proposal.hash.to_string(), Duration::from_secs(25))
                .await
                .unwrap();
        }
        let tx = Transaction::ChangeRepresentative(
            build_change_rep_tx(&owner, &owner, Timestamp::new(base + 64)).unwrap(),
        );
        let mut support = signed_block(&clients[0], &keys[0], tx, governed).await;
        support.block_type = BlockType::GovernanceEndorse;
        support.link = proposal.hash;
        support.brn_balance += governed.governance_proposal_cost;
        support.hash = support.compute_hash();
        submit(clients, &sign_state_block(support, &keys[0].private)).await;
        proposals.push(proposal.hash);
    }
    clock.store(base + 75, Ordering::SeqCst);
    for proposal in &proposals {
        for key in keys {
            let owner = burst_crypto::derive_address(&key.public);
            let tx = Transaction::GovernanceVote(
                build_governance_vote_tx(
                    &owner,
                    proposal.into_tx_hash(),
                    burst_transactions::governance::GovernanceVote::Yea,
                    Timestamp::new(base + 75),
                )
                .unwrap(),
            );
            submit(clients, &signed_block(&clients[0], key, tx, governed).await).await;
        }
    }
    clock.store(base + 97, Ordering::SeqCst);
    installed(stores, 4, initial).await;
    clock.store(base + 108, Ordering::SeqCst);
    let epoch = installed(stores, 5, initial).await;
    let winner = if proposals[0].as_bytes() < proposals[1].as_bytes() {
        0
    } else {
        1
    };
    let mut outcomes = Vec::new();
    for (index, proposal) in proposals.iter().enumerate() {
        let phase = if index < 2 && index != winner {
            "Conflicted"
        } else {
            "Activated"
        };
        outcomes.push((*proposal, phase));
        for client in clients {
            let view = client
                .rpc_call(
                    "governance_proposal_info",
                    json!({"hash": proposal.to_string()}),
                )
                .await
                .unwrap();
            assert_eq!(view["phase"], phase);
            assert_eq!(view["votes_yea"], 3);
            assert_eq!(view["phase_deadline"], 0);
            if phase == "Conflicted" {
                assert_eq!(
                    view["activation_conflict"],
                    json!({"ArticleAlreadyRepealed": 1})
                );
            }
        }
    }
    for client in clients {
        check_document(client).await;
    }
    for store in stores {
        let engine: burst_consti::ConstiEngine =
            bincode::deserialize(&store.meta_store().get_meta("consti_engine").unwrap()).unwrap();
        assert_eq!(
            engine.get_constitution().get_article(4).unwrap().title,
            format!("Patch {winner}")
        );
        let events = chain::load(store, NetworkId::Dev, initial).unwrap();
        assert_eq!(events[4].certificate().signatures.len(), 3);
    }
    let tx = Transaction::Burn(
        build_burn_tx(
            &owner,
            &burst_crypto::derive_address(&keys[1].public),
            1,
            Timestamp::new(base + 108),
        )
        .unwrap(),
    );
    let payment = signed_block(&clients[0], &keys[0], tx, governed).await;
    assert_eq!(payment.payment_epoch, epoch);
    submit(clients, &payment).await;
    (payment, epoch, outcomes)
}
