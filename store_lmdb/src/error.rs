use thiserror::Error;

#[derive(Debug, Error)]
pub enum LmdbError {
    #[error("LMDB error: {0}")]
    Heed(String),

    #[error("LMDB error: {0}")]
    MapFull(String),

    #[error("LMDB error: {0}")]
    GrowthLimit(String),

    #[error("key not found: {0}")]
    NotFound(String),

    #[error("serialization error: {0}")]
    Serialization(String),
}

impl From<heed::Error> for LmdbError {
    fn from(e: heed::Error) -> Self {
        match e {
            heed::Error::Mdb(heed::MdbError::GrowthLimit) => LmdbError::GrowthLimit(e.to_string()),
            heed::Error::Mdb(heed::MdbError::MapFull) => LmdbError::MapFull(e.to_string()),
            _ => LmdbError::Heed(e.to_string()),
        }
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
            LmdbError::GrowthLimit(msg) => burst_store::StoreError::GrowthLimit(msg),
            LmdbError::MapFull(msg) => burst_store::StoreError::MapFull(msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_capacity_is_typed_without_classifying_other_failures_as_retryable() {
        for native in [
            heed::MdbError::MapFull,
            heed::MdbError::TxnFull,
            heed::MdbError::ReadersFull,
            heed::MdbError::Corrupted,
            heed::MdbError::GrowthLimit,
        ] {
            let message = heed::Error::Mdb(native).to_string();
            let lmdb = LmdbError::from(heed::Error::Mdb(native));
            assert_eq!(lmdb.to_string(), format!("LMDB error: {message}"));
            let store = burst_store::StoreError::from(lmdb);
            assert_eq!(
                store.to_string(),
                format!("storage backend error: {message}")
            );
            assert_eq!(
                matches!(store, burst_store::StoreError::GrowthLimit(_)),
                native == heed::MdbError::GrowthLimit
            );
            assert_eq!(
                matches!(store, burst_store::StoreError::MapFull(_)),
                native == heed::MdbError::MapFull
            );
        }
        let io = std::io::Error::from(std::io::ErrorKind::WriteZero);
        let error = burst_store::StoreError::from(LmdbError::from(heed::Error::Io(io)));
        assert!(matches!(error, burst_store::StoreError::Backend(_)));
    }
}
