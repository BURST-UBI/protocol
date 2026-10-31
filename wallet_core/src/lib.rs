//! Wallet core library for BURST.
//!
//! Shared wallet operations. Applications supply the interface and storage policy.
//! File-backed journals and signing-device bindings are native-only.
//!
//! Modules include:
//! - Key generation and management (primary + delegation key pairs)
//! - BRN balance display (computed from time)
//! - TRST portfolio (transferable, expired, revoked)
//! - Transaction building and signing (burn, send, merge)
//! - Delegation management
//! - Voting interface
//! - Group trust policy evaluation

pub mod appeals;
pub mod auto_merge;
pub mod balance;
pub mod custodianship;
pub mod delegation;
pub mod error;
pub mod governance_content;
pub mod keys;
pub mod keystore;
pub mod offline;
pub mod portfolio;
pub mod provenance;
mod runtime;
pub mod signed_client;
#[cfg(not(target_arch = "wasm32"))]
pub mod signing_device;
pub mod transaction_builder;
pub mod trust_policy;
pub mod wallet;

pub use custodianship::{
    Custodianship, CustodianshipError, CustodianshipRegistry, CustodianshipStatus,
};
pub use error::WalletError;
pub use keystore::{decrypt_keystore, encrypt_keystore, KeystoreFile};
#[cfg(not(target_arch = "wasm32"))]
pub use keystore::{load_keystore, save_keystore};
pub use wallet::{NodeClient, Wallet};
