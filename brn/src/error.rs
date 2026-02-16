//! BRN-specific errors.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum BrnError {
    #[error("insufficient BRN: need {needed}, available {available}")]
    InsufficientBalance { needed: u128, available: u128 },

    #[error("wallet is not verified, cannot accrue BRN")]
    WalletNotVerified,

    #[error("stake {0} not found")]
    StakeNotFound(u64),

    #[error("stake {0} has already been resolved")]
    StakeAlreadyResolved(u64),

    #[error("BRN rate must be non-negative")]
    InvalidRate,
}
