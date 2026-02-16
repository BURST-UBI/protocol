//! Key management — primary and delegation key pairs.

use burst_types::KeyPair;
use crate::error::WalletError;

/// Generate a new primary key pair for a wallet.
pub fn generate_primary_keypair() -> Result<KeyPair, WalletError> {
    todo!("generate Ed25519 key pair")
}

/// Generate a delegation key pair for vote delegation.
pub fn generate_delegation_keypair() -> Result<KeyPair, WalletError> {
    todo!("generate secondary Ed25519 key pair for delegation")
}

/// Export a private key as bytes (for backup).
pub fn export_private_key(_key: &burst_types::PrivateKey) -> Vec<u8> {
    todo!("serialize private key to bytes")
}

/// Import a private key from bytes (for restoration).
pub fn import_private_key(_bytes: &[u8]) -> Result<burst_types::PrivateKey, WalletError> {
    todo!("deserialize private key from bytes")
}
