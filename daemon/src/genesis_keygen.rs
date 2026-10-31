//! Generate a genesis identity on the device that will hold its private key.
//!
//! Choose `--seed-file` to create a new owner-only file on Unix, or
//! `--show-secret` to print the seed to a private, unrecorded terminal.
//! Both modes print the public key, address, genesis block hash and signature.
//!
//! For a private Test build, pin the public key with `BURST_TEST_GENESIS_PUBLIC_KEY`
//! and enable `private-testnet`. For Live, set `LIVE_GENESIS_PUBLIC_KEY_HEX`.
//! Supply the matching secret only to the creator's node through
//! `BURST_GENESIS_SEED_FILE` or `BURST_GENESIS_SEED` (64 hex characters).
//! Keep it out of the repository and shared logs. Ordinary Test and Dev builds
//! instead use their published development seeds.
//!
//! Example:
//! ```text
//! burst-daemon genesis-keygen test --seed-file /private/genesis.seed
//! ```

use burst_types::{NetworkId, ProtocolParams};
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
pub(super) struct Options {
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

pub(super) fn run(options: &Options) -> anyhow::Result<()> {
    let network = match options.network {
        Network::Test => NetworkId::Test,
        Network::Dev => NetworkId::Dev,
        Network::Live => NetworkId::Live,
    };

    // The 32-byte private key is the seed accepted by keypair_from_seed().
    let kp = burst_crypto::generate_keypair();
    let seed = kp.private.0;
    let address = burst_crypto::derive_address(&kp.public);

    // Use the node's genesis constructor so the printed hash commits the same fields.
    let params = match network {
        NetworkId::Live => ProtocolParams::burst_defaults(),
        NetworkId::Test => ProtocolParams::testnet_defaults(),
        NetworkId::Dev => ProtocolParams::devnet_defaults(),
    };
    let block = burst_ledger::genesis::configured_genesis_block(address.clone(), &params);
    let sig = burst_crypto::sign_message(block.hash.as_bytes(), &kp.private);

    if let Some(path) = &options.seed_file {
        save_seed(path, &seed)?;
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
