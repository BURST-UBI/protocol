//! Normal-daemon Test governance acceptance using the disposable [42; 32] founder.
use burst_governance::GovernableParam;
use burst_transactions::{governance::GovernanceVote, Transaction};
use burst_types::{Timestamp, TxHash};
use burst_wallet_core::{
    signed_client::parse_hash, transaction_builder::build_governance_vote_tx, NodeClient,
};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().ok_or("provide advance or verify")?;
    let report = args.next().ok_or("provide evidence path")?;
    let nodes = args.map(NodeClient::new).collect::<Result<Vec<_>, _>>()?;
    assert!(!nodes.is_empty(), "provide disposable Test RPC URLs");
    let keys = burst_crypto::keypair_from_seed(&[42; 32]);
    let founder = burst_crypto::derive_address(&keys.public);
    let accelerating = mode == "advance-test-accrual";
    let parameter = if accelerating {
        GovernableParam::BrnRate
    } else {
        GovernableParam::MinVerifierBrnBalance
    };
    let value = if accelerating {
        100 * burst_types::BRN_UNIT
    } else {
        1
    };
    for node in &nodes {
        assert_eq!(
            node.rpc_call("node_info", json!({})).await?["network"],
            "test"
        );
        let state = if mode.starts_with("verify") {
            node.inspect_signing_state(&founder).await?
        } else {
            node.signing_state(&founder).await?
        };
        assert!(
            !state.head.is_zero(),
            "not the private-testnet fixture ledger"
        );
    }
    let id =
        match mode.as_str() {
            "advance" | "advance-test-accrual" => {
                assert!(
                    !std::path::Path::new(&report).exists(),
                    "evidence already exists; do not resubmit"
                );
                let submitted = nodes[0]
                    .propose_parameter_signed(&keys, parameter, value)
                    .await?;
                assert!(submitted.accepted, "{:?}", submitted.detail);
                let id = parse_hash(&submitted.hash)?.into_tx_hash();
                std::fs::write(
                    &report,
                    serde_json::to_vec_pretty(
                        &json!({"proposal": id.to_string(), "test_accrual": accelerating}),
                    )?,
                )?;
                for node in &nodes {
                    node.wait_confirmed(&submitted.hash, Duration::from_secs(60))
                        .await?;
                }
                println!("PASS signed parameter proposal confirmed on all nodes: {id}");
                let started = Instant::now();
                let mut voted = false;
                loop {
                    let proposal = nodes[0]
                        .rpc_call("governance_proposal_info", json!({"hash":id.to_string()}))
                        .await?;
                    let phase = proposal["phase"].as_str().ok_or("missing proposal phase")?;
                    if phase == "Activated" {
                        break;
                    }
                    assert!(
                        !matches!(phase, "Rejected" | "Withdrawn" | "Expired"),
                        "proposal ended: {proposal}"
                    );
                    assert!(
                        started.elapsed() < Duration::from_secs(300),
                        "governance stalled: {proposal}"
                    );
                    if phase == "Exploration" && !voted {
                        let vote = build_governance_vote_tx(
                            &founder,
                            id,
                            GovernanceVote::Yea,
                            Timestamp::now(),
                        )?;
                        let submitted = nodes[0]
                            .submit_signed(&keys, &Transaction::GovernanceVote(vote), TxHash::ZERO)
                            .await?;
                        assert!(submitted.accepted, "{:?}", submitted.detail);
                        for node in &nodes {
                            node.wait_confirmed(&submitted.hash, Duration::from_secs(60))
                                .await?;
                        }
                        voted = true;
                        println!("PASS signed governance vote confirmed on all nodes");
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                assert!(voted, "activation occurred without our approved vote");
                id.to_string()
            }
            "verify" | "verify-joining" | "verify-snapshot" => {
                let value: Value = serde_json::from_slice(&std::fs::read(&report)?)?;
                value["proposal"]
                    .as_str()
                    .ok_or("missing proposal id")?
                    .to_owned()
            }
            _ => return Err(
                "expected advance, advance-test-accrual, verify, verify-joining or verify-snapshot"
                    .into(),
            ),
        };
    let saved: Value = serde_json::from_slice(&std::fs::read(&report)?)?;
    let accelerating = saved["test_accrual"] == true;
    let recovering = matches!(mode.as_str(), "verify-joining" | "verify-snapshot");
    let mut proposals = vec![id.as_str()];
    if let Some(native) = saved.get("native_constitution") {
        proposals.push(native["proposal"].as_str().ok_or("missing amendment id")?);
    }
    for node in &nodes {
        for id in &proposals {
            if mode == "verify-joining" {
                node.wait_confirmed(id, Duration::from_secs(180)).await?;
            }
            let started = Instant::now();
            loop {
                let proposal = node
                    .rpc_call("governance_proposal_info", json!({"hash":id}))
                    .await?;
                if proposal["phase"] == "Activated" {
                    assert_eq!(
                        proposal["phase_projected"], false,
                        "activation must be certified"
                    );
                    break;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(if recovering { 180 } else { 60 }),
                    "activation not recovered: {proposal}"
                );
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        let params = node.rpc_call("protocol_params", json!({})).await?;
        let verifier_balance = params["governable_parameters"]
            .as_array()
            .ok_or("missing governable parameters")?
            .iter()
            .find(|value| value["name"] == "min_verifier_brn_balance")
            .ok_or("missing verifier balance parameter")?;
        if let Some(expected) = saved.get("expected_parameters") {
            let expected = expected.as_object().ok_or("invalid expected parameters")?;
            assert!(expected.contains_key("min_verifier_brn_balance"));
            assert!(expected.contains_key("min_verification_age_secs"));
            for (name, value) in expected {
                let actual = params["governable_parameters"].as_array().unwrap()
                    .iter().find(|row| row["name"] == name.as_str())
                    .ok_or("expected parameter absent from node")?;
                assert_eq!(&actual["current"], value, "governed value changed: {name}");
            }
        } else {
            assert_eq!(
                verifier_balance["current"],
                if accelerating {
                    (50 * burst_types::BRN_UNIT).to_string()
                } else {
                    "1".into()
                }
            );
        }
        assert_eq!(
            params["brn_rate"],
            if accelerating {
                (100 * burst_types::BRN_UNIT).to_string()
            } else {
                burst_types::ProtocolParams::BRN_RATE_1_PER_HOUR.to_string()
            }
        );
        if let Some(native) = saved.get("native_constitution") {
            assert_eq!(
                node.rpc_call("consti", json!({})).await?,
                native["document"]
            );
            let proposal = node
                .rpc_call(
                    "governance_proposal_info",
                    json!({"hash":native["proposal"]}),
                )
                .await?;
            assert_eq!(proposal["votes_yea"], native["votes_yea"]);
            assert_eq!(proposal["votes_nay"], 0);
            let started = Instant::now();
            loop {
                let delegation = node
                    .rpc_call("delegation_keys", native["delegation_query"].clone())
                    .await?;
                if delegation["entries"] == json!([native["delegation"]]) {
                    break;
                }
                // The revocation happened after the adopted amendment. Recovering
                // that checkpoint alone does not mean its later blocks have arrived.
                assert!(
                    started.elapsed() < Duration::from_secs(if recovering { 180 } else { 60 }),
                    "post-count delegation state did not recover: {delegation}"
                );
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            let age = params["governable_parameters"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["name"] == "min_verification_age_secs")
                .ok_or("missing governed age")?;
            assert_eq!(
                age["current"].as_str(),
                Some(saved["expected_parameters"]["min_verification_age_secs"].as_str().unwrap_or("1"))
            );
        }
    }
    println!("PASS governed parameter activated on {} nodes; disposable test accrual override: {accelerating}", nodes.len());
    Ok(())
}
