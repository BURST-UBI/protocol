use super::*;
use burst_wallet_core::{
    offline::{load_payment, payment_was_submitted, save_payment},
    signed_client::parse_hash,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn saved_openings_resolve_exact_or_competing_confirmation_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let drafts = tempfile::tempdir().unwrap();
    let config = config(directory.path());
    let client = NodeClient::new(format!("http://127.0.0.1:{}", config.rpc_port)).unwrap();
    let founder = burst_crypto::keypair_from_seed(&[0; 32]);
    let mut cases = Vec::new();
    for round in 0..2 {
        let mut node = BurstNode::new(config.clone()).await.unwrap();
        node.initialize_genesis().unwrap();
        let shutdown = node.shutdown.clone();
        let task = tokio::spawn(async move {
            node.start().await.unwrap();
            node.stop().await.unwrap();
        });
        let result = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                if client.rpc_call("params_hash", json!({})).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            if round == 0 {
                for (seed, competing) in [(81, false), (82, true)] {
                    let keys = burst_crypto::keypair_from_seed(&[seed; 32]);
                    let account = burst_crypto::derive_address(&keys.public);
                    let burn = client.pay_signed(&founder, &account, 10).await.unwrap();
                    assert!(burn.accepted);
                    client
                        .wait_confirmed(&burn.hash, Duration::from_secs(20))
                        .await
                        .unwrap();
                    let source = parse_hash(&burn.hash).unwrap().into_tx_hash();
                    let context = client
                        .prepare_offline_receive(&account, &burn.hash)
                        .await
                        .unwrap();
                    assert_eq!(context.review().amount, "10");
                    assert_eq!(context.review().payment, Some(burn.hash.clone()));
                    let draft = context.sign(&keys, Timestamp::now()).unwrap();
                    assert!(draft.previous.is_zero());
                    assert_eq!(draft.block_type, burst_ledger::BlockType::Open);
                    let path = drafts.path().join(format!("{seed}.json"));
                    save_payment(&path, &draft).unwrap();
                    let bytes = std::fs::read(&path).unwrap();
                    let review = client.review_offline_payment(&draft).await.unwrap();
                    assert_eq!(review.kind, "receive");
                    assert_eq!(review.amount, "10");
                    assert_eq!(review.hash, draft.hash.to_string());
                    assert!(client.confirmed_offline_successor(&draft).await.is_err());
                    assert_eq!(client.excluded_offline_payment(&draft).await.unwrap(), None);
                    assert_eq!(std::fs::read(&path).unwrap(), bytes);
                    let opening = if competing {
                        let other = client.pay_signed(&founder, &account, 11).await.unwrap();
                        assert!(other.accepted);
                        client
                            .wait_confirmed(&other.hash, Duration::from_secs(20))
                            .await
                            .unwrap();
                        client.receive_signed(&keys, &other.hash).await.unwrap()
                    } else {
                        client
                            .submit_saved_offline_payment(&path, &account)
                            .await
                            .unwrap()
                    };
                    assert!(opening.accepted);
                    client
                        .wait_confirmed(&opening.hash, Duration::from_secs(20))
                        .await
                        .unwrap();
                    assert_eq!(
                        payment_was_submitted(&path, &draft.hash).unwrap(),
                        !competing
                    );
                    assert_eq!(std::fs::read(&path).unwrap(), bytes);
                    cases.push((
                        keys,
                        path,
                        parse_hash(&opening.hash).unwrap(),
                        source,
                        competing,
                    ));
                }
            } else {
                for (keys, path, opening, source, competing) in &cases {
                    let draft = load_payment(path).unwrap();
                    let account = burst_crypto::derive_address(&keys.public);
                    assert_eq!(
                        client.confirmed_offline_successor(&draft).await.unwrap(),
                        Some(*opening)
                    );
                    assert_eq!(client.excluded_offline_payment(&draft).await.unwrap(), None);
                    let info = client
                        .rpc_call("account_info", json!({"account":account.as_str()}))
                        .await
                        .unwrap();
                    assert_eq!(info["confirmed_open"], opening.to_string());
                    assert_eq!(draft.hash == *opening, !competing);
                    assert_eq!(
                        payment_was_submitted(path, &draft.hash).unwrap(),
                        !competing
                    );
                    if *competing {
                        let received = client
                            .receive_signed(keys, &source.to_string())
                            .await
                            .unwrap();
                        assert!(
                            received.accepted,
                            "superseded opening must not consume the other receipt"
                        );
                        client
                            .wait_confirmed(&received.hash, Duration::from_secs(20))
                            .await
                            .unwrap();
                        let info = client
                            .rpc_call("account_info", json!({"account":account.as_str()}))
                            .await
                            .unwrap();
                        assert_eq!(info["trst_balance"], "21");
                        assert_eq!(info["confirmed_open"], opening.to_string());
                    }
                }
            }
        })
        .await;
        shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(20), task)
            .await
            .unwrap()
            .unwrap();
        result.expect("opening recovery RPC/restart journey timed out");
    }
}
