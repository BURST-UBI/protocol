//! A 24-word BIP39 encoding of a 32-byte wallet seed.
//! Account zero uses BLAKE2b-256(seed || index.to_be_bytes()), as in Nano.

use bip39::Mnemonic;
use burst_types::KeyPair;
use thiserror::Error;
use zeroize::Zeroizing;

#[derive(Debug, Error)]
pub enum MnemonicError {
    #[error("invalid mnemonic phrase: {0}")]
    InvalidMnemonic(String),
    #[error("recovery requires a 24-word phrase encoding a 32-byte wallet seed")]
    InvalidEntropyLength,
    #[error("key derivation failed: {0}")]
    DerivationFailed(String),
}

/// Encode 256 bits of random wallet-seed entropy as 24 BIP39 words.
pub fn generate_mnemonic() -> Result<String, MnemonicError> {
    let mut entropy = Zeroizing::new([0u8; 32]);
    rand::RngCore::try_fill_bytes(&mut rand::rngs::OsRng, &mut *entropy)
        .map_err(|e| MnemonicError::DerivationFailed(e.to_string()))?;
    let mnemonic = Mnemonic::from_entropy(&*entropy)
        .map_err(|e| MnemonicError::DerivationFailed(e.to_string()))?;
    Ok(mnemonic.to_string())
}

/// Recover account zero from the phrase's original entropy, not a PBKDF2 seed.
/// This intentionally replaces BURST's former path-string HMAC derivation.
pub fn keypair_from_mnemonic(phrase: &str) -> Result<KeyPair, MnemonicError> {
    let mnemonic = Mnemonic::parse_normalized(phrase)
        .map_err(|e| MnemonicError::InvalidMnemonic(e.to_string()))?;
    let entropy = Zeroizing::new(mnemonic.to_entropy());
    let seed: &[u8; 32] = entropy.as_slice().try_into()
        .map_err(|_| MnemonicError::InvalidEntropyLength)?;
    Ok(crate::keypair_from_private(crate::derive_account_key(seed, 0)))
}

/// Whether the phrase has a valid BIP39 checksum and a 32-byte wallet seed.
pub fn validate_mnemonic(phrase: &str) -> bool {
    Mnemonic::parse_normalized(phrase).is_ok_and(|mnemonic| mnemonic.word_count() == 24)
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
    fn known_mnemonic_uses_entropy_and_account_zero() {
        let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";
        let keypair = keypair_from_mnemonic(phrase).unwrap();
        // Independent hashlib.blake2b(bytes(32) + bytes(4), digest_size=32).
        assert_eq!(keypair.private.0, [159, 14, 68, 76, 105, 247, 122, 73, 189, 11, 232, 157, 185, 44, 56, 254, 113, 62, 9, 99, 22, 92, 202, 18, 250, 245, 113, 45, 118, 87, 18, 15]);
        assert_eq!(keypair.public.0, crate::public_from_private(&keypair.private).0);
    }

    #[test]
    fn shorter_bip39_phrases_are_not_wallet_seeds() {
        for bytes in [16, 20, 24, 28] {
            let phrase = Mnemonic::from_entropy(&vec![0; bytes]).unwrap().to_string();
            assert!(!validate_mnemonic(&phrase));
            assert!(matches!(keypair_from_mnemonic(&phrase), Err(MnemonicError::InvalidEntropyLength)));
        }
    }
}
