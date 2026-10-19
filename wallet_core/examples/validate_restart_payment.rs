//! Post-restart liveness probe for disposable Dev or explicitly selected private Test.
//! Uses public fixture keys; never use for networks carrying real assets.
use burst_wallet_core::{NodeClient, Wallet};
use serde_json::json;
use std::time::{Duration, Instant};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().is_some_and(|arg| arg == "--protocol-epoch") {
        args.next();
        let mode = args.next().ok_or("provide marker or successor")?;
        let url = args.next().ok_or("provide disposable Dev RPC URL")?;
        assert!(args.next().is_none(), "unexpected protocol-epoch argument");
        return protocol_epoch(&mode, &url).await;
    }
    let private_test = args.peek().is_some_and(|arg| arg == "--private-testnet");
    if private_test {
        args.next();
    }
    let network = if private_test { "test" } else { "dev" };
    let nodes: Vec<_> = args.map(NodeClient::new).collect::<Result<_, _>>()?;
    assert!(!nodes.is_empty(), "provide disposable network RPC URLs");
    for node in &nodes {
        let started = Instant::now();
        loop {
            if let Ok(info) = node.rpc_call("node_info", json!({})).await {
                if info["network"] != network {
                    return Err(
                        "restart payment probe requires the selected disposable network".into(),
                    );
                }
                if private_test {
                    assert_eq!(
                        node.rpc_call("protocol_params", json!({})).await?["brn_rate"],
                        (100 * burst_types::BRN_UNIT).to_string(),
                        "requires explicitly governed disposable Test accrual"
                    );
                }
                let readiness = node.rpc_call("params_hash", json!({})).await?;
                if matches!(readiness["online_signing"].as_str(), Some("local" | "peer_matches")) {
                    break;
                }
            }
            if started.elapsed() > Duration::from_secs(30) {
                return Err("node did not become ready to sign after restart".into());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    let recipient = Wallet::create()?;
    let keys = burst_crypto::keypair_from_seed(&[if private_test { 42 } else { 0 }; 32]);
    // Reuse the fixture owner's confirmed local binding; never adopt an RPC grant.
    let sender = if let Some(path) = std::env::var_os("VALIDATION_DEVICE_BINDING") {
        let account = burst_crypto::derive_address(&keys.public);
        let grant = burst_wallet_core::signing_device::load(std::path::Path::new(&path), &account)?;
        assert!(!grant.is_zero(), "explicit fixture binding must be enrolled");
        nodes[0].clone().with_signing_grant(grant)
    } else {
        nodes[0].clone()
    };
    let amount = 1_000_000_000_000u128;
    let payment = sender
        .pay_signed(&keys, &recipient.address, amount)
        .await?;
    for node in &nodes {
        node.wait_confirmed(&payment.hash, Duration::from_secs(40))
            .await?;
        let pending = node
            .account_pending(recipient.address.as_str(), 100)
            .await?;
        let entry = pending
            .blocks
            .iter()
            .find(|p| p.hash == payment.hash)
            .ok_or("missing pending payment")?;
        assert_eq!(entry.amount, amount.to_string());
    }
    println!(
        "PASS new signed payment after restart confirmed on {} nodes: {}",
        nodes.len(),
        payment.hash
    );
    println!(
        "Recipient {} has the same pending amount everywhere; no unrequested receive",
        recipient.address
    );
    Ok(())
}

async fn protocol_epoch(mode: &str, url: &str) -> Result<(), Box<dyn std::error::Error>> {
    use burst_ledger::{BlockType, StateBlock};
    use burst_types::{BlockHash, Timestamp, TxHash};
    let node = NodeClient::new(url)?;
    assert_eq!(node.rpc_call("node_info", json!({})).await?["network"], "dev");
    let keys = burst_crypto::keypair_from_seed(&[0; 32]);
    let account = burst_crypto::derive_address(&keys.public);
    let state = node.inspect_signing_state(&account).await?;
    assert!(state.signing_grant.is_zero(), "requires an unenrolled disposable creator");
    let response = node.rpc_call("block_info", json!({
        "hash":state.head.to_string(), "include_signed_block":true,
    })).await?;
    let previous: StateBlock = serde_json::from_str(response["signed_block"].as_str().ok_or("missing signed predecessor")?)?;
    assert_eq!(previous.account, account);
    assert_eq!((previous.brn_balance, previous.trst_balance), (0, 0));
    let kind = match mode {
        "marker" => {
            assert_eq!(previous.block_type, BlockType::Open);
            assert!(previous.previous.is_zero(), "requires fresh Dev genesis");
            BlockType::Epoch
        }
        "successor" => {
            assert_eq!(previous.block_type, BlockType::Epoch);
            BlockType::ChangeRepresentative
        }
        _ => return Err("expected marker or successor".into()),
    };
    let params = node.rpc_call("params_hash", json!({})).await?;
    let mut block = previous.clone();
    block.block_type = kind;
    block.previous = previous.hash;
    block.payment_epoch = state.payment_epoch;
    block.params_hash = burst_wallet_core::signed_client::parse_hash(params["params_hash"].as_str().ok_or("missing params hash")?)?;
    block.timestamp = Timestamp::now();
    block.link = BlockHash::ZERO;
    block.origin = TxHash::ZERO;
    block.transaction = TxHash::ZERO;
    block.hash = block.compute_hash();
    block.signature = burst_crypto::sign_message(block.hash.as_bytes(), &keys.private);
    let work = node.rpc_call("work_generate", json!({
        "hash":block.work_root().to_string(), "block_type":block.block_type,
    })).await?;
    block.work = u64::from_str_radix(
        work["work"].as_str().ok_or("missing work nonce")?.trim_start_matches("0x"), 16,
    )?;
    let signed = serde_json::to_string(&block)?;
    let result = node.process(&signed).await?;
    assert!(result.accepted, "protocol epoch action rejected: {result:?}");
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if node.inspect_signing_state(&account).await.is_ok_and(|state| state.head == block.hash) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }).await?;
    println!("{}", json!({"account":account.to_string(), "hash":block.hash.to_string(), "signedBlock":signed}));
    Ok(())
}
