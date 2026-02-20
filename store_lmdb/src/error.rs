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

impl From<heed::Error> for LmdbError {
    fn from(e: heed::Error) -> Self {
        LmdbError::Heed(e.to_string())
    }
}

impl From<bincode::Error> for LmdbError {
    fn from(e: bincode::Error) -> Self {
        LmdbError::Serialization(e.to_string())
    }
}

impl From<LmdbError> for burst_store::StoreError {
    fn from(e: LmdbError) -> Self {
        match e {
            LmdbError::NotFound(msg) => burst_store::StoreError::NotFound(msg),
            LmdbError::Serialization(msg) => burst_store::StoreError::Serialization(msg),
            LmdbError::Heed(msg) => burst_store::StoreError::Backend(msg),
        }
    }
}
