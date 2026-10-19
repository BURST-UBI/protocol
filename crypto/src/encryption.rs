//! Versioned delegation-key envelopes. Authorization belongs to the primary-key
//! signed grant; encryption alone does not authenticate the granting wallet.

use burst_types::PrivateKey;
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use rand::{rngs::OsRng, RngCore};
use x25519_dalek::{PublicKey as X25519Public, StaticSecret};
use zeroize::Zeroizing;

const MAGIC: &[u8; 4] = b"BDK\x01";
pub const DELEGATION_ENVELOPE_LEN: usize = 4 + 32 + 24 + 48;
const MAX_CONTEXT: usize = 4096;

pub fn validate_delegation_envelope(envelope: &[u8]) -> Result<(), &'static str> {
    if envelope.len() != DELEGATION_ENVELOPE_LEN || &envelope[..4] != MAGIC {
        return Err("invalid delegation envelope version or length");
    }
    Ok(())
}

fn cipher(
    shared: &x25519_dalek::SharedSecret,
    ephemeral: &[u8; 32],
    recipient: &[u8; 32],
    context: &[u8],
) -> Result<XChaCha20Poly1305, &'static str> {
    if context.is_empty() || context.len() > MAX_CONTEXT {
        return Err("delegation context is empty or oversized");
    }
    if !shared.was_contributory() {
        return Err("non-contributory delegation key exchange");
    }
    let key = Zeroizing::new(crate::blake2b_256_multi(&[
        b"BURST/delegation-key/v1\0",
        shared.as_bytes(),
        ephemeral,
        recipient,
        &(context.len() as u64).to_be_bytes(),
        context,
    ]));
    Ok(XChaCha20Poly1305::new((&*key).into()))
}

/// Encrypt a secondary seed with a fresh ephemeral X25519 key and random nonce.
/// Context must encode the grant's network, owner, delegate, scope, generation
/// and secondary public key. Decryption must use the authenticated grant's
/// context, not an independent context supplied by a peer.
pub fn encrypt_delegation_key(
    delegation_private_key: &[u8; 32],
    recipient_x25519_public: &[u8; 32],
    context: &[u8],
) -> Result<Vec<u8>, &'static str> {
    let mut seed = Zeroizing::new([0; 32]);
    OsRng
        .try_fill_bytes(&mut *seed)
        .map_err(|_| "delegation entropy unavailable")?;
    let secret = StaticSecret::from(*seed);
    let ephemeral = X25519Public::from(&secret);
    let recipient = X25519Public::from(*recipient_x25519_public);
    let shared = secret.diffie_hellman(&recipient);
    let cipher = cipher(&shared, ephemeral.as_bytes(), recipient.as_bytes(), context)?;
    let mut nonce = [0; 24];
    OsRng
        .try_fill_bytes(&mut nonce)
        .map_err(|_| "delegation entropy unavailable")?;
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: delegation_private_key,
                aad: context,
            },
        )
        .map_err(|_| "delegation encryption failed")?;
    let mut envelope = Vec::with_capacity(DELEGATION_ENVELOPE_LEN);
    envelope.extend_from_slice(MAGIC);
    envelope.extend_from_slice(ephemeral.as_bytes());
    envelope.extend_from_slice(&nonce);
    envelope.extend_from_slice(&ciphertext);
    Ok(envelope)
}

/// Reject legacy deterministic-nonce ciphertexts and noncanonical lengths.
/// The returned secret is zeroized on drop and cannot be formatted with Debug.
pub fn decrypt_delegation_key(
    envelope: &[u8],
    recipient_x25519_secret: &[u8; 32],
    context: &[u8],
) -> Result<PrivateKey, &'static str> {
    validate_delegation_envelope(envelope)?;
    let ephemeral = X25519Public::from(<[u8; 32]>::try_from(&envelope[4..36]).unwrap());
    let secret = StaticSecret::from(*recipient_x25519_secret);
    let recipient = X25519Public::from(&secret);
    let shared = secret.diffie_hellman(&ephemeral);
    let cipher = cipher(&shared, ephemeral.as_bytes(), recipient.as_bytes(), context)?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                XNonce::from_slice(&envelope[36..60]),
                Payload {
                    msg: &envelope[60..],
                    aad: context,
                },
            )
            .map_err(|_| "delegation authentication failed")?,
    );
    let seed = plaintext
        .as_slice()
        .try_into()
        .map_err(|_| "invalid delegation seed length")?;
    Ok(PrivateKey(seed))
}

#[cfg(test)]
mod tests {
    use super::*;
    const CONTEXT: &[u8] = b"test-network/owner/delegate/proposal/generation/key";
    fn recipient() -> ([u8; 32], [u8; 32]) {
        let secret = [7; 32];
        let public = X25519Public::from(&StaticSecret::from(secret)).to_bytes();
        (secret, public)
    }

    #[test]
    fn roundtrip_uses_fresh_ephemeral_key_and_nonce() {
        let (secret, public) = recipient();
        let a = encrypt_delegation_key(&[42; 32], &public, CONTEXT).unwrap();
        let b = encrypt_delegation_key(&[42; 32], &public, CONTEXT).unwrap();
        assert_eq!(a.len(), DELEGATION_ENVELOPE_LEN);
        assert_ne!(&a[4..36], &b[4..36]);
        assert_ne!(&a[36..60], &b[36..60]);
        assert_eq!(
            decrypt_delegation_key(&a, &secret, CONTEXT).unwrap().0,
            [42; 32]
        );
        assert_eq!(
            decrypt_delegation_key(&b, &secret, CONTEXT).unwrap().0,
            [42; 32]
        );
    }

    #[test]
    fn changing_any_envelope_byte_context_or_recipient_fails() {
        let (secret, public) = recipient();
        let envelope = encrypt_delegation_key(&[42; 32], &public, CONTEXT).unwrap();
        for i in 0..envelope.len() {
            let mut changed = envelope.clone();
            changed[i] ^= 1;
            assert!(
                decrypt_delegation_key(&changed, &secret, CONTEXT).is_err(),
                "byte {i}"
            );
        }
        assert!(decrypt_delegation_key(&envelope, &[8; 32], CONTEXT).is_err());
        assert!(decrypt_delegation_key(&envelope, &secret, b"another grant").is_err());
        assert!(decrypt_delegation_key(&envelope[..envelope.len() - 1], &secret, CONTEXT).is_err());
        let mut extended = envelope.clone();
        extended.push(0);
        assert!(decrypt_delegation_key(&extended, &secret, CONTEXT).is_err());
    }

    #[test]
    fn rejects_low_order_keys_legacy_format_and_empty_context() {
        let (secret, public) = recipient();
        for public in [[0; 32], {
            let mut one = [0; 32];
            one[0] = 1;
            one
        }] {
            assert!(encrypt_delegation_key(&[42; 32], &public, CONTEXT).is_err());
        }
        assert!(decrypt_delegation_key(&[0; 48], &secret, CONTEXT).is_err());
        assert!(encrypt_delegation_key(&[42; 32], &public, b"").is_err());
        assert!(encrypt_delegation_key(&[42; 32], &public, &[1; 4097]).is_err());
    }
}
