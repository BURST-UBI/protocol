use thiserror::Error;

#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("block gap: previous block {previous} not found")]
    BlockGap { previous: String },

    #[error("fork detected: account {account} has conflicting blocks")]
    Fork { account: String },

    #[error("invalid block: {reason}")]
    InvalidBlock { reason: String },

    #[error("account not found: {0}")]
    AccountNotFound(String),

    #[error("storage error: {0}")]
    Storage(#[from] burst_store::StoreError),
}
