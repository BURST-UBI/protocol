//! Core wallet struct.

use burst_types::{KeyPair, Timestamp, WalletAddress, WalletState};
use crate::error::WalletError;

/// A BURST wallet.
pub struct Wallet {
    /// Primary key pair (identity and transaction signing).
    pub primary_keys: KeyPair,
    /// Wallet address (derived from primary public key).
    pub address: WalletAddress,
    /// Current verification state.
    pub state: WalletState,
    /// When this wallet was verified (None if not verified).
    pub verified_at: Option<Timestamp>,
}

impl Wallet {
    /// Create a new wallet with fresh key pair.
    pub fn create() -> Result<Self, WalletError> {
        todo!("generate key pair, derive address")
    }

    /// Restore a wallet from an existing private key.
    pub fn from_private_key(_private_key_bytes: &[u8]) -> Result<Self, WalletError> {
        todo!("reconstruct key pair, derive address, query state from node")
    }

    /// Get the current BRN balance (computed from time).
    pub fn brn_balance(&self, _now: Timestamp, _brn_rate: u128) -> u128 {
        todo!("compute BRN based on verified_at, rate, total_burned")
    }

    /// Get transferable TRST balance.
    pub fn trst_balance(&self) -> u128 {
        todo!("query node for current TRST holdings")
    }

    /// Get expired TRST (virtue points / reputation).
    pub fn trst_expired(&self) -> u128 {
        todo!("query node for expired TRST")
    }

    /// Get revoked TRST.
    pub fn trst_revoked(&self) -> u128 {
        todo!("query node for revoked TRST")
    }

    /// Sign a message with the primary private key.
    pub fn sign(&self, _message: &[u8]) -> burst_types::Signature {
        todo!("use primary private key to sign")
    }
}
