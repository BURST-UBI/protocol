//! Wallet core library for BURST.
//!
//! Provides everything a wallet application needs:
//! - Key generation and management (primary + delegation key pairs)
//! - BRN balance display (computed from time)
//! - TRST portfolio (transferable, expired, revoked)
//! - Transaction building and signing (burn, send, split, merge)
//! - Delegation management
//! - Voting interface
//! - Group trust policy evaluation

pub mod auto_merge;
pub mod balance;
pub mod custodianship;
pub mod delegation;
pub mod error;
pub mod keys;
pub mod keystore;
pub mod portfolio;
pub mod transaction_builder;
pub mod trust_policy;
pub mod wallet;

pub use custodianship::{
    Custodianship, CustodianshipError, CustodianshipRegistry, CustodianshipStatus,
};
pub use error::WalletError;
pub use keystore::{
    decrypt_keystore, encrypt_keystore, load_keystore, save_keystore, KeystoreFile,
};
pub use wallet::{NodeClient, Wallet};
