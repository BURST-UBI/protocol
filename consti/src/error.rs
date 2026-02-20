use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConstiError {
    #[error("amendment {0} not found")]
    AmendmentNotFound(String),

    #[error("amendment is not in the correct phase")]
    WrongPhase,

    #[error("constitutional supermajority not met: {have_bps} < {need_bps} basis points")]
    SupermajorityNotMet { have_bps: u32, need_bps: u32 },

    #[error("quorum not met: {have_bps} < {need_bps} basis points")]
    QuorumNotMet { have_bps: u32, need_bps: u32 },

    #[error("article {0} not found")]
    ArticleNotFound(u64),

    #[error("article {0} is already repealed")]
    ArticleAlreadyRepealed(u64),

    #[error("voter {0} has already voted on this amendment")]
    AlreadyVoted(String),

    #[error("amendment has no operations")]
    NoOperations,

    #[error("{0}")]
    Governance(#[from] burst_governance::GovernanceError),

    #[error("{0}")]
    Other(String),
}
