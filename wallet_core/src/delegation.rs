//! Delegation management — delegate and revoke voting power.

use burst_types::{PrivateKey, Signature, WalletAddress};
use crate::error::WalletError;

/// Delegate voting power to a representative.
///
/// Generates a delegation key pair, encrypts the private key for the delegate
/// using X25519 Diffie-Hellman, and builds the delegation transaction.
/// The transaction must be signed externally by the delegator's primary key.
///
/// Key derivation uses the standard Ed25519-to-X25519 conversion:
/// - Delegator: X25519 secret derived from their Ed25519 private key via `to_scalar_bytes()`
/// - Delegate: X25519 public derived from their Ed25519 public key via `to_montgomery()`
///
/// The delegator's X25519 public key is included in the transaction so the
/// delegate can reconstruct the DH shared secret and decrypt.
pub fn create_delegation(
    delegator: &WalletAddress,
    delegate: &WalletAddress,
    delegator_private: &PrivateKey,
) -> Result<burst_transactions::delegate::DelegateTx, WalletError> {
    let delegation_keys = burst_crypto::generate_keypair();
    let delegation_public_key = delegation_keys.public.as_bytes().to_vec();

    let delegator_x25519_secret = burst_crypto::ed25519_private_to_x25519(&delegator_private.0);
    let delegator_x25519_pub = x25519_dalek::PublicKey::from(
        &x25519_dalek::StaticSecret::from(delegator_x25519_secret),
    );

    let delegate_ed25519_pub = burst_crypto::decode_address(delegate.as_str())
        .ok_or(WalletError::InvalidAddress(delegate.as_str().to_string()))?;
    let delegate_x25519_pub_bytes = burst_crypto::ed25519_public_to_x25519(&delegate_ed25519_pub)
        .ok_or(WalletError::InvalidAddress(format!(
            "failed to convert {} to X25519",
            delegate.as_str()
        )))?;

    let encrypted_delegation_key = burst_crypto::encrypt_delegation_key(
        &delegation_keys.private.0,
        &delegate_x25519_pub_bytes,
        &delegator_x25519_secret,
    );

    let tx_bytes = burst_crypto::blake2b_256_multi(&[
        b"delegate",
        delegator.as_str().as_bytes(),
        delegate.as_str().as_bytes(),
        &delegation_public_key,
    ]);
    let hash = burst_types::TxHash::new(tx_bytes);

    Ok(burst_transactions::delegate::DelegateTx {
        hash,
        delegator: delegator.clone(),
        delegate: delegate.clone(),
        delegation_public_key,
        encrypted_delegation_key,
        delegator_x25519_public: delegator_x25519_pub.as_bytes().to_vec(),
        timestamp: burst_types::Timestamp::now(),
        work: 0,
        signature: Signature([0u8; 64]),
    })
}

/// Decrypt the delegation private key from a received DelegateTx.
///
/// The delegate uses their own Ed25519 private key to derive their X25519
/// secret, then performs DH with the delegator's X25519 public key
/// (included in the transaction) to recover the encryption key.
pub fn receive_delegation(
    tx: &burst_transactions::delegate::DelegateTx,
    delegate_private: &PrivateKey,
) -> Result<[u8; 32], WalletError> {
    let delegate_x25519_secret = burst_crypto::ed25519_private_to_x25519(&delegate_private.0);

    if tx.delegator_x25519_public.len() != 32 {
        return Err(WalletError::Key(
            "delegation tx missing delegator X25519 public key".into(),
        ));
    }
    let mut delegator_x25519_pub = [0u8; 32];
    delegator_x25519_pub.copy_from_slice(&tx.delegator_x25519_public);

    burst_crypto::decrypt_delegation_key(
        &tx.encrypted_delegation_key,
        &delegator_x25519_pub,
        &delegate_x25519_secret,
    )
    .map_err(|e| WalletError::Key(format!("delegation decryption failed: {e}")))
}

/// Revoke a delegation by generating a new delegation public key.
///
/// Broadcasting this transaction (signed by the primary key) invalidates
/// any previous delegation, since the old delegation key pair is replaced.
pub fn revoke_delegation(
    delegator: &WalletAddress,
) -> Result<burst_transactions::delegate::RevokeDelegationTx, WalletError> {
    let new_keys = burst_crypto::generate_keypair();
    let new_delegation_public_key = new_keys.public.as_bytes().to_vec();

    let tx_bytes = burst_crypto::blake2b_256_multi(&[
        b"revoke-delegation",
        delegator.as_str().as_bytes(),
        &new_delegation_public_key,
    ]);
    let hash = burst_types::TxHash::new(tx_bytes);

    Ok(burst_transactions::delegate::RevokeDelegationTx {
        hash,
        delegator: delegator.clone(),
        new_delegation_public_key,
        timestamp: burst_types::Timestamp::now(),
        work: 0,
        signature: Signature([0u8; 64]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use burst_crypto::{derive_address, keypair_from_seed};

    fn make_kp(seed: u8) -> burst_types::KeyPair {
        keypair_from_seed(&[seed; 32])
    }

    #[test]
    fn delegation_create_receive_roundtrip() {
        let delegator_kp = make_kp(0xAA);
        let delegate_kp = make_kp(0xBB);
        let delegator_addr = derive_address(&delegator_kp.public);
        let delegate_addr = derive_address(&delegate_kp.public);

        let tx = create_delegation(&delegator_addr, &delegate_addr, &delegator_kp.private)
            .expect("create_delegation should succeed");

        assert_eq!(tx.delegator, delegator_addr);
        assert_eq!(tx.delegate, delegate_addr);
        assert!(!tx.delegation_public_key.is_empty());
        assert!(!tx.encrypted_delegation_key.is_empty());
        assert_eq!(tx.delegator_x25519_public.len(), 32);

        let decrypted = receive_delegation(&tx, &delegate_kp.private)
            .expect("receive_delegation should succeed");
        assert_eq!(decrypted.len(), 32);
        assert_ne!(decrypted, [0u8; 32], "decrypted key should be non-zero");
    }

    #[test]
    fn delegation_wrong_key_fails_decryption() {
        let delegator_kp = make_kp(0xCC);
        let delegate_kp = make_kp(0xDD);
        let wrong_kp = make_kp(0xEE);
        let delegator_addr = derive_address(&delegator_kp.public);
        let delegate_addr = derive_address(&delegate_kp.public);

        let tx = create_delegation(&delegator_addr, &delegate_addr, &delegator_kp.private)
            .expect("create_delegation should succeed");

        let result = receive_delegation(&tx, &wrong_kp.private);
        assert!(result.is_err(), "wrong private key should fail decryption");
    }

    #[test]
    fn delegation_deterministic_for_same_keys() {
        let delegator_kp = make_kp(0x11);
        let delegate_kp = make_kp(0x22);
        let delegator_addr = derive_address(&delegator_kp.public);
        let delegate_addr = derive_address(&delegate_kp.public);

        let tx1 = create_delegation(&delegator_addr, &delegate_addr, &delegator_kp.private).unwrap();
        let tx2 = create_delegation(&delegator_addr, &delegate_addr, &delegator_kp.private).unwrap();

        // Delegation keys are randomly generated each time, so encrypted keys differ
        assert_ne!(tx1.encrypted_delegation_key, tx2.encrypted_delegation_key);
        // But both should be decryptable by the delegate
        let k1 = receive_delegation(&tx1, &delegate_kp.private).unwrap();
        let k2 = receive_delegation(&tx2, &delegate_kp.private).unwrap();
        assert_ne!(k1, k2, "different delegation key pairs each time");
    }

    #[test]
    fn revoke_delegation_produces_new_key() {
        let delegator_addr = derive_address(&make_kp(0x33).public);
        let tx1 = revoke_delegation(&delegator_addr).unwrap();
        let tx2 = revoke_delegation(&delegator_addr).unwrap();

        assert_eq!(tx1.delegator, delegator_addr);
        assert_ne!(
            tx1.new_delegation_public_key, tx2.new_delegation_public_key,
            "each revocation generates a fresh key"
        );
    }

    #[test]
    fn delegation_invalid_delegate_address_fails() {
        let delegator_kp = make_kp(0x44);
        let delegator_addr = derive_address(&delegator_kp.public);
        let bad_addr = WalletAddress::new("brst_invalid_not_a_real_address");

        let result = create_delegation(&delegator_addr, &bad_addr, &delegator_kp.private);
        assert!(result.is_err());
    }
}
