use thiserror::Error;

#[derive(Debug, Error)]
pub enum VrfError {
    #[error("failed to fetch randomness: {0}")]
    FetchFailed(String),

    #[error("verification failed: {0}")]
    VerificationFailed(String),

    #[error("invalid proof: {0}")]
    InvalidProof(String),

    #[error("provider not available: {0}")]
    Unavailable(String),

    #[error("commit-reveal: {0}")]
    CommitReveal(String),

    #[error("drand fetch error: {0}")]
    DrandFetch(String),

    #[error("invalid BLS public key: {0}")]
    InvalidPublicKey(String),

    #[error("invalid BLS signature: {0}")]
    InvalidSignature(String),

    #[error("BLS verification error: {0}")]
    BlsVerification(String),

    #[error("beacon round {round} is from the future (available at UNIX {available_at})")]
    FutureRound { round: u64, available_at: u64 },

    #[error("{0}")]
    Other(String),
}
