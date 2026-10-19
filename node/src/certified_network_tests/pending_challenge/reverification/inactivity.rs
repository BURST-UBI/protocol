use super::*;

pub(super) async fn transfer_originated_trst(
    clients: &[NodeClient],
    params: &ProtocolParams,
    addresses: &[WalletAddress],
    now: Timestamp,
) {
    let pending = clients[0]
        .rpc_call("account_pending", json!({"account":addresses[0].as_str()}))
        .await
        .unwrap();
    let entry = pending["pending"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["source"] == addresses[4].as_str())
        .unwrap();
    assert_eq!(entry["amount"], BRN_UNIT.to_string());
    let source = TxHash::new(
        hex::decode(entry["hash"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap(),
    );
    let holder = burst_crypto::keypair_from_seed(&[0; 32]);
    let claim = signed_block(
        &clients[0],
        &holder,
        Transaction::Receive(build_receive_tx(&addresses[0], source, BRN_UNIT, now).unwrap()),
        params,
    )
    .await;
    submit(clients, &claim).await;
    let transfer = signed_block(
        &clients[0],
        &holder,
        Transaction::Send(
            build_send_tx(
                &addresses[0],
                &addresses[1],
                BRN_UNIT,
                claim.hash.into_tx_hash(),
                source,
                now,
            )
            .unwrap(),
        ),
        params,
    )
    .await;
    submit(clients, &transfer).await;
    let receiver = burst_crypto::keypair_from_seed(&[1; 32]);
    let received = signed_block(
        &clients[0],
        &receiver,
        Transaction::Receive(
            build_receive_tx(&addresses[1], transfer.hash.into_tx_hash(), BRN_UNIT, now).unwrap(),
        ),
        params,
    )
    .await;
    submit(clients, &received).await;
    for client in clients {
        let status = client
            .rpc_call(
                "verification_status",
                json!({"account":addresses[4].as_str()}),
            )
            .await
            .unwrap();
        assert_eq!(status["status"], "deactivated");
        let portfolio = client
            .rpc_call("trst_portfolio", json!({"account":addresses[1].as_str()}))
            .await
            .unwrap();
        assert!(
            portfolio["batches"]
                .as_array()
                .unwrap()
                .iter()
                .any(|batch| batch["origin"] == source.to_string()
                    && batch["amount"] == BRN_UNIT.to_string()
                    && batch["originator"] == addresses[4].as_str()
                    && batch["status"] == "active"),
            "inactivity must not revoke the originator's TRST"
        );
    }
}
