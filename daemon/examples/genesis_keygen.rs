//! Genesis identity generator — RUN THIS ON THE CREATOR'S DEVICE ONLY.
//!
//! Generates a fresh genesis keypair and the canonical, signed genesis block
//! for a network. By default, writes the secret to a new owner-only file and prints:
//!
//!   - the PUBLIC key   → pin with BURST_TEST_GENESIS_PUBLIC_KEY for a
//!     private-testnet build, or LIVE_GENESIS_PUBLIC_KEY_HEX for Live.
//!   - the genesis address, block hash, and signature (all public).
//!
//! The secret seed NEVER goes in the repo, config, or any shared channel.
//! On the creator's node, supply it at runtime via `BURST_GENESIS_SEED`
//! (64 hex chars) or `BURST_GENESIS_SEED_FILE`. Every other node runs without
//! it and simply cannot author genesis authority blocks.
//!
//! Usage:
//!   cargo run --example genesis_keygen -p burst-daemon -- test --seed-file /private/genesis.seed
//! File output currently requires Unix permissions. Explicit --show-secret keeps
//! the interactive terminal workflow available; never use it in shared logs.

use burst_ledger::{BlockType, StateBlock, CURRENT_BLOCK_VERSION};
use burst_types::{BlockHash, NetworkId, ProtocolParams, Signature, Timestamp, TxHash};
use clap::{ArgGroup, Parser, ValueEnum};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Network {
    Live,
    Test,
    Dev,
}

#[derive(Debug, Parser)]
#[command(group(ArgGroup::new("output").required(true).args(["seed_file", "show_secret"])))]
struct Options {
    #[arg(value_enum)]
    network: Network,
    /// Create a new seed file with owner-only permissions; never overwrite.
    #[arg(long)]
    seed_file: Option<PathBuf>,
    /// Print the secret seed to stdout. Do not use in shared or recorded terminals.
    #[arg(long)]
    show_secret: bool,
}

fn save_seed(path: &std::path::Path, seed: &[u8; 32]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        writeln!(file, "{}", hex(seed))?;
        file.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = (path, seed);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "owner-only seed-file creation is currently supported on Unix only",
        ))
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() -> anyhow::Result<()> {
    let options = Options::parse();
    let network = match options.network {
        Network::Test => NetworkId::Test,
        Network::Dev => NetworkId::Dev,
        Network::Live => NetworkId::Live,
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
    let params = match network {
        NetworkId::Live => ProtocolParams::burst_defaults(),
        NetworkId::Test => ProtocolParams::testnet_defaults(),
        NetworkId::Dev => ProtocolParams::devnet_defaults(),
    };
    let mut block = StateBlock {
        signing_grant: burst_types::BlockHash::ZERO,
        delegation_payload: Vec::new(),
        version: CURRENT_BLOCK_VERSION,
        payment_epoch: BlockHash::ZERO,
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
        target_head: burst_types::BlockHash::ZERO,
        merge_sources: Vec::new(),
        release_sources: Vec::new(),
        work: 0,
        signature: Signature([0u8; 64]),
        hash: BlockHash::ZERO,
    };
    block.hash = block.compute_hash();
    let sig = burst_crypto::sign_message(block.hash.as_bytes(), &kp.private);

    if let Some(path) = options.seed_file {
        save_seed(&path, &seed)?;
        println!(
            "Seed saved to {}. Back it up privately before starting the network.",
            path.display()
        );
        println!("Set BURST_GENESIS_SEED_FILE to that path on the creator node only.");
    } else {
        println!("SECRET seed (store safely, NEVER commit): {}", hex(&seed));
    }
    println!("\n=== BURST genesis identity for {network:?} ===\n");
    if network == NetworkId::Test {
        println!("\nPUBLIC  key (build with --features private-testnet and BURST_TEST_GENESIS_PUBLIC_KEY):");
    } else {
        println!("\nPUBLIC  key (bake into node/src/genesis_key.rs):");
    }
    println!("  {}", hex(&kp.public.0));
    println!("\nGenesis address:   {}", address.as_str());
    println!("Genesis block hash: {}", hex(block.hash.as_bytes()));
    println!("Genesis signature:  {}", hex(&sig.0));
    println!("\nKeep the seed offline. Anyone with it controls the network's genesis authority.\n");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_explicit_network_and_output() {
        for args in [
            vec!["keygen"],
            vec!["keygen", "test"],
            vec!["keygen", "typo", "--show-secret"],
            vec!["keygen", "test", "--show-secret", "--seed-file", "seed"],
        ] {
            assert!(Options::try_parse_from(args).is_err());
        }
        assert!(Options::try_parse_from(["keygen", "test", "--seed-file", "seed"]).is_ok());
        assert!(Options::try_parse_from(["keygen", "live", "--show-secret"]).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn seed_file_is_private_and_never_replaced() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("seed");
        save_seed(&path, &[42; 32]).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{}\n", "2a".repeat(32))
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            save_seed(&path, &[43; 32]).unwrap_err().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        let link = directory.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(save_seed(&link, &[43; 32]).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{}\n", "2a".repeat(32))
        );
    }
}
