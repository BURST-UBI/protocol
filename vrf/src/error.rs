use thiserror::Error;

#[derive(Debug, Error)]
pub enum VrfError {
    #[error("failed to fetch randomness: {0}")]
    FetchFailed(String),

    #[error("verification failed: {0}")]
    VerificationFailed(String),

    #[error("invalid proof")]
    InvalidProof,

    #[error("provider not available: {0}")]
    Unavailable(String),

    #[error("commit-reveal: {0}")]
    CommitReveal(String),

    #[error("{0}")]
    Other(String),
}
