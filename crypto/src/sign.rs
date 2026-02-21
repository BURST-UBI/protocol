//! Ed25519 message signing and verification.

use burst_types::{PrivateKey, PublicKey, Signature};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};

/// Sign a message with a private key, returning the signature.
pub fn sign_message(message: &[u8], private_key: &PrivateKey) -> Signature {
    let signing_key = SigningKey::from_bytes(&private_key.0);
    let sig = signing_key.sign(message);
    Signature(sig.to_bytes())
}

/// Verify a signature against a message and public key.
///
/// Returns `true` if the signature is valid, `false` otherwise.
/// Also rejects non-canonical signatures (malleability protection).
pub fn verify_signature(message: &[u8], signature: &Signature, public_key: &PublicKey) -> bool {
    let Ok(verifying_key) = VerifyingKey::from_bytes(&public_key.0) else {
        return false;
    };
    let dalek_sig = ed25519_dalek::Signature::from_bytes(&signature.0);
    verifying_key.verify(message, &dalek_sig).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::generate_keypair;

    #[test]
    fn sign_and_verify() {
        let kp = generate_keypair();
        let msg = b"test message for burst protocol";
        let sig = sign_message(msg, &kp.private);
        assert!(verify_signature(msg, &sig, &kp.public));
    }

    #[test]
    fn wrong_message_fails() {
        let kp = generate_keypair();
        let sig = sign_message(b"correct message", &kp.private);
        assert!(!verify_signature(b"wrong message", &sig, &kp.public));
    }

    #[test]
    fn wrong_key_fails() {
        let kp1 = generate_keypair();
        let kp2 = generate_keypair();
        let msg = b"test";
        let sig = sign_message(msg, &kp1.private);
        assert!(!verify_signature(msg, &sig, &kp2.public));
    }

    #[test]
    fn signature_deterministic() {
        let seed = [99u8; 32];
        let kp = crate::keys::keypair_from_seed(&seed);
        let msg = b"deterministic test";
        let sig1 = sign_message(msg, &kp.private);
        let sig2 = sign_message(msg, &kp.private);
        assert_eq!(sig1.0, sig2.0);
    }

    #[test]
    fn empty_message() {
        let kp = generate_keypair();
        let sig = sign_message(b"", &kp.private);
        assert!(verify_signature(b"", &sig, &kp.public));
    }

    #[test]
    fn invalid_public_key() {
        let kp = generate_keypair();
        let sig = sign_message(b"test", &kp.private);
        let bad_key = PublicKey([0xFF; 32]);
        assert!(!verify_signature(b"test", &sig, &bad_key));
    }
}
