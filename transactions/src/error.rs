use thiserror::Error;

#[derive(Debug, Error)]
pub enum TransactionError {
    #[error("invalid signature on transaction {tx_hash}")]
    InvalidSignature { tx_hash: String },

    #[error("sender wallet is not verified")]
    SenderNotVerified,

    #[error("invalid timestamp: {reason}")]
    InvalidTimestamp { reason: String },

    #[error("transaction references unknown parent: {hash}")]
    UnknownParent { hash: String },

    #[error("amount must be positive")]
    ZeroAmount,

    #[error("{0}")]
    Brn(String),

    #[error("{0}")]
    Trst(String),

    #[error("{0}")]
    Other(String),
}
