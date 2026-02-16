use thiserror::Error;

#[derive(Debug, Error)]
pub enum LmdbError {
    #[error("LMDB error: {0}")]
    Heed(String),

    #[error("key not found: {0}")]
    NotFound(String),

    #[error("serialization error: {0}")]
    Serialization(String),
}

impl From<LmdbError> for burst_store::StoreError {
    fn from(e: LmdbError) -> Self {
        burst_store::StoreError::Backend(e.to_string())
    }
}
