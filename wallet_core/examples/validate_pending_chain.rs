//! Hard-crash recovery probe for the isolated Dev cluster in run-validation.sh.
use burst_types::{Timestamp, TxHash};
use burst_wallet_core::{
    signed_client::parse_hash,
    transaction_builder::{build_burn_tx, build_state_block, sign_state_block},
    NodeClient, Wallet,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::{Duration, Instant};

#[derive(Serialize, Deserialize)]
struct PendingChain {
    payer: String,
    recipient: String,
    hashes: Vec<String>,
    amounts: Vec<String>,
    block_count: u64,
    total_brn_burned: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mode = args
        .next()
        .ok_or("provide ready, stage, verify-isolated or verify")?;
    let path = args.next().ok_or("provide an evidence file")?;
    let nodes = args.map(NodeClient::new).collect::<Result<Vec<_>, _>>()?;
    assert!(!nodes.is_empty(), "provide disposable Dev RPC URLs");
    for node in &nodes {
        assert_eq!(
            node.rpc_call("node_info", json!({})).await?["network"],
            "dev"
        );
    }
    match mode.as_str() {
        "ready" => {
            let survivor = Wallet::from_seed(&[2; 32]);
            let started = Instant::now();
            loop {
                let mut rosters = Vec::new();
                for node in &nodes {
                    let response = node.rpc_call("representatives", json!({})).await?;
                    let mut roster = std::collections::BTreeMap::new();
                    for rep in response["representatives"]
                        .as_array()
                        .ok_or("missing representatives")?
                    {
                        let account = rep["account"].as_str().ok_or("missing account")?;
                        let weight: u128 =
                            rep["weight"].as_str().ok_or("missing weight")?.parse()?;
                        roster.insert(account.to_owned(), weight);
                    }
                    rosters.push(roster);
                }
                let roster = &rosters[0];
                let total: u128 = roster.values().sum();
                let local = roster.get(survivor.address.as_str()).copied().unwrap_or(0);
                if local > 0 && local * 2 < total && rosters.iter().all(|r| r == roster) {
                    println!("PASS all nodes installed the same authority; survivor weight {local}/{total}");
                    break;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(180),
                    "authority did not converge before partition: {rosters:?}"
                );
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        "stage" => {
            assert_eq!(nodes.len(), 1, "stage on the sole surviving representative");
            let node = &nodes[0];
            let started = Instant::now();
            while node.rpc_call("peers", json!({})).await?["count"] != 0 {
                assert!(
                    started.elapsed() < Duration::from_secs(40),
                    "peers did not disconnect"
                );
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            let survivor = Wallet::from_seed(&[2; 32]);
            let reps = node.rpc_call("representatives", json!({})).await?;
            let reps = reps["representatives"]
                .as_array()
                .ok_or("missing representatives")?;
            let mut total = 0u128;
            let mut local = 0u128;
            for rep in reps {
                let weight: u128 = rep["weight"].as_str().ok_or("missing weight")?.parse()?;
                total += weight;
                if rep["account"] == survivor.address.as_str() {
                    local = weight;
                }
            }
            assert!(
                local > 0 && local * 2 < total,
                "survivor must represent a minority"
            );
            let keys = burst_crypto::keypair_from_seed(&[0; 32]);
            let payer = burst_crypto::derive_address(&keys.public);
            let recipient = Wallet::create()?.address;
            let before = node.account_info(payer.as_str()).await?;
            assert_eq!(before.confirmation_height, before.block_count);
            let params = node.rpc_call("params_hash", json!({})).await?;
            let params_hash = parse_hash(
                params["params_hash"]
                    .as_str()
                    .ok_or("missing params hash")?,
            )?;
            let mut report = PendingChain {
                payer: payer.to_string(),
                recipient: recipient.to_string(),
                hashes: Vec::new(),
                amounts: Vec::new(),
                block_count: before.block_count + 2,
                total_brn_burned: (before.total_brn_burned.parse::<u128>()? + 3_000_000_000_000)
                    .to_string(),
            };
            for amount in [1_000_000_000_000, 2_000_000_000_000] {
                let state = node.signing_state(&payer).await?;
                if let Some(previous) = report.hashes.last() {
                    assert_eq!(state.head.to_string(), *previous);
                }
                let tx = burst_transactions::Transaction::Burn(build_burn_tx(
                    &payer,
                    &recipient,
                    amount,
                    Timestamp::now(),
                )?);
                let mut block = sign_state_block(
                    build_state_block(&state, &tx, TxHash::ZERO, params_hash)?,
                    &keys.private,
                );
                let work = node.rpc_call("work_generate", json!({"hash":block.work_root().to_string(), "block_type":block.block_type})).await?;
                block.work = u64::from_str_radix(
                    work["work"]
                        .as_str()
                        .ok_or("missing work")?
                        .trim_start_matches("0x"),
                    16,
                )?;
                let submitted = node.process(&serde_json::to_string(&block)?).await?;
                assert!(submitted.accepted, "{:?}", submitted.detail);
                assert_eq!(submitted.hash, block.hash.to_string());
                report.hashes.push(submitted.hash);
                report.amounts.push(amount.to_string());
            }
            tokio::time::sleep(Duration::from_secs(4)).await;
            for hash in &report.hashes {
                assert_eq!(
                    node.rpc_call("block_info", json!({"hash":hash})).await?["confirmed"],
                    false
                );
            }
            assert_eq!(
                node.account_info(payer.as_str()).await?.confirmation_height,
                before.confirmation_height
            );
            std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
            println!("PASS two signed payments remain unconfirmed with only a minority online");
        }
        "verify-isolated" => {
            assert_eq!(nodes.len(), 1, "check only the restarted minority");
            let node = &nodes[0];
            let report: PendingChain = serde_json::from_slice(&std::fs::read(path)?)?;
            assert_eq!(report.hashes.len(), 2);
            let survivor = Wallet::from_seed(&[2; 32]);
            for sample in 0..5 {
                if sample > 0 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                assert_eq!(node.rpc_call("peers", json!({})).await?["count"], 0);
                let response = node.rpc_call("representatives", json!({})).await?;
                let mut total = 0u128;
                let mut local = 0u128;
                for rep in response["representatives"]
                    .as_array()
                    .ok_or("missing representatives")?
                {
                    let weight: u128 = rep["weight"].as_str().ok_or("missing weight")?.parse()?;
                    total += weight;
                    if rep["account"] == survivor.address.as_str() {
                        local = weight;
                    }
                }
                assert!(
                    local > 0 && local * 2 < total,
                    "restart lost minority authority"
                );
                for hash in &report.hashes {
                    assert_eq!(
                        node.rpc_call("block_info", json!({"hash":hash})).await?["confirmed"],
                        false
                    );
                }
                let account = node.account_info(&report.payer).await?;
                assert_eq!(account.head, report.hashes[1]);
                assert_eq!(account.block_count, report.block_count);
                assert_eq!(account.confirmation_height, report.block_count - 2);
                assert_eq!(account.total_brn_burned, report.total_brn_burned);
            }
            println!(
                "PASS isolated hard restart retains both signed payments without minority finality"
            );
        }
        "verify" => {
            let report: PendingChain = serde_json::from_slice(&std::fs::read(path)?)?;
            for node in &nodes {
                for hash in &report.hashes {
                    node.wait_confirmed(hash, Duration::from_secs(60)).await?;
                }
                let account = node.account_info(&report.payer).await?;
                assert_eq!(
                    account.head,
                    *report.hashes.last().ok_or("empty payment chain")?
                );
                assert_eq!(account.block_count, report.block_count);
                assert_eq!(account.confirmation_height, report.block_count);
                assert_eq!(account.total_brn_burned, report.total_brn_burned);
                let pending = node.account_pending(&report.recipient, 100).await?;
                assert_eq!(
                    pending.blocks.len(),
                    2,
                    "payments must remain receivable, not auto-accepted"
                );
                for (hash, amount) in report.hashes.iter().zip(&report.amounts) {
                    assert_eq!(
                        &pending
                            .blocks
                            .iter()
                            .find(|p| &p.hash == hash)
                            .ok_or("missing pending payment")?
                            .amount,
                        amount
                    );
                }
            }
            println!("PASS pending payment chain confirmed after hard restart, without resubmission, on {} nodes", nodes.len());
        }
        _ => return Err("expected ready, stage, verify-isolated or verify".into()),
    }
    Ok(())
}
