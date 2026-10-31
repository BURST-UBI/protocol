use thiserror::Error;

#[derive(Clone, Debug, Error)]
pub enum StoreError {
    #[error("key not found: {0}")]
    NotFound(String),

    #[error("duplicate key: {0}")]
    Duplicate(String),

    #[error("storage backend error: {0}")]
    Backend(String),

    /// Native map capacity was exhausted. This alone does not authorize retry:
    /// the caller must first release the failed transaction and its live guards.
    #[error("storage backend error: {0}")]
    MapFull(String),

    /// A process-local file-growth guard rejected a native page allocation.
    #[error("storage backend error: {0}")]
    GrowthLimit(String),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("database is corrupted: {0}")]
    Corruption(String),

    #[error("storage changed during preparation: {0}")]
    StaleRead(String),
}
