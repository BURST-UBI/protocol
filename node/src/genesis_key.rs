//! Genesis identity — the creator's key that bootstraps a network.
//!
//! Security model (whitepaper §Genesis): the genesis account endorses the
//! first wallets and authors governance-activation blocks. Its authority is
//! only meaningful if the *private* key is held solely by the creator.
//!
//! This module splits the identity in two:
//!
//! - **Public identity** (address, verifying key) — baked per network, known
//!   to every node. Genesis block *identity* (its hash) derives from this
//!   alone, because `StateBlock::compute_hash` excludes the signature — so
//!   every node agrees on the same genesis block without holding the key.
//!
//! - **Private seed** — loaded at runtime, only on the creator's node, from
//!   `BURST_GENESIS_SEED` (64-hex-char = 32 bytes) or the file named by
//!   `BURST_GENESIS_SEED_FILE`. A node without it (every non-creator node,
//!   e.g. cloud peers) runs normally but cannot author genesis authority
//!   blocks — which is exactly the intended restriction.
//!
//! Disposable Dev/Test builds retain published fixture seeds. A
//! `private-testnet` build pins BURST_TEST_GENESIS_PUBLIC_KEY at compile time
//! and requires the creator's runtime seed, just like Live. It is a separate
//! Test ledger, not an upgrade of the published-seed network.

use burst_types::{KeyPair, NetworkId, PublicKey, Signature, WalletAddress};
use std::sync::OnceLock;

/// Published dev seed — genesis key for the local dev network. Public by design.
const DEV_GENESIS_SEED: [u8; 32] = [0u8; 32];

/// Published test seed — genesis key for the shared test network. Public by design.
const TEST_GENESIS_SEED: [u8; 32] = [7u8; 32];

#[cfg(feature = "private-testnet")]
const PRIVATE_TEST_PUBLIC_KEY: [u8; 32] = match decode_hex32(env!(
    "BURST_TEST_GENESIS_PUBLIC_KEY",
    "private-testnet requires the creator's 64-character public key at build time"
)) {
    Some(key) => key,
    None => panic!("BURST_TEST_GENESIS_PUBLIC_KEY must be 64 hexadecimal characters"),
};

/// Baked genesis PUBLIC key for the live network (32-byte Ed25519 verifying
/// key, hex). Generated once by the creator with `genesis_keygen` and pasted
/// here before launch. Until then it is all-zeros, and live authority actions
/// are refused (see [`live_identity_configured`]).
const LIVE_GENESIS_PUBLIC_KEY_HEX: &str =
    "d481ea12094da14808a783c74cbabc28561c50c0348eaf419718d6080359ca4a";

const fn decode_hex32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let bytes = hex.as_bytes();
    let mut i = 0;
    while i < 64 {
        let digit = match bytes[i] {
            b'0'..=b'9' => bytes[i] - b'0',
            b'a'..=b'f' => bytes[i] - b'a' + 10,
            b'A'..=b'F' => bytes[i] - b'A' + 10,
            _ => return None,
        };
        out[i / 2] = out[i / 2] * 16 + digit;
        i += 1;
    }
    Some(out)
}

/// Whether the live network's genesis public identity has been configured
/// (i.e. the baked key is no longer the all-zeros placeholder).
pub fn live_identity_configured() -> bool {
    decode_hex32(LIVE_GENESIS_PUBLIC_KEY_HEX).is_some_and(|k| k != [0u8; 32])
}

/// The genesis public (verifying) key for a network.
pub fn genesis_public_key(network: NetworkId) -> PublicKey {
    match network {
        NetworkId::Dev => burst_crypto::keypair_from_seed(&DEV_GENESIS_SEED).public,
        NetworkId::Test => {
            #[cfg(feature = "private-testnet")]
            {
                PublicKey(PRIVATE_TEST_PUBLIC_KEY)
            }
            #[cfg(not(feature = "private-testnet"))]
            burst_crypto::keypair_from_seed(&TEST_GENESIS_SEED).public
        }
        NetworkId::Live => {
            PublicKey(decode_hex32(LIVE_GENESIS_PUBLIC_KEY_HEX).unwrap_or([0u8; 32]))
        }
    }
}

/// Refuse fixture or live identities in a private pilot before opening its DB.
pub(crate) fn validate_identity(network: NetworkId) -> Result<(), &'static str> {
    if network == NetworkId::Test && cfg!(feature = "private-testnet") {
        let public = genesis_public_key(network);
        validate_private_test_key(public)?;
    }
    Ok(())
}

fn validate_private_test_key(public: PublicKey) -> Result<(), &'static str> {
    // The existing conversion rejects malformed and weak Ed25519 points.
    if burst_crypto::ed25519_public_to_x25519(&public.0).is_none()
        || public == burst_crypto::keypair_from_seed(&DEV_GENESIS_SEED).public
        || public == burst_crypto::keypair_from_seed(&TEST_GENESIS_SEED).public
        || public == genesis_public_key(NetworkId::Live)
    {
        return Err("private testnet needs a distinct creator-owned genesis identity");
    }
    Ok(())
}

/// The genesis account address for a network.
pub fn genesis_address(network: NetworkId) -> WalletAddress {
    burst_crypto::derive_address(&genesis_public_key(network))
}

/// Load a raw 32-byte seed from the runtime environment, if provided.
/// `BURST_GENESIS_SEED` (hex) takes precedence over `BURST_GENESIS_SEED_FILE`.
fn seed_from_env() -> Option<[u8; 32]> {
    if let Ok(hex) = std::env::var("BURST_GENESIS_SEED") {
        return decode_hex32(hex.trim());
    }
    if let Ok(path) = std::env::var("BURST_GENESIS_SEED_FILE") {
        let contents = std::fs::read_to_string(&path).ok()?;
        return decode_hex32(contents.trim());
    }
    None
}

static DEV_SEED: OnceLock<Option<[u8; 32]>> = OnceLock::new();
static TEST_SEED: OnceLock<Option<[u8; 32]>> = OnceLock::new();
static LIVE_SEED: OnceLock<Option<[u8; 32]>> = OnceLock::new();

/// The genesis SIGNING keypair for this node, if it holds the private seed.
///
/// - Disposable Dev/Test: derived from the published seed (always available).
/// - Live/private Test: derived from the runtime seed (`BURST_GENESIS_SEED[_FILE]`), and
///   only returned if it matches the baked live public key — a mismatched
///   seed yields `None` (with a warning) rather than a silently-wrong key.
///
/// `None` means this node cannot author genesis authority blocks. That is the
/// normal, correct state for every non-creator node. The validated seed is
/// memoized per network (env/file read and any warning logged once); the
/// keypair is reconstructed per call (cheap, and the secret is zeroized on
/// drop rather than lingering in a static).
pub fn genesis_signing_key(network: NetworkId) -> Option<KeyPair> {
    let cell = match network {
        NetworkId::Dev => &DEV_SEED,
        NetworkId::Test => &TEST_SEED,
        NetworkId::Live => &LIVE_SEED,
    };
    cell.get_or_init(|| resolve_seed(network))
        .as_ref()
        .map(burst_crypto::keypair_from_seed)
}

fn resolve_seed(network: NetworkId) -> Option<[u8; 32]> {
    let seed = match network {
        NetworkId::Dev => Some(DEV_GENESIS_SEED),
        NetworkId::Test if !cfg!(feature = "private-testnet") => Some(TEST_GENESIS_SEED),
        NetworkId::Test => seed_from_env(),
        NetworkId::Live => seed_from_env(),
    }?;
    let kp = burst_crypto::keypair_from_seed(&seed);
    let expected = genesis_public_key(network);
    if kp.public.0 != expected.0 {
        tracing::error!(
            "BURST_GENESIS_SEED does not match the baked genesis public key for {:?} — \
             genesis authority disabled on this node",
            network
        );
        return None;
    }
    if network == NetworkId::Live {
        tracing::warn!("genesis signing key loaded — this node is the LIVE genesis authority");
    }
    Some(seed)
}

/// True if this node can act as the genesis authority (holds the private seed).
pub fn has_genesis_authority(network: NetworkId) -> bool {
    genesis_signing_key(network).is_some()
}

/// The validated genesis seed for this node, if held. Used to hand the
/// signing material to the RPC layer (decoupled from this crate) so the
/// authority node can author bootstrap endorsement blocks. `None` on every
/// non-creator node.
pub fn genesis_seed(network: NetworkId) -> Option<[u8; 32]> {
    let cell = match network {
        NetworkId::Dev => &DEV_SEED,
        NetworkId::Test => &TEST_SEED,
        NetworkId::Live => &LIVE_SEED,
    };
    *cell.get_or_init(|| resolve_seed(network))
}

/// Sign the genesis block hash with the genesis key if this node holds it,
/// otherwise return a zero signature. Genesis validity is established by hash
/// (well-known per network), not by signature re-verification, so a zero
/// signature on a non-creator node is safe; the creator's node produces the
/// authoritative signed copy.
pub fn sign_genesis(network: NetworkId, genesis_hash: &[u8]) -> Signature {
    match genesis_signing_key(network) {
        Some(kp) => burst_crypto::sign_message(genesis_hash, &kp.private),
        None => Signature([0u8; 64]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_and_test_have_signing_keys_without_env() {
        assert!(genesis_signing_key(NetworkId::Dev).is_some());
        assert_eq!(
            genesis_signing_key(NetworkId::Test).is_some(),
            !cfg!(feature = "private-testnet")
        );
    }

    #[test]
    fn private_test_identity_excludes_fixture_and_live_authority() {
        let mut identity = [0; 32];
        identity[0] = 1;
        for public in [
            PublicKey([0; 32]),
            PublicKey(identity),
            burst_crypto::keypair_from_seed(&DEV_GENESIS_SEED).public,
            burst_crypto::keypair_from_seed(&TEST_GENESIS_SEED).public,
            genesis_public_key(NetworkId::Live),
        ] {
            assert!(validate_private_test_key(public).is_err());
        }
        assert!(
            validate_private_test_key(burst_crypto::keypair_from_seed(&[42; 32]).public).is_ok()
        );
    }

    #[test]
    fn genesis_address_is_deterministic_per_network() {
        assert_eq!(
            genesis_address(NetworkId::Dev),
            genesis_address(NetworkId::Dev)
        );
        assert_ne!(
            genesis_address(NetworkId::Dev),
            genesis_address(NetworkId::Test)
        );
    }

    #[test]
    fn live_identity_is_configured_and_pinned() {
        // The creator's genesis public key is baked in. This pins the live
        // genesis address so an accidental key edit is caught immediately.
        assert!(live_identity_configured());
        assert_eq!(
            genesis_address(NetworkId::Live).as_str(),
            "brst_tk1yn6ibbpini479ih5nsgow73d3rn818k9cyieq55d1i1tssb71wpywzqag"
        );
    }

    #[test]
    fn live_has_no_signing_key_without_env() {
        // Without BURST_GENESIS_SEED, a node cannot act as the live genesis
        // authority — the correct default for every non-creator node.
        assert!(genesis_signing_key(NetworkId::Live).is_none());
    }

    #[test]
    fn hex_decode_roundtrip() {
        assert_eq!(decode_hex32(&"00".repeat(32)), Some([0u8; 32]));
        assert_eq!(decode_hex32(&"ff".repeat(32)), Some([0xffu8; 32]));
        assert_eq!(decode_hex32("zz"), None);
        assert_eq!(decode_hex32("abc"), None);
    }
}
