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

    #[error("BRN error: {0}")]
    Brn(String),

    #[error("{0}")]
    Other(String),
}
