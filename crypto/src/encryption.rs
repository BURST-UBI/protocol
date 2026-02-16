//! Encryption helpers for delegation key sharing.
//!
//! Uses X25519 Diffie-Hellman for key agreement, then symmetric encryption
//! of the delegation private key so only the delegate can decrypt it.

/// Encrypt a delegation private key for a specific delegate.
///
/// The sender's Ed25519 key is converted to X25519 for DH key exchange.
/// The resulting shared secret is used as an AES-256-GCM key.
pub fn encrypt_delegation_key(
    _delegation_private_key: &[u8],
    _delegate_public_key: &[u8; 32],
    _sender_private_key: &[u8; 32],
) -> Vec<u8> {
    todo!("X25519 DH -> shared secret -> AES-256-GCM encrypt")
}

/// Decrypt a delegation private key that was encrypted for this delegate.
pub fn decrypt_delegation_key(
    _encrypted: &[u8],
    _sender_public_key: &[u8; 32],
    _delegate_private_key: &[u8; 32],
) -> Result<Vec<u8>, &'static str> {
    todo!("X25519 DH -> shared secret -> AES-256-GCM decrypt")
}
