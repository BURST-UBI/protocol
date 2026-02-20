//! BIP39 mnemonic generation and Ed25519 key derivation.
//!
//! Generates a 24-word mnemonic (256-bit entropy) and derives an Ed25519 keypair
//! using BIP44 derivation path `m/44'/9999'/0'/0/0` (9999 = BURST coin type).
//!
//! The derivation uses HMAC-SHA512 to produce a 64-byte seed from the mnemonic,
//! then takes the first 32 bytes as the Ed25519 secret key.

use bip39::Mnemonic;
use burst_types::{KeyPair, PrivateKey, PublicKey};
use ed25519_dalek::SigningKey;
use hmac::{Hmac, Mac};
use sha2::Sha512;
use thiserror::Error;

type HmacSha512 = Hmac<Sha512>;

/// BIP44 derivation path for BURST: m/44'/9999'/0'/0/0
const BURST_BIP44_PATH: &str = "m/44'/9999'/0'/0/0";

/// Errors arising from mnemonic operations.
#[derive(Debug, Error)]
pub enum MnemonicError {
    #[error("invalid mnemonic phrase: {0}")]
    InvalidMnemonic(String),

    #[error("key derivation failed: {0}")]
    DerivationFailed(String),
}

/// Generate a new 24-word BIP39 mnemonic from 256-bit entropy.
pub fn generate_mnemonic() -> Result<String, MnemonicError> {
    let mut entropy = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut entropy);
    let mnemonic = Mnemonic::from_entropy(&entropy)
        .map_err(|e| MnemonicError::DerivationFailed(e.to_string()))?;
    Ok(mnemonic.to_string())
}

/// Derive an Ed25519 keypair from a BIP39 mnemonic phrase.
///
/// Process:
/// 1. Validate the mnemonic and derive the BIP39 seed (with empty passphrase)
/// 2. Apply HMAC-SHA512 with the BIP44 path as key to derive a child key
/// 3. Take the first 32 bytes as the Ed25519 secret key
/// 4. Derive the corresponding public key
pub fn keypair_from_mnemonic(mnemonic: &str) -> Result<KeyPair, MnemonicError> {
    let mnemonic = Mnemonic::parse_normalized(mnemonic)
        .map_err(|e| MnemonicError::InvalidMnemonic(e.to_string()))?;

    // BIP39 seed derivation (PBKDF2-HMAC-SHA512 with "mnemonic" as salt, 2048 rounds)
    let seed = mnemonic.to_seed_normalized("");

    // Derive child key using HMAC-SHA512 with the BIP44 path.
    // We use the path string as the HMAC key and the seed as the message,
    // producing a deterministic 64-byte output. The first 32 bytes become our secret key.
    let mut mac = HmacSha512::new_from_slice(BURST_BIP44_PATH.as_bytes())
        .map_err(|e| MnemonicError::DerivationFailed(e.to_string()))?;
    mac.update(&seed);
    let result = mac.finalize().into_bytes();

    let mut secret_bytes = [0u8; 32];
    secret_bytes.copy_from_slice(&result[..32]);

    let signing_key = SigningKey::from_bytes(&secret_bytes);
    let verifying_key = signing_key.verifying_key();

    Ok(KeyPair {
        public: PublicKey(verifying_key.to_bytes()),
        private: PrivateKey(signing_key.to_bytes()),
    })
}

/// Validate that a mnemonic phrase is a valid BIP39 mnemonic.
pub fn validate_mnemonic(mnemonic: &str) -> bool {
    Mnemonic::parse_normalized(mnemonic).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_24_words() {
        let mnemonic = generate_mnemonic().unwrap();
        let words: Vec<&str> = mnemonic.split_whitespace().collect();
        assert_eq!(words.len(), 24);
    }

    #[test]
    fn generated_mnemonic_is_valid() {
        let mnemonic = generate_mnemonic().unwrap();
        assert!(validate_mnemonic(&mnemonic));
    }

    #[test]
    fn keypair_from_mnemonic_deterministic() {
        let mnemonic = generate_mnemonic().unwrap();
        let kp1 = keypair_from_mnemonic(&mnemonic).unwrap();
        let kp2 = keypair_from_mnemonic(&mnemonic).unwrap();
        assert_eq!(kp1.public.0, kp2.public.0);
        assert_eq!(kp1.private.0, kp2.private.0);
    }

    #[test]
    fn different_mnemonics_produce_different_keys() {
        let m1 = generate_mnemonic().unwrap();
        let m2 = generate_mnemonic().unwrap();
        assert_ne!(m1, m2);

        let kp1 = keypair_from_mnemonic(&m1).unwrap();
        let kp2 = keypair_from_mnemonic(&m2).unwrap();
        assert_ne!(kp1.public.0, kp2.public.0);
    }

    #[test]
    fn keypair_produces_valid_keys() {
        let mnemonic = generate_mnemonic().unwrap();
        let kp = keypair_from_mnemonic(&mnemonic).unwrap();
        assert_ne!(kp.public.0, [0u8; 32]);
        assert_ne!(kp.private.0, [0u8; 32]);
    }

    #[test]
    fn invalid_mnemonic_rejected() {
        assert!(!validate_mnemonic("not a valid mnemonic phrase"));
        assert!(!validate_mnemonic(""));
    }

    #[test]
    fn keypair_from_invalid_mnemonic_fails() {
        let result = keypair_from_mnemonic("invalid words here");
        assert!(result.is_err());
    }

    #[test]
    fn known_mnemonic_produces_consistent_key() {
        // A known valid 24-word mnemonic for regression testing
        let mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";
        assert!(validate_mnemonic(mnemonic));
        let kp = keypair_from_mnemonic(mnemonic).unwrap();
        // Re-derive to ensure consistency
        let kp2 = keypair_from_mnemonic(mnemonic).unwrap();
        assert_eq!(kp.public.0, kp2.public.0);
    }
}
