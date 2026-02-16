//! TRST-specific errors.

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

    #[error("cannot merge zero tokens")]
    EmptyMerge,

    #[error("token {0} not found")]
    TokenNotFound(String),

    #[error("origin {0} not found in merger graph")]
    OriginNotFound(String),
}
