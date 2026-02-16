use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkError {
    #[error("work difficulty {actual} below minimum {minimum}")]
    InsufficientDifficulty { actual: u64, minimum: u64 },

    #[error("work generation cancelled")]
    Cancelled,
}
