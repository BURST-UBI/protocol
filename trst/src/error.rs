//! TRST-specific errors.

use burst_types::WalletAddress;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TrstError {
    #[error("TRST token has expired")]
    Expired,

    #[error("TRST token has been revoked")]
    Revoked,

    #[error("TRST token is not transferable (state: {0})")]
    NotTransferable(String),

    #[error("insufficient TRST: need {needed}, have {available}")]
    InsufficientBalance { needed: u128, available: u128 },

    #[error("split amounts ({total}) do not equal parent amount ({parent})")]
    SplitMismatch { total: u128, parent: u128 },

    #[error("merge requires at least 2 tokens")]
    EmptyMerge,

    #[error("token not owned by {actual}, expected {expected}")]
    NotOwner {
        expected: WalletAddress,
        actual: WalletAddress,
    },

    #[error("token {0} not found")]
    TokenNotFound(String),

    #[error("origin {0} not found in merger graph")]
    OriginNotFound(String),

    #[error("token {0} is not in Revoked state, cannot un-revoke")]
    NotRevoked(String),

    #[error("token {0} is not in Pending state")]
    NotPending(String),

    #[error("{0}")]
    Other(String),
}
