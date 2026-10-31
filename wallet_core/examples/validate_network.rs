//! Signed wallet operations on a disposable Dev network, with bootstrap endorsements.
//! Supply one or more RPC URLs; multiple endpoints enable cross-node assertions.
//! `--verify` compares saved state, while `--load` measures concurrent burn/receive pairs.
use burst_wallet_core::signed_client::parse_hash;
use burst_wallet_core::{NodeClient, Wallet};
use serde_json::{json, Value};
use std::time::Duration;

#[path = "validate_network/load.rs"]
mod load;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut urls: Vec<_> = std::env::args().skip(1).collect();
    if urls.first().map(String::as_str) == Some("--load-preflight") {
        return load::preflight(&urls[1..]).await.map_err(|error| error as Box<dyn std::error::Error>);
    }
    if urls.first().map(String::as_str) == Some("--load") {
        return load::execute(&urls[1..]).await.map_err(|error| error as Box<dyn std::error::Error>);
    }
    let experimental = urls.first().map(String::as_str) == Some("--experimental-testnet");
    if experimental {
        urls.remove(0);
    }
    let verify = if urls.first().map(String::as_str) == Some("--verify") {
        urls.remove(0);
        if urls.is_empty() {
            return Err("--verify requires an evidence file".into());
        }
        Some(urls.remove(0))
    } else {
        None
    };
    let epoch_source = if urls.first().map(String::as_str) == Some("--epoch-source") {
        urls.remove(0);
        if verify.is_none() || urls.is_empty() {
            return Err("--epoch-source requires --verify and a reference RPC URL".into());
        }
        Some(NodeClient::new(urls.remove(0))?)
    } else {
        None
    };
    assert!(
        !urls.is_empty(),
        "provide one or more disposable dev/test RPC URLs"
    );
    let nodes: Vec<_> = urls.iter().map(NodeClient::new).collect::<Result<_, _>>()?;
    if let Some(path) = verify {
        let expected: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        for node in &nodes {
            for wallet in expected["wallets"]
                .as_array()
                .ok_or("missing wallet snapshots")?
            {
                let mut wallet = wallet.clone();
                if let Some(reference) = &epoch_source {
                    // The source may have certified another epoch since the
                    // report, without changing these accounts. Compare every
                    // economic field to the report and the epoch to that source.
                    let address = wallet["account"]["address"]
                        .as_str()
                        .ok_or("missing snapshot address")?;
                    let current = snapshot(reference, address).await?;
                    let epoch = current["account"]["payment_epoch"]
                        .as_str()
                        .ok_or("missing reference epoch")?;
                    parse_hash(epoch)?;
                    wallet["account"]["payment_epoch"] = json!(epoch);
                    assert_eq!(
                        current, wallet,
                        "reference economic state differs from saved evidence"
                    );
                }
                let address = wallet["account"]["address"]
                    .as_str()
                    .ok_or("missing snapshot address")?;
                assert_eq!(
                    snapshot(node, address).await?,
                    wallet,
                    "recovered state differs at {address}"
                );
            }
            let governance = &expected["governance"];
            let actual = node
                .rpc_call(
                    "governance_proposal_info",
                    json!({"hash":governance["hash"]}),
                )
                .await?;
            assert_eq!(
                actual, *governance,
                "governance state did not survive recovery"
            );
        }
        println!("PASS durable balances, portfolios, confirmations and governance record after restart on all nodes");
        return Ok(());
    }
    let primary_node = &nodes[0];
    for node in &nodes {
        if node.rpc_call("node_info", json!({})).await?["network"] != "dev" {
            return Err(
                "write validation requires every endpoint to report the disposable Dev network"
                    .into(),
            );
        }
    }
    let public_dev_key = burst_crypto::keypair_from_seed(&[0u8; 32]);
    let genesis_address = burst_crypto::derive_address(&public_dev_key.public);
    let initial_reps = primary_node.rpc_call("representatives", json!({})).await?;
    let initial_genesis = initial_reps["representatives"]
        .as_array()
        .ok_or("missing representatives")?
        .iter()
        .find(|rep| rep["account"] == genesis_address.as_str())
        .ok_or("genesis has no delegated human weight")?;
    let initial_people = initial_genesis["delegator_count"]
        .as_u64()
        .ok_or("missing delegator count")?;
    let initial_weight: u128 = initial_genesis["weight"]
        .as_str()
        .ok_or("missing weight")?
        .parse()?;
    assert_eq!(initial_weight, u128::from(initial_people) * 10_000);
    let info = primary_node.rpc_call("node_info", json!({})).await?;
    if info["verified_count"] == 1 {
        assert_eq!(
            initial_people, 1,
            "fresh genesis must have exactly one human unit"
        );
    }
    // Public fixture keys, matching the independent representative nodes in
    // run-validation.sh. Never use these accounts outside disposable Dev ledgers.
    let alice = Wallet::from_seed(&[1; 32]);
    let bob = Wallet::from_seed(&[2; 32]);
    println!(
        "Public disposable test accounts: Alice={}, Bob={}",
        alice.address, bob.address
    );
    let endorsement = primary_node
        .rpc_call("genesis_endorse", json!({"account":alice.address.as_str()}))
        .await?;
    let hash = endorsement["block_hash"]
        .as_str()
        .ok_or("no endorsement hash")?;
    for node in &nodes {
        node.wait_confirmed(hash, Duration::from_secs(40)).await?;
    }
    println!("PASS genesis endorsement confirmed on all nodes");
    for node in &nodes {
        let info = node.account_info(alice.address.as_str()).await?;
        assert_eq!(info.verification_state, "verified");
    }
    // Open Alice by receiving a signed genesis burn.
    let fund = primary_node
        .pay_signed(&public_dev_key, &alice.address, 1_000_000_000_000_000)
        .await?;
    println!("Genesis burn confirmed: {}", fund.hash);
    for node in &nodes {
        node.wait_confirmed(&fund.hash, Duration::from_secs(40))
            .await?;
    }
    let open = primary_node.receive_signed(&alice.primary_keys, &fund.hash).await?;
    for node in &nodes {
        node.wait_confirmed(&open.hash, Duration::from_secs(40))
            .await?;
    }
    println!("PASS receive-open from real genesis burn");
    // Exercise delegation while Alice's independent representative is online.
    // Offline humans stay in the denominator; genesis has no extra weight.
    // Prepare work before constructing the transaction, using a second peer if supplied.
    let prepared = nodes
        .get(1)
        .unwrap_or(primary_node)
        .prepare_work(&alice.address)
        .await?;
    let change = burst_wallet_core::transaction_builder::build_change_rep_tx(
        &alice.address,
        &genesis_address,
        burst_types::Timestamp::now(),
    )?;
    let delegated = primary_node
        .submit_signed_with_prepared_work(
            &alice.primary_keys,
            &burst_transactions::Transaction::ChangeRepresentative(change),
            burst_types::TxHash::ZERO,
            Some(&prepared),
        )
        .await?;
    println!("PASS work prepared on another peer before constructing the signed transaction");
    for node in &nodes {
        node.wait_confirmed(&delegated.hash, Duration::from_secs(40))
            .await?;
        let start = std::time::Instant::now();
        loop {
            let response = node.rpc_call("representatives", json!({})).await?;
            let reps = response["representatives"]
                .as_array()
                .ok_or("missing representatives")?;
            if reps.iter().any(|rep| {
                rep["account"] == genesis_address.as_str()
                    && rep["weight"] == (initial_weight + 10_000).to_string()
                    && rep["delegator_count"] == initial_people + 1
            }) && !reps
                .iter()
                .any(|rep| rep["account"] == alice.address.as_str())
            {
                break;
            }
            if start.elapsed() > Duration::from_secs(30) {
                return Err(format!(
                    "expected exactly one additional delegated human unit, got {response}"
                )
                .into());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    println!(
        "PASS signed consensus delegation: genesis has exactly {} human units, no bonus",
        initial_people + 1
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    let amount = 100_000_000_000_000u128;
    let pay = primary_node
        .pay_signed(&alice.primary_keys, &bob.address, amount)
        .await?;
    for node in &nodes {
        node.wait_confirmed(&pay.hash, Duration::from_secs(40))
            .await?;
        let pending = node.account_pending(bob.address.as_str(), 100).await?;
        assert_eq!(
            pending
                .blocks
                .iter()
                .find(|p| p.hash == pay.hash)
                .expect("pending burn")
                .amount,
            amount.to_string()
        );
    }
    println!("PASS signed payment is pending, not automatically pocketed");
    let received = nodes
        .last()
        .unwrap()
        .receive_signed(&bob.primary_keys, &pay.hash)
        .await?;
    for node in &nodes {
        node.wait_confirmed(&received.hash, Duration::from_secs(40))
            .await?;
    }
    println!("PASS unverified recipient explicitly accepts TRST");
    let before = primary_node.account_info(bob.address.as_str()).await?;
    assert!(
        primary_node.transfer_signed(
            &bob.primary_keys,
            &alice.address,
            1,
            parse_hash(&pay.hash)?.into_tx_hash()
        )
        .await
        .is_err(),
        "unverified TRST spending must fail"
    );
    assert_eq!(
        primary_node.account_info(bob.address.as_str()).await?.head,
        before.head
    );
    println!("PASS unverified TRST holder cannot spend");
    assert!(
        primary_node.set_verifier_opt_in_signed(&bob.primary_keys, true)
            .await
            .is_err(),
        "unverified wallet must not opt in to verification"
    );
    assert_eq!(
        primary_node.account_info(bob.address.as_str()).await?.head,
        before.head
    );
    for opted_in in [true, false, true] {
        let result = primary_node
            .set_verifier_opt_in_signed(&alice.primary_keys, opted_in)
            .await?;
        for node in &nodes {
            node.wait_confirmed(&result.hash, Duration::from_secs(40))
                .await?;
            let account = node
                .rpc_call("account_info", json!({"account":alice.address.as_str()}))
                .await?;
            assert_eq!(account["verifier_opted_in_at"].as_u64().is_some(), opted_in);
        }
    }
    println!("PASS client-signed verifier opt-in/out and rejection for an unverified wallet");
    // Bob may receive before verification but may not spend until verified.
    let endorsement = primary_node
        .rpc_call("genesis_endorse", json!({"account":bob.address.as_str()}))
        .await?;
    let hash = endorsement["block_hash"]
        .as_str()
        .ok_or("no endorsement hash")?;
    for node in &nodes {
        node.wait_confirmed(hash, Duration::from_secs(40)).await?;
    }
    let send = nodes
        .last()
        .unwrap()
        .transfer_signed(
            &bob.primary_keys,
            &alice.address,
            amount / 2,
            parse_hash(&pay.hash)?.into_tx_hash(),
        )
        .await?;
    for node in &nodes {
        node.wait_confirmed(&send.hash, Duration::from_secs(40))
            .await?;
    }
    let receive = primary_node.receive_signed(&alice.primary_keys, &send.hash).await?;
    for node in &nodes {
        node.wait_confirmed(&receive.hash, Duration::from_secs(40))
            .await?;
    }
    for wallet in [&alice, &bob] {
        let expected = primary_node.account_info(wallet.address.as_str()).await?;
        for node in &nodes {
            let actual = node.account_info(wallet.address.as_str()).await?;
            assert_eq!(actual.head, expected.head);
            assert_eq!(actual.trst_balance, expected.trst_balance);
            assert_eq!(actual.total_brn_burned, expected.total_brn_burned);
            assert_eq!(actual.verified_at, expected.verified_at);
            assert_eq!(actual.confirmation_height, actual.block_count);
        }
    }
    println!(
        "PASS signed transfer/receive, confirmations, frontiers and balances agree on every node"
    );
    // Merge exact holding IDs, retain provenance, then transfer the new origin.
    let portfolio = primary_node
        .rpc_call("trst_portfolio", json!({"account":alice.address.as_str()}))
        .await?;
    let sources = portfolio["batches"]
        .as_array()
        .ok_or("missing batches")?
        .iter()
        .map(|b| parse_hash(b["id"].as_str().unwrap()).map(|h| h.into_tx_hash()))
        .collect::<Result<Vec<_>, _>>()?;
    let before_merge = snapshot(primary_node, alice.address.as_str()).await?;
    let invalid_merge = burst_wallet_core::transaction_builder::build_merge_tx(
        &alice.address,
        vec![sources[0], burst_types::TxHash::new([0xf3; 32])],
        burst_types::Timestamp::now(),
    )?;
    let rejected = primary_node
        .submit_signed(
            &alice.primary_keys,
            &burst_transactions::Transaction::Merge(invalid_merge),
            burst_types::TxHash::ZERO,
        )
        .await;
    assert!(rejected
        .err()
        .ok_or("missing-input merge was accepted")?
        .to_string()
        .contains("not held by this wallet"));
    for node in &nodes {
        assert_eq!(snapshot(node, alice.address.as_str()).await?, before_merge);
    }
    println!(
        "PASS invalid merge rejected without changing frontier, holdings or provenance on any node"
    );
    let merge = burst_wallet_core::transaction_builder::build_merge_tx(
        &alice.address,
        sources,
        burst_types::Timestamp::now(),
    )?;
    let merged = primary_node
        .submit_signed(
            &alice.primary_keys,
            &burst_transactions::Transaction::Merge(merge),
            burst_types::TxHash::ZERO,
        )
        .await?;
    for node in &nodes {
        node.wait_confirmed(&merged.hash, Duration::from_secs(40))
            .await?;
    }
    let merged_send = primary_node
        .transfer_signed(
            &alice.primary_keys,
            &bob.address,
            1,
            parse_hash(&merged.hash)?.into_tx_hash(),
        )
        .await?;
    for node in &nodes {
        node.wait_confirmed(&merged_send.hash, Duration::from_secs(40))
            .await?;
    }
    let accepted = primary_node
        .receive_signed(&bob.primary_keys, &merged_send.hash)
        .await?;
    for node in &nodes {
        node.wait_confirmed(&accepted.hash, Duration::from_secs(40))
            .await?;
    }
    println!("PASS provenance-preserving merge and transfer of merged TRST");
    let return_amount = 7u128;
    let alice_before = primary_node.account_info(alice.address.as_str()).await?;
    let bob_before = primary_node.account_info(bob.address.as_str()).await?;
    let declined_send = primary_node
        .transfer_signed(
            &alice.primary_keys,
            &bob.address,
            return_amount,
            parse_hash(&merged.hash)?.into_tx_hash(),
        )
        .await?;
    for node in &nodes {
        node.wait_confirmed(&declined_send.hash, Duration::from_secs(40))
            .await?;
    }
    let reject = burst_wallet_core::transaction_builder::build_reject_receive_tx(
        &bob.address,
        parse_hash(&declined_send.hash)?.into_tx_hash(),
        burst_types::Timestamp::now(),
    )?;
    let rejected = primary_node
        .submit_signed(
            &bob.primary_keys,
            &burst_transactions::Transaction::RejectReceive(reject),
            burst_types::TxHash::ZERO,
        )
        .await?;
    for node in &nodes {
        node.wait_confirmed(&rejected.hash, Duration::from_secs(40))
            .await?;
        assert!(!node
            .account_pending(bob.address.as_str(), 100)
            .await?
            .blocks
            .iter()
            .any(|p| p.hash == declined_send.hash));
        assert_eq!(
            node.account_pending(alice.address.as_str(), 100)
                .await?
                .blocks
                .iter()
                .find(|p| p.hash == rejected.hash)
                .expect("returned pending transfer")
                .amount,
            return_amount.to_string()
        );
        assert_eq!(
            node.account_info(bob.address.as_str()).await?.trst_balance,
            bob_before.trst_balance
        );
    }
    let returned = primary_node
        .receive_signed(&alice.primary_keys, &rejected.hash)
        .await?;
    for node in &nodes {
        node.wait_confirmed(&returned.hash, Duration::from_secs(40))
            .await?;
        assert_eq!(
            node.account_info(alice.address.as_str())
                .await?
                .trst_balance,
            alice_before.trst_balance
        );
        assert!(!node
            .account_pending(alice.address.as_str(), 100)
            .await?
            .blocks
            .iter()
            .any(|p| p.hash == rejected.hash));
    }
    println!(
        "PASS declined transfer returns to sender exactly once without changing recipient balance"
    );
    let delegate = burst_wallet_core::transaction_builder::build_delegate_tx(
        &alice.address,
        &bob.address,
        burst_types::Timestamp::now(),
    )?;
    let delegated = primary_node
        .submit_signed(
            &alice.primary_keys,
            &burst_transactions::Transaction::Delegate(delegate),
            burst_types::TxHash::ZERO,
        )
        .await?;
    for node in &nodes {
        node.wait_confirmed(&delegated.hash, Duration::from_secs(40))
            .await?;
        let info = node
            .rpc_call("delegation_info", json!({"account":alice.address.as_str()}))
            .await?;
        assert_eq!(info["delegated_to"], bob.address.as_str());
    }
    let revoke = burst_wallet_core::transaction_builder::build_revoke_delegation_tx(
        &alice.address,
        burst_types::Timestamp::now(),
    )?;
    let revoked = primary_node
        .submit_signed(
            &alice.primary_keys,
            &burst_transactions::Transaction::RevokeDelegation(revoke),
            burst_types::TxHash::ZERO,
        )
        .await?;
    for node in &nodes {
        node.wait_confirmed(&revoked.hash, Duration::from_secs(40))
            .await?;
        assert!(node
            .rpc_call("delegation_info", json!({"account":alice.address.as_str()}))
            .await?["delegated_to"]
            .is_null());
    }
    println!("PASS signed delegation and revocation on all nodes");

    let proposal_block = primary_node
        .propose_parameter_signed(
            &public_dev_key,
            burst_governance::GovernableParam::BrnRate,
            123,
        )
        .await?;
    for node in &nodes {
        node.wait_confirmed(&proposal_block.hash, Duration::from_secs(40))
            .await?;
    }
    let proposal_id = parse_hash(&proposal_block.hash)?.into_tx_hash();
    let params = primary_node.rpc_call("protocol_params", json!({})).await?;
    let endorsement_cost: u128 = params["governance_endorsement_cost"]
        .as_str()
        .ok_or("missing governance endorsement cost")?
        .parse()?;
    let supported = primary_node
        .endorse_proposal_signed(&public_dev_key, proposal_id, endorsement_cost)
        .await?;
    for node in &nodes {
        node.wait_confirmed(&supported.hash, Duration::from_secs(40))
            .await?;
    }
    let before = snapshot(primary_node, genesis_address.as_str()).await?;
    let duplicate = primary_node
        .endorse_proposal_signed(&public_dev_key, proposal_id, endorsement_cost)
        .await
        .unwrap_err();
    assert!(
        duplicate.to_string().contains("already endorsed"),
        "{duplicate}"
    );
    assert_eq!(snapshot(primary_node, genesis_address.as_str()).await?, before);
    let before_bob = snapshot(primary_node, bob.address.as_str()).await?;
    let unauthorized = primary_node
        .withdraw_proposal_signed(&bob.primary_keys, proposal_id)
        .await
        .unwrap_err();
    assert!(
        unauthorized.to_string().contains("only the proposer"),
        "{unauthorized}"
    );
    assert_eq!(snapshot(primary_node, bob.address.as_str()).await?, before_bob);
    let withdrawn = primary_node
        .withdraw_proposal_signed(&public_dev_key, proposal_id)
        .await?;
    for node in &nodes {
        node.wait_confirmed(&withdrawn.hash, Duration::from_secs(40))
            .await?;
        let proposal = node
            .rpc_call(
                "governance_proposal_info",
                json!({"hash":proposal_id.to_string()}),
            )
            .await?;
        assert_eq!(proposal["phase"], "Withdrawn");
    }
    println!("PASS signed governance proposal, paid endorsement, duplicate protection and authorized withdrawal");
    // Default artifacts reject both operations at their release gate. The
    // experimental artifact must instead reject an uncertified vote on merit.
    // A valid challenge is not a negative test when challenges are enabled.
    let params = burst_types::ProtocolParams::default();
    for node in &nodes {
        let status = node
            .rpc_call(
                "verification_status",
                json!({"account":bob.address.as_str()}),
            )
            .await?;
        assert_eq!(status["selection_available"], experimental);
        assert_eq!(
            status["selection_unavailable_reason"],
            if experimental {
                Value::Null
            } else {
                json!("certified_checkpoint_selection_release_gated")
            }
        );
        let before = snapshot(node, genesis_address.as_str()).await?;
        let challenge = burst_wallet_core::transaction_builder::build_challenge_tx(
            &genesis_address,
            &bob.address,
            params.challenge_stake_amount,
            burst_types::ChallengeReason::Fraud,
            burst_types::Timestamp::now(),
        )?;
        let vote = burst_wallet_core::transaction_builder::build_verification_vote_tx(
            &genesis_address,
            &bob.address,
            burst_types::BlockHash::new([99; 32]),
            3,
            0,
            burst_types::Timestamp::now(),
        )?;
        let mut transactions = vec![burst_transactions::Transaction::VerificationVote(vote)];
        if !experimental {
            transactions.push(burst_transactions::Transaction::Challenge(challenge));
        }
        for transaction in transactions {
            let error = node
                .submit_signed(&public_dev_key, &transaction, burst_types::TxHash::ZERO)
                .await
                .expect_err("uncertified selection must not lock stakes")
                .to_string();
            assert!(
                error.contains(if experimental {
                    "verification committee is not certified"
                } else {
                    "certified verification is disabled"
                }),
                "unexpected rejection: {error}"
            );
            assert_eq!(snapshot(node, genesis_address.as_str()).await?, before);
        }
    }
    println!("PASS uncertified votes rejected without changing balances, stakes or frontiers on all nodes");
    if experimental {
        println!("NOT COVERED: valid challenge/appeal journeys require their separate lifecycle acceptance");
    } else {
        println!("PASS release-gated challenges leave balances, stakes and frontiers unchanged");
    }
    let mut snapshots = Vec::new();
    for address in [alice.address.as_str(), bob.address.as_str()] {
        let expected = snapshot(primary_node, address).await?;
        for node in &nodes {
            assert_eq!(snapshot(node, address).await?, expected);
        }
        snapshots.push(expected);
    }
    if let Ok(path) = std::env::var("BURST_VALIDATION_REPORT") {
        let governance = primary_node
            .rpc_call(
                "governance_proposal_info",
                json!({"hash":proposal_id.to_string()}),
            )
            .await?;
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&json!({"wallets":snapshots,"governance":governance}))?,
        )?;
    }
    Ok(())
}

async fn snapshot(node: &NodeClient, address: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let mut account = node
        .rpc_call("account_info", json!({"account":address}))
        .await?;
    account.as_object_mut().unwrap().remove("brn_balance"); // live accrual, not ledger state
    let mut brn = node
        .rpc_call("brn_components", json!({"account":address}))
        .await?;
    brn.as_object_mut().unwrap().remove("as_of");
    let mut portfolio = node
        .rpc_call("trst_portfolio", json!({"account":address}))
        .await?;
    portfolio["batches"]
        .as_array_mut()
        .unwrap()
        .sort_by_key(|batch| batch["id"].as_str().unwrap_or("").to_owned());
    let pending = node
        .rpc_call("account_pending", json!({"account":address}))
        .await?;
    Ok(json!({"account":account,"brn":brn,"portfolio":portfolio,"pending":pending}))
}
