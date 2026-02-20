use thiserror::Error;

#[derive(Clone, Debug, Error)]
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

    #[error("election capacity reached: maximum {0} active elections")]
    ElectionCapacityReached(usize),

    #[error("election not found: {0}")]
    ElectionNotFound(String),

    #[error("final vote already cast by {0}")]
    FinalVoteAlreadyCast(String),

    #[error("election already confirmed")]
    ElectionAlreadyConfirmed,

    #[error("{0}")]
    Other(String),
}
