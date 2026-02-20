//! Ed25519 key generation.

use burst_types::{KeyPair, PrivateKey, PublicKey};
use ed25519_dalek::SigningKey;
use rand::rngs::OsRng;

/// Generate a new Ed25519 key pair from a secure random source.
pub fn generate_keypair() -> KeyPair {
    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    KeyPair {
        public: PublicKey(verifying_key.to_bytes()),
        private: PrivateKey(signing_key.to_bytes()),
    }
}

/// Derive the public key from a private key.
pub fn public_from_private(private: &PrivateKey) -> PublicKey {
    let signing_key = SigningKey::from_bytes(&private.0);
    let verifying_key = signing_key.verifying_key();
    PublicKey(verifying_key.to_bytes())
}

/// Reconstruct a full key pair from a private key.
pub fn keypair_from_private(private: PrivateKey) -> KeyPair {
    let public = public_from_private(&private);
    KeyPair { public, private }
}

/// Derive a key pair from a 32-byte seed (deterministic).
///
/// This is useful for deriving keys from a BIP39 mnemonic + BIP44 derivation path.
pub fn keypair_from_seed(seed: &[u8; 32]) -> KeyPair {
    let signing_key = SigningKey::from_bytes(seed);
    let verifying_key = signing_key.verifying_key();
    KeyPair {
        public: PublicKey(verifying_key.to_bytes()),
        private: PrivateKey(signing_key.to_bytes()),
    }
}

/// Convert an Ed25519 private key (seed) to X25519 scalar bytes.
///
/// Uses `SigningKey::to_scalar_bytes()` which produces the unclamped
/// scalar suitable for use as an `x25519_dalek::StaticSecret`.
/// The corresponding X25519 public key is `ed25519_public_to_x25519(&public_key)`.
pub fn ed25519_private_to_x25519(ed25519_private: &[u8; 32]) -> [u8; 32] {
    let signing_key = SigningKey::from_bytes(ed25519_private);
    signing_key.to_scalar_bytes()
}

/// Convert an Ed25519 public key to its X25519 (Montgomery) equivalent.
///
/// Uses the birational map from Edwards to Montgomery form.
/// Returns `None` if the public key bytes are invalid.
pub fn ed25519_public_to_x25519(ed25519_public: &[u8; 32]) -> Option<[u8; 32]> {
    let verifying_key = ed25519_dalek::VerifyingKey::from_bytes(ed25519_public).ok()?;
    Some(verifying_key.to_montgomery().to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_valid_keypair() {
        let kp = generate_keypair();
        assert_ne!(kp.public.0, [0u8; 32]);
        assert_ne!(kp.private.0, [0u8; 32]);
    }

    #[test]
    fn public_from_private_is_deterministic() {
        let kp = generate_keypair();
        let pub2 = public_from_private(&kp.private);
        assert_eq!(kp.public.0, pub2.0);
    }

    #[test]
    fn keypair_from_private_roundtrip() {
        let kp1 = generate_keypair();
        let kp2 = keypair_from_private(PrivateKey(kp1.private.0));
        assert_eq!(kp1.public.0, kp2.public.0);
    }

    #[test]
    fn keypair_from_seed_deterministic() {
        let seed = [42u8; 32];
        let kp1 = keypair_from_seed(&seed);
        let kp2 = keypair_from_seed(&seed);
        assert_eq!(kp1.public.0, kp2.public.0);
        assert_eq!(kp1.private.0, kp2.private.0);
    }

    #[test]
    fn different_seeds_produce_different_keys() {
        let kp1 = keypair_from_seed(&[1u8; 32]);
        let kp2 = keypair_from_seed(&[2u8; 32]);
        assert_ne!(kp1.public.0, kp2.public.0);
    }

    #[test]
    fn ed25519_to_x25519_keypair_is_consistent() {
        let kp = generate_keypair();
        let x25519_secret = ed25519_private_to_x25519(&kp.private.0);
        let x25519_pub_from_ed = ed25519_public_to_x25519(&kp.public.0).unwrap();

        let static_secret = x25519_dalek::StaticSecret::from(x25519_secret);
        let x25519_pub_from_secret = x25519_dalek::PublicKey::from(&static_secret);

        assert_eq!(x25519_pub_from_ed, *x25519_pub_from_secret.as_bytes());
    }

    #[test]
    fn ed25519_to_x25519_is_deterministic() {
        let kp = keypair_from_seed(&[99u8; 32]);
        let x1 = ed25519_private_to_x25519(&kp.private.0);
        let x2 = ed25519_private_to_x25519(&kp.private.0);
        assert_eq!(x1, x2);
    }
}
