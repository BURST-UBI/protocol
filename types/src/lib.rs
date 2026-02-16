//! Fundamental types for the BURST protocol.
//!
//! This crate defines the core types shared across every other crate in the workspace:
//! addresses, hashes, amounts, timestamps, protocol parameters, and state enums.

pub mod address;
pub mod amount;
pub mod block;
pub mod error;
pub mod hash;
pub mod keys;
pub mod network;
pub mod params;
pub mod state;
pub mod time;

pub use address::WalletAddress;
pub use amount::{BrnAmount, TrstAmount};
pub use block::BlockHash;
pub use error::BurstError;
pub use hash::TxHash;
pub use keys::{KeyPair, PrivateKey, PublicKey, Signature};
pub use network::NetworkId;
pub use params::ProtocolParams;
pub use state::{TrstState, WalletState};
pub use time::Timestamp;
