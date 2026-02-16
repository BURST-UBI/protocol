//! Top-level error type shared across crates.

use thiserror::Error;

/// Common error type for the BURST protocol.
#[derive(Debug, Error)]
pub enum BurstError {
    #[error("insufficient BRN balance: need {needed}, have {available}")]
    InsufficientBrn { needed: u128, available: u128 },

    #[error("insufficient TRST balance: need {needed}, have {available}")]
    InsufficientTrst { needed: u128, available: u128 },

    #[error("TRST token has expired")]
    TrstExpired,

    #[error("TRST token has been revoked")]
    TrstRevoked,

    #[error("invalid signature")]
    InvalidSignature,

    #[error("invalid wallet address: {0}")]
    InvalidAddress(String),

    #[error("wallet is not verified")]
    WalletNotVerified,

    #[error("wallet is revoked")]
    WalletRevoked,

    #[error("duplicate transaction hash")]
    DuplicateTransaction,

    #[error("invalid block: {reason}")]
    InvalidBlock { reason: String },

    #[error("invalid proof of work")]
    InvalidWork,

    #[error("governance error: {0}")]
    Governance(String),

    #[error("verification error: {0}")]
    Verification(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("network error: {0}")]
    Network(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("{0}")]
    Other(String),
}
