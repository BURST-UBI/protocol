//! Fundamental types for the BURST protocol.
//!
//! This crate defines the core types shared across every other crate in the workspace:
//! addresses, hashes, amounts, timestamps, protocol parameters, and state enums.

pub mod address;
pub mod amount;
pub mod appeal;
pub mod block;
pub mod error;
pub mod hash;
pub mod keys;
pub mod network;
pub mod params;
pub mod payment_recovery;
pub mod state;
pub mod time;

pub use address::WalletAddress;
pub use amount::{BrnAmount, TrstAmount, BRN_UNIT, MBRN_UNIT, MTRST_UNIT, TRST_UNIT};
pub use block::BlockHash;
pub use error::BurstError;
pub use hash::TxHash;
pub use keys::{KeyPair, PrivateKey, PublicKey, Signature};
pub use network::NetworkId;
pub use params::ProtocolParams;
pub use state::{
    ChallengeReason, TrstBasis, TrstRevocationChunk, TrstState, WalletState,
    INACTIVITY_CHALLENGE_MARKER,
};
pub use time::Timestamp;

/// Maximum P2P message body, excluding the four-byte framing prefix (decision 23.15).
pub const MAX_P2P_MESSAGE_BYTES: usize = 64 * 1024;

/// Signed blocks carry account-chain signing-device grants.
pub const PROTOCOL_VERSION: u16 = 36;
