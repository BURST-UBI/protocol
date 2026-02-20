use thiserror::Error;

#[derive(Debug, Error)]
pub enum WalletError {
    #[error("wallet not verified")]
    NotVerified,

    #[error("insufficient BRN: need {needed}, have {available}")]
    InsufficientBrn { needed: u128, available: u128 },

    #[error("insufficient TRST: need {needed}, have {available}")]
    InsufficientTrst { needed: u128, available: u128 },

    #[error("invalid address: {0}")]
    InvalidAddress(String),

    #[error("key error: {0}")]
    Key(String),

    #[error("transaction building error: {0}")]
    TransactionBuild(String),

    #[error("signing error: {0}")]
    Signing(String),

    #[error("node RPC error: {0}")]
    Node(String),

    #[error("no node connection configured")]
    NoNodeConnection,

    #[error("{0}")]
    Other(String),
}
