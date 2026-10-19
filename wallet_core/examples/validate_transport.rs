//! Repeatable NAT/firewall journey with one public dev signer and non-voting peers.
//! Independent-signer consensus is exercised separately by run-validation.sh.
use burst_types::{BlockHash, Timestamp, TxHash};
use burst_wallet_core::{
    transaction_builder::{build_merge_tx, build_reject_receive_tx},
    NodeClient, Wallet,
};
use serde_json::json;
use std::time::Duration;

type Error = Box<dyn std::error::Error>;

fn hash(value: &str) -> Result<BlockHash, Error> {
    Ok(burst_wallet_core::signed_client::parse_hash(value)?)
}

async fn confirmed(nodes: &[NodeClient], hash: &str) -> Result<(), Error> {
    for node in nodes {
        node.wait_confirmed(hash, Duration::from_secs(40)).await?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let nodes = std::env::args()
        .skip(1)
        .map(NodeClient::new)
        .collect::<Result<Vec<_>, _>>()?;
    assert!(
        nodes.len() >= 2,
        "provide at least two disposable Dev RPC URLs"
    );
    for node in &nodes {
        if node.rpc_call("node_info", json!({})).await?["network"] != "dev" {
            return Err("transport probe requires disposable Dev ledgers".into());
        }
    }
    let genesis = Wallet::from_seed(&[0; 32]);
    let recipient = Wallet::create()?;
    let seed = &nodes[0];
    let remote = nodes.last().unwrap();
    let mut origins = Vec::new();
    for _ in 0..2 {
        let payment = seed
            .pay_signed(&genesis.primary_keys, &genesis.address, 1_000_000_000_000)
            .await?;
        confirmed(&nodes, &payment.hash).await?;
        let receive = remote
            .receive_signed(&genesis.primary_keys, &payment.hash)
            .await?;
        confirmed(&nodes, &receive.hash).await?;
        origins.push(payment.hash);
    }
    let portfolio = remote
        .rpc_call(
            "trst_portfolio",
            json!({"account":genesis.address.as_str()}),
        )
        .await?;
    let sources = portfolio["batches"]
        .as_array()
        .ok_or("missing portfolio")?
        .iter()
        .filter(|batch| {
            origins
                .iter()
                .any(|origin| batch["origin"].as_str() == Some(origin.as_str()))
        })
        .map(|batch| {
            hash(batch["id"].as_str().ok_or("missing holding ID")?).map(|h| h.into_tx_hash())
        })
        .collect::<Result<Vec<_>, Error>>()?;
    assert_eq!(sources.len(), 2);
    let merge = build_merge_tx(&genesis.address, sources, Timestamp::now())?;
    let merged = remote
        .submit_signed(
            &genesis.primary_keys,
            &burst_transactions::Transaction::Merge(merge),
            TxHash::ZERO,
        )
        .await?;
    confirmed(&nodes, &merged.hash).await?;
    let origin = hash(&merged.hash)?.into_tx_hash();
    let sent = seed
        .transfer_signed(&genesis.primary_keys, &recipient.address, 100, origin)
        .await?;
    confirmed(&nodes, &sent.hash).await?;
    for node in &nodes {
        assert!(node
            .account_pending(recipient.address.as_str(), 100)
            .await?
            .blocks
            .iter()
            .any(|entry| entry.hash == sent.hash && entry.amount == "100"));
    }
    let receive = remote
        .receive_signed(&recipient.primary_keys, &sent.hash)
        .await?;
    confirmed(&nodes, &receive.hash).await?;
    let before = remote.account_info(recipient.address.as_str()).await?;
    assert!(remote
        .transfer_signed(&recipient.primary_keys, &genesis.address, 1, origin)
        .await
        .is_err());
    assert_eq!(
        remote.account_info(recipient.address.as_str()).await?.head,
        before.head
    );
    let genesis_before = seed.account_info(genesis.address.as_str()).await?;
    let declined = seed
        .transfer_signed(&genesis.primary_keys, &recipient.address, 7, origin)
        .await?;
    confirmed(&nodes, &declined.hash).await?;
    let rejection = build_reject_receive_tx(
        &recipient.address,
        hash(&declined.hash)?.into_tx_hash(),
        Timestamp::now(),
    )?;
    let rejected = remote
        .submit_signed(
            &recipient.primary_keys,
            &burst_transactions::Transaction::RejectReceive(rejection),
            TxHash::ZERO,
        )
        .await?;
    confirmed(&nodes, &rejected.hash).await?;
    let returned = seed
        .receive_signed(&genesis.primary_keys, &rejected.hash)
        .await?;
    confirmed(&nodes, &returned.hash).await?;
    assert!(seed
        .receive_signed(&genesis.primary_keys, &rejected.hash)
        .await
        .is_err());
    for node in &nodes {
        assert_eq!(
            node.account_info(genesis.address.as_str())
                .await?
                .trst_balance,
            genesis_before.trst_balance
        );
        assert_eq!(
            node.account_info(recipient.address.as_str())
                .await?
                .trst_balance,
            before.trst_balance
        );
    }
    let mut report = serde_json::Map::new();
    for address in [&genesis.address, &recipient.address] {
        let mut expected = seed
            .rpc_call("account_info", json!({"account":address.as_str()}))
            .await?;
        expected
            .as_object_mut()
            .ok_or("invalid account response")?
            .remove("brn_balance");
        for node in &nodes {
            let mut actual = node
                .rpc_call("account_info", json!({"account":address.as_str()}))
                .await?;
            actual
                .as_object_mut()
                .ok_or("invalid account response")?
                .remove("brn_balance");
            assert_eq!(
                actual, expected,
                "account disagreement after transport journey"
            );
            assert_eq!(actual["confirmation_height"], actual["block_count"]);
        }
        report.insert(address.to_string(), expected);
    }
    if let Ok(path) = std::env::var("BURST_VALIDATION_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    }
    println!("PASS signed burns, receives, merge, transfer, refusal, return and identical confirmed accounts on {} nodes", nodes.len());
    println!("Transport-only fixture: one dev representative; no distributed-quorum claim");
    Ok(())
}
