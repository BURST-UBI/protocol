//! Ed25519 key generation.

use burst_types::{KeyPair, PrivateKey, PublicKey};

/// Generate a new Ed25519 key pair from a secure random source.
pub fn generate_keypair() -> KeyPair {
    todo!("use ed25519_dalek::SigningKey::generate(&mut OsRng)")
}

/// Derive the public key from a private key.
pub fn public_from_private(_private: &PrivateKey) -> PublicKey {
    todo!("use ed25519_dalek::SigningKey -> VerifyingKey")
}

/// Reconstruct a full key pair from a private key.
pub fn keypair_from_private(private: PrivateKey) -> KeyPair {
    let public = public_from_private(&private);
    KeyPair { public, private }
}
