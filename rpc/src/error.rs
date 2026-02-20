//! RPC error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("account not found: {0}")]
    AccountNotFound(String),

    #[error("block not found: {0}")]
    BlockNotFound(String),

    #[error("proposal not found: {0}")]
    ProposalNotFound(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("node not connected")]
    NodeNotConnected,

    #[error("node error: {0}")]
    Node(String),

    #[error("server error: {0}")]
    Server(String),

    #[error("store error: {0}")]
    Store(String),

    #[error("work generation error: {0}")]
    WorkError(String),

    #[error("rate limited")]
    RateLimited,
}

impl From<burst_store::StoreError> for RpcError {
    fn from(e: burst_store::StoreError) -> Self {
        match e {
            burst_store::StoreError::NotFound(ref key) => {
                RpcError::Store(format!("not found: {key}"))
            }
            other => RpcError::Store(other.to_string()),
        }
    }
}
