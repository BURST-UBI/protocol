use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConsensusError {
    #[error("fork detected for account {account}: blocks {block_a} and {block_b}")]
    ForkDetected {
        account: String,
        block_a: String,
        block_b: String,
    },

    #[error("insufficient voting weight: {have} < {need}")]
    InsufficientWeight { have: u128, need: u128 },

    #[error("representative {0} not found")]
    RepresentativeNotFound(String),

    #[error("{0}")]
    Other(String),
}
