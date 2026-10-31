//! Certified onboarding with real elapsed time and a governed fixture BRN rate.
//! Bootstrap helpers use genesis endorsements on the disposable private Test ledger;
//! the subject uses paid endorsements and selected-verifier votes.
//! `continue-selection` resumes saved ledger state; it does not test fresh onboarding.
use burst_transactions::Transaction;
use burst_types::{Timestamp, TxHash, BRN_UNIT};
use burst_wallet_core::{signed_client::parse_hash, transaction_builder::*, NodeClient, Wallet};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

type Error = Box<dyn std::error::Error>;

async fn confirmed(nodes: &[NodeClient], hash: &str) -> Result<(), Error> {
    for node in nodes {
        node.wait_confirmed(hash, Duration::from_secs(60)).await?;
    }
    Ok(())
}

async fn status(node: &NodeClient, wallet: &Wallet) -> Result<Value, Error> {
    Ok(node
        .rpc_call(
            "verification_status",
            json!({"account": wallet.address.as_str()}),
        )
        .await?)
}

fn comparable_status(mut status: Value) -> Value {
    // Replay may visit independent account chains in another order. Preserve
    // every field and the selected panel order; only these lists are sets.
    status["endorsers"]
        .as_array_mut()
        .expect("endorsers")
        .sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    status["verifier_votes"]
        .as_array_mut()
        .expect("verifier votes")
        .sort_by(|a, b| a["verifier"].as_str().cmp(&b["verifier"].as_str()));
    status
}

async fn open_delegated(
    nodes: &[NodeClient],
    founder: &Wallet,
    wallet: &Wallet,
) -> Result<(), Error> {
    let send = nodes[0]
        .pay_signed(&founder.primary_keys, &wallet.address, BRN_UNIT / 1000)
        .await?;
    confirmed(nodes, &send.hash).await?;
    let receive = nodes[0]
        .receive_signed(&wallet.primary_keys, &send.hash)
        .await?;
    confirmed(nodes, &receive.hash).await?;
    let delegate = build_change_rep_tx(&wallet.address, &founder.address, Timestamp::now())?;
    let changed = nodes[0]
        .submit_signed(
            &wallet.primary_keys,
            &Transaction::ChangeRepresentative(delegate),
            TxHash::ZERO,
        )
        .await?;
    confirmed(nodes, &changed.hash).await
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().ok_or("provide advance or verify")?;
    let report = args.next().ok_or("provide evidence path")?;
    let nodes = args.map(NodeClient::new).collect::<Result<Vec<_>, _>>()?;
    assert!(!nodes.is_empty());
    let founder = Wallet::from_seed(&[42; 32]);
    let subject = Wallet::from_seed(&[91; 32]);
    for node in &nodes {
        assert_eq!(
            node.rpc_call("node_info", json!({})).await?["network"],
            "test"
        );
        let founder_state = if mode.starts_with("verify") {
            node.inspect_signing_state(&founder.address).await?
        } else {
            node.signing_state(&founder.address).await?
        };
        assert!(!founder_state.head.is_zero(), "wrong Test fixture identity");
        let parameters = node.rpc_call("protocol_params", json!({})).await?;
        assert_eq!(
            parameters["brn_rate"],
            (100 * BRN_UNIT).to_string(),
            "requires the explicitly governed disposable accrual rate"
        );
        assert_eq!(
            parameters["verifier_stake_amount"],
            (500 * BRN_UNIT).to_string()
        );
    }
    if mode == "advance" || mode == "continue-selection" || mode == "bootstrap-only" {
        assert!(
            !std::path::Path::new(&report).exists(),
            "do not resubmit a completed journey"
        );
        let helpers: Vec<_> = (100..149)
            .map(|seed| Wallet::from_seed(&[seed; 32]))
            .collect();
        if mode == "advance" || mode == "bootstrap-only" {
            assert_eq!(status(&nodes[0], &subject).await?["status"], "unverified");
            for (index, helper) in helpers.iter().enumerate() {
                // Delegate while unverified so this test's offline wallet keys do
                // not strand equal-human quorum after their bootstrap endorsement.
                if status(&nodes[0], helper).await?["status"] != "verified" {
                    open_delegated(&nodes, &founder, helper).await?;
                    let endorsement = nodes[0]
                        .rpc_call(
                            "genesis_endorse",
                            json!({"account":helper.address.as_str()}),
                        )
                        .await?;
                    confirmed(
                        &nodes,
                        endorsement["block_hash"]
                            .as_str()
                            .ok_or("missing bootstrap block")?,
                    )
                    .await?;
                }
                for node in &nodes {
                    assert_eq!(status(node, helper).await?["status"], "verified");
                }
                if index < 7 {
                    if status(&nodes[0], helper).await?["verifier_opted_in"] != true {
                        let opt_in = nodes[0]
                            .set_verifier_opt_in_signed(&helper.primary_keys, true)
                            .await?;
                        confirmed(&nodes, &opt_in.hash).await?;
                    }
                    for node in &nodes {
                        assert_eq!(status(node, helper).await?["verifier_opted_in"], true);
                    }
                }
                println!("PASS helper bootstrap/opt-in state {}/49", index + 1);
            }
            if mode == "bootstrap-only" {
                use std::io::Write;
                let evidence = json!({"kind":"certified-helper-baseline", "founder":founder.address,
                    "helpers":helpers.iter().map(|wallet| wallet.address.as_str()).collect::<Vec<_>>(),
                    "verified_helpers":49,"opted_in_helpers":7,
                    "subject_verified":false});
                let mut output = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&report)?;
                output.write_all(&serde_json::to_vec_pretty(&evidence)?)?;
                output.sync_all()?;
                println!("PASS certified helper baseline only; consumer onboarding has not run");
                return Ok(());
            }
            open_delegated(&nodes, &founder, &subject).await?;
            // BRN accrues from actual elapsed time. No faucet, injected balance,
            // fixture clock, precomputed panel or bypassed beacon is used.
            tokio::time::sleep(Duration::from_secs(6)).await;
            for endorser in &helpers[7..10] {
                let transaction = build_endorse_tx(
                    &endorser.address,
                    &subject.address,
                    336 * BRN_UNIT,
                    Timestamp::now(),
                )?;
                let submitted = nodes[0]
                    .submit_signed(
                        &endorser.primary_keys,
                        &Transaction::Endorse(transaction),
                        TxHash::ZERO,
                    )
                    .await?;
                confirmed(&nodes, &submitted.hash).await?;
            }
            println!("PASS three normal paid endorsements confirmed; waiting for certified beacon selection");
        } else {
            for node in &nodes {
                let current = status(node, &subject).await?;
                assert_eq!(
                    current["endorser_count"], 3,
                    "resume requires completed paid endorsements"
                );
                for endorser in &helpers[7..10] {
                    assert!(current["endorsers"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|entry| entry == endorser.address.as_str()));
                }
                assert_eq!(
                    node.signing_state(&subject.address).await?.representative,
                    founder.address
                );
            }
            println!("PASS resuming the existing paid endorsements without resubmission");
        }
        let started = Instant::now();
        let panel = loop {
            let current = status(&nodes[0], &subject).await?;
            if current["selected_verifiers"]
                .as_array()
                .is_some_and(|panel| panel.len() == 7)
                && current["selection_context"].as_str().is_some()
            {
                break current;
            }
            assert!(
                started.elapsed() < Duration::from_secs(240),
                "certified selection stalled: {current}"
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        };
        let context = parse_hash(panel["selection_context"].as_str().unwrap())?;
        let selected = panel["selected_verifiers"].as_array().unwrap();
        for verifier in &helpers[..7] {
            assert!(
                selected
                    .iter()
                    .any(|entry| entry == verifier.address.as_str()),
                "unexpected panel: {panel}"
            );
            let current = status(&nodes[0], &subject).await?;
            if current["verifier_votes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|vote| vote["verifier"] == verifier.address.as_str())
            {
                continue;
            }
            let started = Instant::now();
            for node in &nodes {
                loop {
                    let assignments = node
                        .rpc_call(
                            "verifier_assignments",
                            json!({"account":verifier.address.as_str()}),
                        )
                        .await?;
                    if assignments["targets"].as_array().is_some_and(|targets| {
                        targets
                            .iter()
                            .any(|entry| entry == subject.address.as_str())
                    }) {
                        break;
                    }
                    assert!(
                        started.elapsed() < Duration::from_secs(60),
                        "assignment not visible: {assignments}"
                    );
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
            let transaction = build_verification_vote_tx(
                &verifier.address,
                &subject.address,
                context,
                1,
                500 * BRN_UNIT,
                Timestamp::now(),
            )?;
            let submitted = nodes[0]
                .submit_signed(
                    &verifier.primary_keys,
                    &Transaction::VerificationVote(transaction),
                    TxHash::ZERO,
                )
                .await?;
            confirmed(&nodes, &submitted.hash).await?;
            println!("PASS selected human vote confirmed: {}", verifier.address);
        }
        let started = Instant::now();
        for node in &nodes {
            loop {
                let current = status(node, &subject).await?;
                if current["status"] == "verified" {
                    break;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(180),
                    "verification settlement stalled: {current}"
                );
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
        let final_status = status(&nodes[0], &subject).await?;
        assert_eq!(final_status["endorser_count"], 3);
        assert_eq!(final_status["verifier_votes"].as_array().unwrap().len(), 7);
        tokio::time::sleep(Duration::from_secs(1)).await;
        let payment = nodes[0]
            .pay_signed(&subject.primary_keys, &founder.address, BRN_UNIT)
            .await?;
        confirmed(&nodes, &payment.hash).await?;
        let receive = nodes[0]
            .receive_signed(&founder.primary_keys, &payment.hash)
            .await?;
        confirmed(&nodes, &receive.hash).await?;
        std::fs::write(
            &report,
            serde_json::to_vec_pretty(
                &json!({"subject_status":final_status, "payment":payment.hash, "receive":receive.hash}),
            )?,
        )?;
        println!("PASS normally verified subject accrued BRN, burned it and completed a received payment");
    } else if mode != "verify" && mode != "verify-joining" {
        return Err("expected advance, continue-selection, verify or verify-joining".into());
    }
    let expected: Value = serde_json::from_slice(&std::fs::read(&report)?)?;
    let account = expected["subject_status"]["account"]
        .as_str()
        .ok_or("missing verification account")?;
    // A cold join downloads the whole bootstrap history before the payment.
    // Keep ordinary confirmation/restart checks at their shorter deadline.
    let recovery_wait = Duration::from_secs(if mode == "verify-joining" { 180 } else { 60 });
    for node in &nodes {
        node.wait_confirmed(expected["payment"].as_str().unwrap(), recovery_wait)
            .await?;
        node.wait_confirmed(
            expected["receive"].as_str().unwrap(),
            Duration::from_secs(60),
        )
        .await?;
        for operation in ["challenge", "appeal"] {
            if let Some(hash) = expected.get(operation) {
                let hash = hash.as_str().ok_or("invalid saved lifecycle block hash")?;
                node.wait_confirmed(hash, Duration::from_secs(60)).await?;
            }
        }
        assert_eq!(
            comparable_status(
                node.rpc_call("verification_status", json!({"account":account}))
                    .await?
            ),
            comparable_status(expected["subject_status"].clone()),
            "verification state changed across recovery"
        );
        if let Some(balance) = expected.get("receiver_balance") {
            let portfolio = node
                .rpc_call("trst_portfolio", json!({"account":expected["receiver"]}))
                .await?;
            for field in ["active", "expired", "revoked", "pending"] {
                assert_eq!(
                    portfolio[format!("total_{field}")],
                    balance[field],
                    "receiver {field} changed across recovery"
                );
            }
        }
    }
    println!(
        "PASS certified verification state and post-verification payment on {} nodes",
        nodes.len()
    );
    Ok(())
}
