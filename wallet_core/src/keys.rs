//! Key management — primary and delegation key pairs.

use burst_types::KeyPair;
use crate::error::WalletError;

/// Generate a new primary key pair for a wallet.
pub fn generate_primary_keypair() -> Result<KeyPair, WalletError> {
    Ok(burst_crypto::generate_keypair())
}

/// Generate a delegation key pair for vote delegation.
pub fn generate_delegation_keypair() -> Result<KeyPair, WalletError> {
    Ok(burst_crypto::generate_keypair())
}

/// Export a private key as bytes (for backup).
pub fn export_private_key(key: &burst_types::PrivateKey) -> Vec<u8> {
    key.0.to_vec()
}

/// Import a private key from bytes (for restoration).
pub fn import_private_key(bytes: &[u8]) -> Result<burst_types::PrivateKey, WalletError> {
    if bytes.len() != 32 {
        return Err(WalletError::Key(format!(
            "private key must be 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(bytes);
    Ok(burst_types::PrivateKey(key_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_primary_keypair() {
        let kp = generate_primary_keypair().unwrap();
        // Key pair should have valid public and private keys
        assert_eq!(kp.public.as_bytes().len(), 32);
        assert_eq!(kp.private.0.len(), 32);
    }

    #[test]
    fn test_generate_delegation_keypair() {
        let kp = generate_delegation_keypair().unwrap();
        // Key pair should have valid public and private keys
        assert_eq!(kp.public.as_bytes().len(), 32);
        assert_eq!(kp.private.0.len(), 32);
    }

    #[test]
    fn test_generate_keypairs_unique() {
        let kp1 = generate_primary_keypair().unwrap();
        let kp2 = generate_primary_keypair().unwrap();
        
        // Each key pair should be unique
        assert_ne!(kp1.public.as_bytes(), kp2.public.as_bytes());
        assert_ne!(kp1.private.0, kp2.private.0);
    }

    #[test]
    fn test_export_import_private_key() {
        let kp = generate_primary_keypair().unwrap();
        
        // Export private key
        let exported = export_private_key(&kp.private);
        assert_eq!(exported.len(), 32);
        assert_eq!(exported, kp.private.0.to_vec());
        
        // Import private key
        let imported = import_private_key(&exported).unwrap();
        assert_eq!(imported.0, kp.private.0);
    }

    #[test]
    fn test_import_private_key_invalid_length() {
        // Test with wrong length
        let short_key = vec![0u8; 16];
        assert!(import_private_key(&short_key).is_err());
        
        let long_key = vec![0u8; 64];
        assert!(import_private_key(&long_key).is_err());
    }

    #[test]
    fn test_export_import_roundtrip() {
        let original_key = burst_types::PrivateKey([42u8; 32]);
        
        let exported = export_private_key(&original_key);
        let imported = import_private_key(&exported).unwrap();
        
        assert_eq!(original_key.0, imported.0);
    }
}
