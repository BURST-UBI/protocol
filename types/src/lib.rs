//! Fundamental types for the BURST protocol.
//!
//! Shared addresses, hashes, amounts, timestamps, parameters and state enums.
//! These types do not by themselves establish protocol validity.

pub mod address;
pub mod amount;
pub mod appeal;
pub mod block_hash;
pub mod error;
pub mod keys;
pub mod network;
pub mod params;
pub mod payment_recovery;
pub mod state;
pub mod timestamp;
pub mod transaction_hash;

pub use address::WalletAddress;
pub use amount::{BrnAmount, TrstAmount, BRN_UNIT, MBRN_UNIT, MTRST_UNIT, TRST_UNIT};
pub use block_hash::BlockHash;
pub use error::BurstError;
pub use keys::{KeyPair, PrivateKey, PublicKey, Signature};
pub use network::NetworkId;
pub use params::ProtocolParams;
pub use state::{
    ChallengeReason, TrstBasis, TrstReference, TrstRevocationChunk, TrstState, WalletState,
    INACTIVITY_CHALLENGE_MARKER,
};
pub use timestamp::Timestamp;
pub use transaction_hash::TxHash;

/// Maximum P2P message body, excluding the four-byte framing prefix.
pub const MAX_P2P_MESSAGE_BYTES: usize = 64 * 1024;

/// Peer compatibility version, distinct from the signed block format version.
pub const PROTOCOL_VERSION: u16 = 39;

mod transport;
pub use transport::{PeerAddress, TrafficClass};
mod protocol_error;
pub use protocol_error::ProtocolError;
