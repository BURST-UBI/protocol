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

    #[error("exploration vote failed: quorum or supermajority not met")]
    ExplorationFailed,

    #[error("promotion vote failed: quorum or supermajority not met")]
    PromotionFailed,

    #[error("delegation error: {0}")]
    Delegation(String),

    #[error("only the proposer can withdraw a proposal")]
    NotProposer,

    #[error("cannot delegate to self")]
    SelfDelegation,

    #[error("insufficient BRN: have {have}, need {need}")]
    InsufficientBrn { have: u128, need: u128 },

    #[error("proposer must be verified to submit a proposal")]
    ProposerNotVerified,

    #[error("voting closed, in propagation buffer — counting not yet allowed")]
    PropagationBuffer,

    #[error("voting window has closed for the current phase")]
    VotingClosed,

    #[error("{0}")]
    Other(String),
}
