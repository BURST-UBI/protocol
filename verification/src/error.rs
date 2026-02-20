use thiserror::Error;

#[derive(Debug, Error)]
pub enum VerificationError {
    #[error("endorsement threshold not met: have {have}, need {need}")]
    ThresholdNotMet { have: u32, need: u32 },

    #[error("wallet {0} is already verified")]
    AlreadyVerified(String),

    #[error("wallet {0} is already under verification")]
    AlreadyInProgress(String),

    #[error("verifier {0} was not selected for this verification")]
    NotSelected(String),

    #[error("verifier {0} has already voted")]
    AlreadyVoted(String),

    #[error("maximum revotes ({0}) exceeded")]
    MaxRevotesExceeded(u32),

    #[error("no active challenge for wallet {0}")]
    NoChallengeActive(String),

    #[error("endorser {0} has already endorsed this wallet")]
    AlreadyEndorsed(String),

    #[error("verifier {0} is penalized for excessive Neither votes")]
    NeitherPenalty(String),

    #[error("challenger {0} is not verified")]
    ChallengerNotVerified(String),

    #[error("insufficient stake: needed {needed}, provided {provided}")]
    InsufficientStake { needed: u128, provided: u128 },

    #[error("bootstrap phase has ended — normal verification rules apply")]
    BootstrapPhaseEnded,

    #[error("only the genesis creator can perform genesis endorsements")]
    NotGenesisCreator,

    #[error("self-verification is not allowed")]
    SelfVerification,

    #[error("BRN error: {0}")]
    Brn(String),

    #[error("{0}")]
    Other(String),
}
