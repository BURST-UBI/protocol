use thiserror::Error;

#[derive(Debug, Error)]
pub enum GovernanceError {
    #[error("proposal {0} not found")]
    ProposalNotFound(String),

    #[error("proposal is not in the correct phase for this action")]
    WrongPhase,

    #[error("wallet {0} has already voted on this proposal")]
    AlreadyVoted(String),

    #[error("quorum not met: {have_bps} < {need_bps} basis points")]
    QuorumNotMet { have_bps: u32, need_bps: u32 },

    #[error("supermajority not met: {have_bps} < {need_bps} basis points")]
    SupermajorityNotMet { have_bps: u32, need_bps: u32 },

    #[error("insufficient endorsements: {have} < {need}")]
    InsufficientEndorsements { have: u32, need: u32 },

    #[error("proposal phase has not expired yet")]
    PhaseNotExpired,

    #[error("delegation error: {0}")]
    Delegation(String),

    #[error("{0}")]
    Other(String),
}
