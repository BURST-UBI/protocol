//! Encryption helpers for delegation key sharing.
//!
//! Uses X25519 Diffie-Hellman for key agreement, then ChaCha20-Poly1305
//! AEAD for authenticated encryption of the delegation private key.

use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Nonce,
};
use x25519_dalek::{PublicKey as X25519Public, StaticSecret};

/// Encrypt a delegation private key for secure transmission.
///
/// Uses X25519 Diffie-Hellman key agreement + ChaCha20-Poly1305 AEAD.
/// The nonce is derived deterministically from the sender's public key
/// (first 12 bytes), which is unique per sender and known to both parties.
///
/// Returns the 48-byte ciphertext (32 bytes plaintext + 16 bytes auth tag).
pub fn encrypt_delegation_key(
    delegation_private_key: &[u8; 32],
    recipient_x25519_public: &[u8; 32],
    sender_x25519_secret: &[u8; 32],
) -> Vec<u8> {
    let secret = StaticSecret::from(*sender_x25519_secret);
    let recipient_pub = X25519Public::from(*recipient_x25519_public);
    let shared = secret.diffie_hellman(&recipient_pub);

    let sym_key = crate::hash::blake2b_256_multi(&[shared.as_bytes(), b"burst-delegation"]);
    let cipher = ChaCha20Poly1305::new_from_slice(&sym_key).expect("valid key length");

    let sender_pub = X25519Public::from(&secret);
    let mut nonce_bytes = [0u8; 12];
    nonce_bytes.copy_from_slice(&sender_pub.as_bytes()[..12]);
    let nonce = Nonce::from(nonce_bytes);

    cipher
        .encrypt(&nonce, delegation_private_key.as_ref())
        .expect("encryption should not fail")
}

/// Decrypt a delegation private key.
///
/// The sender's X25519 public key is needed both for DH shared secret
/// derivation and for reconstructing the deterministic nonce.
pub fn decrypt_delegation_key(
    encrypted: &[u8],
    sender_x25519_public: &[u8; 32],
    recipient_x25519_secret: &[u8; 32],
) -> Result<[u8; 32], &'static str> {
    let secret = StaticSecret::from(*recipient_x25519_secret);
    let sender_pub = X25519Public::from(*sender_x25519_public);
    let shared = secret.diffie_hellman(&sender_pub);

    let sym_key = crate::hash::blake2b_256_multi(&[shared.as_bytes(), b"burst-delegation"]);
    let cipher = ChaCha20Poly1305::new_from_slice(&sym_key).expect("valid key length");

    let mut nonce_bytes = [0u8; 12];
    nonce_bytes.copy_from_slice(&sender_pub.as_bytes()[..12]);
    let nonce = Nonce::from(nonce_bytes);

    let decrypted = cipher
        .decrypt(&nonce, encrypted)
        .map_err(|_| "decryption failed: authentication check failed")?;

    if decrypted.len() != 32 {
        return Err("invalid key length after decryption");
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&decrypted);
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let sender_secret = [1u8; 32];
        let delegate_secret = [2u8; 32];

        let sender_pub = X25519Public::from(&StaticSecret::from(sender_secret));
        let delegate_pub = X25519Public::from(&StaticSecret::from(delegate_secret));

        let delegation_key = [42u8; 32];
        let encrypted =
            encrypt_delegation_key(&delegation_key, delegate_pub.as_bytes(), &sender_secret);

        // 32 bytes plaintext + 16 bytes Poly1305 auth tag
        assert_eq!(encrypted.len(), 48);
        assert_ne!(&encrypted[..32], delegation_key.as_slice());

        let decrypted =
            decrypt_delegation_key(&encrypted, sender_pub.as_bytes(), &delegate_secret).unwrap();

        assert_eq!(decrypted, delegation_key);
    }

    #[test]
    fn wrong_key_fails_authentication() {
        let sender_secret = [1u8; 32];
        let delegate_secret = [2u8; 32];
        let wrong_secret = [3u8; 32];

        let delegate_pub = X25519Public::from(&StaticSecret::from(delegate_secret));
        let sender_pub = X25519Public::from(&StaticSecret::from(sender_secret));

        let delegation_key = [42u8; 32];
        let encrypted =
            encrypt_delegation_key(&delegation_key, delegate_pub.as_bytes(), &sender_secret);

        let result = decrypt_delegation_key(&encrypted, sender_pub.as_bytes(), &wrong_secret);

        assert!(
            result.is_err(),
            "AEAD should reject decryption with wrong key"
        );
    }

    #[test]
    fn tampered_ciphertext_fails_authentication() {
        let sender_secret = [1u8; 32];
        let delegate_secret = [2u8; 32];

        let delegate_pub = X25519Public::from(&StaticSecret::from(delegate_secret));
        let sender_pub = X25519Public::from(&StaticSecret::from(sender_secret));

        let delegation_key = [42u8; 32];
        let mut encrypted =
            encrypt_delegation_key(&delegation_key, delegate_pub.as_bytes(), &sender_secret);

        encrypted[0] ^= 0xFF;

        let result = decrypt_delegation_key(&encrypted, sender_pub.as_bytes(), &delegate_secret);

        assert!(result.is_err(), "AEAD should detect tampered ciphertext");
    }
}
