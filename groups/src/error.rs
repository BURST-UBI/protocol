use thiserror::Error;

#[derive(Debug, Error)]
pub enum GroupError {
    #[error("group {0} not found")]
    GroupNotFound(String),

    #[error("HTTP request to group endpoint failed: {0}")]
    RequestFailed(String),

    #[error("invalid response from group: {0}")]
    InvalidResponse(String),

    #[error("group endpoint unreachable: {0}")]
    Unreachable(String),

    #[error("{0}")]
    Other(String),
}
