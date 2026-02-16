//! Ed25519 message signing and verification.

use burst_types::{PrivateKey, PublicKey, Signature};

/// Sign a message with a private key, returning the signature.
pub fn sign_message(_message: &[u8], _private_key: &PrivateKey) -> Signature {
    todo!("use ed25519_dalek::SigningKey::sign()")
}

/// Verify a signature against a message and public key.
pub fn verify_signature(
    _message: &[u8],
    _signature: &Signature,
    _public_key: &PublicKey,
) -> bool {
    todo!("use ed25519_dalek::VerifyingKey::verify()")
}
