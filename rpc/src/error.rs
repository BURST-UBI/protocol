use thiserror::Error;

#[derive(Debug, Error)]
pub enum RpcError {
    #[error("account not found: {0}")]
    AccountNotFound(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("node error: {0}")]
    Node(String),

    #[error("server error: {0}")]
    Server(String),
}
