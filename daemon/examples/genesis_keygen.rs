//! Genesis identity generator — RUN THIS ON THE CREATOR'S DEVICE ONLY.
//!
//! Generates a fresh genesis keypair and the canonical, signed genesis block
//! for a network, then prints:
//!
//!   - the SECRET seed  → store it safely; it is the network's founding key.
//!   - the PUBLIC key   → bake into `node/src/genesis_key.rs`
//!                        (`LIVE_GENESIS_PUBLIC_KEY_HEX`).
//!   - the genesis address, block hash, and signature (all public).
//!
//! The secret seed NEVER goes in the repo, config, or any shared channel.
//! On the creator's node, supply it at runtime via `BURST_GENESIS_SEED`
//! (64 hex chars) or `BURST_GENESIS_SEED_FILE`. Every other node runs without
//! it and simply cannot author genesis authority blocks.
//!
//! Usage:
//!   cargo run --example genesis_keygen -p burst-daemon -- [live|test|dev]

use burst_ledger::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
use burst_types::{BlockHash, NetworkId, ProtocolParams, Signature, Timestamp, TxHash};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() {
    let network = match std::env::args().nth(1).as_deref() {
        Some("test") => NetworkId::Test,
        Some("dev") => NetworkId::Dev,
        _ => NetworkId::Live,
    };

    // A fresh Ed25519 keypair. The 32-byte private key IS the seed that
    // reproduces this identity via keypair_from_seed().
    let kp = burst_crypto::generate_keypair();
    let seed = kp.private.0;
    let address = burst_crypto::derive_address(&kp.public);

    // Build the canonical genesis block EXACTLY as the node does in
    // `initialize_genesis` (timestamp 0, zero balances, params_hash of the
    // network's initial params) so the printed signature matches what the
    // creator's node will produce at first startup.
    let params = ProtocolParams::burst_defaults();
    let mut block = StateBlock {
        version: CURRENT_BLOCK_VERSION,
        block_type: BlockType::Open,
        account: address.clone(),
        previous: BlockHash::ZERO,
        representative: address.clone(),
        brn_balance: 0,
        trst_balance: 0,
        link: BlockHash::ZERO,
        origin: TxHash::ZERO,
        transaction: TxHash::ZERO,
        timestamp: Timestamp::new(0),
        params_hash: params.params_hash(),
        merge_sources: Vec::new(),
        work: 0,
        signature: Signature([0u8; 64]),
        hash: BlockHash::ZERO,
    };
    block.hash = block.compute_hash();
    let sig = burst_crypto::sign_message(block.hash.as_bytes(), &kp.private);

    println!("\n=== BURST genesis identity for {network:?} ===\n");
    println!("SECRET  seed (store safely, NEVER commit):");
    println!("  {}", hex(&seed));
    println!("\n  Run the creator node with:");
    println!("    export BURST_GENESIS_SEED={}", hex(&seed));
    println!("\nPUBLIC  key (bake into node/src/genesis_key.rs):");
    println!("  {}", hex(&kp.public.0));
    println!("\nGenesis address:   {}", address.as_str());
    println!("Genesis block hash: {}", hex(block.hash.as_bytes()));
    println!("Genesis signature:  {}", hex(&sig.0));
    println!("\nKeep the seed offline. Anyone with it controls the network's genesis authority.\n");
}
