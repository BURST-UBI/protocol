use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("key not found: {0}")]
    NotFound(String),

    #[error("duplicate key: {0}")]
    Duplicate(String),

    #[error("storage backend error: {0}")]
    Backend(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("database is corrupted: {0}")]
    Corruption(String),
}
